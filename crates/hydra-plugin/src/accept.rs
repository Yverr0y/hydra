//! Plan acceptance: the checks and clamps every plan, and every refreshed
//! source or header set, passes before reaching a front end.

use std::collections::{BTreeMap, HashSet};

use hya_plugin_api::limits::{
    MAX_CHUNK_HINT, MAX_HEADERS_PER_TRACK, MAX_HEADER_NAME, MAX_HEADER_VALUE,
    MAX_SOURCES_PER_TRACK, MAX_STRING_FIELD, MAX_TRACKS, MAX_URL, MIN_CHUNK_HINT,
};
use hya_plugin_api::{ErrorCode, Plan, PluginError, Source};
use url::Url;

use crate::matcher::HostList;

const FORBIDDEN_HEADERS: &[&str] = &[
    "cookie",
    "authorization",
    "proxy-authorization",
    "host",
    "range",
    "if-range",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
];

const DIGEST_ALGOS: &[(&str, usize)] = &[("sha-256", 64), ("sha-1", 40), ("md5", 32)];

fn invalid(msg: impl Into<String>) -> PluginError {
    PluginError::new(ErrorCode::InvalidPlan, msg)
}

/// The host-side bounds a plan is clamped to.
#[derive(Debug, Clone, Copy)]
pub struct Clamps {
    pub max_connections: u32,
}

/// Checks one source URL against the `sources` permission.
///
/// # Errors
/// `invalid_plan` when the URL is malformed, not http(s)/ftp, or not allowed.
pub fn check_source(url: &str, allowed: &HostList) -> Result<(), PluginError> {
    if url.len() > MAX_URL {
        return Err(invalid("source url is too long"));
    }
    let parsed = Url::parse(url).map_err(|e| invalid(format!("source `{url}`: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https" | "ftp") {
        return Err(invalid(format!(
            "source `{url}` has scheme `{}`",
            parsed.scheme()
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(format!("source `{url}` carries credentials")));
    }
    if !allowed.allows(&parsed) {
        return Err(invalid(format!(
            "source host `{}` is outside the `sources` permission",
            parsed.host_str().unwrap_or("")
        )));
    }
    Ok(())
}

/// Checks a header set a plugin may attach to a track.
///
/// # Errors
/// `invalid_plan` on a forbidden, malformed or oversized header.
pub fn check_headers(headers: &BTreeMap<String, String>) -> Result<(), PluginError> {
    if headers.len() > MAX_HEADERS_PER_TRACK {
        return Err(invalid("too many headers on a track"));
    }
    for (name, value) in headers {
        if name.is_empty()
            || name.len() > MAX_HEADER_NAME
            || !name.bytes().all(is_token_byte)
            || value.len() > MAX_HEADER_VALUE
            || value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
        {
            return Err(invalid(format!("header `{name}` is malformed")));
        }
        let lower = name.to_ascii_lowercase();
        if FORBIDDEN_HEADERS.contains(&lower.as_str()) || lower.starts_with("proxy-") {
            return Err(invalid(format!(
                "header `{name}` may not be set by a plugin"
            )));
        }
    }
    Ok(())
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn check_digest(digest: &str) -> Result<(), PluginError> {
    let ok = digest.split_once(':').is_some_and(|(algo, hex)| {
        DIGEST_ALGOS
            .iter()
            .any(|&(a, len)| a == algo && hex.len() == len)
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
    });
    ok.then_some(())
        .ok_or_else(|| invalid(format!("digest `{digest}` is not `algo:hex`")))
}

/// Validates sources and headers, as `refresh` replies carry only those.
///
/// # Errors
/// `invalid_plan` for the first violated rule.
pub fn check_track_transport(
    sources: &[Source],
    headers: &BTreeMap<String, String>,
    allowed: &HostList,
) -> Result<(), PluginError> {
    if sources.is_empty() {
        return Err(invalid("a track has no sources"));
    }
    if sources.len() > MAX_SOURCES_PER_TRACK {
        return Err(invalid("too many sources on a track"));
    }
    for s in sources {
        check_source(&s.url, allowed)?;
    }
    check_headers(headers)
}

/// Validates a plan and clamps its tuning hints in place.
///
/// # Errors
/// `invalid_plan` for the first violated rule.
pub fn accept(plan: &mut Plan, allowed: &HostList, clamps: Clamps) -> Result<(), PluginError> {
    if let Some(transfer) = &plan.transfer {
        crate::transfer::validate(transfer)?;
        if !plan.tracks.is_empty()
            || !plan.entries.is_empty()
            || plan.assemble != hya_plugin_api::Assemble::None
        {
            return Err(invalid(
                "transfer plans cannot contain tracks, playlists or assembly",
            ));
        }
    }
    if plan.id.is_empty() || plan.id.len() > MAX_STRING_FIELD {
        return Err(invalid("plan id is empty or too long"));
    }
    if plan
        .title
        .as_ref()
        .is_some_and(|t| t.len() > MAX_STRING_FIELD)
    {
        return Err(invalid("plan title is too long"));
    }
    if !plan.entries.is_empty() {
        if !plan.tracks.is_empty() || plan.entries.len() > MAX_TRACKS {
            return Err(invalid(
                "playlist must contain bounded entries and no media tracks",
            ));
        }
        let mut ids = HashSet::new();
        for entry in &plan.entries {
            if entry.id.is_empty() || entry.id.len() > MAX_STRING_FIELD || !ids.insert(&entry.id) {
                return Err(invalid("playlist entry id is empty or repeated"));
            }
            if entry
                .title
                .as_ref()
                .is_some_and(|title| title.len() > MAX_STRING_FIELD)
            {
                return Err(invalid("playlist entry title is too long"));
            }
            check_source(&entry.url, allowed)?;
            if !entry.url.starts_with("https://") && !entry.url.starts_with("http://") {
                return Err(invalid("playlist entries must use HTTP or HTTPS"));
            }
        }
        return Ok(());
    }
    if plan.transfer.is_some() {
        return Ok(());
    }
    if plan.tracks.is_empty() {
        return Err(invalid("plan has no tracks"));
    }
    if plan.tracks.len() > MAX_TRACKS {
        return Err(invalid("plan has too many tracks"));
    }
    let mut seen = HashSet::new();
    for t in &mut plan.tracks {
        if t.id.is_empty() || t.id.len() > MAX_STRING_FIELD || !seen.insert(t.id.clone()) {
            return Err(invalid(format!("track id `{}` is empty or repeated", t.id)));
        }
        check_track_transport(&t.sources, &t.headers, allowed)?;
        if let Some(d) = &t.digest {
            check_digest(d)?;
        }
        for field in [&t.container, &t.codec, &t.language].into_iter().flatten() {
            if field.len() > MAX_STRING_FIELD {
                return Err(invalid(format!("a field of track `{}` is too long", t.id)));
            }
        }
        for s in &mut t.sources {
            s.max_connections = s
                .max_connections
                .map(|m| m.clamp(1, clamps.max_connections.max(1)));
        }
        t.chunk_hint = t
            .chunk_hint
            .map(|c| c.clamp(MIN_CHUNK_HINT, MAX_CHUNK_HINT));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_plugin_api::Track;

    const CLAMPS: Clamps = Clamps { max_connections: 8 };

    fn allowed() -> HostList {
        HostList::parse(&["cdn.example.com".into(), "*.media.net".into()]).unwrap()
    }

    fn plan_of(track: Track) -> Plan {
        Plan::single("t", track)
    }

    fn rejected(mut plan: Plan) -> String {
        accept(&mut plan, &allowed(), CLAMPS).unwrap_err().message
    }

    #[test]
    fn playlists_accept_ordered_pages_and_reject_ambiguous_or_ungranted_entries() {
        let mut p = plan_of(Track::file("f", "https://cdn.example.com/media"));
        p.tracks.clear();
        p.entries = vec![hya_plugin_api::PlaylistEntry {
            id: "one".into(),
            url: "https://cdn.example.com/watch?v=one".into(),
            title: Some("First".into()),
        }];
        accept(&mut p, &allowed(), CLAMPS).unwrap();
        let mut mixed = p.clone();
        mixed
            .tracks
            .push(Track::file("f", "https://cdn.example.com/media"));
        assert!(accept(&mut mixed, &allowed(), CLAMPS).is_err());
        let mut duplicate = p.clone();
        duplicate.entries.push(duplicate.entries[0].clone());
        assert!(accept(&mut duplicate, &allowed(), CLAMPS).is_err());
        for url in [
            "https://outside.example/video",
            "ftp://cdn.example.com/video",
            "https://user:password@cdn.example.com/video",
        ] {
            let mut denied = p.clone();
            denied.entries[0].url = url.into();
            assert!(accept(&mut denied, &allowed(), CLAMPS).is_err());
        }
        for id in [String::new(), "x".repeat(MAX_STRING_FIELD + 1)] {
            let mut denied = p.clone();
            denied.entries[0].id = id;
            assert!(accept(&mut denied, &allowed(), CLAMPS).is_err());
        }
        let mut denied = p.clone();
        denied.entries[0].title = Some("x".repeat(MAX_STRING_FIELD + 1));
        assert!(accept(&mut denied, &allowed(), CLAMPS).is_err());
        p.entries = vec![p.entries[0].clone(); MAX_TRACKS + 1];
        assert!(accept(&mut p, &allowed(), CLAMPS).is_err());
    }

    #[test]
    fn a_plan_inside_the_permission_is_accepted() {
        let mut p = plan_of(
            Track::file("f", "https://cdn.example.com/a.bin").header("Referer", "https://x"),
        );
        accept(&mut p, &allowed(), CLAMPS).unwrap();
    }

    #[test]
    fn a_source_outside_the_permission_is_refused() {
        let msg = rejected(plan_of(Track::file("f", "https://evil.example.org/a")));
        assert!(msg.contains("outside"), "{msg}");
    }

    #[test]
    fn exfiltration_through_query_string_still_needs_an_allowed_host() {
        let msg = rejected(plan_of(Track::file("f", "https://evil.org/?k=secret")));
        assert!(msg.contains("outside"), "{msg}");
    }

    #[test]
    fn ipv6_literal_sources_cannot_pass() {
        assert!(accept(
            &mut plan_of(Track::file("f", "http://[::1]/x")),
            &HostList::parse(&["*".into()]).unwrap(),
            CLAMPS
        )
        .is_err());
    }

    #[test]
    fn only_web_and_ftp_schemes_and_no_credentials() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://u:p@cdn.example.com/x",
            "not a url",
        ] {
            assert!(
                accept(&mut plan_of(Track::file("f", url)), &allowed(), CLAMPS).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn forbidden_and_malformed_headers_are_refused() {
        for name in [
            "Cookie",
            "authorization",
            "Proxy-Authorization",
            "Proxy-Foo",
            "Host",
            "Range",
            "Content-Length",
            "Bad Name",
            "",
        ] {
            let t = Track::file("f", "https://cdn.example.com/x").header(name, "v");
            assert!(
                accept(&mut plan_of(t), &allowed(), CLAMPS).is_err(),
                "{name}"
            );
        }
        let t = Track::file("f", "https://cdn.example.com/x").header("X-A", "a\r\nB: c");
        assert!(accept(&mut plan_of(t), &allowed(), CLAMPS).is_err());
    }

    #[test]
    fn structural_limits() {
        let mut empty = plan_of(Track::file("f", "https://cdn.example.com/x"));
        empty.tracks.clear();
        assert!(rejected(empty).contains("no tracks"));

        let mut dup = plan_of(Track::file("f", "https://cdn.example.com/x"));
        dup.tracks
            .push(Track::file("f", "https://cdn.example.com/y"));
        assert!(rejected(dup).contains("repeated"));

        let mut nosrc = plan_of(Track::file("f", "https://cdn.example.com/x"));
        nosrc.tracks[0].sources.clear();
        assert!(rejected(nosrc).contains("no sources"));

        let mut many = plan_of(Track::file("f", "https://cdn.example.com/x"));
        many.tracks[0].sources = (0..=MAX_SOURCES_PER_TRACK)
            .map(|i| Source::new(format!("https://cdn.example.com/{i}")))
            .collect();
        assert!(rejected(many).contains("too many sources"));

        let mut tracks = plan_of(Track::file("t0", "https://cdn.example.com/x"));
        tracks.tracks = (0..=MAX_TRACKS)
            .map(|i| Track::file(format!("t{i}"), "https://cdn.example.com/x"))
            .collect();
        assert!(rejected(tracks).contains("too many tracks"));
    }

    #[test]
    fn digests_must_be_algo_hex_of_the_right_length() {
        let mut ok = plan_of(Track::file("f", "https://cdn.example.com/x"));
        ok.tracks[0].digest = Some(format!("sha-256:{}", "ab".repeat(32)));
        accept(&mut ok, &allowed(), CLAMPS).unwrap();
        for bad in [
            "sha-256:abc",
            "crc32:00000000",
            "nocolon",
            "md5:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ] {
            let mut p = plan_of(Track::file("f", "https://cdn.example.com/x"));
            p.tracks[0].digest = Some(bad.into());
            assert!(accept(&mut p, &allowed(), CLAMPS).is_err(), "{bad}");
        }
    }

    #[test]
    fn hints_are_clamped_not_rejected() {
        let mut p = plan_of(Track::file("f", "https://cdn.example.com/x"));
        p.tracks[0].sources[0].max_connections = Some(500);
        p.tracks[0].chunk_hint = Some(1);
        accept(&mut p, &allowed(), CLAMPS).unwrap();
        assert_eq!(p.tracks[0].sources[0].max_connections, Some(8));
        assert_eq!(p.tracks[0].chunk_hint, Some(MIN_CHUNK_HINT));
        p.tracks[0].chunk_hint = Some(u64::MAX);
        p.tracks[0].sources[0].max_connections = Some(0);
        accept(&mut p, &allowed(), CLAMPS).unwrap();
        assert_eq!(p.tracks[0].chunk_hint, Some(MAX_CHUNK_HINT));
        assert_eq!(p.tracks[0].sources[0].max_connections, Some(1));
    }

    #[test]
    fn refreshed_transport_goes_through_the_same_checks() {
        let ok = [Source::new("https://a.media.net/x")];
        check_track_transport(&ok, &BTreeMap::new(), &allowed()).unwrap();
        let bad = [Source::new("https://elsewhere.org/x")];
        assert!(check_track_transport(&bad, &BTreeMap::new(), &allowed()).is_err());
        let mut h = BTreeMap::new();
        h.insert("Cookie".to_string(), "a=b".to_string());
        assert!(check_track_transport(&ok, &h, &allowed()).is_err());
    }

    #[test]
    fn oversized_url_and_error_code() {
        let long = format!("https://cdn.example.com/{}", "a".repeat(MAX_URL));
        let err = accept(&mut plan_of(Track::file("f", long)), &allowed(), CLAMPS).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPlan);
    }
}
