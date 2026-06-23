//! The Semantic Surface — the LLM-first projection of the page.
//!
//! From the *same* layout geometry the human painter consumes, this builds a
//! ref-tagged, box-grounded semantic tree for agents: per node a role, an
//! accessible name, **layout box geometry** (the signal pure-DOM agents lack
//! and pure-vision agents lose), a per-snapshot `ref` (`e1`, `e2`, …), and a
//! **stable id** derived from `role + name + structural path` that is designed
//! to survive re-renders (so an agent's handle does not die on every mutation).
//!
//! Affordances are computed in-engine from the tag + computed style + geometry,
//! not read from author ARIA. Non-rendered subtrees (`display:none`, `<head>`)
//! are excluded — the basis of the provenance/visibility gating that keeps
//! hidden/injected text off the action surface.
//!
//! P0 emits the full visible tree; tiered summary + expand-on-demand and the
//! nonce-bound TOCTOU-safe `act` revalidation come next.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use janus_dom::{Dom, NodeData, NodeId};
use janus_layout::{LayoutBox, Rect};
use janus_style::{Display, StyleMap};

/// Integer box geometry (rounded CSS px) for an element.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    /// Left.
    pub x: i32,
    /// Top.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// One node of the semantic tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SemanticNode {
    /// Per-snapshot handle (`e1`, `e2`, …, or `root`).
    pub ref_id: String,
    /// Re-render-stable id derived from role + name + structural path.
    pub stable_id: String,
    /// The computed role (e.g. `link`, `heading`, `button`, `generic`).
    pub role: String,
    /// The accessible name (for named roles), else empty.
    pub name: String,
    /// Layout box geometry, if the node was laid out.
    pub geometry: Option<Geometry>,
    /// For links/areas: the raw `href` target (unresolved); else `None`.
    pub href: Option<String>,
    /// State flags computed from the element (e.g. `disabled`, `checked`).
    pub state: Vec<String>,
    /// Child semantic nodes (document order).
    pub children: Vec<SemanticNode>,
}

/// Build the semantic snapshot for a styled, laid-out document.
#[must_use]
pub fn build_snapshot(dom: &Dom, styles: &StyleMap, layout_root: &LayoutBox) -> SemanticNode {
    let geometry = geometry_map(layout_root);
    let mut counter = 0usize;
    let mut children = Vec::new();
    for &child in dom.children(dom.document()) {
        build_nodes(
            dom,
            styles,
            &geometry,
            child,
            "",
            &mut counter,
            &mut children,
        );
    }
    SemanticNode {
        ref_id: "root".to_string(),
        stable_id: "root".to_string(),
        role: "document".to_string(),
        name: String::new(),
        geometry: None,
        href: None,
        state: Vec::new(),
        children,
    }
}

/// Build and serialize the snapshot to the compact text form in one call.
#[must_use]
pub fn snapshot_text(dom: &Dom, styles: &StyleMap, layout_root: &LayoutBox) -> String {
    to_text(&build_snapshot(dom, styles, layout_root))
}

/// Serialize a semantic tree to a compact, indented, agent-friendly text form:
/// `- role "name" [ref=e1] @x,y WxH`.
#[must_use]
pub fn to_text(root: &SemanticNode) -> String {
    let mut out = String::new();
    write_node(root, 0, &mut out);
    out
}

/// Format a single node as one line (no indent, no children): the same
/// `role "name" [ref=eN] @x,y WxH -> href` form used by [`to_text`].
#[must_use]
pub fn node_line(node: &SemanticNode) -> String {
    let mut s = String::new();
    s.push_str(&node.role);
    if !node.name.is_empty() {
        s.push_str(" \"");
        s.push_str(&node.name);
        s.push('"');
    }
    s.push_str(" [ref=");
    s.push_str(&node.ref_id);
    s.push(']');
    if let Some(g) = node.geometry {
        s.push_str(&format!(" @{},{} {}x{}", g.x, g.y, g.width, g.height));
    }
    if let Some(href) = &node.href {
        s.push_str(" -> ");
        s.push_str(href);
    }
    for flag in &node.state {
        s.push_str(" [");
        s.push_str(flag);
        s.push(']');
    }
    s
}

fn write_node(node: &SemanticNode, depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str("- ");
    out.push_str(&node_line(node));
    out.push('\n');
    for child in &node.children {
        write_node(child, depth + 1, out);
    }
}

fn build_nodes(
    dom: &Dom,
    styles: &StyleMap,
    geometry: &HashMap<NodeId, Rect>,
    node: NodeId,
    parent_path: &str,
    counter: &mut usize,
    out: &mut Vec<SemanticNode>,
) {
    let Some(tag) = dom.element_name(node) else {
        return; // text/comment contribute to a parent's accessible name, not nodes
    };
    let Some(style) = styles.get(&node) else {
        return;
    };
    if style.display == Display::None {
        return; // provenance: not rendered ⇒ off the action surface
    }

    let path = format!("{parent_path}/{tag}");

    // <html>/<body> are transparent wrappers: flatten their children upward.
    if matches!(tag, "html" | "body") {
        for &child in dom.children(node) {
            build_nodes(dom, styles, geometry, child, &path, counter, out);
        }
        return;
    }

    let role = role_of(dom, node, tag);
    let name = if is_named_role(role) {
        accessible_name(dom, node, tag)
    } else {
        String::new()
    };
    *counter += 1;
    let ref_id = format!("e{counter}");
    let stable_id = stable_id(role, &name, &path);
    let href = if matches!(tag, "a" | "area") {
        dom.attr(node, "href").map(str::to_string)
    } else {
        None
    };
    let state = compute_state(dom, node, role);
    // Inline elements have no box of their own; fall back to the union of
    // their descendants' boxes so links/spans still carry geometry.
    let geom = resolved_rect(dom, node, geometry).map(to_geometry);

    let mut children = Vec::new();
    for &child in dom.children(node) {
        build_nodes(dom, styles, geometry, child, &path, counter, &mut children);
    }

    out.push(SemanticNode {
        ref_id,
        stable_id,
        role: role.to_string(),
        name,
        geometry: geom,
        href,
        state,
        children,
    });
}

/// Compute the ARIA-ish role, resolving `<input>` by its `type`.
fn role_of(dom: &Dom, node: NodeId, tag: &str) -> &'static str {
    if tag == "input" {
        return match dom
            .attr(node, "type")
            .unwrap_or("text")
            .to_ascii_lowercase()
            .as_str()
        {
            "checkbox" => "checkbox",
            "radio" => "radio",
            "submit" | "button" | "reset" | "image" => "button",
            "hidden" => "generic",
            _ => "textbox",
        };
    }
    role_for(tag)
}

/// State flags an agent cares about: `disabled`, and `checked` for toggles.
fn compute_state(dom: &Dom, node: NodeId, role: &str) -> Vec<String> {
    let mut state = Vec::new();
    if dom.attr(node, "disabled").is_some() {
        state.push("disabled".to_string());
    }
    if matches!(role, "checkbox" | "radio") && dom.attr(node, "checked").is_some() {
        state.push("checked".to_string());
    }
    state
}

fn role_for(tag: &str) -> &'static str {
    match tag {
        "a" => "link",
        "button" => "button",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "img" => "image",
        "p" => "paragraph",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "nav" => "navigation",
        "header" => "banner",
        "footer" => "contentinfo",
        "main" => "main",
        "aside" => "complementary",
        "input" | "textarea" => "textbox",
        "select" => "combobox",
        "table" => "table",
        "form" => "form",
        _ => "generic",
    }
}

fn is_named_role(role: &str) -> bool {
    matches!(
        role,
        "link"
            | "button"
            | "heading"
            | "image"
            | "paragraph"
            | "listitem"
            | "textbox"
            | "combobox"
            | "checkbox"
            | "radio"
    )
}

fn accessible_name(dom: &Dom, node: NodeId, tag: &str) -> String {
    if let Some(label) = dom.attr(node, "aria-label") {
        let label = label.trim();
        if !label.is_empty() {
            return label.to_string();
        }
    }
    match tag {
        "img" => dom.attr(node, "alt").unwrap_or_default().trim().to_string(),
        "input" | "textarea" => dom
            .attr(node, "value")
            .or_else(|| dom.attr(node, "placeholder"))
            .unwrap_or_default()
            .trim()
            .to_string(),
        _ => text_content(dom, node),
    }
}

fn text_content(dom: &Dom, node: NodeId) -> String {
    let mut buf = String::new();
    collect_text(dom, node, &mut buf);
    buf.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_text(dom: &Dom, node: NodeId, out: &mut String) {
    let Some(n) = dom.node(node) else {
        return;
    };
    match &n.data {
        NodeData::Text(t) => {
            out.push(' ');
            out.push_str(t);
        }
        _ => {
            for &child in dom.children(node) {
                collect_text(dom, child, out);
            }
        }
    }
}

fn stable_id(role: &str, name: &str, path: &str) -> String {
    let mut hasher = DefaultHasher::new();
    role.hash(&mut hasher);
    0u8.hash(&mut hasher);
    name.hash(&mut hasher);
    0u8.hash(&mut hasher);
    path.hash(&mut hasher);
    format!("s{:016x}", hasher.finish())
}

fn geometry_map(root: &LayoutBox) -> HashMap<NodeId, Rect> {
    let mut map: HashMap<NodeId, Rect> = HashMap::new();
    root.for_each(&mut |b| {
        if let Some(node) = b.node {
            map.entry(node)
                .and_modify(|r| *r = union(*r, b.rect))
                .or_insert(b.rect);
        }
    });
    map
}

/// A node's box geometry: its own if laid out, else the union of its
/// descendants' boxes (so inline elements inherit their text fragments' bounds).
fn resolved_rect(dom: &Dom, node: NodeId, geometry: &HashMap<NodeId, Rect>) -> Option<Rect> {
    let mut acc = geometry.get(&node).copied();
    for &child in dom.children(node) {
        if let Some(r) = resolved_rect(dom, child, geometry) {
            acc = Some(acc.map_or(r, |a| union(a, r)));
        }
    }
    acc
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

fn to_geometry(r: Rect) -> Geometry {
    Geometry {
        x: r.x.round() as i32,
        y: r.y.round() as i32,
        width: r.width.round() as i32,
        height: r.height.round() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_css::Stylesheet;

    fn snapshot(html: &str, css: &str) -> SemanticNode {
        let dom = janus_html::parse(html);
        let styles = janus_style::compute_styles(&dom, &Stylesheet::parse(css));
        let layout = janus_layout::layout_document(&dom, &styles, 800.0).expect("root");
        build_snapshot(&dom, &styles, &layout)
    }

    fn flatten(node: &SemanticNode) -> Vec<SemanticNode> {
        let mut v = vec![node.clone()];
        for c in &node.children {
            v.extend(flatten(c));
        }
        v
    }

    #[test]
    fn form_controls_get_roles_and_state() {
        let root = snapshot(
            "<html><body>\
             <input type=\"checkbox\" checked>\
             <input type=\"submit\" value=\"Search\">\
             <input type=\"text\" placeholder=\"Email\" disabled>\
             </body></html>",
            "",
        );
        let nodes = flatten(&root);
        let checkbox = nodes
            .iter()
            .find(|n| n.role == "checkbox")
            .expect("checkbox");
        assert!(checkbox.state.contains(&"checked".to_string()));
        let button = nodes
            .iter()
            .find(|n| n.role == "button")
            .expect("submit button");
        assert_eq!(button.name, "Search");
        let textbox = nodes.iter().find(|n| n.role == "textbox").expect("textbox");
        assert_eq!(textbox.name, "Email");
        assert!(textbox.state.contains(&"disabled".to_string()));
    }

    #[test]
    fn links_carry_href_and_inline_geometry() {
        let root = snapshot(
            "<html><body><p>x <a href=\"/about\">About</a></p></body></html>",
            "",
        );
        let link = flatten(&root)
            .into_iter()
            .find(|n| n.role == "link")
            .unwrap();
        assert_eq!(link.href.as_deref(), Some("/about"));
        assert!(
            link.geometry.is_some(),
            "inline link should inherit fragment geometry"
        );
    }

    #[test]
    fn roles_names_and_geometry() {
        let root = snapshot(
            "<html><body><h1>Title</h1><a href=\"/\">Home</a><p>Body text</p></body></html>",
            "",
        );
        let nodes = flatten(&root);
        let heading = nodes.iter().find(|n| n.role == "heading").expect("heading");
        assert_eq!(heading.name, "Title");
        assert!(heading.geometry.is_some());
        assert!(heading.ref_id.starts_with('e'));

        let link = nodes.iter().find(|n| n.role == "link").expect("link");
        assert_eq!(link.name, "Home");

        let para = nodes
            .iter()
            .find(|n| n.role == "paragraph")
            .expect("paragraph");
        assert_eq!(para.name, "Body text");
    }

    #[test]
    fn aria_label_overrides_text() {
        let root = snapshot(
            "<html><body><a href=\"/\" aria-label=\"Go home\">x</a></body></html>",
            "",
        );
        let link = flatten(&root)
            .into_iter()
            .find(|n| n.role == "link")
            .unwrap();
        assert_eq!(link.name, "Go home");
    }

    #[test]
    fn display_none_excluded_from_surface() {
        let root = snapshot(
            "<html><body><p>shown</p><p style=\"display:none\">secret instructions</p></body></html>",
            "",
        );
        let names: Vec<String> = flatten(&root).into_iter().map(|n| n.name).collect();
        assert!(names.iter().any(|n| n == "shown"));
        assert!(
            !names.iter().any(|n| n.contains("secret")),
            "hidden text must not surface"
        );
    }

    #[test]
    fn stable_id_is_deterministic_across_builds() {
        let a = snapshot("<html><body><a href=\"/\">Home</a></body></html>", "");
        let b = snapshot("<html><body><a href=\"/\">Home</a></body></html>", "");
        let la = flatten(&a).into_iter().find(|n| n.role == "link").unwrap();
        let lb = flatten(&b).into_iter().find(|n| n.role == "link").unwrap();
        assert_eq!(la.stable_id, lb.stable_id);
    }

    #[test]
    fn text_serialization_is_compact_and_grounded() {
        let text = {
            let dom = janus_html::parse("<html><body><h1>Hi</h1></body></html>");
            let styles = janus_style::compute_styles(&dom, &Stylesheet::default());
            let layout = janus_layout::layout_document(&dom, &styles, 800.0).unwrap();
            snapshot_text(&dom, &styles, &layout)
        };
        assert!(text.contains("heading \"Hi\" [ref=e1]"), "{text}");
        assert!(text.contains('@'), "geometry should be present: {text}");
    }
}
