//! HTTP/1.1 message encoding and decoding — the pure, offline-testable core of
//! the client (no sockets here). The live transport lives in the crate root.

/// A parsed HTTP response: status, headers, and the decoded body.
pub(crate) struct ParsedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Build a `Connection: close` HTTP/1.1 GET request for `host` and
/// `path_and_query`, with an optional `Cookie` header. We close the connection
/// so the body runs to EOF, and ask for `identity` encoding (no decompress yet).
pub(crate) fn build_request(host: &str, path_and_query: &str, cookie: Option<&str>) -> String {
    let mut request = format!(
        "GET {path_and_query} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: janus/0.0 (+https://example.invalid/janus)\r\n\
         Accept: text/html,application/xhtml+xml,*/*\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n"
    );
    if let Some(cookie) = cookie {
        request.push_str("Cookie: ");
        request.push_str(cookie);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request
}

/// Parse a complete raw HTTP/1.1 response. Dechunks a `chunked` body.
pub(crate) fn parse_response(raw: &[u8]) -> Result<ParsedResponse, String> {
    let split = find_subsequence(raw, b"\r\n\r\n").ok_or("no header/body separator")?;
    let header_text = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];

    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().ok_or("empty response")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("malformed status line")?;

    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let chunked = header(&headers, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_vec()
    };

    Ok(ParsedResponse {
        status,
        headers,
        body,
    })
}

/// Case-insensitive header lookup (first match).
pub(crate) fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Decode an HTTP/1.1 `chunked` transfer-encoded body.
fn dechunk(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let Some(rel) = find_subsequence(&data[i..], b"\r\n") else {
            break;
        };
        let line_end = i + rel;
        let size_line = String::from_utf8_lossy(&data[i..line_end]);
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_hex, 16) else {
            break;
        };
        i = line_end + 2; // past CRLF after the size line
        if size == 0 {
            break; // last chunk
        }
        // Reject oversized/truncated chunks (checked add avoids overflow on a
        // hostile `Transfer-Encoding: chunked` size like `ffffffffffffffff`).
        let Some(end) = i.checked_add(size).filter(|&e| e <= data.len()) else {
            break;
        };
        out.extend_from_slice(&data[i..end]);
        // Each chunk must be followed by CRLF; stop if it is missing/misaligned.
        if data.get(end..end + 2) != Some(b"\r\n".as_slice()) {
            break;
        }
        i = end + 2; // past chunk data + trailing CRLF
    }
    out
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_host_and_close() {
        let req = build_request("example.com", "/path?q=1", None);
        assert!(req.starts_with("GET /path?q=1 HTTP/1.1\r\n"));
        assert!(req.contains("Host: example.com\r\n"));
        assert!(req.contains("Connection: close\r\n"));
        assert!(!req.contains("Cookie:"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn request_includes_cookie_header_when_present() {
        let req = build_request("example.com", "/", Some("sid=abc; theme=dark"));
        assert!(req.contains("Cookie: sid=abc; theme=dark\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn parses_status_headers_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhello";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(header(&r.headers, "content-type"), Some("text/html"));
        assert_eq!(r.body, b"hello");
    }

    #[test]
    fn dechunks_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.body, b"hello world");
    }

    #[test]
    fn dechunk_rejects_oversized_chunk_without_panic() {
        // A hostile chunk size of usize::MAX must not overflow/panic.
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nhello";
        let r = parse_response(raw).unwrap();
        assert!(r.body.is_empty());
    }

    #[test]
    fn dechunk_stops_on_missing_crlf() {
        // Chunk data not followed by CRLF: take the chunk, then stop (no desync).
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.body, b"hello");
    }

    #[test]
    fn finds_location_header_case_insensitively() {
        let raw = b"HTTP/1.1 301 Moved\r\nLOCATION: /new\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 301);
        assert_eq!(header(&r.headers, "location"), Some("/new"));
    }
}
