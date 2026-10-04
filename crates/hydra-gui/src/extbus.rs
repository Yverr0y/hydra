// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Browser-extension bridge: a loopback TCP listener the native-messaging
//! host (`hydra-host`) connects to on behalf of the Chrome/Firefox add-ons.
//!
//! Why TCP and not a unix socket / named pipe: one code path on every OS,
//! plain `std::net` (no extra runtime plumbing), and the browser can never
//! reach it directly anyway — only `hydra-host` does, authenticated by a
//! random token. Port and token are published to `<app_dir>/ipc.json`,
//! readable only by the owning user, which is the same trust boundary the
//! config itself lives behind.
//!
//! Wire format: one JSON object per line, one reply line per request.
//! Every reply carries the current capture settings so the extension's
//! cached filter list converges without a dedicated poll.
//!
//! There are two front doors to the same request handler:
//!  - the ephemeral line-protocol port, used by `hydra-host`, authenticated
//!    by the ipc.json token (browsers cannot read files, hosts can);
//!  - a WebSocket listener on a KNOWN port (6799, fallback 16799), spoken
//!    directly by the browser extension. The `Origin` header (browsers
//!    always send `chrome-extension://...` and never let a page forge it)
//!    admits extension contexts to the handshake; the socket then stays
//!    unauthenticated until its first frame, `{"type":"auth","token":...}`,
//!    carries the same ipc.json token — which the extension obtains from
//!    the native host with `{"type":"ws-token"}`, so any extension the host
//!    manifest allow-lists can prove itself. The pinned Chromium ids in
//!    `nmhost::CHROMIUM_EXT_IDS` skip the token, and so do the origins the
//!    user allowed when an extension with no host to ask kept knocking
//!    (see [`TrustLedger`]). A live, authenticated WS connection doubles as
//!    the extension's "hydra is running" indicator.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// A download handed over by the browser. Everything except `url` is
/// best-effort: the dialog shows what arrived and probes the rest.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExtDownload {
    pub url: String,
    #[serde(default)]
    pub filename: Option<String>,
    /// Verbatim `Cookie:` header assembled by the extension for this URL.
    #[serde(default)]
    pub cookies: Option<String>,
    /// The page the file was linked from, replayed as `Referer:` on every
    /// request: a CDN with hotlink protection refuses the object without it.
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mime: Option<String>,
    /// Page the download started from (shown nowhere yet; logged).
    #[serde(default)]
    pub tab_url: Option<String>,
    /// The proxy the BROWSER is using for this URL, as a full specification
    /// (`socks5://host:port`), when the extension was allowed to read it.
    /// Applied to this download alone — see [`ExtStream::proxy`].
    #[serde(default)]
    pub proxy: Option<String>,
}

/// One rendition of a stream, as the extension read it out of the manifest.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExtVariant {
    /// The variant playlist itself, when the manifest named one. Used as the
    /// fallback target when `ExtStream::variant_url` is absent.
    #[serde(default)]
    pub url: Option<String>,
    /// Height is what a person picks by, and what the engine matches on.
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub bandwidth: Option<u64>,
    #[serde(default)]
    pub codecs: Option<String>,
}

/// An adaptive stream handed over by the browser. Distinct from
/// [`ExtDownload`] on purpose: the URL names a MANIFEST, not a file, so the
/// ordinary capture path would save a few kilobytes of playlist text.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExtStream {
    /// The manifest, as the browser requested it.
    pub url: String,
    /// "hls" or "dash".
    #[serde(default)]
    pub protocol: Option<String>,
    /// The exact variant playlist the user chose in the popup or the
    /// in-page panel.
    #[serde(default)]
    pub variant_url: Option<String>,
    #[serde(default)]
    pub variant: Option<ExtVariant>,
    /// "MP4" or "TS": the container the user picked.
    #[serde(default)]
    pub container: Option<String>,
    /// Suggested name, taken from the page title.
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub tab_url: Option<String>,
    #[serde(default)]
    pub live: bool,
    /// The browser's own proxy, as a full specification. It is the route for
    /// this recording only: Options is the user's own answer to the same
    /// question, and a browser setting that changes tomorrow must not have
    /// overwritten it today.
    #[serde(default)]
    pub proxy: Option<String>,
    /// Seconds, when the manifest stated one.
    #[serde(default)]
    pub duration: Option<f64>,
    /// Bitrate x duration, as the extension estimated it.
    #[serde(default)]
    pub size: Option<u64>,
}

/// Receipt for a capture: the socket thread answers the browser only once
/// the UI thread has taken the download.
///
/// The extension CANCELS AND ERASES the browser's own copy when it sees
/// `ok`, so that word has to mean "hydra owns this now". It used to mean
/// "a message went onto a channel", and the difference is a lost file: an
/// app that dies between the two (on Windows the browser kills the whole
/// native-messaging job the moment the host answers) takes the download
/// with it, having already told the browser to let go.
#[derive(Clone, Debug)]
pub struct Ack(std::sync::mpsc::SyncSender<()>);

impl Ack {
    /// A receipt and the end the socket thread waits on. Rendezvous, not
    /// buffered: `send` only succeeds while someone is still waiting, so
    /// [`Ack::confirm`] can say whether the browser was told `ok`.
    pub(crate) fn pair() -> (Ack, std::sync::mpsc::Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(0);
        (Ack(tx), rx)
    }

    /// Called from the UI thread once the item is in the download list.
    ///
    /// `false` when the socket thread has stopped waiting — its timeout ran
    /// out and the browser has already been told to keep the download. The
    /// caller then owns a copy the browser is also fetching, and must drop it.
    #[must_use]
    pub fn confirm(&self) -> bool {
        self.0.send(()).is_ok()
    }
}

/// How long the socket thread waits for that receipt. Generous, because a
/// cold start hands the capture over while `boot` is still running and the
/// subscription that drains this channel only starts after it; a browser
/// download stays paused meanwhile, and resumes untouched on timeout.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Debug)]
pub enum ExtEvent {
    /// Local file association asks for package review, never silent installation.
    InstallPlugin(std::path::PathBuf),
    /// Single captured download -> Download File Info dialog.
    Download(ExtDownload, Ack),
    /// A manifest -> the stream-aware download path.
    Stream(Box<ExtStream>),
    /// "Download all links": many URLs -> the batch window.
    Links(Vec<String>),
    /// Popup's "Open Hydra": surface the main window.
    Open,
    /// An extension origin with no token to show asks to be allowed.
    TrustRequest(String),
    /// A newer build launched and asked this instance to step aside so the
    /// surviving process is the new version (see `signal_existing`).
    Shutdown,
}

/// Capture-policy snapshot the UI thread publishes for the socket threads.
/// Mirrors the Options fields the extension needs for filtering.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ExtConfig {
    /// Capture flag for the browser that asked. Resolved per request from
    /// `browsers` — an extension only ever sees its own row.
    pub capture: bool,
    /// Space-separated extension list (Options > File Types).
    pub auto_types: String,
    /// Sites whose downloads are never captured.
    pub dont_start_sites: String,
    /// Every row of Options > General > "Capture downloads from the following
    /// browsers", in order. Not serialized: the extension is told about
    /// itself, not about the user's other browsers.
    #[serde(skip)]
    pub browsers: Vec<(String, bool)>,
    /// Extension origins admitted to the WebSocket without the token.
    #[serde(skip)]
    pub allowed: Vec<String>,
}

static TX: OnceLock<UnboundedSender<ExtEvent>> = OnceLock::new();
static RX: Mutex<Option<UnboundedReceiver<ExtEvent>>> = Mutex::new(None);
static CFG: Mutex<Option<ExtConfig>> = Mutex::new(None);

fn sender() -> UnboundedSender<ExtEvent> {
    TX.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        if let Ok(mut g) = RX.lock() {
            *g = Some(rx);
        }
        tx
    })
    .clone()
}

pub(crate) fn install_plugin_file(path: std::path::PathBuf) {
    let _ = sender().send(ExtEvent::InstallPlugin(path));
}

/// Take the receiving end (once), for the iced subscription.
pub fn take_events() -> Option<UnboundedReceiver<ExtEvent>> {
    let _ = sender();
    RX.lock().ok().and_then(|mut g| g.take())
}

/// UI thread pushes the fields the extension mirrors; called at boot and on
/// every config save, so the socket side never touches `App`.
pub fn publish_config(cfg: &crate::model::ConfigFile) {
    let browsers = cfg.settings.capture_browsers.clone();
    // Default for a request that does not say which browser it is (an
    // extension older than the `browser` field): the Chromium-family row,
    // which is what the previous single-flag behaviour meant.
    let capture = browsers
        .iter()
        .find(|(name, _)| name.contains("Chrome"))
        .map(|(_, on)| *on)
        .unwrap_or(true);
    let snap = ExtConfig {
        capture,
        auto_types: cfg.settings.auto_types.clone(),
        dont_start_sites: cfg.settings.dont_start_sites.clone(),
        browsers,
        allowed: cfg.settings.allowed_extensions.clone(),
    };
    if let Ok(mut g) = CFG.lock() {
        *g = Some(snap);
    }
}

/// Browsers that have spoken to us recently, by the label Options shows.
/// The extension heartbeats every 20 s, so anything inside a minute is a
/// live connection; the window is generous so one dropped beat does not
/// make the row flicker.
static SEEN: Mutex<Option<Vec<(String, std::time::Instant)>>> = Mutex::new(None);
const SEEN_TTL: std::time::Duration = std::time::Duration::from_secs(60);

fn note_browser(name: &str) {
    let Ok(mut g) = SEEN.lock() else { return };
    let list = g.get_or_insert_with(Vec::new);
    let now = std::time::Instant::now();
    match list.iter_mut().find(|(n, _)| n == name) {
        Some(e) => e.1 = now,
        None => list.push((name.to_string(), now)),
    }
}

/// Labels of the browsers whose extension is connected right now, for the
/// status column in Options > General.
pub fn live_browsers() -> Vec<String> {
    let Ok(g) = SEEN.lock() else { return vec![] };
    let now = std::time::Instant::now();
    g.as_ref()
        .map(|l| {
            l.iter()
                .filter(|(_, t)| now.duration_since(*t) < SEEN_TTL)
                .map(|(n, _)| n.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the row for the browser that sent this request. Matching is by
/// the exact label the extension reports, then by keyword, so a rename in
/// the settings list does not silently disable capture.
fn capture_for(cfg: &ExtConfig, browser: Option<&str>) -> bool {
    let Some(want) = browser else {
        return cfg.capture;
    };
    let want_l = want.to_ascii_lowercase();
    if let Some((_, on)) = cfg
        .browsers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(want))
    {
        return *on;
    }
    for key in ["edge", "opera", "firefox", "safari", "chrome"] {
        if want_l.contains(key) {
            if let Some((_, on)) = cfg
                .browsers
                .iter()
                .find(|(n, _)| n.to_ascii_lowercase().contains(key))
            {
                return *on;
            }
        }
    }
    cfg.capture
}

fn config_snapshot() -> ExtConfig {
    CFG.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

/// 128 bits from two independently OS-seeded SipHash keys. Not a general
/// CSPRNG, but ample for a loopback token whose alternative reader would
/// already own the user account (and could read ipc.json directly).
fn make_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for salt in 0u64..2 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(std::process::id() as u64 ^ salt);
        h.write_u128(crate::fmt::since_epoch().as_nanos());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Probe for an already-running instance via its ipc.json. When one
/// answers, ask it to surface its main window (unless we were an autostart
/// `--minimized` launch — then a bare ping) and report `true` so the caller
/// can exit: one instance owns the state db, the tray, and this socket.
/// A stale ipc.json (dead port, wrong token) reads as "no instance".
///
/// Exception: when the answering instance reports a DIFFERENT version, an
/// old build is still resident after an upgrade — handing over would keep
/// the old code (and its About-window version) on screen forever. Instead
/// it is asked to quit, and once its socket goes dark this returns `false`
/// so the caller boots as the new instance. Builds that predate "shutdown"
/// refuse it as unknown; they get the classic hand-over.
pub fn signal_existing(minimized: bool, plugin_file: Option<&std::path::Path>) -> bool {
    let Ok(text) = std::fs::read_to_string(crate::model::app_dir().join("ipc.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let (Some(port), Some(token)) = (
        v.get("port").and_then(|p| p.as_u64()),
        v.get("token").and_then(|t| t.as_str()),
    ) else {
        return false;
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port as u16));
    let Ok(s) = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(400)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let Ok(mut out) = s.try_clone() else {
        return false;
    };
    let mut reader = BufReader::new(s);
    let mut request = move |kind: &str| -> Option<serde_json::Value> {
        let mut body = serde_json::json!({"type":kind,"token":token});
        if kind == "install-plugin" {
            body["path"] = serde_json::json!(plugin_file?.to_str()?);
        }
        writeln!(out, "{body}").ok()?;
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    };
    let acked = |r: &serde_json::Value| r.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);

    let Some(pong) = request("ping") else {
        return false;
    };
    if !acked(&pong) {
        return false;
    }

    let theirs = pong.get("version").and_then(|t| t.as_str()).unwrap_or("");
    if theirs != env!("CARGO_PKG_VERSION") && request("shutdown").is_some_and(|r| acked(&r)) {
        // It saves state and exits; the socket dying is the all-clear.
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_err() {
                return false;
            }
        }
        // Still alive after 5 s (wedged exit path?): treat it as the owner.
    }

    if plugin_file.is_some() {
        return request("install-plugin").is_some_and(|reply| acked(&reply));
    }
    if !minimized {
        return request("open").is_some_and(|r| acked(&r));
    }
    true
}

/// Bind the listener and publish `<app_dir>/ipc.json`. Failure to bind is
/// logged and otherwise ignored: the GUI works fine without the bridge.
pub fn start() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => {
            crate::log::warn(&format!("extbus: bind failed: {e}"));
            return;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            crate::log::warn(&format!("extbus: local_addr failed: {e}"));
            return;
        }
    };
    let token = make_token();

    // The extension-facing WebSocket port must be knowable without reading
    // a file, so it is fixed with one fallback.
    let ws = WS_PORTS
        .iter()
        .find_map(|p| TcpListener::bind(("127.0.0.1", *p)).ok());
    let ws_port = ws
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port());
    match ws_port {
        Some(p) => crate::log::info(&format!("extbus: websocket on 127.0.0.1:{p}")),
        None => crate::log::warn(
            "extbus: websocket ports busy; extension will use the native host path",
        ),
    }

    let dir = crate::model::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ipc.json");
    let body = format!(
        "{{\"port\":{port},\"token\":\"{token}\",\"ws_port\":{},\"pid\":{}}}\n",
        ws_port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "null".into()),
        std::process::id()
    );
    if let Err(e) = std::fs::write(&path, &body) {
        crate::log::warn(&format!("extbus: cannot write {}: {e}", path.display()));
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    crate::log::info(&format!("extbus: listening on 127.0.0.1:{port}"));

    let ws_token = token.clone();
    std::thread::Builder::new()
        .name("extbus-accept".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let tok = token.clone();
                let _ = std::thread::Builder::new()
                    .name("extbus-conn".into())
                    .spawn(move || serve(stream, &tok));
            }
        })
        .ok();

    if let Some(ws) = ws {
        let gate = std::sync::Arc::new(WsGate::new(
            ws_token,
            || config_snapshot().allowed,
            sender(),
        ));
        std::thread::Builder::new()
            .name("extbus-ws-accept".into())
            .spawn(move || {
                for conn in ws.incoming() {
                    let Ok(stream) = conn else { continue };
                    let gate = gate.clone();
                    let _ = std::thread::Builder::new()
                        .name("extbus-ws".into())
                        .spawn(move || serve_ws(stream, &gate));
                }
            })
            .ok();
    }
}

/// Extension-facing WebSocket ports, fixed so the browser needs no file
/// access to find us. One fallback for the unlucky case of the first being
/// taken by something else.
const WS_PORTS: &[u16] = &[6799, 16799];

/// A proxy specification with any `user:pass@` removed, for the log.
///
/// Nothing sends one today — neither browser's proxy API exposes credentials
/// — but this is the one place a specification from outside the app is
/// written to a file, and a log that can hold a password is not a thing to
/// leave to the good manners of every future caller.
fn without_credentials(spec: &str) -> String {
    match spec.rsplit_once('@') {
        Some((head, tail)) => match head.split_once("://") {
            Some((scheme, _)) => format!("{scheme}://{tail}"),
            None => tail.to_string(),
        },
        None => spec.to_string(),
    }
}

/// Handle one authenticated request; the reply always carries the capture
/// settings and echoes any `id` (the WebSocket path multiplexes on it).
/// `allowed` is true for the token-bearing line protocol only — the
/// WebSocket path is origin-authenticated, which any extension context
/// satisfies, so takeover commands are refused there.
fn dispatch(req: &serde_json::Value, allowed: bool) -> serde_json::Value {
    let (ok, err): (bool, Option<&str>) = match req.get("type").and_then(|t| t.as_str()) {
        Some("ping") | Some("config") => (true, None),
        Some("open") => {
            let _ = sender().send(ExtEvent::Open);
            (true, None)
        }
        // Version-mismatch takeover from signal_existing: quit gracefully
        // so the newly launched build can become the instance.
        Some("shutdown") if allowed => {
            let _ = sender().send(ExtEvent::Shutdown);
            (true, None)
        }
        Some("install-plugin") if allowed => match req
            .get("path")
            .and_then(|path| path.as_str())
            .map(std::path::PathBuf::from)
        {
            Some(path)
                if path.is_absolute()
                    && path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("hyaplugin")) =>
            {
                install_plugin_file(path);
                (true, None)
            }
            _ => (false, Some("invalid plugin package path")),
        },
        Some("download") => match serde_json::from_value::<ExtDownload>(req.clone()) {
            Ok(dl) if !dl.url.is_empty() => {
                // The referer travels with the item and onto every request
                // (`DownloadItem::referer`); user_agent and size are logged
                // for support, the transfer using the configured agent.
                crate::log::info(&format!(
                    "extbus: capture {} (mime={} size={} referer={} ua={} proxy={} from={})",
                    dl.url,
                    dl.mime.as_deref().unwrap_or("-"),
                    dl.size.map(|s| s.to_string()).as_deref().unwrap_or("-"),
                    dl.referer.as_deref().unwrap_or("-"),
                    dl.user_agent.as_deref().unwrap_or("-"),
                    dl.proxy
                        .as_deref()
                        .map(without_credentials)
                        .unwrap_or("-".into()),
                    dl.tab_url.as_deref().unwrap_or("-"),
                ));
                let (ack, receipt) = Ack::pair();
                let _ = sender().send(ExtEvent::Download(dl, ack));
                // Dropped sender (the event never reached the handler) ends
                // the wait at once; a timeout means a wedged UI thread.
                match receipt.recv_timeout(ACK_TIMEOUT) {
                    Ok(()) => (true, None),
                    Err(_) => {
                        crate::log::warn(
                            "extbus: capture was not taken up; handing it back to the browser",
                        );
                        (false, Some("hydra did not take the download"))
                    }
                }
            }
            _ => (false, Some("bad download request")),
        },
        Some("stream") => match serde_json::from_value::<ExtStream>(req.clone()) {
            Ok(s) if !s.url.is_empty() => {
                crate::log::info(&format!(
                    "extbus: stream {} ({} {}, container={}, live={}, from={})",
                    s.url,
                    s.protocol.as_deref().unwrap_or("?"),
                    s.variant
                        .as_ref()
                        .and_then(|v| v.height)
                        .map(|h| format!("{h}p"))
                        .unwrap_or_else(|| "auto".into()),
                    s.container.as_deref().unwrap_or("MP4"),
                    s.live,
                    s.tab_url.as_deref().unwrap_or("-"),
                ));
                crate::log::debug(&format!(
                    "extbus: stream proxy={} codecs={} duration={} ua={}",
                    s.proxy
                        .as_deref()
                        .map(without_credentials)
                        .unwrap_or("-".into()),
                    s.variant
                        .as_ref()
                        .and_then(|v| v.codecs.as_deref())
                        .unwrap_or("-"),
                    s.duration
                        .map(|d| format!("{d:.0}s"))
                        .as_deref()
                        .unwrap_or("-"),
                    s.user_agent.as_deref().unwrap_or("-"),
                ));
                let _ = sender().send(ExtEvent::Stream(Box::new(s)));
                (true, None)
            }
            _ => (false, Some("bad stream request")),
        },
        Some("links") => {
            let urls: Vec<String> = req
                .get("urls")
                .and_then(|u| u.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if urls.is_empty() {
                (false, Some("no urls"))
            } else {
                crate::log::info(&format!("extbus: {} links", urls.len()));
                let _ = sender().send(ExtEvent::Links(urls));
                (true, None)
            }
        }
        _ => (false, Some("unknown type")),
    };

    let cfg = config_snapshot();
    // Every reply carries the settings the extension mirrors — but `capture`
    // is that BROWSER's checkbox, not one flag shared by all of them.
    let browser = req.get("browser").and_then(|b| b.as_str());
    if let Some(b) = browser {
        note_browser(b);
    }
    let mut v = serde_json::json!({
        "ok": ok,
        "capture": capture_for(&cfg, browser),
        "auto_types": cfg.auto_types,
        "dont_start_sites": cfg.dont_start_sites,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(e) = err {
        v["error"] = serde_json::Value::String(e.into());
    }
    if let Some(id) = req.get("id") {
        v["id"] = id.clone();
    }
    v
}

// ------------------------------------------------------- line protocol

fn serve(stream: TcpStream, token: &str) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // Hard cap: nothing legitimate sends megabytes per request.
        if line.len() > 4 * 1024 * 1024 {
            break;
        }
        let reply = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(req) if req.get("token").and_then(|t| t.as_str()) == Some(token) => {
                dispatch(&req, true)
            }
            Ok(_) => {
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::json!({"ok": false, "error": "bad token"})
                );
                break;
            }
            Err(_) => serde_json::json!({"ok": false, "error": "bad json"}),
        };
        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

// ----------------------------------------------------------- websocket

/// How long a socket may stay unauthenticated. The extension sends `auth`
/// as its first frame, so anything slower is not the extension.
const WS_AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const WS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// What every WebSocket connection checks its peer against.
struct WsGate {
    token: String,
    /// The origins the user allowed, read per connection: the list changes
    /// under a running listener when the user answers or revokes.
    allowed: fn() -> Vec<String>,
    ledger: Mutex<TrustLedger>,
    events: UnboundedSender<ExtEvent>,
}

impl WsGate {
    fn new(token: String, allowed: fn() -> Vec<String>, events: UnboundedSender<ExtEvent>) -> Self {
        WsGate {
            token,
            allowed,
            ledger: Mutex::new(TrustLedger::default()),
            events,
        }
    }
}

/// When to ask the user about an extension that cannot show the token.
///
/// With a native host, one refusal is routine: the extension's cached token
/// is stale or missing, it asks the host, and the redial authenticates.
/// Without one (a registry-free portable copy, a Firefox install whose
/// origin cannot be pinned) the same origin is refused again 30 s later.
/// That second refusal, with no token in between, is what raises the
/// question, and each origin is asked at most once per run.
#[derive(Debug, Default)]
struct TrustLedger {
    refused_once: Vec<String>,
    asked: Vec<String>,
}

impl TrustLedger {
    /// Records a refusal; `true` when it is the one to ask the user about.
    fn refused(&mut self, origin: &str) -> bool {
        if self.asked.iter().any(|o| o == origin) {
            return false;
        }
        match self.refused_once.iter().position(|o| o == origin) {
            Some(i) => {
                self.refused_once.swap_remove(i);
                self.asked.push(origin.to_string());
                true
            }
            None => {
                self.refused_once.push(origin.to_string());
                false
            }
        }
    }

    fn authenticated(&mut self, origin: &str) {
        self.refused_once.retain(|o| o != origin);
    }
}

/// Minimal RFC 6455 server side: enough for one browser extension speaking
/// small text frames. No fragmentation, no extensions, no TLS (loopback).
fn serve_ws(stream: TcpStream, gate: &WsGate) {
    let _ = stream.set_read_timeout(Some(WS_IDLE_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);

    // ---- handshake -----------------------------------------------------
    let mut head = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
        if head.len() > 16 * 1024 {
            return;
        }
    }
    let header = |name: &str| -> Option<String> {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };

    // A browser always stamps extension contexts with their real origin and
    // pages cannot forge it, so only extensions get past the handshake; the
    // token frame that follows says which one. (Native processes could
    // connect, but a same-user process already owns ~/.config/hydra.)
    let origin = header("Origin").unwrap_or_default();
    let origin_ok = origin_is_extension(&origin);
    let key = header("Sec-WebSocket-Key");
    let upgrade_ok = header("Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let (Some(key), true, true) = (key, upgrade_ok, origin_ok) else {
        let _ = out.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
        crate::log::warn(&format!(
            "extbus: rejected ws handshake (origin: {origin:?})"
        ));
        return;
    };

    let accept = {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        h.update(key.as_bytes());
        h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        hya_net::base64::encode(&h.finalize())
    };
    if out
        .write_all(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .as_bytes(),
        )
        .is_err()
    {
        return;
    }
    let mut authed = origin_preauthorized(&origin, &(gate.allowed)());
    crate::log::info(&format!(
        "extbus: ws connected ({origin}; {})",
        if authed {
            "allow-listed"
        } else {
            "awaiting token"
        }
    ));
    if !authed {
        let _ = out.set_read_timeout(Some(WS_AUTH_TIMEOUT));
    }

    // ---- frames --------------------------------------------------------
    while let Some((opcode, payload)) = ws_read_frame(&mut reader) {
        if !authed {
            match ws_authenticate(opcode, &payload, &gate.token) {
                Ok(reply) => {
                    authed = true;
                    if let Ok(mut ledger) = gate.ledger.lock() {
                        ledger.authenticated(&origin);
                    }
                    let _ = out.set_read_timeout(Some(WS_IDLE_TIMEOUT));
                    crate::log::info(&format!("extbus: ws authenticated ({origin})"));
                    if ws_write_frame(&mut out, 0x1, reply.to_string().as_bytes()).is_err() {
                        break;
                    }
                }
                Err(reply) => {
                    crate::log::warn(&format!("extbus: ws refused ({origin}): unauthorized"));
                    let _ = ws_write_frame(&mut out, 0x1, reply.to_string().as_bytes());
                    let _ = ws_write_frame(&mut out, 0x8, &[]);
                    let ask = gate
                        .ledger
                        .lock()
                        .is_ok_and(|mut ledger| ledger.refused(&origin));
                    if ask {
                        crate::log::info(&format!("extbus: asking whether to trust {origin}"));
                        let _ = gate.events.send(ExtEvent::TrustRequest(origin));
                    }
                    break;
                }
            }
            continue;
        }
        match opcode {
            0x9 => {
                // Ping: the extension's keep-alive heartbeat.
                if ws_write_frame(&mut out, 0xA, &payload).is_err() {
                    break;
                }
            }
            0x8 => {
                let _ = ws_write_frame(&mut out, 0x8, &[]);
                break;
            }
            0x1 => {
                let reply = match std::str::from_utf8(&payload)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                {
                    // The extension opens every socket with `auth`; on one
                    // the origin already vouched for, that is simply agreed.
                    Some(req) if req.get("type").and_then(|t| t.as_str()) == Some("auth") => {
                        ws_auth_reply(true, req.get("id"))
                    }
                    Some(req) => dispatch(&req, false),
                    None => serde_json::json!({"ok": false, "error": "bad json"}),
                };
                if ws_write_frame(&mut out, 0x1, reply.to_string().as_bytes()).is_err() {
                    break;
                }
            }
            // Binary/continuation: nothing legitimate sends these here.
            _ => break,
        }
    }
}

/// One frame; None on error/EOF. Client frames must be masked (RFC 6455).
fn ws_read_frame(reader: &mut BufReader<TcpStream>) -> Option<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 2];
    reader.read_exact(&mut hdr).ok()?;
    let opcode = hdr[0] & 0x0F;
    let masked = hdr[1] & 0x80 != 0;
    let mut len = (hdr[1] & 0x7F) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        reader.read_exact(&mut b).ok()?;
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        reader.read_exact(&mut b).ok()?;
        len = u64::from_be_bytes(b);
    }
    if !masked || len > 4 * 1024 * 1024 {
        return None;
    }
    let mut mask = [0u8; 4];
    reader.read_exact(&mut mask).ok()?;
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).ok()?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Some((opcode, payload))
}

fn ws_write_frame(out: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => frame.push(n as u8),
        n if n <= u16::MAX as usize => {
            frame.push(126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    out.write_all(&frame)?;
    out.flush()
}

/// Whether a WebSocket `Origin` is a browser-extension context at all. Any
/// such peer may open the socket; proving it is one of Hydra's own is the
/// token frame's job, since Firefox and Safari mint a fresh id per install
/// and a side-loaded Chromium build gets one of its own too.
fn origin_is_extension(origin: &str) -> bool {
    [
        "chrome-extension://",
        "moz-extension://",
        "safari-web-extension://",
    ]
    .iter()
    .any(|p| origin.len() > p.len() && origin.starts_with(p))
}

/// The Chromium ids Hydra ships under (`nmhost::CHROMIUM_EXT_IDS`) are known
/// in advance and need no token; nor do the origins the user `allowed`.
fn origin_preauthorized(origin: &str, allowed: &[String]) -> bool {
    let origin = origin.trim_end_matches('/');
    let pinned = origin
        .strip_prefix("chrome-extension://")
        .is_some_and(|id| crate::nmhost::CHROMIUM_EXT_IDS.contains(&id));
    pinned || allowed.iter().any(|t| t.trim_end_matches('/') == origin)
}

/// The one frame an unauthenticated socket may send: `auth` with the
/// ipc.json token. `Ok` carries the reply that admits the peer; `Err` the
/// refusal after which the socket is closed.
fn ws_authenticate(
    opcode: u8,
    payload: &[u8],
    token: &str,
) -> Result<serde_json::Value, serde_json::Value> {
    let req = (opcode == 0x1)
        .then(|| std::str::from_utf8(payload).ok())
        .flatten()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let id = req.as_ref().and_then(|r| r.get("id"));
    let presented = req
        .as_ref()
        .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("auth"))
        .and_then(|r| r.get("token"))
        .and_then(|t| t.as_str());
    match presented {
        Some(t) if constant_time_eq(t.as_bytes(), token.as_bytes()) => Ok(ws_auth_reply(true, id)),
        _ => Err(ws_auth_reply(false, id)),
    }
}

fn ws_auth_reply(ok: bool, id: Option<&serde_json::Value>) -> serde_json::Value {
    let mut v = serde_json::json!({ "ok": ok });
    if !ok {
        v["error"] = "unauthorized".into();
    }
    if let Some(id) = id {
        v["id"] = id.clone();
    }
    v
}

/// Equal without an early exit, so a wrong token costs the same time
/// however many of its bytes happen to match.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole capture hand-over rests on what `ok` means: the extension
    /// cancels and ERASES the browser's own download when it sees it. So it
    /// may only be said once the UI thread has the item — and must not be
    /// said at all when the event is dropped on the floor, which is what a
    /// dying app looks like from here.
    #[test]
    fn a_capture_is_acknowledged_only_once_the_app_takes_it() {
        let mut rx = take_events().expect("the receiver is free in this test");
        let req = serde_json::json!({"type": "download", "url": "https://example.invalid/f.zip"});
        let ok = |reply: serde_json::Value| reply["ok"].as_bool().expect("ok flag");

        let replying = {
            let req = req.clone();
            std::thread::spawn(move || dispatch(&req, true))
        };
        let Some(ExtEvent::Download(dl, ack)) = rx.blocking_recv() else {
            panic!("a download event should have been queued");
        };
        assert_eq!(dl.url, "https://example.invalid/f.zip");
        assert!(ack.confirm(), "the socket thread was still waiting");
        assert!(ok(replying.join().expect("dispatch thread")));

        let replying = std::thread::spawn(move || dispatch(&req, true));
        // Never confirmed: the receipt channel dies with the event, and the
        // browser is told to keep the download rather than wait out the
        // whole timeout.
        drop(rx.blocking_recv());
        assert!(!ok(replying.join().expect("dispatch thread")));
        let path = std::path::absolute("local.HYAPLUGIN").unwrap();
        let package = serde_json::json!({"type":"install-plugin", "path":path});
        assert!(!ok(dispatch(&package, false)));
        assert!(ok(dispatch(&package, true)));
        let Some(ExtEvent::InstallPlugin(received)) = rx.blocking_recv() else {
            panic!("package review event expected")
        };
        assert_eq!(received, path);
        for invalid in ["relative.hyaplugin", "/tmp/not-a-package.txt"] {
            assert!(!ok(dispatch(
                &serde_json::json!({"type":"install-plugin", "path":invalid}),
                true
            )));
        }
    }

    /// The other half of the same contract: once the socket thread has
    /// stopped waiting and told the browser `ok: false`, a late confirm must
    /// say so, or the app keeps a download the browser is also fetching.
    #[test]
    fn a_confirm_after_the_receipt_expired_reports_it() {
        let (ack, receipt) = Ack::pair();
        drop(receipt);
        assert!(!ack.confirm());

        let (ack, receipt) = Ack::pair();
        let waiting = std::thread::spawn(move || receipt.recv().is_ok());
        assert!(ack.confirm());
        assert!(waiting.join().expect("receiver thread"));
    }

    /// Any extension context may open the socket — a side-loaded Chromium
    /// build, every Firefox and Safari install — but only the ids Hydra
    /// ships under are allowed on sight; the rest prove themselves with the
    /// token. A web page never gets past the handshake.
    #[test]
    fn any_extension_may_connect_but_only_pinned_ids_skip_the_token() {
        for id in crate::nmhost::CHROMIUM_EXT_IDS {
            assert!(origin_is_extension(&format!("chrome-extension://{id}")));
            assert!(origin_preauthorized(
                &format!("chrome-extension://{id}"),
                &[]
            ));
            assert!(origin_preauthorized(
                &format!("chrome-extension://{id}/"),
                &[]
            ));
        }
        let sideloaded = "chrome-extension://kbopajngnjmmidookpofpjllbjfdlbhp";
        assert!(origin_is_extension(sideloaded));
        assert!(!origin_preauthorized(sideloaded, &[]));
        for origin in [
            "moz-extension://8b2c1f0e-1d2e-4c5a-9f00-000000000000",
            "safari-web-extension://ABCDEF",
        ] {
            assert!(origin_is_extension(origin));
            assert!(!origin_preauthorized(origin, &[]));
        }
        for origin in ["chrome-extension://", "https://example.com", ""] {
            assert!(!origin_is_extension(origin), "{origin:?}");
            assert!(!origin_preauthorized(origin, &[]), "{origin:?}");
        }
    }

    /// A Firefox origin is a per-install UUID nobody can pin, so the user's
    /// own answer is what admits it — that origin exactly, and no other.
    #[test]
    fn an_allowed_origin_skips_the_token_and_only_that_one() {
        let allowed = vec!["moz-extension://e88b5464-98e6-41c8-a457-f4887b0486e1".to_string()];
        assert!(origin_preauthorized(&allowed[0], &allowed));
        assert!(origin_preauthorized(&format!("{}/", allowed[0]), &allowed));
        for other in [
            "moz-extension://00000000-98e6-41c8-a457-f4887b0486e1",
            "moz-extension://e88b5464-98e6-41c8-a457-f4887b0486e",
            "chrome-extension://e88b5464-98e6-41c8-a457-f4887b0486e1",
        ] {
            assert!(!origin_preauthorized(other, &allowed), "{other}");
        }
    }

    /// With a native host the extension is refused once (stale token), asks
    /// the host and comes back with the token: nobody is bothered. Without
    /// one, the second refusal asks — once per run, whatever the answer.
    #[test]
    fn only_a_second_tokenless_refusal_asks_and_only_once() {
        let mut ledger = TrustLedger::default();
        let (hosted, hostless) = ("moz-extension://hosted", "moz-extension://hostless");

        assert!(!ledger.refused(hosted));
        ledger.authenticated(hosted);
        assert!(!ledger.refused(hosted), "a token in between starts over");

        assert!(!ledger.refused(hostless));
        assert!(ledger.refused(hostless));
        for _ in 0..3 {
            assert!(!ledger.refused(hostless), "asked already this run");
        }
    }

    /// A WebSocket client the way a browser extension is one: the upgrade
    /// with an `Origin`, then masked text frames. Returns the read and write
    /// halves once the server has said 101.
    fn ws_client(port: u16, origin: &str) -> Option<(BufReader<TcpStream>, TcpStream)> {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("read timeout");
        let mut out = stream.try_clone().expect("write half");
        write!(
            out,
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nOrigin: {origin}\r\n\r\n"
        )
        .expect("handshake");
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).expect("status line");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("header line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        status.contains(" 101 ").then_some((reader, out))
    }

    fn client_send(out: &mut TcpStream, text: &str) {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut frame = vec![0x81, 0x80 | text.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(text.bytes().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out.write_all(&frame).expect("send frame");
    }

    /// (opcode, payload) of the next unmasked server frame; None at EOF.
    fn client_recv(reader: &mut BufReader<TcpStream>) -> Option<(u8, serde_json::Value)> {
        let mut hdr = [0u8; 2];
        reader.read_exact(&mut hdr).ok()?;
        let len = match hdr[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                reader.read_exact(&mut b).ok()?;
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                reader.read_exact(&mut b).ok()?;
                u64::from_be_bytes(b) as usize
            }
            n => n as usize,
        };
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).ok()?;
        let body = serde_json::from_slice(&payload).unwrap_or(serde_json::Value::Null);
        Some((hdr[0] & 0x0F, body))
    }

    /// A server serving `serve_ws` with token "tok" on an ephemeral port,
    /// with its own event channel so no test sees another's trust requests.
    fn ws_server_with_events() -> (u16, UnboundedReceiver<ExtEvent>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
        let port = listener.local_addr().expect("address").port();
        let (tx, rx) = unbounded_channel();
        let allowed = || vec![ALLOWED.to_string()];
        let gate = std::sync::Arc::new(WsGate::new("tok".into(), allowed, tx));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let gate = gate.clone();
                std::thread::spawn(move || serve_ws(stream, &gate));
            }
        });
        (port, rx)
    }

    fn ws_server() -> u16 {
        ws_server_with_events().0
    }

    /// Opens a socket, sends a tokenless `auth` and reads to the close.
    fn knock_without_token(port: u16, origin: &str) -> serde_json::Value {
        let (mut reader, mut out) = ws_client(port, origin).expect("admitted");
        client_send(&mut out, r#"{"type":"auth","id":1}"#);
        let (_, reply) = client_recv(&mut reader).expect("auth reply");
        while client_recv(&mut reader).is_some() {}
        reply
    }

    const SIDELOADED: &str = "chrome-extension://kbopajngnjmmidookpofpjllbjfdlbhp";
    const ALLOWED: &str = "moz-extension://7d0c1a4e-allowed-by-the-user";

    /// The defect: a freshly loaded unpacked build was thrown out at the
    /// handshake. Now it gets in, and the ipc.json token — which it can only
    /// have got from the native host — makes the socket good for the rest
    /// of its life.
    #[test]
    fn a_good_token_authenticates_the_socket_for_its_lifetime() {
        let (mut reader, mut out) = ws_client(ws_server(), SIDELOADED).expect("admitted");
        client_send(&mut out, r#"{"type":"auth","token":"tok","id":1}"#);
        let (op, reply) = client_recv(&mut reader).expect("auth reply");
        assert_eq!(
            (op, reply["ok"].as_bool(), reply["id"].as_u64()),
            (0x1, Some(true), Some(1))
        );

        for id in 2..4 {
            client_send(&mut out, &format!(r#"{{"type":"ping","id":{id}}}"#));
            let (_, reply) = client_recv(&mut reader).expect("ping reply");
            assert_eq!(reply["ok"], true);
            assert_eq!(reply["id"], id);
            assert_eq!(reply["version"], env!("CARGO_PKG_VERSION"));
        }
    }

    /// A wrong token, or any request before the token, is answered once and
    /// then the socket is closed: nothing else is learned from it.
    #[test]
    fn a_bad_token_or_an_early_request_is_refused_and_the_socket_closed() {
        let port = ws_server();
        for first in [
            r#"{"type":"auth","token":"nope","id":7}"#,
            r#"{"type":"auth","id":7}"#,
            r#"{"type":"ping","id":7}"#,
            r#"{"type":"download","url":"https://example.invalid/f.zip","id":7}"#,
        ] {
            let (mut reader, mut out) = ws_client(port, SIDELOADED).expect("admitted");
            client_send(&mut out, first);
            let (op, reply) = client_recv(&mut reader).expect("refusal");
            assert_eq!(op, 0x1, "{first}");
            assert_eq!(reply["ok"], false, "{first}");
            assert_eq!(reply["error"], "unauthorized", "{first}");
            assert_eq!(reply["id"], 7, "{first}");
            assert_eq!(
                client_recv(&mut reader).map(|(op, _)| op),
                Some(0x8),
                "close frame"
            );
            assert_eq!(client_recv(&mut reader), None, "the socket is gone");
        }
    }

    /// The ids Hydra ships under are allowed on sight: a request goes
    /// through at once, and the `auth` the extension sends anyway (it does
    /// not know which id it has) is agreed to whatever it carries.
    #[test]
    fn an_allow_listed_id_needs_no_token() {
        let port = ws_server();
        let origin = format!("chrome-extension://{}", crate::nmhost::CHROMIUM_EXT_IDS[0]);
        let (mut reader, mut out) = ws_client(port, &origin).expect("admitted");
        client_send(&mut out, r#"{"type":"ping","id":1}"#);
        let (_, reply) = client_recv(&mut reader).expect("ping reply");
        assert_eq!(
            (reply["ok"].as_bool(), reply["id"].as_u64()),
            (Some(true), Some(1))
        );

        client_send(&mut out, r#"{"type":"auth","id":2}"#);
        let (_, reply) = client_recv(&mut reader).expect("auth reply");
        assert_eq!(
            (reply["ok"].as_bool(), reply["id"].as_u64()),
            (Some(true), Some(2))
        );
    }

    /// The reported defect: Firefox with no native host to fetch a token was
    /// refused forever. Its second tokenless knock now puts the question to
    /// the user, and still admits nothing by itself.
    #[test]
    fn a_hostless_extension_is_refused_but_raises_one_trust_request() {
        let (port, mut events) = ws_server_with_events();
        let origin = "moz-extension://e88b5464-98e6-41c8-a457-f4887b0486e1";
        for _ in 0..3 {
            let reply = knock_without_token(port, origin);
            assert_eq!(reply["error"], "unauthorized");
        }
        match events.try_recv() {
            Ok(ExtEvent::TrustRequest(o)) => assert_eq!(o, origin),
            other => panic!("expected one trust request, got {other:?}"),
        }
        assert!(events.try_recv().is_err(), "asked once, not per knock");
    }

    /// Once allowed, the same tokenless `auth` is agreed to and requests go
    /// through, without a token ever reaching the extension.
    #[test]
    fn an_allowed_origin_is_admitted_without_a_token() {
        let (port, mut events) = ws_server_with_events();
        let (mut reader, mut out) = ws_client(port, ALLOWED).expect("admitted");
        client_send(&mut out, r#"{"type":"auth","id":1}"#);
        let (_, reply) = client_recv(&mut reader).expect("auth reply");
        assert_eq!(reply["ok"], true);
        client_send(&mut out, r#"{"type":"ping","id":2}"#);
        let (_, reply) = client_recv(&mut reader).expect("ping reply");
        assert_eq!(
            (reply["ok"].as_bool(), reply["id"].as_u64()),
            (Some(true), Some(2))
        );
        assert!(events.try_recv().is_err());
    }

    /// A page is not an extension, and no token helps it.
    #[test]
    fn a_web_page_is_rejected_at_the_handshake() {
        assert!(ws_client(ws_server(), "https://example.com").is_none());
    }

    #[test]
    fn a_token_comparison_does_not_stop_at_the_first_difference() {
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    /// The browser's proxy is the route for THAT download, so it has to
    /// survive the wire — and an extension too old to send one still parses.
    #[test]
    fn a_capture_carries_the_browsers_proxy() {
        let with = serde_json::json!({
            "type": "download",
            "url": "https://example.invalid/f.zip",
            "proxy": "socks5://127.0.0.1:10808",
        });
        let dl: ExtDownload = serde_json::from_value(with).expect("a download request");
        assert_eq!(dl.proxy.as_deref(), Some("socks5://127.0.0.1:10808"));

        let without =
            serde_json::json!({"type": "download", "url": "https://example.invalid/f.zip"});
        let dl: ExtDownload = serde_json::from_value(without).expect("a download request");
        assert_eq!(dl.proxy, None);
    }

    /// The log is the one place a specification from outside the app is
    /// written to a file.
    #[test]
    fn a_logged_proxy_never_carries_its_password() {
        assert_eq!(
            without_credentials("socks5://joe:hunter2@10.0.0.1:1080"),
            "socks5://10.0.0.1:1080"
        );
        // A password may itself contain '@'; the credentials end at the last.
        assert_eq!(
            without_credentials("http://joe:a@b@proxy.example:8080"),
            "http://proxy.example:8080"
        );
        assert_eq!(
            without_credentials("socks5://127.0.0.1:10808"),
            "socks5://127.0.0.1:10808"
        );
        assert_eq!(without_credentials("joe:pw@proxy.example"), "proxy.example");
    }
}
