//! Chrome-style match patterns for `claims` and the `http`, `sources` and
//! `cookies` host lists. One implementation, one set of test vectors.

use url::{Host, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchemeMatch {
    HttpOrHttps,
    Literal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostMatch {
    Any,
    Subdomains(String),
    Exact(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortMatch {
    Any,
    Exact(u16),
}

/// A host pattern: `example.com`, `*.example.com`, `*`, optionally `:port` or `:*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPattern {
    host: HostMatch,
    port: PortMatch,
}

impl HostPattern {
    /// Parses a host pattern without scheme or path.
    ///
    /// # Errors
    /// Returns why the pattern is malformed.
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.contains("://") || s.contains('/') {
            return Err(format!(
                "host pattern `{s}` must not carry a scheme or path"
            ));
        }
        let (host, port) = match s.rsplit_once(':') {
            Some((h, "*")) => (h, PortMatch::Any),
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (
                h,
                PortMatch::Exact(p.parse().map_err(|_| format!("bad port in `{s}`"))?),
            ),
            Some(_) => return Err(format!("host pattern `{s}` has an unusable port")),
            None => (s, PortMatch::Any),
        };
        let host = match host {
            "" => return Err("host pattern is empty".into()),
            "*" => HostMatch::Any,
            h => {
                if let Some(rest) = h.strip_prefix("*.") {
                    HostMatch::Subdomains(normalize_host(rest)?)
                } else if h.contains('*') {
                    return Err(format!("`*` may only lead a host pattern: `{s}`"));
                } else {
                    HostMatch::Exact(normalize_host(h)?)
                }
            }
        };
        Ok(Self { host, port })
    }

    /// Whether `host` (already a URL host, not yet normalized) and `port` match.
    pub fn matches(&self, host: &str, port: Option<u16>) -> bool {
        let Ok(host) = normalize_host(host) else {
            return false;
        };
        if host.contains(':') {
            return false;
        }
        let port_ok = match (self.port, port) {
            (PortMatch::Any, _) => true,
            (PortMatch::Exact(want), Some(got)) => want == got,
            (PortMatch::Exact(_), None) => false,
        };
        port_ok
            && match &self.host {
                HostMatch::Any => true,
                HostMatch::Exact(h) => *h == host,
                HostMatch::Subdomains(base) => host
                    .strip_suffix(base.as_str())
                    .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1),
            }
    }

    /// Whether the URL's host and port match.
    pub fn matches_url(&self, url: &Url) -> bool {
        match url.host() {
            Some(Host::Domain(d)) => self.matches(d, url.port_or_known_default()),
            Some(Host::Ipv4(ip)) => self.matches(&ip.to_string(), url.port_or_known_default()),
            _ => false,
        }
    }
}

fn normalize_host(h: &str) -> Result<String, String> {
    let h = h.trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return Err("empty host".into());
    }
    if h.contains(':') || h.starts_with('[') {
        return Ok(h);
    }
    idna::domain_to_ascii(&h).map_err(|e| format!("host `{h}` is not valid: {e}"))
}

/// A full claim pattern: `scheme://host[:port]/path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPattern {
    scheme: SchemeMatch,
    host: HostPattern,
    path: String,
}

impl UrlPattern {
    /// Parses a Chrome-style match pattern.
    ///
    /// # Errors
    /// Returns why the pattern is malformed.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (scheme, rest) = s
            .split_once("://")
            .ok_or_else(|| format!("pattern `{s}` has no scheme"))?;
        let scheme = match scheme {
            "*" => SchemeMatch::HttpOrHttps,
            "" => return Err("empty scheme".into()),
            lit => SchemeMatch::Literal(lit.to_ascii_lowercase()),
        };
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => return Err(format!("pattern `{s}` needs a path (use `/*`)")),
        };
        Ok(Self {
            scheme,
            host: HostPattern::parse(host)?,
            path: path.to_string(),
        })
    }

    /// Whether `address` matches. Unparseable addresses match nothing.
    pub fn matches(&self, address: &str) -> bool {
        let Ok(url) = Url::parse(address) else {
            return false;
        };
        let scheme_ok = match &self.scheme {
            SchemeMatch::HttpOrHttps => matches!(url.scheme(), "http" | "https"),
            SchemeMatch::Literal(l) => url.scheme() == l,
        };
        if !scheme_ok || !self.host.matches_url(&url) {
            return false;
        }
        let mut target = url.path().to_string();
        if let Some(q) = url.query() {
            target.push('?');
            target.push_str(q);
        }
        glob(&self.path, &target)
    }
}

fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&b| b == b'*')
}

/// A compiled list of host patterns; an empty list matches nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostList(Vec<HostPattern>);

impl HostList {
    /// # Errors
    /// Returns the first malformed pattern.
    pub fn parse(patterns: &[String]) -> Result<Self, String> {
        patterns
            .iter()
            .map(|p| HostPattern::parse(p))
            .collect::<Result<_, _>>()
            .map(Self)
    }

    pub fn allows(&self, url: &Url) -> bool {
        self.0.iter().any(|p| p.matches_url(url))
    }

    pub fn allows_host(&self, host: &str, port: Option<u16>) -> bool {
        self.0.iter().any(|p| p.matches(host, port))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether some entry names exactly this host, not through a wildcard.
    pub fn names_literal(&self, host: &str) -> bool {
        let Ok(host) = normalize_host(host) else {
            return false;
        };
        self.0
            .iter()
            .any(|p| matches!(&p.host, HostMatch::Exact(h) if *h == host))
    }

    /// True when some entry is the bare `*`.
    pub fn is_unrestricted(&self) -> bool {
        self.0
            .iter()
            .any(|p| p.host == HostMatch::Any && p.port == PortMatch::Any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(pattern: &str, url: &str) -> bool {
        UrlPattern::parse(pattern).unwrap().matches(url)
    }

    #[test]
    fn scheme_rules() {
        assert!(claim("https://example.com/*", "https://example.com/a"));
        assert!(!claim("https://example.com/*", "http://example.com/a"));
        assert!(claim("*://example.com/*", "http://example.com/a"));
        assert!(claim("*://example.com/*", "https://example.com/a"));
        assert!(!claim("*://example.com/*", "ftp://example.com/a"));
        assert!(claim("ftp://example.com/*", "ftp://example.com/a"));
    }

    #[test]
    fn wildcard_subdomain_excludes_the_bare_domain() {
        assert!(claim("*://*.example.com/*", "https://a.example.com/"));
        assert!(claim("*://*.example.com/*", "https://a.b.example.com/"));
        assert!(!claim("*://*.example.com/*", "https://example.com/"));
        assert!(!claim("*://*.example.com/*", "https://badexample.com/"));
        assert!(!claim(
            "*://*.example.com/*",
            "https://example.com.evil.net/"
        ));
    }

    #[test]
    fn host_is_case_insensitive_and_ignores_trailing_dot() {
        assert!(claim("https://Example.COM/*", "https://example.com./x"));
        assert!(claim("https://example.com/*", "https://EXAMPLE.com/x"));
    }

    #[test]
    fn idna_hosts_match_through_punycode() {
        assert!(claim(
            "https://bücher.example/*",
            "https://xn--bcher-kva.example/"
        ));
        assert!(claim(
            "https://xn--bcher-kva.example/*",
            "https://bücher.example/"
        ));
    }

    #[test]
    fn port_rules() {
        assert!(claim("https://example.com/*", "https://example.com:8443/"));
        assert!(claim(
            "https://example.com:8443/*",
            "https://example.com:8443/"
        ));
        assert!(!claim("https://example.com:8443/*", "https://example.com/"));
        assert!(!claim(
            "https://example.com:8443/*",
            "https://example.com:9000/"
        ));
        assert!(claim(
            "https://example.com:*/*",
            "https://example.com:9000/"
        ));
        assert!(claim("https://example.com:443/*", "https://example.com/"));
    }

    #[test]
    fn path_is_matched_with_the_query_and_without_the_fragment() {
        assert!(claim(
            "*://example.com/watch*",
            "https://example.com/watch?v=x"
        ));
        assert!(claim(
            "*://example.com/watch?v=*",
            "https://example.com/watch?v=x#t=5"
        ));
        assert!(!claim(
            "*://example.com/watch",
            "https://example.com/watch?v=x"
        ));
        assert!(claim("*://example.com/a/*/c", "https://example.com/a/b/c"));
        assert!(!claim("*://example.com/a/*/c", "https://example.com/a/b/d"));
        assert!(claim("*://example.com/*", "https://example.com/"));
    }

    #[test]
    fn ipv4_matches_only_itself_and_ipv6_never() {
        assert!(claim("*://10.0.0.5/*", "http://10.0.0.5/x"));
        assert!(!claim("*://10.0.0.5/*", "http://10.0.0.6/x"));
        assert!(!claim("*://*/*", "http://[::1]/x"));
        assert!(UrlPattern::parse("*://[::1]/*").is_err());
    }

    #[test]
    fn any_host_pattern_matches_every_domain() {
        assert!(claim("*://*/*", "https://anything.example/x"));
    }

    #[test]
    fn unparseable_addresses_match_nothing() {
        assert!(!claim("*://*/*", "not a url"));
        assert!(!claim("*://*/*", ""));
    }

    #[test]
    fn rejects_malformed_patterns() {
        for bad in [
            "example.com/*",
            "https://example.com",
            "https://exa*mple.com/*",
            "https://*example.com/*",
            "https:///x",
            "://example.com/*",
            "https://example.com:abc/*",
        ] {
            assert!(UrlPattern::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn host_list_semantics() {
        let list = HostList::parse(&["example.com".into(), "*.cdn.net".into()]).unwrap();
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(list.allows(&u("https://example.com/x")));
        assert!(list.allows(&u("https://a.cdn.net/x")));
        assert!(!list.allows(&u("https://cdn.net/x")));
        assert!(!list.allows(&u("https://other.org/x")));
        assert!(!HostList::default().allows(&u("https://example.com/")));
        assert!(!list.is_unrestricted());
        assert!(HostList::parse(&["*".into()]).unwrap().is_unrestricted());
    }

    #[test]
    fn host_list_rejects_scheme_or_path() {
        assert!(HostList::parse(&["https://example.com".into()]).is_err());
        assert!(HostList::parse(&["example.com/x".into()]).is_err());
    }

    #[test]
    fn a_portless_grant_covers_every_port() {
        let list = HostList::parse(&["example.com".into()]).unwrap();
        assert!(list.allows_host("example.com", Some(8080)));
        let pinned = HostList::parse(&["example.com:8443".into()]).unwrap();
        assert!(!pinned.allows_host("example.com", Some(8080)));
        assert!(pinned.allows_host("example.com", Some(8443)));
    }
}
