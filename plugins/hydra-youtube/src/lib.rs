//! Official resolver: yt-dlp extracts metadata; Hydra transfers the media.
use base64::Engine;
use hya_plugin_sdk::{
    Assemble, ErrorCode, ExecOutput, Host, Plan, PlaylistEntry, Plugin, PluginError, Resolve,
    ResolveRequest, Track, TrackKind,
};
use serde_json::Value;

#[derive(Default)]
pub struct Youtube;
impl Plugin for Youtube {
    fn check(&mut self, host: &Host) -> hya_plugin_sdk::Result<()> {
        ensure_backend(host)
    }
    fn resolve(&mut self, host: &Host, req: ResolveRequest) -> hya_plugin_sdk::Result<Resolve> {
        let url = url::Url::parse(&req.url)
            .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e.to_string()))?;
        let hostname = url.host_str().unwrap_or("");
        if !matches!(url.scheme(), "http" | "https")
            || !(hostname == "youtu.be"
                || hostname == "youtube.com"
                || hostname.ends_with(".youtube.com"))
        {
            return Ok(Resolve::NotClaimed);
        }
        let playlist = url.path() == "/playlist";
        ensure_backend(host)?;
        let mut args: Vec<String> = [
            "--ignore-config",
            "--no-plugin-dirs",
            "--no-playlist",
            "-J",
            "--skip-download",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if playlist {
            args[2] = "--yes-playlist".into();
            args.extend([
                "--flat-playlist".into(),
                "--playlist-end".into(),
                "512".into(),
            ]);
        }
        let settings = host.settings()?;
        if let Some(runtime) = settings
            .get("javascript_runtime")
            .and_then(hya_plugin_sdk::Value::as_text)
            .filter(|value| *value != "default")
        {
            args.extend(["--js-runtimes".into(), runtime.into()]);
        }
        if settings
            .get("use_cookies")
            .and_then(hya_plugin_sdk::Value::as_bool)
            .unwrap_or(false)
        {
            args.extend(["--cookies".into(), "{cookie_file}".into()]);
        }
        args.extend(["--".into(), req.url]);
        let _ = host.log(
            "debug",
            if playlist {
                "Reading playlist entries"
            } else {
                "Reading available media formats"
            },
        );
        let output = extraction_output(host, host.exec("yt-dlp", &args), playlist)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(output.stdout_b64)
            .map_err(|e| PluginError::new(ErrorCode::InvalidReply, e.to_string()))?;
        let info = serde_json::from_slice(&bytes)
            .map_err(|e| PluginError::new(ErrorCode::InvalidReply, e.to_string()))?;
        let plan = plan(&info)?;
        let _ = host.log(
            "debug",
            &format!(
                "Resolved {} media tracks and {} playlist entries",
                plan.tracks.len(),
                plan.entries.len()
            ),
        );
        Ok(Resolve::Plan(plan))
    }
}

fn extraction_output(
    host: &Host,
    output: hya_plugin_sdk::Result<ExecOutput>,
    playlist: bool,
) -> hya_plugin_sdk::Result<ExecOutput> {
    let error = match output {
        Ok(output) if output.status == 0 => return Ok(output),
        Ok(output) => PluginError::new(ErrorCode::ToolFailed, output.stderr),
        Err(error) => error,
    };
    Err(extraction_error(host, error, playlist))
}

fn extraction_error(host: &Host, error: PluginError, playlist: bool) -> PluginError {
    if error.code != ErrorCode::ToolFailed
        || !error.message.contains("Sign in to confirm")
        || !error.message.contains("not a bot")
    {
        return error;
    }
    let _ = host.log("warn", &error.message);
    PluginError::new(
        ErrorCode::Network,
        if playlist {
            "YouTube blocked playlist inspection with bot verification. yt-dlp could not retrieve the playlist on this connection. See plugin logs for the original error."
        } else {
            "YouTube blocked this video's media formats with bot verification. A playlist can list a video even when YouTube refuses its download. See plugin logs for the original error."
        },
    )
}

const BACKEND_VERSION: &str = "2026.08.19";
fn backend_asset(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64" | "x86_64") => Some("yt-dlp_macos"),
        ("windows", "x86_64") => Some("yt-dlp.exe"),
        ("windows", "aarch64") => Some("yt-dlp_arm64.exe"),
        ("linux", "x86_64") => Some("yt-dlp_linux"),
        ("linux", "aarch64") => Some("yt-dlp_linux_aarch64"),
        _ => None,
    }
}
fn ensure_backend(host: &Host) -> hya_plugin_sdk::Result<()> {
    let args = [
        "--ignore-config".into(),
        "--no-plugin-dirs".into(),
        "--version".into(),
    ];
    match host.exec("yt-dlp", &args) {
        Ok(output) if output.status == 0 => return Ok(()),
        Ok(output) => return Err(PluginError::new(ErrorCode::ToolFailed, output.stderr)),
        Err(error) if error.code != ErrorCode::ToolMissing => return Err(error),
        Err(error) => {
            if !host
                .settings()?
                .get("download_backend")
                .and_then(hya_plugin_sdk::Value::as_bool)
                .unwrap_or(false)
            {
                return Err(error);
            }
        }
    }
    let platform: Value = host.call("platform", &serde_json::json!({}))?;
    let asset = backend_asset(
        platform["os"].as_str().unwrap_or(""),
        platform["arch"].as_str().unwrap_or(""),
    )
    .ok_or_else(|| {
        PluginError::new(
            ErrorCode::Unsupported,
            "install yt-dlp manually for this platform",
        )
    })?;
    let base = format!("https://github.com/yt-dlp/yt-dlp/releases/download/{BACKEND_VERSION}");
    let response: Value = host.call(
        "http",
        &serde_json::json!({"url":format!("{base}/SHA2-256SUMS")}),
    )?;
    if response["status"] != 200 {
        return Err(PluginError::new(
            ErrorCode::Network,
            "could not fetch backend checksums",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(response["body_b64"].as_str().unwrap_or(""))
        .map_err(|e| PluginError::new(ErrorCode::InvalidReply, e.to_string()))?;
    let sums = String::from_utf8(bytes)
        .map_err(|e| PluginError::new(ErrorCode::InvalidReply, e.to_string()))?;
    let digest = sums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let digest = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == asset).then_some(digest)
        })
        .ok_or_else(|| {
            PluginError::new(
                ErrorCode::InvalidReply,
                "release has no checksum for this platform",
            )
        })?;
    host.log(
        "info",
        &format!("Installing yt-dlp {BACKEND_VERSION} for {}", platform["os"]),
    )?;
    let _: Value = host.call(
        "program_install",
        &serde_json::json!({"program":"yt-dlp","url":format!("{base}/{asset}"),"sha256":digest}),
    )?;
    let result = host.exec("yt-dlp", &args)?;
    if result.status != 0 {
        return Err(PluginError::new(ErrorCode::ToolFailed, result.stderr));
    }
    Ok(())
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}
fn expiry(url: &str) -> Option<u64> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find_map(|(k, v)| (k == "expire").then(|| v.parse().ok()).flatten())
}
fn headers(info: &Value, format: &Value) -> std::collections::BTreeMap<String, String> {
    let mut result = std::collections::BTreeMap::new();
    for value in [info.get("http_headers"), format.get("http_headers")]
        .into_iter()
        .flatten()
    {
        if let Some(map) = value.as_object() {
            for (k, v) in map {
                if let Some(v) = v.as_str() {
                    result.insert(k.clone(), v.into());
                }
            }
        }
    }
    result
}
/// Converts yt-dlp metadata into direct, independently selectable media tracks.
pub fn plan(info: &Value) -> hya_plugin_sdk::Result<Plan> {
    if let Some(entries) = info.get("entries").and_then(Value::as_array) {
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<PlaylistEntry> = entries
            .iter()
            .filter_map(|entry| {
                let id = text(entry, "id")?;
                if id.is_empty()
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    || !seen.insert(id)
                {
                    return None;
                }
                Some(PlaylistEntry {
                    id: id.into(),
                    url: format!("https://www.youtube.com/watch?v={id}"),
                    title: text(entry, "title").map(str::to_owned),
                })
            })
            .take(hya_plugin_sdk::limits::MAX_TRACKS)
            .collect();
        if entries.is_empty() {
            return Err(PluginError::new(
                ErrorCode::Unsupported,
                "playlist has no available videos",
            ));
        }
        return Ok(Plan {
            id: text(info, "id").unwrap_or("playlist").into(),
            title: text(info, "title").map(str::to_owned),
            expires_at: None,
            assemble: Assemble::None,
            tracks: Vec::new(),
            transfer: None,
            entries,
        });
    }
    if info.get("is_live").and_then(Value::as_bool) == Some(true) {
        return Err(PluginError::new(
            ErrorCode::Unsupported,
            "live streams are not supported",
        ));
    }
    let id = text(info, "id")
        .ok_or_else(|| PluginError::new(ErrorCode::InvalidReply, "metadata has no id"))?;
    let mut tracks = Vec::new();
    for f in info
        .get("formats")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(fid), Some(url)) = (text(f, "format_id"), text(f, "url")) else {
            continue;
        };
        if f.get("has_drm").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if !matches!(text(f, "protocol"), Some("https" | "http") | None) {
            continue;
        }
        let video = text(f, "vcodec").is_some_and(|v| v != "none");
        let audio = text(f, "acodec").is_some_and(|v| v != "none");
        if !video && !audio {
            continue;
        }
        let mut t = Track::file(fid, url);
        t.kind = if video {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        t.default = false;
        t.container = text(f, "ext").map(str::to_owned);
        t.codec = text(f, if video { "vcodec" } else { "acodec" }).map(str::to_owned);
        t.height = f
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|n| n.try_into().ok());
        t.fps = f.get("fps").and_then(Value::as_f64).map(|n| n as f32);
        t.bitrate = f
            .get(if video { "vbr" } else { "abr" })
            .and_then(Value::as_f64)
            .map(|n| (n * 1000.0) as u64);
        t.size = f.get("filesize").and_then(Value::as_u64);
        t.language = text(f, "language").map(str::to_owned);
        t.headers = headers(info, f);
        t.expires_at = expiry(url);
        t.chunk_hint = Some(10 * 1024 * 1024);
        t.sources[0].max_connections = Some(4);
        tracks.push(t);
    }
    for (key, auto) in [("subtitles", false), ("automatic_captions", true)] {
        if let Some(languages) = info.get(key).and_then(Value::as_object) {
            for (language, formats) in languages {
                if auto
                    && tracks.iter().any(|t| {
                        t.kind == TrackKind::Subtitle && t.language.as_deref() == Some(language)
                    })
                {
                    continue;
                }
                let selected = formats.as_array().and_then(|formats| {
                    formats
                        .iter()
                        .filter(|f| text(f, "url").is_some())
                        .min_by_key(|f| match text(f, "ext") {
                            Some("vtt") => 0,
                            Some("srt") => 1,
                            _ => 2,
                        })
                });
                if let Some(f) = selected {
                    let url = text(f, "url").unwrap_or_default();
                    let mut t = Track::file(format!("{key}:{language}"), url);
                    t.kind = TrackKind::Subtitle;
                    t.language = Some(language.clone());
                    t.auto_generated = auto;
                    t.default = false;
                    t.container = text(f, "ext").map(str::to_owned);
                    t.headers = headers(info, f);
                    t.expires_at = expiry(url);
                    tracks.push(t);
                }
            }
        }
    }
    if tracks.iter().all(|t| t.kind == TrackKind::Subtitle) {
        return Err(PluginError::new(
            ErrorCode::Unsupported,
            "no direct HTTP media formats available",
        ));
    }
    let expires_at = tracks.iter().filter_map(|t| t.expires_at).min();
    Ok(Plan {
        id: id.into(),
        title: text(info, "title").map(str::to_owned),
        expires_at,
        assemble: Assemble::Mux,
        transfer: None,
        entries: Vec::new(),
        tracks,
    })
}
#[cfg(target_arch = "wasm32")]
hya_plugin_sdk::export!(Youtube);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_success_and_both_failure_reply_shapes() {
        let output = extraction_output(
            &Host,
            Ok(ExecOutput {
                status: 0,
                stdout_b64: "e30=".into(),
                stderr: "metadata warning".into(),
            }),
            false,
        )
        .unwrap();
        assert_eq!(output.stdout_b64, "e30=");
        assert_eq!(output.stderr, "metadata warning");
        let message = "ERROR: Sign in to confirm you're not a bot";
        let failed_process = extraction_output(
            &Host,
            Ok(ExecOutput {
                status: 1,
                stdout_b64: "".into(),
                stderr: message.into(),
            }),
            false,
        )
        .unwrap_err();
        let failed_host = extraction_output(
            &Host,
            Err(PluginError::new(ErrorCode::ToolFailed, message)),
            false,
        )
        .unwrap_err();
        assert_eq!(failed_process, failed_host);
        assert_eq!(failed_host.code, ErrorCode::Network);
    }

    #[test]
    fn recognizes_bot_rejection_after_metadata_warning() {
        let error = PluginError::new(
            ErrorCode::ToolFailed,
            "exited with 1: WARNING: [youtube] No title found in player responses; falling back to title from initial data.\nERROR: [youtube] 9KkbfDVQYO0: Sign in to confirm you’re not a bot.",
        );
        let video = extraction_error(&Host, error.clone(), false);
        assert_eq!(video.code, ErrorCode::Network);
        assert!(video.message.contains("video's media formats"));
        assert!(video.message.contains("playlist can list a video"));
        let playlist = extraction_error(&Host, error, true);
        assert_eq!(playlist.code, ErrorCode::Network);
        assert!(playlist.message.contains("playlist inspection"));
    }

    #[test]
    fn leaves_other_tool_and_permission_errors_intact() {
        for (code, message) in [
            (
                ErrorCode::ToolFailed,
                "WARNING: No title found in player responses",
            ),
            (ErrorCode::ToolFailed, "ERROR: Video unavailable"),
            (
                ErrorCode::PermissionDenied,
                "Sign in to confirm you're not a bot",
            ),
            (ErrorCode::ToolFailed, "Sign in to confirm your age"),
            (ErrorCode::ToolMissing, "yt-dlp is missing"),
        ] {
            let error = PluginError::new(code, message);
            assert_eq!(extraction_error(&Host, error.clone(), false), error);
        }
    }

    #[test]
    fn playlist_metadata_preserves_order_skips_unavailable_and_deduplicates() {
        let p = plan(
            &serde_json::json!({"id":"PLtest","title":"Playlist","entries":[
                {"id":"video-one","title":"One"}, null, {"title":"Deleted"},
                {"id":"video-two","title":"Two"}, {"id":"video-one"}, {"id":"bad&url=evil"}
            ]}),
        )
        .unwrap();
        assert_eq!(
            p.entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["video-one", "video-two"]
        );
        assert!(p.tracks.is_empty());
        assert_eq!(
            p.entries[0].url,
            "https://www.youtube.com/watch?v=video-one"
        );
        assert_eq!(p.entries[1].title.as_deref(), Some("Two"));
    }

    #[test]
    fn maps_tracks_and_filters_segmented_or_encrypted_media() {
        let p = plan(&serde_json::json!({"id":"v","title":"title","formats":[
            {"format_id":"137","url":"https://r.googlevideo.com/a?expire=123","vcodec":"avc1","acodec":"none","height":1080,"filesize":99,"protocol":"https"},
            {"format_id":"140","url":"https://r.googlevideo.com/b","vcodec":"none","acodec":"mp4a","abr":128},
            {"format_id":"bad","url":"https://r.googlevideo.com/c","vcodec":"avc1","protocol":"m3u8_native"},
            {"format_id":"drm","url":"https://r.googlevideo.com/d","vcodec":"avc1","has_drm":true}],
            "subtitles":{"en":[{"url":"https://www.youtube.com/caption","ext":"vtt"}]}})).unwrap();
        assert_eq!(p.tracks.len(), 3);
        assert_eq!(p.tracks[0].size, Some(99));
        assert_eq!(p.expires_at, Some(123));
        assert_eq!(p.tracks[1].bitrate, Some(128000));
        assert_eq!(p.tracks[2].kind, TrackKind::Subtitle);
    }
    #[test]
    fn refuses_empty_playlists_and_empty_metadata() {
        for v in [
            serde_json::json!({"entries":[]}),
            serde_json::json!({}),
            serde_json::json!({"id":"x","formats":[]}),
        ] {
            assert!(plan(&v).is_err());
        }
    }
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    #[test]
    fn native_backend_assets_cover_supported_desktops() {
        assert_eq!(backend_asset("macos", "aarch64"), Some("yt-dlp_macos"));
        assert_eq!(backend_asset("macos", "x86_64"), Some("yt-dlp_macos"));
        assert_eq!(backend_asset("windows", "x86_64"), Some("yt-dlp.exe"));
        assert_eq!(
            backend_asset("windows", "aarch64"),
            Some("yt-dlp_arm64.exe")
        );
        assert_eq!(backend_asset("linux", "x86_64"), Some("yt-dlp_linux"));
        assert_eq!(
            backend_asset("linux", "aarch64"),
            Some("yt-dlp_linux_aarch64")
        );
        assert_eq!(backend_asset("unknown", "unknown"), None);
    }
}
