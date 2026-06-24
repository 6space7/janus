//! Layout: turn the styled DOM into a tree of positioned boxes with geometry.
//!
//! This is the geometry both painters consume — the heart of the dual-painter
//! architecture. It implements a pragmatic block + inline formatting model:
//! block boxes stack vertically and fill their container's content width;
//! inline content (text and inline elements) flows left-to-right into line
//! boxes that wrap at the container edge. Mixed block/inline children are
//! handled by wrapping inline runs in anonymous block boxes.
//!
//! Single-line row flexbox (`display:flex`) is supported: main-axis sizing from
//! `width` + `flex-grow`, `justify-content`, and `align-items`. Text is measured
//! with a simple metric (`0.5em` advance, `1.2em` line height) as a stand-in
//! until layout consumes `janus-text`'s real metrics. Floats, grid, multi-line
//! flex/shrink, positioning, and stacking contexts are the next layer.

use janus_dom::{Dom, NodeData, NodeId};
use janus_style::{
    AlignItems, Color, ComputedStyle, Display, Edges, JustifyContent, Length, StyleMap,
};

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
    flex: bool,
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
            let flex = style.display == Display::Flex;
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
                flex,
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
                flex: false,
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
        flex: false,
        style,
        text: None,
        children,
    }
}

// --- layout -------------------------------------------------------------------

fn layout_block(b: &BuildBox, containing_width: f32, origin_x: f32, origin_y: f32) -> LayoutBox {
    let content_width = content_width_of(b, containing_width);
    layout_block_sized(b, content_width, containing_width, origin_x, origin_y)
}

/// The content-box width of `b`: its explicit width, else it fills the
/// container (minus its own margins/padding/border).
fn content_width_of(b: &BuildBox, containing_width: f32) -> f32 {
    let fs = b.style.font_size;
    let m = resolve_edges(b.style.margin, fs, containing_width);
    let p = resolve_edges(b.style.padding, fs, containing_width);
    let bd = resolve_edges(b.style.border_width, fs, containing_width);
    match b.style.width {
        Length::Auto => {
            (containing_width - m.left - m.right - p.left - p.right - bd.left - bd.right).max(0.0)
        }
        other => other.to_px(fs, containing_width),
    }
}

/// Lay out `b` with an already-decided `content_width` (so flex can force item
/// main sizes). `containing_width` is used only to resolve percentage edges.
fn layout_block_sized(
    b: &BuildBox,
    content_width: f32,
    containing_width: f32,
    origin_x: f32,
    origin_y: f32,
) -> LayoutBox {
    let fs = b.style.font_size;
    let margin = resolve_edges(b.style.margin, fs, containing_width);
    let padding = resolve_edges(b.style.padding, fs, containing_width);
    let border = resolve_edges(b.style.border_width, fs, containing_width);

    let border_x = origin_x + margin.left;
    let border_y = origin_y + margin.top;
    let content_x = border_x + border.left + padding.left;
    let content_y = border_y + border.top + padding.top;

    let mut children = Vec::new();
    let content_height = if b.flex {
        let (boxes, height) = layout_flex(
            &b.children,
            content_x,
            content_y,
            content_width,
            b.style.justify_content,
            b.style.align_items,
        );
        children = boxes;
        height
    } else if b.children.iter().any(|c| c.block) {
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

/// Lay out flex items in a row: main-axis sizing from width + `flex-grow`,
/// `justify-content` distribution of free space, and `align-items` on the
/// cross axis. (Single line, no wrap/shrink/basis-from-content yet.)
fn layout_flex(
    items: &[BuildBox],
    content_x: f32,
    content_y: f32,
    container_width: f32,
    justify: JustifyContent,
    align: AlignItems,
) -> (Vec<LayoutBox>, f32) {
    if items.is_empty() {
        return (Vec::new(), 0.0);
    }
    let n = items.len();

    // Base main sizes (auto basis = 0 for now) and horizontal box extras.
    let bases: Vec<f32> = items
        .iter()
        .map(|it| flex_base_width(it, container_width))
        .collect();
    let extras: Vec<f32> = items
        .iter()
        .map(|it| horizontal_extras(it, container_width))
        .collect();
    let total_base: f32 = bases.iter().zip(&extras).map(|(b, e)| b + e).sum();
    let free = (container_width - total_base).max(0.0);
    let sum_grow: f32 = items.iter().map(|it| it.style.flex_grow).sum();

    let widths: Vec<f32> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            if sum_grow > 0.0 {
                bases[i] + it.style.flex_grow / sum_grow * free
            } else {
                bases[i]
            }
        })
        .collect();

    let outer_w: Vec<f32> = (0..n).map(|i| widths[i] + extras[i]).collect();
    // Measure cross sizes (item heights) by laying each out once.
    let outer_h: Vec<f32> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let laid = layout_block_sized(it, widths[i], container_width, content_x, content_y);
            laid.margin.top + laid.rect.height + laid.margin.bottom
        })
        .collect();
    let cross = outer_h.iter().copied().fold(0.0_f32, f32::max);

    let used: f32 = outer_w.iter().sum();
    let leftover = (container_width - used).max(0.0);
    let (leading, gap) = justify_offsets(justify, leftover, n);

    let mut boxes = Vec::with_capacity(n);
    let mut cursor = content_x + leading;
    for (i, it) in items.iter().enumerate() {
        let cross_off = cross_offset(align, outer_h[i], cross);
        boxes.push(layout_block_sized(
            it,
            widths[i],
            container_width,
            cursor,
            content_y + cross_off,
        ));
        cursor += outer_w[i] + gap;
    }
    (boxes, cross)
}

fn flex_base_width(item: &BuildBox, container_width: f32) -> f32 {
    match item.style.width {
        Length::Auto => 0.0,
        other => other.to_px(item.style.font_size, container_width),
    }
}

fn horizontal_extras(item: &BuildBox, container_width: f32) -> f32 {
    let fs = item.style.font_size;
    let m = resolve_edges(item.style.margin, fs, container_width);
    let p = resolve_edges(item.style.padding, fs, container_width);
    let bd = resolve_edges(item.style.border_width, fs, container_width);
    m.left + m.right + p.left + p.right + bd.left + bd.right
}

fn justify_offsets(justify: JustifyContent, leftover: f32, n: usize) -> (f32, f32) {
    match justify {
        JustifyContent::Start => (0.0, 0.0),
        JustifyContent::Center => (leftover / 2.0, 0.0),
        JustifyContent::End => (leftover, 0.0),
        JustifyContent::SpaceBetween => (
            0.0,
            if n > 1 {
                leftover / (n - 1) as f32
            } else {
                0.0
            },
        ),
        JustifyContent::SpaceAround => {
            let gap = leftover / n as f32;
            (gap / 2.0, gap)
        }
    }
}

fn cross_offset(align: AlignItems, item_height: f32, cross: f32) -> f32 {
    match align {
        AlignItems::Start | AlignItems::Stretch => 0.0,
        AlignItems::Center => (cross - item_height) / 2.0,
        AlignItems::End => cross - item_height,
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

        // An unbreakable word wider than the line overflows visually (CSS
        // `overflow: visible`), but keep the *reported* box within the
        // container so agent geometry never points outside the page.
        let reported_width = advance.min((right - cursor_x).max(0.0));
        boxes.push(LayoutBox {
            node: fragment.node,
            rect: Rect {
                x: cursor_x,
                y: cursor_y,
                width: reported_width,
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
    fn flex_row_distributes_free_space_by_grow() {
        // body content width = 400 - 16 = 384. Item 1 is 100px; item 2 has
        // flex:1 so it absorbs the remaining 284px.
        let root = layout(
            "<html><body><div style=\"display:flex\">\
             <div style=\"width:100px;height:20px\"></div>\
             <div style=\"flex:1;height:20px\"></div></div></body></html>",
            "",
            400.0,
        );
        let boxes = collect(&root);
        let has = |x: f32, w: f32| {
            boxes
                .iter()
                .any(|b| (b.rect.x - x).abs() < 0.5 && (b.rect.width - w).abs() < 0.5)
        };
        assert!(has(8.0, 100.0), "first item at x=8 w=100");
        assert!(has(108.0, 284.0), "grown item at x=108 w=284");
    }

    #[test]
    fn flex_justify_content_end_right_aligns() {
        let root = layout(
            "<html><body><div style=\"display:flex;justify-content:flex-end\">\
             <div style=\"width:50px;height:10px\"></div>\
             <div style=\"width:50px;height:10px\"></div></div></body></html>",
            "",
            400.0,
        );
        let boxes = collect(&root);
        // leftover = 384 - 100 = 284 of leading; items sit at the right edge.
        assert!(boxes
            .iter()
            .any(|b| (b.rect.x - 292.0).abs() < 0.5 && (b.rect.width - 50.0).abs() < 0.5));
        assert!(boxes
            .iter()
            .any(|b| (b.rect.x - 342.0).abs() < 0.5 && (b.rect.width - 50.0).abs() < 0.5));
    }

    #[test]
    fn over_wide_word_box_stays_within_container() {
        // div content width = 50px at x=8 → right edge 58. An unbreakable word
        // overflows visually, but its reported box must not exceed the box.
        let root = layout(
            "<html><body><div style=\"width:50px\">verylongunbreakableword</div></body></html>",
            "",
            400.0,
        );
        let frag = collect(&root)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("text fragment");
        assert!(
            frag.rect.x + frag.rect.width <= 58.5,
            "fragment {:?} escapes container",
            frag.rect
        );
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
