//! A minimal cookie jar.
//!
//! Parses `Set-Cookie` response headers, scopes cookies by domain and path, and
//! produces the `Cookie` request header for a URL — enough for session/auth
//! flows across a browsing session. P0 scope: session cookies only (no
//! `Expires`/`Max-Age` lifetime handling), `Secure`/`HttpOnly`/`SameSite`
//! ignored; the security policy around those layers on later.

use janus_bytes::Url;

/// A stored cookie.
#[derive(Clone, Debug)]
pub struct Cookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Scope domain (no leading dot; host-only cookies store the request host).
    pub domain: String,
    /// Scope path.
    pub path: String,
}

/// An in-memory cookie store.
#[derive(Default, Debug)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    /// An empty jar.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored cookies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// Whether the jar is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Store a cookie from one `Set-Cookie` header value, scoped to `request`.
    pub fn set_from_header(&mut self, set_cookie: &str, request: &Url) {
        let mut parts = set_cookie.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
            return;
        };
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.is_empty() {
            return;
        }

        let mut domain = request.host().unwrap_or_default().to_ascii_lowercase();
        let mut path = default_path(request);
        for attr in parts {
            if let Some((key, val)) = attr.split_once('=') {
                match key.trim().to_ascii_lowercase().as_str() {
                    "domain" => {
                        let d = val.trim().trim_start_matches('.');
                        if !d.is_empty() {
                            domain = d.to_ascii_lowercase();
                        }
                    }
                    "path" => {
                        let p = val.trim();
                        if p.starts_with('/') {
                            path = p.to_string();
                        }
                    }
                    _ => {}
                }
            }
        }

        self.cookies
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
        self.cookies.push(Cookie {
            name,
            value,
            domain,
            path,
        });
    }

    /// The `Cookie` header value for `url`, or `None` if nothing matches.
    #[must_use]
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let host = url.host()?;
        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        let pairs: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| domain_matches(host, &c.domain) && path_matches(path, &c.path))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }
}

fn default_path(url: &Url) -> String {
    let p = url.path();
    if !p.starts_with('/') {
        return "/".to_string();
    }
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

fn domain_matches(host: &str, domain: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn stores_and_sends_scoped_by_path() {
        let mut jar = CookieJar::new();
        let req = url("https://example.com/dir/page");
        jar.set_from_header("sid=abc; Path=/", &req);
        jar.set_from_header("theme=dark", &req); // default path = /dir

        assert_eq!(
            jar.header_for(&url("https://example.com/dir/page"))
                .as_deref(),
            Some("sid=abc; theme=dark")
        );
        // Outside /dir, only the root-path cookie is sent.
        assert_eq!(
            jar.header_for(&url("https://example.com/other")).as_deref(),
            Some("sid=abc")
        );
    }

    #[test]
    fn domain_scoping_includes_subdomains_but_not_others() {
        let mut jar = CookieJar::new();
        jar.set_from_header("a=1; Domain=example.com", &url("https://www.example.com/"));
        assert!(jar.header_for(&url("https://api.example.com/")).is_some());
        assert!(jar.header_for(&url("https://example.org/")).is_none());
    }

    #[test]
    fn same_name_domain_path_replaces() {
        let mut jar = CookieJar::new();
        let req = url("https://x.com/");
        jar.set_from_header("k=1", &req);
        jar.set_from_header("k=2", &req);
        assert_eq!(jar.len(), 1);
        assert_eq!(jar.header_for(&req).as_deref(), Some("k=2"));
    }

    #[test]
    fn ignores_malformed() {
        let mut jar = CookieJar::new();
        jar.set_from_header("novalue", &url("https://x.com/"));
        jar.set_from_header("=v", &url("https://x.com/"));
        assert!(jar.is_empty());
    }
}
