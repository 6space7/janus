//! The cascade: match selectors against the [`Dom`] and resolve a
//! [`ComputedStyle`] for every element.
//!
//! Pipeline per element: start from the inherited base, apply the UA defaults
//! for its tag, then apply matching author declarations ordered by
//! `(origin/importance, specificity, source order)`, then the inline
//! `style="…"`. `font-size` is resolved to absolute px during the cascade so it
//! can be inherited; other lengths stay specified for `janus-layout` to resolve.
//!
//! This is a correct-enough core cascade for P0 (the curated property set
//! below). Parallel restyle and fine-grained invalidation come later under the
//! speed pillar; the Stylo calibration oracle backs correctness.

mod values;

pub use values::{
    parse_color, parse_edges, parse_length, Color, Display, Edges, Length, TextAlign,
};

use std::collections::HashMap;

use janus_css::{Declaration, Selector, SimpleSelector, Specificity, Stylesheet};
use janus_dom::{Dom, NodeId};

/// Computed values for the curated P0 property set.
#[derive(Clone, PartialEq, Debug)]
pub struct ComputedStyle {
    /// `display`.
    pub display: Display,
    /// `color` (inherited).
    pub color: Color,
    /// `background-color`.
    pub background_color: Color,
    /// `font-size` in px (inherited, already resolved).
    pub font_size: f32,
    /// `font-weight` (inherited).
    pub font_weight: u16,
    /// `width`.
    pub width: Length,
    /// `height`.
    pub height: Length,
    /// `margin`.
    pub margin: Edges<Length>,
    /// `padding`.
    pub padding: Edges<Length>,
    /// `border-width`.
    pub border_width: Edges<Length>,
    /// `border-color`.
    pub border_color: Color,
    /// `text-align` (inherited).
    pub text_align: TextAlign,
}

impl ComputedStyle {
    /// The initial values (root with no parent).
    #[must_use]
    pub fn initial() -> Self {
        Self {
            display: Display::Inline,
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            font_size: 16.0,
            font_weight: 400,
            width: Length::Auto,
            height: Length::Auto,
            margin: Edges::all(Length::Px(0.0)),
            padding: Edges::all(Length::Px(0.0)),
            border_width: Edges::all(Length::Px(0.0)),
            border_color: Color::BLACK,
            text_align: TextAlign::Left,
        }
    }

    /// A fresh style seeded with the inherited properties of `parent`.
    fn inherited_from(parent: &ComputedStyle) -> Self {
        let mut s = ComputedStyle::initial();
        s.color = parent.color;
        s.font_size = parent.font_size;
        s.font_weight = parent.font_weight;
        s.text_align = parent.text_align;
        s
    }
}

/// Computed styles keyed by node. Non-element nodes inherit from their parent
/// element at layout time and are not present here.
pub type StyleMap = HashMap<NodeId, ComputedStyle>;

/// Compute styles for every rendered element in `dom` under `sheet`.
#[must_use]
pub fn compute_styles(dom: &Dom, sheet: &Stylesheet) -> StyleMap {
    let mut map = StyleMap::new();
    let root = ComputedStyle::initial();
    for &child in dom.children(dom.document()) {
        compute_node(dom, sheet, child, &root, &mut map);
    }
    map
}

fn compute_node(
    dom: &Dom,
    sheet: &Stylesheet,
    node: NodeId,
    parent_style: &ComputedStyle,
    map: &mut StyleMap,
) {
    let Some(name) = dom.element_name(node) else {
        return; // text/comment/doctype carry no computed style of their own
    };

    let mut style = ComputedStyle::inherited_from(parent_style);
    apply_ua_defaults(&mut style, name);

    // Collect matching author declarations tagged with cascade priority.
    let mut decls: Vec<(u8, Specificity, usize, &Declaration)> = Vec::new();
    for (order, rule) in sheet.rules.iter().enumerate() {
        let mut best: Option<Specificity> = None;
        for selector in &rule.selectors {
            if selector_matches(dom, node, selector) {
                let spec = selector.specificity();
                best = Some(best.map_or(spec, |b| b.max(spec)));
            }
        }
        if let Some(spec) = best {
            for d in &rule.declarations {
                decls.push((if d.important { 2 } else { 0 }, spec, order, d));
            }
        }
    }
    // Inline style is its own origin, above author selectors of equal importance.
    let inline = dom
        .attr(node, "style")
        .map(janus_css::parse_declarations)
        .unwrap_or_default();
    for d in &inline {
        decls.push((
            if d.important { 3 } else { 1 },
            Specificity::default(),
            usize::MAX,
            d,
        ));
    }

    // Ascending priority — later application wins.
    decls.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, _, _, d) in &decls {
        apply_declaration(&mut style, &d.name, &d.value, parent_style.font_size);
    }

    let display = style.display;
    map.insert(node, style.clone());

    if display != Display::None {
        for &child in dom.children(node) {
            compute_node(dom, sheet, child, &style, map);
        }
    }
}

fn apply_ua_defaults(style: &mut ComputedStyle, tag: &str) {
    style.display = ua_display(tag);
    match tag {
        "body" => style.margin = Edges::all(Length::Px(8.0)),
        "p" => {
            style.margin.top = Length::Em(1.0);
            style.margin.bottom = Length::Em(1.0);
        }
        "h1" => {
            style.font_size = 32.0;
            style.font_weight = 700;
            style.margin.top = Length::Em(0.67);
            style.margin.bottom = Length::Em(0.67);
        }
        "h2" => {
            style.font_size = 24.0;
            style.font_weight = 700;
        }
        "h3" => {
            style.font_size = 18.72;
            style.font_weight = 700;
        }
        "h4" => style.font_weight = 700,
        "h5" => {
            style.font_size = 13.28;
            style.font_weight = 700;
        }
        "h6" => {
            style.font_size = 10.72;
            style.font_weight = 700;
        }
        "b" | "strong" => style.font_weight = 700,
        "a" => style.color = Color::rgb(0, 0, 238), // default link blue
        "ul" | "ol" => {
            style.margin.top = Length::Em(1.0);
            style.margin.bottom = Length::Em(1.0);
            style.padding.left = Length::Px(40.0); // list indentation
        }
        "blockquote" => {
            style.margin = Edges {
                top: Length::Em(1.0),
                right: Length::Px(40.0),
                bottom: Length::Em(1.0),
                left: Length::Px(40.0),
            };
        }
        "pre" => {
            style.margin.top = Length::Em(1.0);
            style.margin.bottom = Length::Em(1.0);
        }
        _ => {}
    }
}

fn ua_display(tag: &str) -> Display {
    match tag {
        "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "section" | "article"
        | "header" | "footer" | "nav" | "main" | "aside" | "figure" | "figcaption"
        | "blockquote" | "pre" | "ul" | "ol" | "dl" | "dd" | "dt" | "table" | "form"
        | "fieldset" | "address" | "hr" | "html" | "body" => Display::Block,
        "li" => Display::ListItem,
        "head" | "title" | "meta" | "link" | "style" | "script" | "base" => Display::None,
        _ => Display::Inline,
    }
}

fn apply_declaration(style: &mut ComputedStyle, name: &str, value: &str, parent_font_size: f32) {
    match name {
        "display" => {
            if let Some(d) = parse_display(value) {
                style.display = d;
            }
        }
        "color" => {
            if let Some(c) = parse_color(value) {
                style.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = parse_color(value) {
                style.background_color = c;
            }
        }
        "background" => {
            if let Some(c) = first_color_token(value) {
                style.background_color = c;
            }
        }
        "font-size" => {
            if let Some(l) = parse_length(value) {
                style.font_size = resolve_font_size(l, parent_font_size);
            }
        }
        "font-weight" => {
            if let Some(w) = parse_font_weight(value) {
                style.font_weight = w;
            }
        }
        "width" => {
            if let Some(l) = parse_length(value) {
                style.width = l;
            }
        }
        "height" => {
            if let Some(l) = parse_length(value) {
                style.height = l;
            }
        }
        "margin" => {
            if let Some(e) = parse_edges(value) {
                style.margin = e;
            }
        }
        "margin-top" => set_edge(&mut style.margin.top, value),
        "margin-right" => set_edge(&mut style.margin.right, value),
        "margin-bottom" => set_edge(&mut style.margin.bottom, value),
        "margin-left" => set_edge(&mut style.margin.left, value),
        "padding" => {
            if let Some(e) = parse_edges(value) {
                style.padding = e;
            }
        }
        "padding-top" => set_edge(&mut style.padding.top, value),
        "padding-right" => set_edge(&mut style.padding.right, value),
        "padding-bottom" => set_edge(&mut style.padding.bottom, value),
        "padding-left" => set_edge(&mut style.padding.left, value),
        "border-width" => {
            if let Some(e) = parse_edges(value) {
                style.border_width = e;
            }
        }
        "border-color" => {
            if let Some(c) = parse_color(value) {
                style.border_color = c;
            }
        }
        "border" => apply_border_shorthand(style, value),
        "text-align" => {
            if let Some(t) = parse_text_align(value) {
                style.text_align = t;
            }
        }
        _ => {}
    }
}

fn set_edge(slot: &mut Length, value: &str) {
    if let Some(l) = parse_length(value) {
        *slot = l;
    }
}

fn resolve_font_size(l: Length, parent_font_size: f32) -> f32 {
    match l {
        Length::Px(v) => v,
        Length::Em(v) => v * parent_font_size,
        Length::Percent(p) => p / 100.0 * parent_font_size,
        Length::Auto => parent_font_size,
    }
}

fn first_color_token(value: &str) -> Option<Color> {
    value.split_whitespace().find_map(parse_color)
}

fn apply_border_shorthand(style: &mut ComputedStyle, value: &str) {
    for token in value.split_whitespace() {
        if let Some(l) = parse_length(token) {
            style.border_width = Edges::all(l);
        } else if let Some(c) = parse_color(token) {
            style.border_color = c;
        }
        // The line style (solid/dashed/…) is recorded later.
    }
}

fn parse_display(value: &str) -> Option<Display> {
    match value.trim().to_ascii_lowercase().as_str() {
        "inline" => Some(Display::Inline),
        "block" => Some(Display::Block),
        "inline-block" => Some(Display::InlineBlock),
        "list-item" => Some(Display::ListItem),
        "none" => Some(Display::None),
        _ => None,
    }
}

fn parse_font_weight(value: &str) -> Option<u16> {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" => Some(400),
        "bold" | "bolder" => Some(700),
        "lighter" => Some(300),
        other => other.parse::<u16>().ok(),
    }
}

fn parse_text_align(value: &str) -> Option<TextAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "start" => Some(TextAlign::Left),
        "center" => Some(TextAlign::Center),
        "right" | "end" => Some(TextAlign::Right),
        _ => None,
    }
}

// --- selector matching --------------------------------------------------------

fn selector_matches(dom: &Dom, node: NodeId, selector: &Selector) -> bool {
    let Some((subject, ancestors)) = selector.compounds.split_last() else {
        return false;
    };
    if !compound_matches(dom, node, subject) {
        return false;
    }
    // Match ancestor compounds right-to-left against ancestors (descendant).
    let mut current = dom.parent(node);
    for compound in ancestors.iter().rev() {
        loop {
            let Some(ancestor) = current else {
                return false;
            };
            current = dom.parent(ancestor);
            if dom.element_name(ancestor).is_some() && compound_matches(dom, ancestor, compound) {
                break;
            }
        }
    }
    true
}

fn compound_matches(dom: &Dom, node: NodeId, compound: &SimpleSelector) -> bool {
    let Some(name) = dom.element_name(node) else {
        return false;
    };
    if let Some(tag) = &compound.tag {
        if name != tag.as_str() {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        if dom.attr(node, "id") != Some(id.as_str()) {
            return false;
        }
    }
    if !compound.classes.is_empty() {
        let class_attr = dom.attr(node, "class").unwrap_or_default();
        for needed in &compound.classes {
            if !class_attr.split_whitespace().any(|c| c == needed) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(dom: &Dom, tag: &str) -> NodeId {
        fn walk(dom: &Dom, node: NodeId, tag: &str) -> Option<NodeId> {
            if dom.element_name(node) == Some(tag) {
                return Some(node);
            }
            for &c in dom.children(node) {
                if let Some(found) = walk(dom, c, tag) {
                    return Some(found);
                }
            }
            None
        }
        walk(dom, dom.document(), tag).expect("element present")
    }

    #[test]
    fn specificity_wins_and_font_size_resolves() {
        let dom = janus_html::parse("<html><body><p class=\"lead\">hi</p></body></html>");
        let sheet = Stylesheet::parse("p { color: red; } .lead { color: blue; font-size: 20px; }");
        let map = compute_styles(&dom, &sheet);
        let p = &map[&find(&dom, "p")];
        assert_eq!(p.color, Color::rgb(0, 0, 255)); // .lead (0,1,0) beats p (0,0,1)
        assert_eq!(p.font_size, 20.0);
        assert_eq!(p.display, Display::Block); // UA default
    }

    #[test]
    fn color_inherits_from_ancestor() {
        let dom = janus_html::parse("<html><body><div><span>x</span></div></body></html>");
        let sheet = Stylesheet::parse("body { color: green; }");
        let map = compute_styles(&dom, &sheet);
        let span = &map[&find(&dom, "span")];
        assert_eq!(span.color, Color::rgb(0, 128, 0));
        assert_eq!(span.display, Display::Inline);
    }

    #[test]
    fn inline_style_overrides_author_rule() {
        let dom = janus_html::parse("<html><body><p style=\"color: red\">x</p></body></html>");
        let sheet = Stylesheet::parse("p { color: blue; }");
        let map = compute_styles(&dom, &sheet);
        assert_eq!(map[&find(&dom, "p")].color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn important_beats_inline_normal() {
        let dom = janus_html::parse("<html><body><p style=\"color: red\">x</p></body></html>");
        let sheet = Stylesheet::parse("p { color: blue !important; }");
        let map = compute_styles(&dom, &sheet);
        assert_eq!(map[&find(&dom, "p")].color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn descendant_combinator_matches() {
        let dom = janus_html::parse(
            "<html><body><div class=\"box\"><a>in</a></div><a>out</a></body></html>",
        );
        let sheet = Stylesheet::parse(".box a { color: red; }");
        let map = compute_styles(&dom, &sheet);
        let reds = map
            .values()
            .filter(|s| s.color == Color::rgb(255, 0, 0))
            .count();
        assert_eq!(reds, 1);
    }

    #[test]
    fn head_subtree_is_display_none() {
        let dom = janus_html::parse("<html><head><title>T</title></head><body>x</body></html>");
        let map = compute_styles(&dom, &Stylesheet::default());
        assert_eq!(map[&find(&dom, "head")].display, Display::None);
    }

    #[test]
    fn ua_defaults_for_links_and_lists() {
        let dom =
            janus_html::parse("<html><body><a href=\"/\">x</a><ul><li>y</li></ul></body></html>");
        let map = compute_styles(&dom, &Stylesheet::default());
        // Links default to blue; an author rule can still override.
        assert_eq!(map[&find(&dom, "a")].color, Color::rgb(0, 0, 238));
        // Lists are indented via padding-left.
        assert_eq!(map[&find(&dom, "ul")].padding.left, Length::Px(40.0));
    }
}
