// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Options > Proxy/Socks, turned into a route the transport can take.
//!
//! The settings are strings; the transport wants one of two very different
//! things, and which one depends on the proxy's protocol:
//!
//! * a **SOCKS** proxy is a property of the CONNECTION — the socket is opened
//!   to the proxy and the handshake asks it to reach the origin, so it belongs
//!   on the connector ([`hya_net::TlsCapableConnector::with_socks`]);
//! * an **HTTP** proxy is a property of the REQUEST — the request line carries
//!   the origin in absolute form, or a `CONNECT` tunnel is opened for TLS, so
//!   it belongs on each [`hya_net::Target`].
//!
//! Conflating the two sends a `CONNECT` to the origin, or an absolute-form GET
//! to a SOCKS port. [`Route`] keeps them apart, and the engine asks it which
//! of the two it is holding.
//!
//! The route resolved from Options is process-wide because that setting is:
//! it is applied at startup and again whenever Options is accepted, the same
//! way `engine::set_power_save` is. A download may override it — see
//! [`for_choice`] and [`crate::model::ProxyChoice`] — because one large file
//! through the tunnel while everything else takes the fast direct path (or
//! the reverse) is the reason a download manager is often why the proxy is
//! there at all.

use crate::model::{ProxyChoice, ProxyMode, ProxyPick, Settings};
use hya_net::{Proxy, ProxyKind};
use std::sync::RwLock;

/// How this process reaches origins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Route {
    /// `None` = connect straight to the origin.
    proxy: Option<Proxy>,
}

impl Route {
    /// No proxy: every connection goes to the origin itself.
    pub(crate) fn plugin_proxy(&self) -> Option<Proxy> {
        self.proxy.clone()
    }

    pub const fn direct() -> Self {
        Self { proxy: None }
    }

    /// The proxy every connection must be opened through, or `None` when the
    /// proxy is one the connector knows nothing about (HTTP) or absent.
    pub fn socks(&self) -> Option<Proxy> {
        self.proxy.clone().filter(|p| p.kind.is_socks())
    }

    /// The forward proxy each request must be addressed to, as host and port,
    /// or `None` when the route is direct or carried by SOCKS.
    pub fn http(&self) -> Option<(&str, u16)> {
        self.proxy
            .as_ref()
            .filter(|p| p.kind == ProxyKind::Http)
            .map(|p| (p.host.as_str(), p.port))
    }

    /// One line for the log: which proxy, spoken how. Credentials never
    /// appear.
    pub fn describe(&self) -> String {
        match &self.proxy {
            Some(p) => format!("{} proxy {}:{}", p.kind.as_str(), p.host, p.port),
            None => "no proxy".to_string(),
        }
    }
}

static ACTIVE: RwLock<Route> = RwLock::new(Route::direct());

/// Resolve `s` and make it the route every later connection takes.
pub fn apply(s: &Settings) {
    let route = resolve(s);
    crate::log::info(&format!("proxy: {}", route.describe()));
    if let Ok(mut g) = ACTIVE.write() {
        *g = route;
    }
}

/// The route in force. Cheap enough to call per connection, never per chunk.
pub fn active() -> Route {
    ACTIVE.read().map(|g| g.clone()).unwrap_or_default()
}

/// The route ONE download takes.
///
/// # Errors
///
/// A per-download specification that cannot be parsed. It is returned rather
/// than quietly ignored: falling back to the app default — or to a direct
/// connection — would send the transfer somewhere other than where the user
/// pointed it, which is the whole failure this module exists to prevent. The
/// dialogs check the same specification as it is typed, so this is the last
/// line rather than the first.
pub fn for_choice(choice: &ProxyChoice) -> Result<Route, String> {
    match choice {
        ProxyChoice::Default => Ok(active()),
        ProxyChoice::Direct => Ok(Route::direct()),
        ProxyChoice::Custom(spec) => {
            match Proxy::parse(spec.trim()) {
                Ok(px) => Ok(Route { proxy: Some(px) }),
                Err(e) => Err(crate::i18n::tr("This download's proxy is unusable: {why}")
                    .replace("{why}", &e)),
            }
        }
    }
}

/// Whether a per-download specification is one the transport can take, for a
/// dialog to say so while it is being typed.
pub fn spec_error(spec: &str) -> Option<String> {
    Proxy::parse(spec.trim()).err()
}

/// Why Options cannot accept "Use automatic configuration script". Said in
/// the tab and again when OK is refused, so a route the app cannot take is
/// never stored as if it could.
pub const PAC_UNSUPPORTED: &str =
    "Configuration scripts (PAC) are not evaluated yet — choose manual \
     configuration or system settings.";

/// What is wrong with the manual tab's fields, or `None` when they describe
/// a proxy the transport can open. The same rules [`manual`] applies, asked
/// before the settings are accepted instead of logged after they fail.
pub fn manual_problem(s: &Settings) -> Option<String> {
    let raw = s.proxy_host.trim();
    if raw.is_empty() {
        return Some(crate::i18n::tr("Enter the proxy server address."));
    }
    let spec = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("{}://{raw}", s.proxy_type.scheme())
    };
    if let Err(e) = Proxy::parse(&spec) {
        return Some(e);
    }
    let port = s.proxy_port.trim();
    if !port.is_empty() && !port.parse::<u16>().is_ok_and(|n| n > 0) {
        return Some(
            crate::i18n::tr("{port} is not a port number (1-65535).").replace("{port}", port),
        );
    }
    None
}

/// The same question a dialog actually asks: what is wrong with the address
/// the user is TYPING, or `None` while there is nothing to complain about.
///
/// Two cases are not complaints. A picker on anything but "this download
/// only" is not asking for an address at all, and an empty box is an address
/// not yet typed — pointing at it in red the instant the picker moves says
/// the user did something wrong when they have not yet done anything.
pub fn typed_spec_error(pick: ProxyPick, spec: &str) -> Option<String> {
    if pick != ProxyPick::Custom || spec.trim().is_empty() {
        return None;
    }
    spec_error(spec)
}

/// Settings to route, with the reason logged when a configured proxy cannot
/// be used — a proxy that silently does nothing sends the traffic it was
/// meant to tunnel straight to the origin, which is the failure this whole
/// module exists to make impossible.
fn resolve(s: &Settings) -> Route {
    let proxy = match s.proxy_mode {
        ProxyMode::None => None,
        ProxyMode::Manual => manual(s),
        ProxyMode::System => {
            let px = system();
            if px.is_none() {
                crate::log::warn("proxy: the system names no proxy; connecting directly");
            }
            px
        }
        // A PAC file is a JavaScript program: answering it needs an
        // interpreter, which this app does not carry. The Options tab says so
        // beside the field; this is the other half of saying it.
        ProxyMode::Script => {
            crate::log::warn(
                "proxy: automatic configuration scripts (PAC) are not evaluated; \
                 connecting directly",
            );
            None
        }
    };
    Route { proxy }
}

/// The manual tab's fields, as one proxy.
///
/// The address box accepts a full spec (`socks5://127.0.0.1:10808`) as well as
/// a bare host, because that is what a v2rayN or Tor user has on the
/// clipboard. A scheme it states wins over the type picker: sending a SOCKS
/// handshake to an address the user labelled `http://` would fail with a
/// message about the wrong protocol.
fn manual(s: &Settings) -> Option<Proxy> {
    let raw = s.proxy_host.trim();
    if raw.is_empty() {
        crate::log::warn("proxy: manual mode with no address; connecting directly");
        return None;
    }
    let stated_scheme = raw.contains("://");
    let spec = if stated_scheme {
        raw.to_string()
    } else {
        format!("{}://{raw}", s.proxy_type.scheme())
    };
    let mut px = match Proxy::parse(&spec) {
        Ok(px) => px,
        Err(e) => {
            crate::log::warn(&format!("proxy: {e}; connecting directly"));
            return None;
        }
    };
    // The Port box sits next to the address and wins when it is filled in:
    // a port left inside a pasted address is the leftover, not the choice.
    match s.proxy_port.trim() {
        "" => {}
        p => match p.parse::<u16>() {
            Ok(n) if n > 0 => px.port = n,
            _ => {
                crate::log::warn(&format!(
                    "proxy: {p:?} is not a port; using {} instead",
                    px.port
                ));
            }
        },
    }
    let user = s.proxy_user.trim();
    if !user.is_empty() {
        px.username = Some(user.to_string());
        px.password = Some(s.proxy_pass.clone());
    }
    Some(px)
}

/// What the machine says its proxy is.
///
/// The environment is asked first and on every platform: a shell that exports
/// `all_proxy` has said something more specific about THIS process than the
/// desktop-wide setting has. `all_proxy` leads because it is the variable a
/// SOCKS tunnel is conventionally published in, and a SOCKS proxy carries
/// every scheme.
///
/// Only one proxy is taken, not one per scheme: a download manager routes a
/// transfer, and a per-scheme split would have to be resolved again at every
/// redirect that crosses from `http` to `https`.
fn system() -> Option<Proxy> {
    from_env().or_else(platform_proxy)
}

fn from_env() -> Option<Proxy> {
    match Proxy::from_env() {
        Ok(px) => px,
        Err(e) => {
            crate::log::warn(&format!("proxy: ignoring system setting: {e}"));
            None
        }
    }
}

/// A proxy address read from the environment or from a platform setting.
///
/// Failure is a log line rather than an error: nothing the user typed in this
/// app is wrong, so the honest report is that the system's own value could not
/// be used.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn parse_system(raw: &str) -> Option<Proxy> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match Proxy::parse(raw) {
        Ok(px) => Some(px),
        Err(e) => {
            crate::log::warn(&format!("proxy: ignoring system setting {raw:?}: {e}"));
            None
        }
    }
}

/// The desktop-wide proxy, where the platform keeps one.
///
/// Linux has no single such place — the environment is the convention there,
/// and it has already been asked.
#[cfg(target_os = "windows")]
fn platform_proxy() -> Option<Proxy> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY).ok()?;
    // WinINET keeps the switch and the address apart: an address left behind
    // by a proxy client that has since been turned off must not be used.
    if key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) == 0 {
        return None;
    }
    let raw: String = key.get_value("ProxyServer").ok()?;
    parse_win_proxy_server(&raw)
}

/// WinINET's `ProxyServer` value: either one `host:port` for every scheme, or
/// a `;`-separated list of `scheme=host:port` entries.
///
/// `socks=` is preferred when present: it tunnels every scheme, and a client
/// that publishes both (v2rayN, Shadowsocks, Clash) is offering the SOCKS
/// entry as the more capable one. WinINET does not record which SOCKS version
/// it is; every such client speaks 5.
#[cfg(any(target_os = "windows", test))]
fn parse_win_proxy_server(raw: &str) -> Option<Proxy> {
    let entries: Vec<(&str, &str)> = raw
        .split(';')
        .filter_map(|e| e.trim().split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    if entries.is_empty() {
        return parse_system(raw);
    }
    let pick = |scheme: &str| {
        entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(scheme))
            .map(|(_, v)| *v)
    };
    match pick("socks") {
        Some(v) => parse_system(&format!("socks5://{v}")),
        None => parse_system(pick("https").or_else(|| pick("http"))?),
    }
}

/// macOS keeps the answer in the dynamic store, which `scutil --proxy` prints
/// as a plain key/value block. Reading it through the tool rather than through
/// SystemConfiguration keeps a C framework binding out of the GUI for a value
/// read twice per session.
#[cfg(target_os = "macos")]
fn platform_proxy() -> Option<Proxy> {
    let out = std::process::Command::new("/usr/sbin/scutil")
        .arg("--proxy")
        .output()
        .ok()?;
    parse_scutil(&String::from_utf8_lossy(&out.stdout))
}

/// `scutil --proxy` output: `<key> : <value>` lines, one per setting.
///
/// SOCKS first for the same reason as on Windows — it carries every scheme —
/// then HTTPS, then HTTP.
#[cfg(any(target_os = "macos", test))]
fn parse_scutil(text: &str) -> Option<Proxy> {
    let value = |key: &str| {
        text.lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().to_string())
    };
    let enabled = |key: &str| value(key).as_deref() == Some("1");
    for (on, host, port, scheme) in [
        ("SOCKSEnable", "SOCKSProxy", "SOCKSPort", "socks5"),
        ("HTTPSEnable", "HTTPSProxy", "HTTPSPort", "http"),
        ("HTTPEnable", "HTTPProxy", "HTTPPort", "http"),
    ] {
        if !enabled(on) {
            continue;
        }
        let (Some(h), Some(p)) = (value(host), value(port)) else {
            continue;
        };
        return parse_system(&format!("{scheme}://{h}:{p}"));
    }
    None
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_proxy() -> Option<Proxy> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProxyType;

    fn manual_settings(host: &str, port: &str, ty: ProxyType) -> Settings {
        Settings {
            proxy_mode: ProxyMode::Manual,
            proxy_type: ty,
            proxy_host: host.to_string(),
            proxy_port: port.to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn manual_socks5_is_carried_by_the_connector_not_the_request() {
        let r = resolve(&manual_settings("127.0.0.1", "10808", ProxyType::Socks5));
        let px = r.socks().expect("a SOCKS proxy belongs on the connector");
        assert_eq!(px.kind, ProxyKind::Socks5);
        assert_eq!((px.host.as_str(), px.port), ("127.0.0.1", 10808));
        assert_eq!(r.http(), None, "SOCKS must never rewrite a request line");
    }

    #[test]
    fn manual_http_proxy_is_carried_by_the_request_not_the_connector() {
        let r = resolve(&manual_settings("proxy.local", "3128", ProxyType::Http));
        assert_eq!(r.http(), Some(("proxy.local", 3128)));
        assert_eq!(r.socks(), None, "a CONNECT proxy is not a SOCKS proxy");
    }

    #[test]
    fn a_pasted_spec_keeps_the_scheme_it_states() {
        let r = resolve(&manual_settings(
            "socks5://127.0.0.1:10808",
            "",
            ProxyType::Http,
        ));
        assert_eq!(r.socks().map(|p| p.port), Some(10808));
    }

    #[test]
    fn the_port_box_wins_over_a_port_left_in_the_address() {
        let r = resolve(&manual_settings(
            "socks5://127.0.0.1:1080",
            "10808",
            ProxyType::Socks5,
        ));
        assert_eq!(r.socks().map(|p| p.port), Some(10808));
    }

    /// An address with no port at all gets the default for its protocol, not
    /// the HTTP one: 1080 is where every SOCKS proxy listens.
    #[test]
    fn a_bare_socks_address_defaults_to_the_socks_port() {
        let r = resolve(&manual_settings("127.0.0.1", "", ProxyType::Socks5));
        assert_eq!(r.socks().map(|p| p.port), Some(1080));
    }

    #[test]
    fn credentials_come_from_their_own_boxes() {
        let s = Settings {
            proxy_user: "u".into(),
            proxy_pass: "p:@ss".into(),
            ..manual_settings("127.0.0.1", "10808", ProxyType::Socks5)
        };
        let px = resolve(&s).socks().expect("socks");
        assert_eq!(px.username.as_deref(), Some("u"));
        assert_eq!(px.password.as_deref(), Some("p:@ss"));
    }

    /// Every mode that cannot produce a proxy must produce a DIRECT route
    /// that says so, never a half-configured one.
    #[test]
    fn unusable_configurations_resolve_to_direct() {
        for s in [
            Settings::default(),
            manual_settings("", "10808", ProxyType::Socks5),
            manual_settings("socks9://h", "", ProxyType::Socks5),
            Settings {
                proxy_mode: ProxyMode::Script,
                proxy_script: "http://wpad/proxy.pac".into(),
                ..Settings::default()
            },
        ] {
            let r = resolve(&s);
            assert_eq!(r.socks(), None, "{:?} must resolve to direct", s.proxy_mode);
            assert_eq!(r.http(), None, "{:?} must resolve to direct", s.proxy_mode);
            assert_eq!(r.describe(), "no proxy");
        }
    }

    #[test]
    fn a_bad_port_box_falls_back_to_the_address_instead_of_dropping_the_proxy() {
        let r = resolve(&manual_settings(
            "127.0.0.1:10808",
            "half",
            ProxyType::Socks5,
        ));
        assert_eq!(r.socks().map(|p| p.port), Some(10808));
    }

    #[test]
    fn describe_never_prints_credentials() {
        let s = Settings {
            proxy_user: "user".into(),
            proxy_pass: "secret".into(),
            ..manual_settings("127.0.0.1", "10808", ProxyType::Socks5)
        };
        let line = resolve(&s).describe();
        assert_eq!(line, "socks5 proxy 127.0.0.1:10808");
        assert!(!line.contains("secret"));
    }

    /// The route has to be published where every later connection reads it,
    /// which is the step whose absence was the bug: the settings were saved
    /// and nothing ever asked for them.
    #[test]
    fn applying_settings_publishes_the_route_every_connection_takes() {
        apply(&Settings::default());
        assert_eq!(active(), Route::direct());
        assert_eq!(active().describe(), "no proxy");
    }

    /// A system value this app cannot use is ignored whole. Taking the host
    /// and inventing the rest would tunnel to somewhere nobody configured.
    #[test]
    fn an_unusable_system_value_is_ignored_rather_than_half_used() {
        assert_eq!(parse_system(""), None);
        assert_eq!(parse_system("   "), None);
        assert_eq!(parse_system("ftp://p:2121"), None);
        assert_eq!(
            parse_system("socks5://127.0.0.1:10808").map(|p| p.port),
            Some(10808)
        );
    }

    /// A download that names its own proxy takes it whatever Options says,
    /// and one that names none follows Options — including a later change to
    /// it, which is why the default is resolved at start rather than copied
    /// onto the item.
    #[test]
    fn a_download_can_take_its_own_route_or_the_app_default() {
        apply(&Settings::default());
        assert_eq!(for_choice(&ProxyChoice::Default), Ok(Route::direct()));
        assert_eq!(for_choice(&ProxyChoice::Direct), Ok(Route::direct()));
        let own = for_choice(&ProxyChoice::Custom("socks5://127.0.0.1:10808".into()))
            .expect("a parseable specification");
        assert_eq!(own.socks().map(|p| p.port), Some(10808));
        assert_eq!(own.describe(), "socks5 proxy 127.0.0.1:10808");
    }

    /// A per-download specification that cannot be parsed must stop the
    /// transfer, not quietly become a direct connection.
    #[test]
    fn an_unusable_per_download_spec_is_an_error_not_a_direct_connection() {
        let e = for_choice(&ProxyChoice::Custom("gopher://p".into())).unwrap_err();
        assert!(e.contains("gopher"), "the reason must survive: {e}");
        assert!(spec_error("gopher://p").is_some());
        assert_eq!(spec_error("socks5://127.0.0.1:10808"), None);
        assert!(spec_error("  ").is_some(), "an empty box is not a proxy");
    }

    /// What a dialog complains about while someone is typing. An empty box
    /// is an address not yet entered, not a mistake — the red line under a
    /// freshly-chosen "this download only" was pointing at the user before
    /// they had done anything.
    #[test]
    fn a_dialog_complains_only_about_an_address_that_was_actually_typed() {
        assert_eq!(typed_spec_error(ProxyPick::Custom, ""), None);
        assert_eq!(typed_spec_error(ProxyPick::Custom, "   "), None);
        assert_eq!(typed_spec_error(ProxyPick::Default, "gopher://p"), None);
        assert_eq!(typed_spec_error(ProxyPick::Direct, "gopher://p"), None);
        assert!(typed_spec_error(ProxyPick::Custom, "gopher://p").is_some());
        assert_eq!(
            typed_spec_error(ProxyPick::Custom, "socks5://127.0.0.1:10808"),
            None
        );
    }

    #[test]
    fn windows_proxy_server_prefers_socks_over_the_http_entry() {
        let px = parse_win_proxy_server("http=127.0.0.1:10809;socks=127.0.0.1:10808")
            .expect("a socks entry is a proxy");
        assert_eq!(px.kind, ProxyKind::Socks5);
        assert_eq!(px.port, 10808);
    }

    /// What v2rayN's "system proxy" actually writes: one HTTP inbound, either
    /// bare or under `http=`.
    #[test]
    fn windows_proxy_server_reads_both_spellings_of_one_http_proxy() {
        for raw in ["127.0.0.1:10809", "http=127.0.0.1:10809"] {
            let px = parse_win_proxy_server(raw).expect("an address is a proxy");
            assert_eq!(px.kind, ProxyKind::Http);
            assert_eq!((px.host.as_str(), px.port), ("127.0.0.1", 10809));
        }
    }

    #[test]
    fn windows_proxy_server_ignores_an_entry_for_a_scheme_we_do_not_route() {
        assert_eq!(parse_win_proxy_server("ftp=127.0.0.1:2121"), None);
    }

    #[test]
    fn scutil_reads_the_enabled_proxy_only() {
        let text = "\
<dictionary> {
  HTTPEnable : 1
  HTTPPort : 3128
  HTTPProxy : 10.0.0.1
  SOCKSEnable : 0
  SOCKSPort : 1080
  SOCKSProxy : 10.0.0.9
}";
        let px = parse_scutil(text).expect("HTTP is enabled");
        assert_eq!(px.kind, ProxyKind::Http);
        assert_eq!((px.host.as_str(), px.port), ("10.0.0.1", 3128));

        let socks = text.replace("SOCKSEnable : 0", "SOCKSEnable : 1");
        let px = parse_scutil(&socks).expect("SOCKS wins when both are on");
        assert_eq!(px.kind, ProxyKind::Socks5);
        assert_eq!((px.host.as_str(), px.port), ("10.0.0.9", 1080));
    }

    #[test]
    fn scutil_with_nothing_enabled_is_direct() {
        assert_eq!(
            parse_scutil("<dictionary> {\n  ExceptionsList : <array>\n}"),
            None
        );
    }
}
