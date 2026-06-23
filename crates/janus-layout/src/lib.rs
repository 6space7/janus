//! Layout: turn the styled DOM into a tree of positioned boxes with geometry.
//!
//! This is the geometry both painters consume — the heart of the dual-painter
//! architecture. It implements a pragmatic block + inline formatting model:
//! block boxes stack vertically and fill their container's content width;
//! inline content (text and inline elements) flows left-to-right into line
//! boxes that wrap at the container edge. Mixed block/inline children are
//! handled by wrapping inline runs in anonymous block boxes.
//!
//! Text is measured with a simple metric (`0.5em` advance, `1.2em` line
//! height) as a stand-in until `janus-text` brings real shaping (rustybuzz /
//! swash). Floats, flex, grid, positioning, and stacking contexts are the next
//! layer; the box/fragment split and the geometry contract are established here.

use janus_dom::{Dom, NodeData, NodeId};
use janus_style::{Color, ComputedStyle, Display, Edges, Length, StyleMap};

/// An axis-aligned rectangle in CSS pixels (top-left origin).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// A positioned box: its border-box rectangle plus everything a painter needs.
#[derive(Clone, Debug)]
pub struct LayoutBox {
    /// The source node, or `None` for an anonymous/inline-fragment box.
    pub node: Option<NodeId>,
    /// The border-box rectangle (margins excluded).
    pub rect: Rect,
    /// Resolved margins (used by the parent to advance; excluded from `rect`).
    pub margin: Edges<f32>,
    /// Resolved padding.
    pub padding: Edges<f32>,
    /// Resolved border widths.
    pub border: Edges<f32>,
    /// Background fill color.
    pub background_color: Color,
    /// Border color.
    pub border_color: Color,
    /// Text color (for a text fragment).
    pub text_color: Color,
    /// Font size in px.
    pub font_size: f32,
    /// Text content, for a text-fragment box.
    pub text: Option<String>,
    /// Child boxes.
    pub children: Vec<LayoutBox>,
}

impl LayoutBox {
    /// Visit this box and every descendant, pre-order.
    pub fn for_each(&self, f: &mut impl FnMut(&LayoutBox)) {
        f(self);
        for child in &self.children {
            child.for_each(f);
        }
    }
}

/// Lay out the document at `viewport_width`, returning the root box (the root
/// element, usually `<html>`), or `None` if there is no rendered root.
#[must_use]
pub fn layout_document(dom: &Dom, styles: &StyleMap, viewport_width: f32) -> Option<LayoutBox> {
    let root = dom
        .children(dom.document())
        .iter()
        .copied()
        .find(|&n| styles.contains_key(&n))?;
    let initial = ComputedStyle::initial();
    let tree = build(dom, styles, root, &initial).into_iter().next()?;
    Some(layout_block(&tree, viewport_width, 0.0, 0.0))
}

// --- box-tree construction ----------------------------------------------------

struct BuildBox {
    node: Option<NodeId>,
    block: bool,
    style: ComputedStyle,
    text: Option<String>,
    children: Vec<BuildBox>,
}

fn build(
    dom: &Dom,
    styles: &StyleMap,
    node: NodeId,
    parent_style: &ComputedStyle,
) -> Vec<BuildBox> {
    let Some(node_ref) = dom.node(node) else {
        return Vec::new();
    };
    match &node_ref.data {
        NodeData::Element(_) => {
            let Some(style) = styles.get(&node) else {
                return Vec::new();
            };
            if style.display == Display::None {
                return Vec::new();
            }
            let block = style.display != Display::Inline;
            let mut children = Vec::new();
            for &child in dom.children(node) {
                children.extend(build(dom, styles, child, style));
            }
            if block {
                children = wrap_inline_runs(children, style);
            }
            vec![BuildBox {
                node: Some(node),
                block,
                style: style.clone(),
                text: None,
                children,
            }]
        }
        NodeData::Text(text) => {
            let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if collapsed.is_empty() {
                return Vec::new();
            }
            vec![BuildBox {
                node: Some(node),
                block: false,
                style: parent_style.clone(),
                text: Some(collapsed),
                children: Vec::new(),
            }]
        }
        _ => Vec::new(),
    }
}

/// If a block container mixes block- and inline-level children, wrap each run
/// of inline children in an anonymous block box so the container has a uniform
/// formatting context.
fn wrap_inline_runs(children: Vec<BuildBox>, parent_style: &ComputedStyle) -> Vec<BuildBox> {
    if !children.iter().any(|c| c.block) {
        return children; // pure inline formatting context
    }
    let mut out: Vec<BuildBox> = Vec::new();
    let mut run: Vec<BuildBox> = Vec::new();
    for child in children {
        if child.block {
            if !run.is_empty() {
                out.push(anonymous_block(std::mem::take(&mut run), parent_style));
            }
            out.push(child);
        } else {
            run.push(child);
        }
    }
    if !run.is_empty() {
        out.push(anonymous_block(run, parent_style));
    }
    out
}

fn anonymous_block(children: Vec<BuildBox>, parent_style: &ComputedStyle) -> BuildBox {
    let mut style = parent_style.clone();
    style.display = Display::Block;
    style.margin = Edges::all(Length::Px(0.0));
    style.padding = Edges::all(Length::Px(0.0));
    style.border_width = Edges::all(Length::Px(0.0));
    style.background_color = Color::TRANSPARENT;
    style.width = Length::Auto;
    style.height = Length::Auto;
    BuildBox {
        node: None,
        block: true,
        style,
        text: None,
        children,
    }
}

// --- layout -------------------------------------------------------------------

fn layout_block(b: &BuildBox, containing_width: f32, origin_x: f32, origin_y: f32) -> LayoutBox {
    let fs = b.style.font_size;
    let margin = resolve_edges(b.style.margin, fs, containing_width);
    let padding = resolve_edges(b.style.padding, fs, containing_width);
    let border = resolve_edges(b.style.border_width, fs, containing_width);

    let content_width = match b.style.width {
        Length::Auto => (containing_width
            - margin.left
            - margin.right
            - padding.left
            - padding.right
            - border.left
            - border.right)
            .max(0.0),
        other => other.to_px(fs, containing_width),
    };

    let border_x = origin_x + margin.left;
    let border_y = origin_y + margin.top;
    let content_x = border_x + border.left + padding.left;
    let content_y = border_y + border.top + padding.top;

    let mut children = Vec::new();
    let content_height = if b.children.iter().any(|c| c.block) {
        let mut cursor_y = content_y;
        for child in &b.children {
            let laid = layout_block(child, content_width, content_x, cursor_y);
            cursor_y += laid.margin.top + laid.rect.height + laid.margin.bottom;
            children.push(laid);
        }
        cursor_y - content_y
    } else if b.children.is_empty() {
        0.0
    } else {
        let (boxes, height) = layout_inline(&b.children, content_x, content_y, content_width);
        children = boxes;
        height
    };

    let final_height = match b.style.height {
        Length::Auto => content_height,
        other => other.to_px(fs, containing_width),
    };

    LayoutBox {
        node: b.node,
        rect: Rect {
            x: border_x,
            y: border_y,
            width: content_width + padding.left + padding.right + border.left + border.right,
            height: final_height + padding.top + padding.bottom + border.top + border.bottom,
        },
        margin,
        padding,
        border,
        background_color: b.style.background_color,
        border_color: b.style.border_color,
        text_color: b.style.color,
        font_size: fs,
        text: None,
        children,
    }
}

fn layout_inline(
    children: &[BuildBox],
    content_x: f32,
    content_y: f32,
    content_width: f32,
) -> (Vec<LayoutBox>, f32) {
    let mut fragments: Vec<Fragment> = Vec::new();
    collect_fragments(children, &mut fragments);

    let mut boxes = Vec::new();
    let right = content_x + content_width;
    let mut cursor_x = content_x;
    let mut cursor_y = content_y;
    let mut line_height = 0.0f32;

    for fragment in fragments {
        let advance = fragment.word.chars().count() as f32 * 0.5 * fragment.font_size;
        let lh = 1.2 * fragment.font_size;
        let space = 0.5 * fragment.font_size;

        if cursor_x > content_x && cursor_x + advance > right {
            cursor_x = content_x;
            cursor_y += line_height.max(lh);
            line_height = 0.0;
        }

        boxes.push(LayoutBox {
            node: fragment.node,
            rect: Rect {
                x: cursor_x,
                y: cursor_y,
                width: advance,
                height: lh,
            },
            margin: Edges::all(0.0),
            padding: Edges::all(0.0),
            border: Edges::all(0.0),
            background_color: Color::TRANSPARENT,
            border_color: Color::TRANSPARENT,
            text_color: fragment.color,
            font_size: fragment.font_size,
            text: Some(fragment.word),
            children: Vec::new(),
        });

        cursor_x += advance + space;
        line_height = line_height.max(lh);
    }

    let total_height = if boxes.is_empty() {
        0.0
    } else {
        (cursor_y - content_y) + line_height
    };
    (boxes, total_height)
}

struct Fragment {
    word: String,
    color: Color,
    font_size: f32,
    node: Option<NodeId>,
}

fn collect_fragments(children: &[BuildBox], out: &mut Vec<Fragment>) {
    for child in children {
        if let Some(text) = &child.text {
            for word in text.split_whitespace() {
                out.push(Fragment {
                    word: word.to_string(),
                    color: child.style.color,
                    font_size: child.style.font_size,
                    node: child.node,
                });
            }
        } else if !child.block {
            collect_fragments(&child.children, out);
        }
    }
}

fn resolve_edges(edges: Edges<Length>, font_size: f32, basis: f32) -> Edges<f32> {
    Edges {
        top: edges.top.to_px(font_size, basis),
        right: edges.right.to_px(font_size, basis),
        bottom: edges.bottom.to_px(font_size, basis),
        left: edges.left.to_px(font_size, basis),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_css::Stylesheet;

    fn layout(html: &str, css: &str, width: f32) -> LayoutBox {
        let dom = janus_html::parse(html);
        let styles = janus_style::compute_styles(&dom, &Stylesheet::parse(css));
        layout_document(&dom, &styles, width).expect("a rendered root")
    }

    fn collect(root: &LayoutBox) -> Vec<LayoutBox> {
        let mut v = Vec::new();
        root.for_each(&mut |b| v.push(b.clone()));
        v
    }

    #[test]
    fn block_fills_width_minus_body_margin() {
        // <html> fills 800; <body> insets by its 8px UA margin → 784 content.
        let root = layout("<html><body></body></html>", "", 800.0);
        assert_eq!(root.rect.width, 800.0);
        let body = &root.children[0];
        assert_eq!(body.rect.x, 8.0);
        assert_eq!(body.rect.width, 784.0);
    }

    #[test]
    fn explicit_size_and_position() {
        let root = layout(
            "<html><body><div style=\"width:100px;height:50px\"></div></body></html>",
            "",
            800.0,
        );
        let sized = collect(&root)
            .into_iter()
            .find(|b| (b.rect.width - 100.0).abs() < 0.01 && (b.rect.height - 50.0).abs() < 0.01)
            .expect("the sized div");
        // body content origin is (8, 8): body border at (8,8), no padding/border.
        assert!((sized.rect.x - 8.0).abs() < 0.01, "x was {}", sized.rect.x);
        assert!((sized.rect.y - 8.0).abs() < 0.01, "y was {}", sized.rect.y);
    }

    #[test]
    fn blocks_stack_vertically() {
        let root = layout(
            "<html><body><div style=\"height:30px\"></div><div style=\"height:40px\"></div></body></html>",
            "",
            800.0,
        );
        let heights_and_y: Vec<(f32, f32)> = collect(&root)
            .iter()
            .filter(|b| b.rect.height == 30.0 || b.rect.height == 40.0)
            .map(|b| (b.rect.height, b.rect.y))
            .collect();
        // First div at y=8, second stacked directly below at y=38.
        assert!(heights_and_y.contains(&(30.0, 8.0)), "{heights_and_y:?}");
        assert!(heights_and_y.contains(&(40.0, 38.0)), "{heights_and_y:?}");
    }

    #[test]
    fn text_flows_into_word_fragments() {
        let root = layout("<html><body><p>hello world</p></body></html>", "", 800.0);
        let words: Vec<String> = collect(&root).into_iter().filter_map(|b| b.text).collect();
        assert!(words.contains(&"hello".to_string()), "{words:?}");
        assert!(words.contains(&"world".to_string()), "{words:?}");
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        // Narrow container forces wrapping; later words get a larger y.
        let root = layout(
            "<html><body><p>aaaa bbbb cccc dddd eeee ffff gggg</p></body></html>",
            "",
            60.0,
        );
        let ys: Vec<f32> = collect(&root)
            .iter()
            .filter_map(|b| b.text.as_ref().map(|_| b.rect.y))
            .collect();
        let max_y = ys.iter().copied().fold(f32::MIN, f32::max);
        let min_y = ys.iter().copied().fold(f32::MAX, f32::min);
        assert!(max_y > min_y, "expected multiple lines, ys = {ys:?}");
    }
}
