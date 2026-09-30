//! The `http` host function: one policy-checked HTTP/1.1 exchange with
//! per-hop host checks, a private-range guard, cookie scoping and a
//! per-plugin session jar.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use hya_net::cookies::now_secs;
use hya_net::{Connector, CookieJar, Target};
use hya_plugin_api::limits::{MAX_HEADERS_PER_TRACK, MAX_HTTP_BODY, MAX_URL};
use hya_plugin_api::{ErrorCode, PluginError};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use crate::accept::check_headers;
use crate::matcher::HostList;

const MAX_HOPS: usize = 5;
const PER_REQUEST: Duration = Duration::from_secs(30);
const MAX_HEAD: usize = 64 * 1024;
const USER_AGENT: &str = concat!("Hydra/", env!("CARGO_PKG_VERSION"), " plugin");

/// A request as the guest writes it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpRequest {
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Request body, base64.
    #[serde(default)]
    pub body_b64: Option<String>,
}

fn get() -> String {
    "GET".into()
}

/// A response as the guest reads it; `Set-Cookie` never appears.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body_b64: String,
}

/// What one plugin may reach and what cookies ride along.
#[derive(Clone)]
pub struct HttpPolicy {
    pub http: HostList,
    pub cookies: HostList,
    /// The user's browser cookies, already filtered to the `cookies` hosts.
    pub user_cookies: CookieJar,
    pub session: CookieJar,
    /// Explicit host route; HTTP proxy credentials never become guest headers.
    pub proxy: Option<hya_net::Proxy>,
}

/// Builds Hydra's TLS transport with the host-selected SOCKS route when present.
pub fn connector(
    proxy: Option<&hya_net::Proxy>,
) -> Result<hya_net::tls::TlsCapableConnector, PluginError> {
    let connector = hya_net::tls::TlsCapableConnector::new().map_err(|e| net_err(e.to_string()))?;
    Ok(match proxy.filter(|p| p.kind.is_socks()) {
        Some(proxy) => connector.with_socks(proxy.clone()),
        None => connector,
    })
}

/// Evicts oldest session cookies until their serialized jar fits the host quota.
pub fn bound_session(jar: &mut CookieJar) {
    let now = now_secs();
    jar.purge(now, false);
    while hya_net::cookies::netscape::render(jar, true, now).0.len()
        > hya_plugin_api::limits::MAX_SESSION_JAR
    {
        let mut kept = CookieJar::new();
        for cookie in jar.iter().skip(1) {
            kept.insert(cookie.clone());
        }
        *jar = kept;
    }
}

fn net_err(msg: impl Into<String>) -> PluginError {
    PluginError::new(ErrorCode::Network, msg)
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return is_private(IpAddr::V4(m));
            }
            let first = v.segments()[0];
            v.is_loopback()
                || v.is_unspecified()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80
        }
    }
}

async fn guard_destination(url: &Url, policy: &HttpPolicy) -> Result<(), PluginError> {
    let host = url.host_str().unwrap_or_default();
    if !policy.http.allows(url) {
        return Err(net_err(format!(
            "`{host}` is outside the `http` permission"
        )));
    }
    if policy.http.names_literal(host) {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| net_err(format!("cannot resolve `{host}`: {e}")))?;
    for a in addrs {
        if is_private(a.ip()) {
            return Err(net_err(format!(
                "`{host}` resolves to a private address the manifest does not name"
            )));
        }
    }
    Ok(())
}

fn target_for(url: &Url) -> Result<Target, PluginError> {
    let host = url.host_str().ok_or_else(|| net_err("url has no host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let mut path = url.path().to_string();
    if let Some(q) = url.query() {
        path.push('?');
        path.push_str(q);
    }
    Ok(match url.scheme() {
        "https" => Target::direct_tls(host, port, &path),
        _ => Target::direct(host, port, &path),
    })
}

fn authority(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    }
}

fn cookie_header(policy: &HttpPolicy, url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let path = &target_for(url).ok()?.path;
    let secure = url.scheme() == "https";
    let now = now_secs();
    let user = policy
        .cookies
        .allows(url)
        .then(|| policy.user_cookies.header_value(host, path, secure, now))
        .flatten();
    let session = policy.session.header_value(host, path, secure, now);
    match (user, session) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    }
}

struct Raw {
    status: u16,
    head: String,
    body: Vec<u8>,
}

async fn exchange<C: Connector>(
    connector: &C,
    url: &Url,
    method: &str,
    headers: &BTreeMap<String, String>,
    cookie: Option<String>,
    body: &[u8],
    cap: usize,
    proxy: Option<&hya_net::Proxy>,
) -> Result<Raw, PluginError> {
    let mut target = target_for(url)?;
    let proxy = proxy.filter(|p| !p.kind.is_socks());
    if let Some(proxy) = proxy {
        let origin = format!(
            "{}:{}",
            url.host_str().unwrap_or_default(),
            url.port_or_known_default().unwrap_or(80)
        );
        target = Target::via_proxy(&proxy.host, proxy.port, &origin, &target.path);
        target.tls = url.scheme() == "https";
        if let Some(user) = &proxy.username {
            let auth = hya_net::base64::encode(
                format!("{user}:{}", proxy.password.as_deref().unwrap_or_default()).as_bytes(),
            );
            target
                .headers
                .push(format!("Proxy-Authorization: Basic {auth}"));
        }
    }
    let mut stream = connector
        .connect(&target)
        .await
        .map_err(|e| net_err(format!("connect to {}: {e}", authority(url))))?;
    let mut req = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        if proxy.is_some() && !target.tls {url.as_str()} else {&target.path},
        authority(url)
    );
    if !target.tls {
        for header in &target.headers {
            req.push_str(header);
            req.push_str("\r\n");
        }
    }
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("user-agent") && !k.eq_ignore_ascii_case("accept-encoding") {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    if let Some(c) = cookie {
        req.push_str(&format!("Cookie: {c}\r\n"));
    }
    if !body.is_empty() || method == "POST" || method == "PUT" {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    let io_err = |e: std::io::Error| net_err(format!("{}: {e}", authority(url)));
    stream.write_all(req.as_bytes()).await.map_err(io_err)?;
    stream.write_all(body).await.map_err(io_err)?;

    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    let mut head_end: Option<usize> = None;
    let mut want: Option<usize> = None;
    let mut chunked = false;
    loop {
        let n = match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(io_err(e)),
        };
        buf.extend_from_slice(&chunk[..n]);
        if head_end.is_none() {
            if let Some(i) = find(&buf, b"\r\n\r\n") {
                head_end = Some(i + 4);
                let head = String::from_utf8_lossy(&buf[..i]).to_string();
                want = header_of(&head, "content-length").and_then(|v| v.trim().parse().ok());
                chunked = header_of(&head, "transfer-encoding")
                    .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
            } else if buf.len() > MAX_HEAD {
                return Err(net_err("response head is too large"));
            }
        }
        if let Some(start) = head_end {
            if buf.len() - start > cap + 8 {
                return Err(net_err("response body exceeds its size limit"));
            }
            if want.is_some_and(|w| buf.len() - start >= w) {
                break;
            }
            if chunked && buf.ends_with(b"0\r\n\r\n") {
                break;
            }
        }
    }
    let start = head_end.ok_or_else(|| net_err("no response head"))?;
    let head = String::from_utf8_lossy(&buf[..start - 4]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| net_err("malformed status line"))?;
    let mut body = buf.split_off(start);
    if chunked {
        body = dechunk(&body);
    } else if let Some(w) = want {
        body.truncate(w);
    }
    if body.len() > cap {
        return Err(net_err("response body exceeds its size limit"));
    }
    Ok(Raw { status, head, body })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn header_of(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(i) = find(rest, b"\r\n") {
        let size = std::str::from_utf8(&rest[..i])
            .ok()
            .and_then(|l| usize::from_str_radix(l.split(';').next().unwrap_or("").trim(), 16).ok());
        let Some(n) = size.filter(|&n| n > 0) else {
            break;
        };
        let body = &rest[i + 2..];
        if body.len() < n {
            out.extend_from_slice(body);
            break;
        }
        out.extend_from_slice(&body[..n]);
        rest = body.get(n + 2..).unwrap_or_default();
    }
    out
}

/// Runs one request, following and re-checking up to five redirects.
///
/// # Errors
/// `network` for refusals and transport failures, `invalid_input` for a
/// malformed request.
pub async fn perform<C: Connector>(
    connector: &C,
    policy: &mut HttpPolicy,
    req: &HttpRequest,
) -> Result<HttpResponse, PluginError> {
    perform_bounded(connector, policy, req, MAX_HTTP_BODY).await
}

pub(crate) async fn perform_bounded<C: Connector>(
    connector: &C,
    policy: &mut HttpPolicy,
    req: &HttpRequest,
    cap: usize,
) -> Result<HttpResponse, PluginError> {
    if req.url.len() > MAX_URL {
        return Err(PluginError::new(ErrorCode::InvalidInput, "url is too long"));
    }
    let mut url = Url::parse(&req.url)
        .map_err(|e| PluginError::new(ErrorCode::InvalidInput, format!("bad url: {e}")))?;
    let method = req.method.to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "POST" | "HEAD" | "PUT" | "DELETE") {
        return Err(PluginError::new(
            ErrorCode::InvalidInput,
            "unsupported method",
        ));
    }
    if req.headers.len() > MAX_HEADERS_PER_TRACK {
        return Err(PluginError::new(
            ErrorCode::InvalidInput,
            "too many headers",
        ));
    }
    check_headers(&req.headers)
        .map_err(|e| PluginError::new(ErrorCode::PermissionDenied, e.message))?;
    let mut body = match &req.body_b64 {
        Some(b) => hya_net::base64::decode(b)
            .ok_or_else(|| PluginError::new(ErrorCode::InvalidInput, "body_b64 is not base64"))?,
        None => Vec::new(),
    };
    let mut method = method;
    let first_origin = url.origin();
    let mut headers = req.headers.clone();

    for _ in 0..=MAX_HOPS {
        if first_origin.ascii_serialization().starts_with("https://") && url.scheme() != "https" {
            return Err(net_err("refusing HTTPS downgrade"));
        }
        if !matches!(url.scheme(), "http" | "https") {
            return Err(net_err(format!("scheme `{}` is not allowed", url.scheme())));
        }
        guard_destination(&url, policy).await?;
        let cookie = cookie_header(policy, &url);
        let raw = tokio::time::timeout(
            PER_REQUEST,
            exchange(
                connector,
                &url,
                &method,
                &headers,
                cookie,
                &body,
                cap,
                policy.proxy.as_ref(),
            ),
        )
        .await
        .map_err(|_| PluginError::new(ErrorCode::Deadline, "request timed out"))??;

        if let Some(host) = url.host_str() {
            let path = target_for(&url)?.path;
            policy
                .session
                .store_response(&raw.head, host, &path, now_secs());
            bound_session(&mut policy.session);
        }
        if (300..400).contains(&raw.status) && raw.status != 304 {
            if let Some(loc) = header_of(&raw.head, "location") {
                url = url
                    .join(&loc)
                    .map_err(|e| net_err(format!("bad redirect target: {e}")))?;
                if raw.status != 307 && raw.status != 308 {
                    method = "GET".into();
                    body.clear();
                }
                if url.origin() != first_origin {
                    headers.clear();
                }
                continue;
            }
        }
        let mut out = BTreeMap::new();
        for line in raw.head.lines().skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                if k == "set-cookie" {
                    continue;
                }
                out.entry(k)
                    .and_modify(|e: &mut String| {
                        e.push_str(", ");
                        e.push_str(v.trim());
                    })
                    .or_insert_with(|| v.trim().to_string());
            }
        }
        return Ok(HttpResponse {
            status: raw.status,
            url: url.to_string(),
            headers: out,
            body_b64: hya_net::base64::encode(&raw.body),
        });
    }
    Err(net_err("too many redirects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_net::TlsCapableConnector;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[test]
    fn session_quota_evicts_oldest_cookies_and_keeps_the_latest() {
        let mut jar = CookieJar::new();
        for i in 0..100 {
            jar.insert(hya_net::cookies::Cookie::new(
                &format!("cookie-{i}"),
                &"x".repeat(4096),
                "example.com",
            ));
        }
        bound_session(&mut jar);
        assert!(
            hya_net::cookies::netscape::render(&jar, true, now_secs())
                .0
                .len()
                <= hya_plugin_api::limits::MAX_SESSION_JAR
        );
        assert!(!jar.iter().any(|c| c.name == "cookie-0"));
        assert!(jar.iter().any(|c| c.name == "cookie-99"));
    }
    type Seen = Arc<Mutex<Vec<String>>>;

    async fn serve(responses: Vec<String>) -> (u16, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            for resp in responses {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let mut got = Vec::new();
                loop {
                    let n = s.read(&mut buf).await.unwrap();
                    got.extend_from_slice(&buf[..n]);
                    if n == 0 || find(&got, b"\r\n\r\n").is_some() {
                        break;
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&got).into_owned());
                s.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        (port, seen)
    }

    fn policy(hosts: &[&str]) -> HttpPolicy {
        HttpPolicy {
            proxy: None,
            http: HostList::parse(&hosts.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap(),
            cookies: HostList::default(),
            user_cookies: CookieJar::new(),
            session: CookieJar::new(),
        }
    }

    fn req(url: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body_b64: None,
        }
    }

    fn ok(body: &str, extra: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn connector() -> TlsCapableConnector {
        TlsCapableConnector::new().unwrap()
    }

    #[tokio::test]
    async fn host_proxy_routes_http_and_keeps_origin_permissions() {
        let (port, seen) = serve(vec![ok("proxied", "")]).await;
        let mut p = policy(&["origin.example"]);
        p.proxy =
            Some(hya_net::Proxy::parse(&format!("http://user:pass@127.0.0.1:{port}")).unwrap());
        let result = perform(&connector(), &mut p, &req("http://origin.example/file"))
            .await
            .unwrap();
        assert_eq!(
            hya_net::base64::decode(&result.body_b64).unwrap(),
            b"proxied"
        );
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("GET http://origin.example/file HTTP/1.1"));
        assert!(request.contains("Host: origin.example\r\n"));
        assert!(request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"));
        assert!(
            perform(&connector(), &mut p, &req("http://other.example/file"))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn a_named_loopback_host_is_reachable_and_the_body_round_trips() {
        let (port, seen) = serve(vec![ok("hello", "X-A: 1\r\n")]).await;
        let mut p = policy(&["127.0.0.1"]);
        let r = perform(
            &connector(),
            &mut p,
            &req(&format!("http://127.0.0.1:{port}/a?b=1")),
        )
        .await
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(hya_net::base64::decode(&r.body_b64).unwrap(), b"hello");
        assert_eq!(r.headers["x-a"], "1");
        let sent = seen.lock().unwrap()[0].clone();
        assert!(sent.starts_with("GET /a?b=1 HTTP/1.1"), "{sent}");
        assert!(sent.contains("User-Agent: Hydra/"));
    }

    #[tokio::test]
    async fn a_host_outside_the_list_is_refused_before_any_connection() {
        let mut p = policy(&["example.com"]);
        let e = perform(&connector(), &mut p, &req("http://127.0.0.1:9/x"))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Network);
        assert!(e.message.contains("outside"));
    }

    #[tokio::test]
    async fn a_wildcard_grant_does_not_open_loopback() {
        let mut p = policy(&["*"]);
        let e = perform(&connector(), &mut p, &req("http://localhost:9/x"))
            .await
            .unwrap_err();
        assert!(e.message.contains("private"), "{}", e.message);
    }

    #[tokio::test]
    async fn redirects_are_rechecked_per_hop() {
        let (port, _) = serve(vec!["HTTP/1.1 302 Found\r\nLocation: http://evil.example.org/x\r\nContent-Length: 0\r\n\r\n".to_string()])
        .await;
        let mut p = policy(&["127.0.0.1"]);
        let e = perform(
            &connector(),
            &mut p,
            &req(&format!("http://127.0.0.1:{port}/")),
        )
        .await
        .unwrap_err();
        assert!(e.message.contains("outside"), "{}", e.message);
    }

    #[tokio::test]
    async fn a_same_host_redirect_is_followed_and_headers_drop_cross_origin() {
        let (port, seen) = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n".into(),
            ok("done", ""),
        ])
        .await;
        let mut p = policy(&["127.0.0.1"]);
        let mut r = req(&format!("http://127.0.0.1:{port}/"));
        r.headers.insert("X-Token".into(), "t".into());
        let out = perform(&connector(), &mut p, &r).await.unwrap();
        assert!(out.url.ends_with("/next"));
        let log = seen.lock().unwrap();
        assert!(log[1].starts_with("GET /next"));
        assert!(log[1].contains("X-Token: t"));
    }

    #[tokio::test]
    async fn redirect_loops_stop() {
        let loop_resp =
            "HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n".to_string();
        let (port, _) = serve(vec![loop_resp; 8]).await;
        let mut p = policy(&["127.0.0.1"]);
        let e = perform(
            &connector(),
            &mut p,
            &req(&format!("http://127.0.0.1:{port}/")),
        )
        .await
        .unwrap_err();
        assert!(e.message.contains("redirects"));
    }

    #[tokio::test]
    async fn set_cookie_is_absorbed_and_replayed_not_returned() {
        let (port, seen) = serve(vec![
            ok("a", "Set-Cookie: sid=abc; Path=/\r\n"),
            ok("b", ""),
        ])
        .await;
        let mut p = policy(&["127.0.0.1"]);
        let url = format!("http://127.0.0.1:{port}/");
        let first = perform(&connector(), &mut p, &req(&url)).await.unwrap();
        assert!(!first.headers.contains_key("set-cookie"));
        perform(&connector(), &mut p, &req(&url)).await.unwrap();
        assert!(seen.lock().unwrap()[1].contains("Cookie: sid=abc"));
    }

    #[tokio::test]
    async fn browser_cookies_ride_only_for_granted_hosts() {
        let mut jar = CookieJar::new();
        jar.add_pairs("br=1", "127.0.0.1");
        for granted in [true, false] {
            let (port, seen) = serve(vec![ok("x", "")]).await;
            let mut p = policy(&["127.0.0.1"]);
            p.user_cookies = jar.clone();
            if granted {
                p.cookies = HostList::parse(&["127.0.0.1".into()]).unwrap();
            }
            perform(
                &connector(),
                &mut p,
                &req(&format!("http://127.0.0.1:{port}/")),
            )
            .await
            .unwrap();
            assert_eq!(seen.lock().unwrap()[0].contains("br=1"), granted);
        }
    }

    #[tokio::test]
    async fn post_sends_a_body_and_forbidden_headers_are_denied() {
        let (port, seen) = serve(vec![ok("x", "")]).await;
        let mut p = policy(&["127.0.0.1"]);
        let mut r = req(&format!("http://127.0.0.1:{port}/"));
        r.method = "post".into();
        r.body_b64 = Some(hya_net::base64::encode(b"k=v"));
        perform(&connector(), &mut p, &r).await.unwrap();
        assert!(seen.lock().unwrap()[0].contains("Content-Length: 3"));

        r.headers.insert("Cookie".into(), "x=y".into());
        let e = perform(&connector(), &mut p, &r).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn chunked_bodies_and_bad_requests() {
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n".to_string();
        let (port, _) = serve(vec![chunked]).await;
        let mut p = policy(&["127.0.0.1"]);
        let r = perform(
            &connector(),
            &mut p,
            &req(&format!("http://127.0.0.1:{port}/")),
        )
        .await
        .unwrap();
        assert_eq!(
            hya_net::base64::decode(&r.body_b64).unwrap(),
            b"hello world"
        );

        for bad in [
            HttpRequest {
                method: "TRACE".into(),
                ..req("http://127.0.0.1/")
            },
            req("not a url"),
            req("ftp://127.0.0.1/x"),
            HttpRequest {
                body_b64: Some("***".into()),
                ..req("http://127.0.0.1/")
            },
        ] {
            assert!(perform(&connector(), &mut p, &bad).await.is_err());
        }
    }

    #[test]
    fn private_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.1.1",
            "192.168.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2606:4700::1"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }
}
