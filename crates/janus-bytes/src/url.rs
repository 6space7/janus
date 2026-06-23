//! A practical URL parser.
//!
//! Implements RFC 3986 generic-syntax parsing and reference resolution
//! (§5.2–5.3), plus the WHATWG normalizations Janus relies on for the special
//! schemes it fetches (`http`, `https`, `ws`, `wss`, `ftp`, `file`):
//! scheme/host ASCII-lowercasing, default-port elision, dot-segment removal,
//! and per-component percent-encoding.
//!
//! Deliberately out of scope for now (tracked for later): IDNA/punycode host
//! processing, full IPv4 shorthand parsing, and assorted WHATWG state-machine
//! quirks. ASCII hosts and the common real-world forms are handled.

use std::fmt;

use crate::percent::{fragment_set, path_set, percent_encode, query_set, userinfo_set};

/// An error produced while parsing a URL.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ParseError {
    /// The input had no scheme and no base URL was supplied (relative ref).
    MissingScheme,
    /// The scheme was empty or contained an illegal character.
    InvalidScheme,
    /// A special scheme requires a host, but none was present.
    MissingHost,
    /// The host contained a forbidden character.
    InvalidHost,
    /// The port was not a valid 16-bit integer.
    InvalidPort,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            ParseError::MissingScheme => "relative URL with no base",
            ParseError::InvalidScheme => "invalid scheme",
            ParseError::MissingHost => "special scheme requires a host",
            ParseError::InvalidHost => "invalid host",
            ParseError::InvalidPort => "invalid port",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for ParseError {}

/// A parsed, normalized URL.
///
/// Components are stored already percent-encoded and normalized; [`Display`]
/// re-serializes them losslessly.
///
/// [`Display`]: std::fmt::Display
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Url {
    scheme: String,
    username: String,
    password: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl Url {
    /// Parse an absolute URL. Returns [`ParseError::MissingScheme`] for a
    /// relative reference (use [`Url::parse_with_base`] / [`Url::join`]).
    ///
    /// # Errors
    /// See [`ParseError`].
    pub fn parse(input: &str) -> Result<Url, ParseError> {
        Self::parse_inner(input, None)
    }

    /// Parse `input` as a reference resolved against `base` (RFC 3986 §5).
    ///
    /// # Errors
    /// See [`ParseError`].
    pub fn parse_with_base(input: &str, base: &Url) -> Result<Url, ParseError> {
        Self::parse_inner(input, Some(base))
    }

    /// Resolve `input` against `self` as the base.
    ///
    /// # Errors
    /// See [`ParseError`].
    pub fn join(&self, input: &str) -> Result<Url, ParseError> {
        Self::parse_inner(input, Some(self))
    }

    fn parse_inner(input: &str, base: Option<&Url>) -> Result<Url, ParseError> {
        let cleaned = clean_input(input);
        let reference = parse_components(&cleaned);
        let resolved = match base {
            Some(b) => resolve(&b.to_components(), &reference),
            None => reference,
        };
        Url::from_components(resolved)
    }

    /// The scheme, lowercased and without the trailing `:`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The host (domain or bracketed IPv6 literal), if any.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// The explicit port, if one is present and differs from the scheme default.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// The port to connect to: the explicit port, else the scheme's default.
    #[must_use]
    pub fn port_or_default(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    /// The path, beginning with `/` for special schemes with a host.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The query string, without the leading `?`.
    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// The fragment, without the leading `#`.
    #[must_use]
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// The username (may be empty).
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// The password, if present.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// Whether this URL uses a WHATWG "special" scheme.
    #[must_use]
    pub fn is_special(&self) -> bool {
        is_special(&self.scheme)
    }

    fn to_components(&self) -> Components {
        let authority = self.host.as_ref().map(|h| {
            let userinfo = if self.username.is_empty() && self.password.is_none() {
                None
            } else {
                Some(match &self.password {
                    Some(p) => format!("{}:{}", self.username, p),
                    None => self.username.clone(),
                })
            };
            Authority {
                userinfo,
                host: h.clone(),
                port: self.port.map(|p| p.to_string()),
            }
        });
        Components {
            scheme: Some(self.scheme.clone()),
            authority,
            path: self.path.clone(),
            query: self.query.clone(),
            fragment: self.fragment.clone(),
        }
    }

    fn from_components(c: Components) -> Result<Url, ParseError> {
        let scheme = c.scheme.ok_or(ParseError::MissingScheme)?;
        if !is_valid_scheme(&scheme) {
            return Err(ParseError::InvalidScheme);
        }
        let special = is_special(&scheme);

        let mut username = String::new();
        let mut password = None;
        let mut host = None;
        let mut port = None;

        if let Some(auth) = c.authority {
            if let Some(userinfo) = auth.userinfo {
                match userinfo.split_once(':') {
                    Some((u, p)) => {
                        username = percent_encode(u, userinfo_set);
                        password = Some(percent_encode(p, userinfo_set));
                    }
                    None => username = percent_encode(&userinfo, userinfo_set),
                }
            }
            let parsed_host = parse_host(&auth.host, special)?;
            if special && scheme != "file" && parsed_host.is_empty() {
                return Err(ParseError::MissingHost);
            }
            host = Some(parsed_host);
            if let Some(port_str) = auth.port {
                if !port_str.is_empty() {
                    let value: u16 = port_str.parse().map_err(|_| ParseError::InvalidPort)?;
                    if default_port(&scheme) != Some(value) {
                        port = Some(value);
                    }
                }
            }
        } else if special && scheme != "file" {
            return Err(ParseError::MissingHost);
        }

        let mut path = c.path;
        if special || host.is_some() {
            path = remove_dot_segments(&path);
            if path.is_empty() && host.is_some() {
                path.push('/');
            }
        }
        let path = percent_encode(&path, path_set);
        let query = c.query.map(|q| percent_encode(&q, query_set));
        let fragment = c.fragment.map(|fr| percent_encode(&fr, fragment_set));

        Ok(Url {
            scheme,
            username,
            password,
            host,
            port,
            path,
            query,
            fragment,
        })
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.scheme)?;
        if let Some(host) = &self.host {
            f.write_str("//")?;
            if !self.username.is_empty() || self.password.is_some() {
                f.write_str(&self.username)?;
                if let Some(p) = &self.password {
                    write!(f, ":{p}")?;
                }
                f.write_str("@")?;
            }
            f.write_str(host)?;
            if let Some(port) = self.port {
                write!(f, ":{port}")?;
            }
        }
        f.write_str(&self.path)?;
        if let Some(q) = &self.query {
            write!(f, "?{q}")?;
        }
        if let Some(fragment) = &self.fragment {
            write!(f, "#{fragment}")?;
        }
        Ok(())
    }
}

// --- internal syntactic components (RFC 3986) ---------------------------------

#[derive(Clone, Debug)]
struct Authority {
    userinfo: Option<String>,
    host: String,
    port: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Components {
    scheme: Option<String>,
    authority: Option<Authority>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

/// Remove leading/trailing C0-control-or-space and strip all tabs/newlines.
fn clean_input(input: &str) -> String {
    let trimmed = input.trim_matches(|c: char| c <= '\u{20}');
    trimmed
        .chars()
        .filter(|&c| c != '\t' && c != '\n' && c != '\r')
        .collect()
}

fn is_valid_scheme(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn split_authority(s: &str) -> Authority {
    let (userinfo, hostport) = match s.rfind('@') {
        Some(i) => (Some(s[..i].to_string()), &s[i + 1..]),
        None => (None, s),
    };
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        // IPv6 literal: the host is the bracketed span.
        match rest.find(']') {
            Some(j) => {
                let host = format!("[{}]", &rest[..j]);
                let port = rest[j + 1..].strip_prefix(':').map(str::to_string);
                (host, port)
            }
            None => (hostport.to_string(), None),
        }
    } else {
        match hostport.rfind(':') {
            Some(i) => (
                hostport[..i].to_string(),
                Some(hostport[i + 1..].to_string()),
            ),
            None => (hostport.to_string(), None),
        }
    };
    Authority {
        userinfo,
        host,
        port,
    }
}

/// Split a (cleaned) URL or reference into its RFC 3986 components, without
/// validation or normalization.
fn parse_components(input: &str) -> Components {
    let mut rest = input;

    let fragment = rest.find('#').map(|i| {
        let f = rest[i + 1..].to_string();
        rest = &rest[..i];
        f
    });
    let query = rest.find('?').map(|i| {
        let q = rest[i + 1..].to_string();
        rest = &rest[..i];
        q
    });

    let mut scheme = None;
    if let Some(i) = rest.find(':') {
        let candidate = &rest[..i];
        if is_valid_scheme(candidate) {
            scheme = Some(candidate.to_ascii_lowercase());
            rest = &rest[i + 1..];
        }
    }

    let (authority, path) = if let Some(after) = rest.strip_prefix("//") {
        let end = after.find('/').unwrap_or(after.len());
        (
            Some(split_authority(&after[..end])),
            after[end..].to_string(),
        )
    } else {
        (None, rest.to_string())
    };

    Components {
        scheme,
        authority,
        path,
        query,
        fragment,
    }
}

fn parse_host(input: &str, special: bool) -> Result<String, ParseError> {
    if input.is_empty() {
        return Ok(String::new());
    }
    if let Some(inner) = input.strip_prefix('[') {
        let end = inner.find(']').ok_or(ParseError::InvalidHost)?;
        let addr = &inner[..end];
        if addr.is_empty()
            || !addr
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.'))
        {
            return Err(ParseError::InvalidHost);
        }
        return Ok(format!("[{addr}]"));
    }
    for &b in input.as_bytes() {
        let forbidden = b <= 0x20
            || matches!(
                b,
                b'#' | b'/' | b':' | b'<' | b'>' | b'?' | b'@' | b'[' | b'\\' | b']' | b'^' | b'|'
            );
        if forbidden {
            return Err(ParseError::InvalidHost);
        }
    }
    // IDNA/punycode is deferred; special-scheme hosts are ASCII-lowercased.
    if special {
        Ok(input.to_ascii_lowercase())
    } else {
        Ok(input.to_string())
    }
}

/// RFC 3986 §5.2.3 merge.
fn merge(base: &Components, ref_path: &str) -> String {
    if base.authority.is_some() && base.path.is_empty() {
        format!("/{ref_path}")
    } else {
        match base.path.rfind('/') {
            Some(i) => format!("{}{}", &base.path[..=i], ref_path),
            None => ref_path.to_string(),
        }
    }
}

/// RFC 3986 §5.2.4 remove_dot_segments.
fn remove_dot_segments(path: &str) -> String {
    let mut input = path.to_string();
    let mut out = String::with_capacity(path.len());
    while !input.is_empty() {
        if let Some(rest) = input.strip_prefix("../") {
            input = rest.to_string();
        } else if let Some(rest) = input.strip_prefix("./") {
            input = rest.to_string();
        } else if let Some(rest) = input.strip_prefix("/./") {
            input = format!("/{rest}");
        } else if input == "/." {
            input = "/".to_string();
        } else if let Some(rest) = input.strip_prefix("/../") {
            input = format!("/{rest}");
            pop_last_segment(&mut out);
        } else if input == "/.." {
            input = "/".to_string();
            pop_last_segment(&mut out);
        } else if input == "." || input == ".." {
            input.clear();
        } else {
            let seg_end = if let Some(stripped) = input.strip_prefix('/') {
                stripped.find('/').map_or(input.len(), |i| i + 1)
            } else {
                input.find('/').unwrap_or(input.len())
            };
            out.push_str(&input[..seg_end]);
            input.replace_range(0..seg_end, "");
        }
    }
    out
}

fn pop_last_segment(out: &mut String) {
    match out.rfind('/') {
        Some(i) => out.truncate(i),
        None => out.clear(),
    }
}

/// RFC 3986 §5.3 reference resolution of `r` against `base`.
fn resolve(base: &Components, r: &Components) -> Components {
    if r.scheme.is_some() {
        return Components {
            scheme: r.scheme.clone(),
            authority: r.authority.clone(),
            path: remove_dot_segments(&r.path),
            query: r.query.clone(),
            fragment: r.fragment.clone(),
        };
    }
    let (authority, path, query) = if r.authority.is_some() {
        (
            r.authority.clone(),
            remove_dot_segments(&r.path),
            r.query.clone(),
        )
    } else if r.path.is_empty() {
        let query = if r.query.is_some() {
            r.query.clone()
        } else {
            base.query.clone()
        };
        (base.authority.clone(), base.path.clone(), query)
    } else if r.path.starts_with('/') {
        (
            base.authority.clone(),
            remove_dot_segments(&r.path),
            r.query.clone(),
        )
    } else {
        let merged = merge(base, &r.path);
        (
            base.authority.clone(),
            remove_dot_segments(&merged),
            r.query.clone(),
        )
    };
    Components {
        scheme: base.scheme.clone(),
        authority,
        path,
        query,
        fragment: r.fragment.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Url {
        Url::parse(s).expect("should parse")
    }

    #[test]
    fn parses_full_absolute_url() {
        let u = parse("https://user:pw@Example.COM:8443/a/b?x=1&y=2#frag");
        assert_eq!(u.scheme(), "https");
        assert_eq!(u.username(), "user");
        assert_eq!(u.password(), Some("pw"));
        assert_eq!(u.host(), Some("example.com")); // host lowercased
        assert_eq!(u.port(), Some(8443));
        assert_eq!(u.path(), "/a/b");
        assert_eq!(u.query(), Some("x=1&y=2"));
        assert_eq!(u.fragment(), Some("frag"));
    }

    #[test]
    fn elides_default_port_and_lowercases_scheme() {
        let u = parse("HTTP://Example.com:80/");
        assert_eq!(u.scheme(), "http");
        assert_eq!(u.port(), None);
        assert_eq!(u.port_or_default(), Some(80));
        assert_eq!(u.to_string(), "http://example.com/");
    }

    #[test]
    fn empty_path_becomes_root_for_special_scheme() {
        let u = parse("https://example.com");
        assert_eq!(u.path(), "/");
        assert_eq!(u.to_string(), "https://example.com/");
    }

    #[test]
    fn removes_dot_segments() {
        let u = parse("http://h/a/b/../c/./d");
        assert_eq!(u.path(), "/a/c/d");
    }

    #[test]
    fn percent_encodes_spaces_per_component() {
        let u = parse("http://h/a b?c d#e f");
        assert_eq!(u.path(), "/a%20b");
        assert_eq!(u.query(), Some("c%20d"));
        assert_eq!(u.fragment(), Some("e%20f"));
    }

    #[test]
    fn parses_ipv6_host() {
        let u = parse("http://[::1]:8080/x");
        assert_eq!(u.host(), Some("[::1]"));
        assert_eq!(u.port(), Some(8080));
    }

    #[test]
    fn rejects_relative_without_base() {
        assert_eq!(Url::parse("/just/a/path"), Err(ParseError::MissingScheme));
    }

    #[test]
    fn rejects_special_scheme_without_host() {
        assert_eq!(Url::parse("https:///path"), Err(ParseError::MissingHost));
    }

    #[test]
    fn non_special_scheme_keeps_opaque_path() {
        let u = parse("mailto:a@b.com");
        assert_eq!(u.scheme(), "mailto");
        assert_eq!(u.host(), None);
        assert_eq!(u.path(), "a@b.com");
        assert_eq!(u.to_string(), "mailto:a@b.com");
    }

    /// RFC 3986 §5.4.1 "normal examples", base `http://a/b/c/d;p?q`. Special-
    /// scheme empty-path normalization makes `//g` resolve to `http://g/`.
    #[test]
    fn rfc3986_reference_resolution() {
        let base = parse("http://a/b/c/d;p?q");
        let cases = [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
        ];
        for (reference, expected) in cases {
            let got = base.join(reference).expect("resolve").to_string();
            assert_eq!(got, expected, "resolving {reference:?}");
        }
    }
}
