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

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use janus_bytes::Url;

pub use cookie::{Cookie, CookieJar};

use crate::http::{build_request, header, parse_response};

const MAX_REDIRECTS: usize = 10;

/// Cap on a single response body. The open web is hostile: a server (or a
/// redirect chain to one) could stream unbounded bytes into memory. 32 MiB is
/// generous for HTML/CSS/images while bounding the worst case.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// Hard wall-clock deadline for reading one response body. Unlike the per-read
/// socket timeout (which a slowloris resets on every dribbled byte), this bounds
/// total time regardless of how the bytes are paced.
const MAX_READ_TIME: Duration = Duration::from_secs(20);

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
    /// The host resolved to a non-public address (loopback / private / link-local
    /// / etc.) — blocked as an SSRF defense.
    BlockedHost,
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
            NetError::BlockedHost => f.write_str("blocked host (non-public address)"),
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
///
/// This is also the single SSRF chokepoint: it is reached by `fetch_once` for
/// the initial URL *and* every redirect hop, so rejecting non-public addresses
/// here closes the redirect-bypass too. All resolved addresses are inspected
/// (defeating multi-A-record / DNS-rebinding tricks) and the connection is only
/// made to a public one.
fn connect(host: &str, port: u16) -> Result<TcpStream, NetError> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(NetError::BadUrl);
    }
    // If *any* resolved address is non-public, refuse — a hostile resolver could
    // otherwise return one public and one internal address to slip past us.
    if addrs.iter().any(|a| is_blocked_ip(a.ip())) {
        return Err(NetError::BlockedHost);
    }
    let sock = TcpStream::connect_timeout(&addrs[0], Duration::from_secs(15))?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    sock.set_write_timeout(Some(Duration::from_secs(15)))?;
    Ok(sock)
}

/// Is `ip` a non-public address we must never connect to from page-controlled
/// URLs? Covers loopback, private (RFC1918), link-local (incl. the cloud
/// metadata endpoint 169.254.169.254), CGNAT, unspecified, broadcast, and the
/// IPv6 equivalents (incl. IPv4-mapped forms).
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 0x40) // 100.64.0.0/10 CGNAT
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Read a response body, bounded by both [`MAX_BODY`] bytes and [`MAX_READ_TIME`]
/// total wall-clock. A stalled or slow-dripping connection ends the read instead
/// of hanging the caller; whatever was received so far is returned (a partial
/// body is then parsed best-effort, as for an unclean `Connection: close`).
fn read_body(reader: &mut impl Read) -> Vec<u8> {
    let start = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    while buf.len() < MAX_BODY && start.elapsed() < MAX_READ_TIME {
        let want = (MAX_BODY - buf.len()).min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => break, // clean EOF
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A timed-out / stalled read or an unclean close: stop with what we
            // have rather than erroring or looping.
            Err(_) => break,
        }
    }
    buf
}

fn fetch_plain(host: &str, port: u16, request: &[u8]) -> Result<Vec<u8>, NetError> {
    let mut sock = connect(host, port)?;
    sock.write_all(request)?;
    Ok(read_body(&mut sock))
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
    // `read_body` already tolerates an unclean TLS close (it stops on any read
    // error and returns what arrived), so no special UnexpectedEof handling.
    Ok(read_body(&mut tls))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_scheme() {
        let url = Url::parse("ftp://example.com/file").unwrap();
        assert!(matches!(fetch(&url), Err(NetError::UnsupportedScheme)));
    }

    #[test]
    fn ssrf_filter_blocks_internal_addresses() {
        let blocked = [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1", // IPv4-mapped loopback
        ];
        for ip in blocked {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} should be blocked");
        }
        let allowed = ["1.1.1.1", "8.8.8.8", "93.184.216.34", "2606:2800:220:1::1"];
        for ip in allowed {
            assert!(
                !is_blocked_ip(ip.parse().unwrap()),
                "{ip} should be allowed"
            );
        }
    }

    // Connecting to a loopback URL must be refused before any socket work.
    #[test]
    fn fetch_blocks_loopback_host() {
        let url = Url::parse("http://127.0.0.1:9/").unwrap();
        assert!(matches!(fetch(&url), Err(NetError::BlockedHost)));
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
