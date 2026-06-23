//! Minimal MIME sniffing (magic-byte based).
//!
//! A small subset of the WHATWG MIME Sniffing Standard — enough to distinguish
//! HTML, plain text, and common image types for the loader. The full sniffing
//! algorithm (and content-type-hint precedence) grows with `janus-net`.

/// Guess a MIME type from the leading bytes of a resource.
///
/// Magic-byte matches win first; otherwise an HTML-looking prefix yields
/// `text/html`, then the `hint` (e.g. an HTTP `Content-Type`) is honored, then
/// valid-UTF-8 text falls back to `text/plain`, else `application/octet-stream`.
#[must_use]
pub fn sniff(bytes: &[u8], hint: Option<&str>) -> String {
    if let Some(magic) = sniff_magic(bytes) {
        return magic.to_string();
    }
    if looks_like_html(bytes) {
        return "text/html".to_string();
    }
    if let Some(h) = hint {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    if std::str::from_utf8(bytes).is_ok() {
        "text/plain".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

fn sniff_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    None
}

fn looks_like_html(bytes: &[u8]) -> bool {
    // Skip leading whitespace, then match a few case-insensitive HTML markers.
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let rest = &bytes[start..];
    const MARKERS: &[&[u8]] = &[
        b"<!doctype html",
        b"<html",
        b"<head",
        b"<body",
        b"<script",
        b"<!--",
    ];
    MARKERS
        .iter()
        .any(|m| starts_with_ignore_ascii_case(rest, m))
}

fn starts_with_ignore_ascii_case(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_image_magic() {
        assert_eq!(
            sniff(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], None),
            "image/png"
        );
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0x00], None), "image/jpeg");
        assert_eq!(sniff(b"GIF89a....", None), "image/gif");
    }

    #[test]
    fn detects_html_even_with_leading_whitespace() {
        assert_eq!(sniff(b"  \n<!DOCTYPE HTML><html>", None), "text/html");
        assert_eq!(sniff(b"<HtMl>", None), "text/html");
    }

    #[test]
    fn honors_hint_then_falls_back_to_text() {
        assert_eq!(
            sniff(b"key: value", Some("application/yaml")),
            "application/yaml"
        );
        assert_eq!(sniff(b"just text", None), "text/plain");
        assert_eq!(sniff(&[0x00, 0xff, 0xfe], None), "application/octet-stream");
    }
}
