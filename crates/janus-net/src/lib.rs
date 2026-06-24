//! A from-scratch HTTP/1.1 client.
//!
//! Per the from-scratch boundary, we own the HTTP request/response logic and
//! redirect handling; only the TLS *records* are delegated to `rustls` (with
//! the pure-Rust `ring` provider) — crypto is the one thing we never hand-roll.
//! URLs come from our own `janus-bytes` parser.
//!
//! P0 scope: blocking GET over `http`/`https`, `Connection: close`, redirect
//! following, and `chunked` decoding. Cookies, caching, HTTP/2, keep-alive,
//! content decompression, and charset sniffing layer on next.

mod cookie;
mod http;

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use janus_bytes::Url;

pub use cookie::{Cookie, CookieJar};

use crate::http::{build_request, header, parse_response};

const MAX_REDIRECTS: usize = 10;

/// An HTTP response.
#[derive(Clone, Debug)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Response headers (name, value), in order.
    pub headers: Vec<(String, String)>,
    /// Raw response body bytes.
    pub body: Vec<u8>,
    /// The final URL after any redirects.
    pub final_url: Url,
}

impl Response {
    /// The body decoded as UTF-8 (lossily). Real charset detection
    /// (encoding_rs + the `Content-Type`/`<meta>` charset) comes later.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// A response header by case-insensitive name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// Errors from fetching.
#[derive(Debug)]
pub enum NetError {
    /// Socket / IO failure.
    Io(std::io::Error),
    /// TLS setup or handshake failure.
    Tls(String),
    /// Malformed response.
    Parse(String),
    /// Redirected more than [`MAX_REDIRECTS`] times.
    TooManyRedirects,
    /// Scheme other than `http`/`https`.
    UnsupportedScheme,
    /// The URL lacked a usable host/port or a redirect target was invalid.
    BadUrl,
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Io(e) => write!(f, "io error: {e}"),
            NetError::Tls(e) => write!(f, "tls error: {e}"),
            NetError::Parse(e) => write!(f, "parse error: {e}"),
            NetError::TooManyRedirects => f.write_str("too many redirects"),
            NetError::UnsupportedScheme => f.write_str("unsupported scheme (only http/https)"),
            NetError::BadUrl => f.write_str("invalid or unsupported URL"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        NetError::Io(e)
    }
}

/// Parse `url` and [`fetch`] it.
///
/// # Errors
/// See [`NetError`].
pub fn fetch_url(url: &str) -> Result<Response, NetError> {
    let parsed = Url::parse(url).map_err(|_| NetError::BadUrl)?;
    fetch(&parsed)
}

/// Fetch `url`, following up to [`MAX_REDIRECTS`] redirects.
///
/// # Errors
/// See [`NetError`].
pub fn fetch(url: &Url) -> Result<Response, NetError> {
    fetch_inner(url, None)
}

/// Fetch `url` using `jar`: cookies are sent with each request and any
/// `Set-Cookie` responses (including across redirects) are stored back.
///
/// # Errors
/// See [`NetError`].
pub fn fetch_with_jar(url: &Url, jar: &mut CookieJar) -> Result<Response, NetError> {
    fetch_inner(url, Some(jar))
}

fn fetch_inner(url: &Url, mut jar: Option<&mut CookieJar>) -> Result<Response, NetError> {
    let mut current = url.clone();
    for _ in 0..MAX_REDIRECTS {
        let cookie = jar.as_deref().and_then(|j| j.header_for(&current));
        let raw = fetch_once(&current, cookie.as_deref())?;
        let parsed = parse_response(&raw).map_err(NetError::Parse)?;

        if let Some(j) = jar.as_deref_mut() {
            for (name, value) in &parsed.headers {
                if name.eq_ignore_ascii_case("set-cookie") {
                    j.set_from_header(value, &current);
                }
            }
        }

        if (300..400).contains(&parsed.status) && parsed.status != 304 {
            if let Some(location) = header(&parsed.headers, "location") {
                current = current.join(location).map_err(|_| NetError::BadUrl)?;
                continue;
            }
        }
        return Ok(Response {
            status: parsed.status,
            headers: parsed.headers,
            body: parsed.body,
            final_url: current,
        });
    }
    Err(NetError::TooManyRedirects)
}

fn fetch_once(url: &Url, cookie: Option<&str>) -> Result<Vec<u8>, NetError> {
    let host = url.host().ok_or(NetError::BadUrl)?;
    let port = url.port_or_default().ok_or(NetError::UnsupportedScheme)?;

    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let request = build_request(host, &target, cookie);

    match url.scheme() {
        "https" => fetch_tls(host, port, request.as_bytes()),
        "http" => fetch_plain(host, port, request.as_bytes()),
        _ => Err(NetError::UnsupportedScheme),
    }
}

/// Connect with a bounded connect timeout and per-read/write timeouts so a slow
/// or dead server can never hang the caller (e.g. the browser window) forever.
fn connect(host: &str, port: u16) -> Result<TcpStream, NetError> {
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or(NetError::BadUrl)?;
    let sock = TcpStream::connect_timeout(&addr, Duration::from_secs(15))?;
    sock.set_read_timeout(Some(Duration::from_secs(30)))?;
    sock.set_write_timeout(Some(Duration::from_secs(15)))?;
    Ok(sock)
}

fn fetch_plain(host: &str, port: u16, request: &[u8]) -> Result<Vec<u8>, NetError> {
    let mut sock = connect(host, port)?;
    sock.write_all(request)?;
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf)?;
    Ok(buf)
}

fn fetch_tls(host: &str, port: u16, request: &[u8]) -> Result<Vec<u8>, NetError> {
    // Install the ring provider once (ignored if already installed).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let server_name =
        rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| NetError::BadUrl)?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    let mut sock = connect(host, port)?;
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);

    tls.write_all(request)?;
    let mut buf = Vec::new();
    match tls.read_to_end(&mut buf) {
        Ok(_) => Ok(buf),
        // Many servers close the TLS session uncleanly after the body; that is
        // not an error for a `Connection: close` fetch as long as we got data.
        Err(e) if e.kind() == ErrorKind::UnexpectedEof && !buf.is_empty() => Ok(buf),
        Err(e) => Err(NetError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_scheme() {
        let url = Url::parse("ftp://example.com/file").unwrap();
        assert!(matches!(fetch(&url), Err(NetError::UnsupportedScheme)));
    }

    // Live network fetch — ignored by default so CI stays hermetic. Run with:
    //   cargo test -p janus-net -- --ignored
    #[test]
    #[ignore = "requires network"]
    fn live_fetch_example_com() {
        let resp = fetch_url("https://example.com/").expect("fetch");
        assert_eq!(resp.status, 200);
        assert!(resp.text().to_ascii_lowercase().contains("<html"));
    }
}
