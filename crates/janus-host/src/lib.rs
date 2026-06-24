//! The orchestrator: drive the whole pipeline once and hold the result.
//!
//! [`render_html`] / [`render_url`] run bytes → DOM → CSS → cascade → layout in
//! a single pass and return a [`Page`] holding the DOM, computed styles, layout
//! geometry, and base URL. CSS is gathered from inline `<style>` *and* fetched
//! `<link rel="stylesheet">` resolved against the base URL (via `janus-net`), so
//! real sites with external stylesheets render.
//!
//! This is the shared entry point for both painters: `janus-cli` (pixels) and
//! `janus-agent` (the MCP semantic surface) both build a [`Page`] here, so the
//! pipeline lives in exactly one place.

use janus_bytes::Url;
use janus_dom::{Dom, NodeData, NodeId};
use janus_layout::LayoutBox;
use janus_style::{Display, StyleMap};

pub use janus_net::CookieJar;

/// A fully processed page: one layout pass, ready for either painter.
#[derive(Debug)]
pub struct Page {
    /// The parsed document tree.
    pub dom: Dom,
    /// Computed styles per element.
    pub styles: StyleMap,
    /// The positioned box tree (geometry).
    pub layout: LayoutBox,
    /// The document's base URL, if it was fetched from one.
    pub base_url: Option<Url>,
}

impl Page {
    /// The page's ref-tagged, box-grounded semantic snapshot (agent view).
    #[must_use]
    pub fn snapshot(&self) -> String {
        janus_sem::snapshot_text(&self.dom, &self.styles, &self.layout)
    }

    /// The page's visible text content (`display:none` excluded).
    #[must_use]
    pub fn extract_text(&self) -> String {
        let mut buf = String::new();
        for &child in self.dom.children(self.dom.document()) {
            collect_visible_text(&self.dom, &self.styles, child, &mut buf);
        }
        buf.split('\n')
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve the link target of the semantic node `ref_id` to an absolute URL
    /// (against the page's base), or `None` if that ref is not a link.
    #[must_use]
    pub fn resolve_link(&self, ref_id: &str) -> Option<String> {
        let tree = janus_sem::build_snapshot(&self.dom, &self.styles, &self.layout);
        let href = find_ref(&tree, ref_id)?.href.clone()?;
        match &self.base_url {
            Some(base) => base.join(&href).ok().map(|u| u.to_string()),
            None => Url::parse(&href).ok().map(|u| u.to_string()),
        }
    }

    /// Find semantic nodes matching an optional exact `role` and/or a
    /// case-insensitive `name_contains` substring; returns one line per match
    /// (or `"(no matches)"`). With no filter, lists every node.
    #[must_use]
    pub fn find(&self, role: Option<&str>, name_contains: Option<&str>) -> String {
        let tree = janus_sem::build_snapshot(&self.dom, &self.styles, &self.layout);
        let mut out = String::new();
        collect_matches(&tree, role, name_contains, &mut out);
        if out.is_empty() {
            "(no matches)".to_string()
        } else {
            out
        }
    }

    /// The resolved link URL at page coordinate `(x, y)`, if a link's box covers
    /// it. Picks the smallest (most specific) matching box. Used by the shell
    /// for click-to-navigate.
    #[must_use]
    pub fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let tree = janus_sem::build_snapshot(&self.dom, &self.styles, &self.layout);
        let mut best: Option<(String, i64)> = None;
        link_at_point(&tree, x, y, &mut best);
        best.and_then(|(ref_id, _)| self.resolve_link(&ref_id))
    }
}

fn link_at_point(node: &janus_sem::SemanticNode, x: f32, y: f32, best: &mut Option<(String, i64)>) {
    if node.href.is_some() {
        if let Some(g) = node.geometry {
            let (xi, yi) = (x as i32, y as i32);
            if xi >= g.x && xi < g.x + g.width && yi >= g.y && yi < g.y + g.height {
                let area = i64::from(g.width) * i64::from(g.height);
                if best.as_ref().is_none_or(|(_, a)| area < *a) {
                    *best = Some((node.ref_id.clone(), area));
                }
            }
        }
    }
    for child in &node.children {
        link_at_point(child, x, y, best);
    }
}

fn collect_matches(
    node: &janus_sem::SemanticNode,
    role: Option<&str>,
    name_contains: Option<&str>,
    out: &mut String,
) {
    let role_ok = role.is_none_or(|r| node.role == r);
    let name_ok =
        name_contains.is_none_or(|n| node.name.to_lowercase().contains(&n.to_lowercase()));
    if node.ref_id != "root" && role_ok && name_ok {
        out.push_str("- ");
        out.push_str(&janus_sem::node_line(node));
        out.push('\n');
    }
    for child in &node.children {
        collect_matches(child, role, name_contains, out);
    }
}

fn find_ref<'a>(
    node: &'a janus_sem::SemanticNode,
    ref_id: &str,
) -> Option<&'a janus_sem::SemanticNode> {
    if node.ref_id == ref_id {
        return Some(node);
    }
    node.children.iter().find_map(|c| find_ref(c, ref_id))
}

/// Render an HTML string at `width`. When `base_url` is set, `<link>`ed
/// stylesheets are resolved against it and fetched; otherwise only inline
/// `<style>` is used (so this stays hermetic for local input).
#[must_use]
pub fn render_html(html: &str, base_url: Option<Url>, width: f32) -> Option<Page> {
    let dom = janus_html::parse(html);
    let css = gather_css(&dom, base_url.as_ref());
    let styles = janus_style::compute_styles(&dom, &janus_css::Stylesheet::parse(&css));
    let layout = janus_layout::layout_document(&dom, &styles, width)?;
    Some(Page {
        dom,
        styles,
        layout,
        base_url,
    })
}

/// Fetch `url` and render it (resolving and fetching external stylesheets).
///
/// # Errors
/// On a network/parse failure or an unrenderable document.
pub fn render_url(url: &str, width: f32) -> Result<Page, String> {
    let response = janus_net::fetch_url(url).map_err(|e| e.to_string())?;
    let base = response.final_url.clone();
    render_html(&response.text(), Some(base), width).ok_or_else(|| "nothing to render".to_string())
}

/// Like [`render_url`] but sends/stores cookies in `jar`, so a session's
/// cookies persist across navigations.
///
/// # Errors
/// On a network/parse failure or an unrenderable document.
pub fn render_url_with_jar(url: &str, width: f32, jar: &mut CookieJar) -> Result<Page, String> {
    let parsed = Url::parse(url).map_err(|e| e.to_string())?;
    let response = janus_net::fetch_with_jar(&parsed, jar).map_err(|e| e.to_string())?;
    let base = response.final_url.clone();
    render_html(&response.text(), Some(base), width).ok_or_else(|| "nothing to render".to_string())
}

/// Concatenate inline `<style>` text and any `<link rel="stylesheet">` content
/// (fetched + resolved against `base`).
fn gather_css(dom: &Dom, base: Option<&Url>) -> String {
    let mut css = String::new();
    gather_css_from(dom, dom.document(), base, &mut css);
    css
}

fn gather_css_from(dom: &Dom, node: NodeId, base: Option<&Url>, out: &mut String) {
    match dom.element_name(node) {
        Some("style") => {
            for &child in dom.children(node) {
                if let Some(NodeData::Text(text)) = dom.node(child).map(|n| &n.data) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        Some("link") => {
            if let Some(href) = stylesheet_href(dom, node) {
                if let Some(base) = base {
                    if let Ok(resolved) = base.join(href) {
                        if let Ok(resp) = janus_net::fetch(&resolved) {
                            if (200..300).contains(&resp.status) {
                                out.push_str(&resp.text());
                                out.push('\n');
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    for &child in dom.children(node) {
        gather_css_from(dom, child, base, out);
    }
}

fn stylesheet_href(dom: &Dom, node: NodeId) -> Option<&str> {
    let rel = dom.attr(node, "rel")?;
    if rel
        .split_whitespace()
        .any(|t| t.eq_ignore_ascii_case("stylesheet"))
    {
        dom.attr(node, "href")
    } else {
        None
    }
}

fn collect_visible_text(dom: &Dom, styles: &StyleMap, node: NodeId, out: &mut String) {
    let Some(n) = dom.node(node) else {
        return;
    };
    match &n.data {
        NodeData::Text(t) => {
            out.push_str(t);
            out.push(' ');
        }
        NodeData::Element(_) => {
            if styles
                .get(&node)
                .is_some_and(|s| s.display == Display::None)
            {
                return;
            }
            let block = styles
                .get(&node)
                .is_some_and(|s| matches!(s.display, Display::Block | Display::ListItem));
            for &child in dom.children(node) {
                collect_visible_text(dom, styles, child, out);
            }
            if block {
                out.push('\n');
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline_styled_page() {
        let page = render_html(
            "<html><head><style>p{color:red}</style></head><body><p>hi</p></body></html>",
            None,
            800.0,
        )
        .expect("page");
        assert!(page.snapshot().contains("paragraph \"hi\""));
        assert_eq!(page.extract_text(), "hi");
        assert!(page.base_url.is_none());
    }

    #[test]
    fn extract_text_excludes_hidden() {
        let page = render_html(
            "<html><body><p>shown</p><p style=\"display:none\">SECRET</p></body></html>",
            None,
            800.0,
        )
        .expect("page");
        let text = page.extract_text();
        assert!(text.contains("shown"));
        assert!(!text.contains("SECRET"));
    }

    #[test]
    fn empty_document_renders_nothing() {
        assert!(render_html("", None, 800.0).is_none());
    }

    #[test]
    fn resolve_link_absolute_without_base() {
        let page = render_html(
            "<html><body><a href=\"https://x.test/\">go</a></body></html>",
            None,
            800.0,
        )
        .unwrap();
        assert_eq!(page.resolve_link("e1").as_deref(), Some("https://x.test/"));
        assert_eq!(page.resolve_link("e999"), None);
    }

    #[test]
    fn resolve_link_relative_against_base() {
        let base = Url::parse("https://h.test/dir/page").unwrap();
        let page = render_html(
            "<html><body><a href=\"/abs\">x</a></body></html>",
            Some(base),
            800.0,
        )
        .unwrap();
        assert_eq!(
            page.resolve_link("e1").as_deref(),
            Some("https://h.test/abs")
        );
    }

    #[test]
    fn find_by_role_and_name() {
        let page = render_html(
            "<html><body><a href=\"/a\">Login</a><a href=\"/b\">Logout</a><h1>Hi</h1></body></html>",
            None,
            800.0,
        )
        .unwrap();
        assert_eq!(page.find(Some("link"), Some("log")).lines().count(), 2);
        let login = page.find(Some("link"), Some("login"));
        assert!(login.contains("Login"));
        assert!(!login.contains("Logout"));
        assert!(page.find(Some("heading"), None).contains("heading \"Hi\""));
        assert_eq!(page.find(Some("button"), None), "(no matches)");
    }
}
