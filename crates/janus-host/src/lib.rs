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

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use janus_bytes::Url;
use janus_dom::{Dom, NodeData, NodeId};
use janus_layout::{ImageStore, LayoutBox};
use janus_style::{Display, StyleMap};
use janus_traits::RasterImage;

pub use janus_net::CookieJar;

/// Cap on image *fetch/decode attempts* per page (counts work done, not just
/// successes, so a page of thousands of broken `<img>` can't fan out).
const MAX_IMAGES: usize = 100;
/// Wall-clock budget for gathering all of a page's images, so slow/dead hosts
/// can't stall a render indefinitely.
const IMAGE_BUDGET: Duration = Duration::from_secs(15);
/// Aggregate cap on decoded image bytes held by one page (resident memory).
const MAX_TOTAL_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
/// Cap on a single decoded image's RGBA buffer, and the decoder's allocation.
const MAX_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;
/// Cap on a single image's *encoded* bytes (before decode).
const MAX_ENCODED_BYTES: usize = 24 * 1024 * 1024;
/// Cap on a decoded image's pixel dimensions (each axis).
const MAX_IMAGE_DIM: u32 = 8192;
/// Concurrent image-fetch workers. Images are fetched in parallel because each
/// fetch is a blocking round-trip (connect + TLS, no keep-alive); serial fetches
/// would make an image-heavy page take many seconds.
const MAX_IMAGE_WORKERS: usize = 8;

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
    /// Decoded images keyed by their `<img>` node — reused on reflow so the
    /// shell never re-fetches images just to re-lay-out at a new width.
    pub images: ImageStore,
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
    let images = gather_images(&dom, &styles, base_url.as_ref());
    let layout = janus_layout::layout_document_with_images(&dom, &styles, width, &images)?;
    Some(Page {
        dom,
        styles,
        layout,
        base_url,
        images,
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

/// Fetch + decode every *visible* `<img src>` in the document, keyed by node.
///
/// Visible images (`display:none` subtrees excluded — matching the render/extract
/// surface and shrinking the SSRF surface) are gathered, deduplicated by URL, and
/// decoded **concurrently** on a bounded worker pool. Bounded by [`MAX_IMAGES`]
/// distinct images, [`IMAGE_BUDGET`] wall-clock, and [`MAX_TOTAL_IMAGE_BYTES`]
/// resident bytes. Network fetches happen only when a `base` is set (so
/// `render_html` stays hermetic for local input); `data:` URIs are always decoded.
fn gather_images(dom: &Dom, styles: &StyleMap, base: Option<&Url>) -> ImageStore {
    // 1. Collect visible <img> nodes (node, src), capped at MAX_IMAGES.
    let mut nodes: Vec<(NodeId, String)> = Vec::new();
    collect_image_nodes(dom, styles, dom.document(), &mut nodes);

    // 2. Unique srcs (first-seen order) — fetch/decode each at most once.
    let mut unique: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (_, src) in &nodes {
        if seen.insert(src.as_str()) {
            unique.push(src.clone());
        }
    }

    // 3. Decode them concurrently.
    let decoded = decode_all(&unique, base);

    // 4. Assemble the store, charging each distinct image once against the byte
    //    budget; every node sharing a src gets the same shared Arc.
    let mut store = ImageStore::new();
    let mut bytes_remaining = MAX_TOTAL_IMAGE_BYTES;
    let mut admitted: HashSet<&str> = HashSet::new();
    for (node, src) in &nodes {
        let Some(image) = decoded.get(src) else {
            continue;
        };
        if !admitted.contains(src.as_str()) {
            let bytes = image.rgba.len() as u64;
            if bytes > bytes_remaining {
                continue; // over the aggregate memory budget — drop it
            }
            bytes_remaining -= bytes;
            admitted.insert(src.as_str());
        }
        store.insert(*node, image.clone());
    }
    store
}

/// Walk the rendered tree collecting `(node, src)` for each visible `<img>`, up
/// to [`MAX_IMAGES`]. A missing style entry means the node is under a
/// `display:none` ancestor (the cascade prunes those) → treat as hidden and do
/// not recurse into it.
fn collect_image_nodes(
    dom: &Dom,
    styles: &StyleMap,
    node: NodeId,
    out: &mut Vec<(NodeId, String)>,
) {
    if out.len() >= MAX_IMAGES {
        return;
    }
    if matches!(dom.node(node).map(|n| &n.data), Some(NodeData::Element(_))) {
        match styles.get(&node) {
            Some(s) if s.display != Display::None => {}
            _ => return,
        }
        if dom.element_name(node) == Some("img") {
            if let Some(src) = dom.attr(node, "src") {
                out.push((node, src.to_string()));
            }
        }
    }
    for &child in dom.children(node) {
        collect_image_nodes(dom, styles, child, out);
    }
}

/// Decode `srcs` concurrently on a bounded worker pool, mapping each src to its
/// decoded image. A shared wall-clock deadline ([`IMAGE_BUDGET`]) stops workers
/// from *starting* new fetches once it passes; each in-flight fetch is already
/// individually time-bounded by `janus-net`.
fn decode_all(srcs: &[String], base: Option<&Url>) -> HashMap<String, Arc<RasterImage>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    if srcs.is_empty() {
        return HashMap::new();
    }
    let next = AtomicUsize::new(0);
    let out: Mutex<HashMap<String, Arc<RasterImage>>> = Mutex::new(HashMap::new());
    let deadline = Instant::now() + IMAGE_BUDGET;
    let workers = srcs.len().min(MAX_IMAGE_WORKERS);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= srcs.len() || Instant::now() >= deadline {
                    break;
                }
                if let Some(image) = load_image(&srcs[i], base) {
                    out.lock().unwrap().insert(srcs[i].clone(), Arc::new(image));
                }
            });
        }
    });
    out.into_inner().unwrap()
}

/// Load `src` (a `data:` URI, or an http(s) URL resolved against `base`) and
/// decode it to straight-alpha RGBA8, bounded by encoder/decoder limits.
fn load_image(src: &str, base: Option<&Url>) -> Option<RasterImage> {
    let bytes = if let Some(rest) = src.strip_prefix("data:") {
        decode_data_uri(rest)?
    } else {
        // No network without a base URL — keeps local rendering hermetic.
        let resolved = base?.join(src).ok()?;
        let resp = janus_net::fetch(&resolved).ok()?;
        if !(200..300).contains(&resp.status) {
            return None;
        }
        resp.body
    };
    if bytes.len() > MAX_ENCODED_BYTES {
        return None;
    }
    decode_bounded(&bytes)
}

/// Decode encoded image `bytes` with explicit dimension and allocation limits
/// (a decompression-bomb defense — `image`'s defaults allow ~512 MiB per call).
fn decode_bounded(bytes: &[u8]) -> Option<RasterImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    Some(RasterImage {
        width,
        height,
        rgba: decoded.into_raw(),
    })
}

/// Decode the part of a `data:` URI after the `data:` prefix
/// (`[<mediatype>][;base64],<data>`).
fn decode_data_uri(rest: &str) -> Option<Vec<u8>> {
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let data = &rest[comma + 1..];
    if meta.split(';').any(|t| t.eq_ignore_ascii_case("base64")) {
        // Base64 in markup is often line-wrapped (MIME style); strip ASCII
        // whitespace, which the strict STANDARD engine would otherwise reject.
        let cleaned: Vec<u8> = data.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(&cleaned)
            .ok()
    } else {
        // Non-base64 image data URIs are vanishingly rare; take the bytes as-is.
        Some(data.as_bytes().to_vec())
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
    fn decodes_and_lays_out_data_uri_image() {
        // Encode a real 3×2 PNG with the codec, embed it as a base64 data: URI,
        // and confirm the full path: data-URI parse → decode → sized image box.
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(png.get_ref());
        let html = format!("<html><body><img src=\"data:image/png;base64,{b64}\"></body></html>");

        let page = render_html(&html, None, 800.0).expect("page");
        assert_eq!(page.images.len(), 1, "the data-URI image should decode");

        let mut sized = None;
        page.layout.for_each(&mut |b| {
            if b.image.is_some() {
                sized = Some(b.rect);
            }
        });
        let rect = sized.expect("an image box");
        // No width/height attrs → intrinsic 3×2.
        assert!((rect.width - 3.0).abs() < 0.01, "width {}", rect.width);
        assert!((rect.height - 2.0).abs() < 0.01, "height {}", rect.height);
    }

    /// Encode a solid-color PNG and return its base64 (no wrapping).
    fn png_base64(w: u32, h: u32) -> String {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 255, 255]));
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(png.get_ref())
    }

    #[test]
    fn display_none_image_is_not_loaded() {
        let b64 = png_base64(2, 2);
        let html = format!(
            "<html><body><div style=\"display:none\">\
             <img src=\"data:image/png;base64,{b64}\"></div></body></html>"
        );
        let page = render_html(&html, None, 800.0).expect("page");
        assert_eq!(page.images.len(), 0, "hidden image must not be loaded");
    }

    #[test]
    fn line_wrapped_data_uri_decodes() {
        // MIME-style 8-col wrapping inserts newlines the strict decoder rejects;
        // we strip whitespace first, so the image should still decode.
        let b64 = png_base64(2, 2);
        let wrapped = b64
            .as_bytes()
            .chunks(8)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let html =
            format!("<html><body><img src=\"data:image/png;base64,{wrapped}\"></body></html>");
        let page = render_html(&html, None, 800.0).expect("page");
        assert_eq!(
            page.images.len(),
            1,
            "whitespace-wrapped base64 should decode"
        );
    }

    #[test]
    fn undecodable_image_is_skipped() {
        let page = render_html(
            "<html><body><img src=\"data:image/png;base64,not-valid\"><p>hi</p></body></html>",
            None,
            800.0,
        )
        .expect("page");
        assert_eq!(page.images.len(), 0);
        assert_eq!(page.extract_text(), "hi");
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
