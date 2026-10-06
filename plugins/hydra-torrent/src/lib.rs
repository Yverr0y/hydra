//! Magnet and torrent-file resolution through the approved native engine.

use base64::Engine;
use hya_plugin_sdk::{ErrorCode, Host, Plan, Plugin, PluginError, Resolve, ResolveRequest, Result};

const ENGINE: &str = "torrent-engine";

#[derive(Default)]
struct Resolver;

fn error(message: impl std::fmt::Display) -> PluginError {
    PluginError::new(ErrorCode::InvalidInput, message.to_string())
}

fn input(address: &str) -> Result<Option<String>> {
    let url = url::Url::parse(address).map_err(error)?;
    match url.scheme() {
        "magnet" => Ok(Some(address.into())),
        "file" => {
            if !url.path().to_ascii_lowercase().ends_with(".torrent") {
                return Ok(None);
            }
            Ok(Some(address.into()))
        }
        "http" | "https" if url.path().to_ascii_lowercase().ends_with(".torrent") => {
            Ok(Some(address.into()))
        }
        _ => Ok(None),
    }
}

impl Plugin for Resolver {
    fn resolve(&mut self, host: &Host, req: ResolveRequest) -> Result<Resolve> {
        let Some(input) = input(&req.url)? else {
            return Ok(Resolve::NotClaimed);
        };
        let local = req.url.starts_with("file:");
        let remote = input.starts_with("http://") || input.starts_with("https://");
        let _ = host.log(
            "debug",
            if local {
                "Reading selected torrent file"
            } else if remote {
                "Fetching torrent metadata over HTTP"
            } else {
                "Resolving magnet metadata from peers"
            },
        );
        let request = if local {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(host.input_read()?)
                .map_err(error)?;
            hya_plugin_sdk::serde_json::json!({"metainfo_hex":bounded_hex(&bytes)?})
        } else if remote {
            let reply: hya_plugin_sdk::serde_json::Value =
                host.call("http", &hya_plugin_sdk::serde_json::json!({"url": input}))?;
            if reply["status"].as_u64() != Some(200) {
                return Err(error("torrent metadata URL did not return HTTP 200"));
            }
            let body = reply["body_b64"]
                .as_str()
                .ok_or_else(|| error("missing torrent metadata"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(error)?;
            hya_plugin_sdk::serde_json::json!({"metainfo_hex":bounded_hex(&bytes)?})
        } else {
            hya_plugin_sdk::serde_json::json!({"magnet":input})
        };
        let result: Result<Plan> = host.native_call(ENGINE, "inspect", &request);
        let mut plan = result.map_err(|failure| {
            let _ = host.log(
                "error",
                &format!("Torrent metadata resolution failed: {failure}"),
            );
            failure
        })?;
        let _ = host.log(
            "info",
            "Torrent metadata resolved; file selection available",
        );
        let settings = host.settings()?;
        let transfer = plan
            .transfer
            .as_mut()
            .ok_or_else(|| error("engine returned no torrent metadata"))?;
        transfer.metadata["seed_seconds"] = number(&settings, "seed_seconds", 86400)?.into();
        transfer.metadata["upload_limit"] =
            number(&settings, "upload_limit", i32::MAX as u32)?.into();
        Ok(Resolve::Plan(plan))
    }

    fn check(&mut self, host: &Host) -> Result<()> {
        let _: hya_plugin_sdk::serde_json::Value =
            host.native_call(ENGINE, "check", &hya_plugin_sdk::serde_json::json!({}))?;
        let _ = host.log("info", "Native torrent engine health check passed");
        Ok(())
    }
}

fn bounded_hex(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        return Err(error("torrent metadata must be between 1 byte and 4 MiB"));
    }
    Ok(hex(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 15) as usize] as char);
    }
    value
}

fn number(
    settings: &std::collections::BTreeMap<String, hya_plugin_sdk::Value>,
    key: &str,
    max: u32,
) -> Result<u32> {
    match settings.get(key) {
        None => Ok(0),
        Some(hya_plugin_sdk::Value::Number(value))
            if value.is_finite()
                && *value >= 0.0
                && *value <= f64::from(max)
                && value.fract() == 0.0 =>
        {
            Ok(*value as u32)
        }
        _ => Err(error(format!("invalid {key}"))),
    }
}

hya_plugin_sdk::export!(Resolver);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_magnets_and_torrent_files_and_declines_unrelated_files() {
        assert!(
            input("magnet:?xt=urn:btih:0123456789012345678901234567890123456789")
                .unwrap()
                .is_some()
        );
        assert!(input("file:///tmp/a%20b.torrent")
            .unwrap()
            .unwrap()
            .ends_with("a%20b.torrent"));
        assert!(input("https://example.com/file.torrent?token=x")
            .unwrap()
            .is_some());
        assert!(input("file:///tmp/document.pdf").unwrap().is_none());
        assert!(input("https://example.com/file.torrent.exe")
            .unwrap()
            .is_none());
    }

    #[test]
    fn settings_reject_fractional_and_unbounded_limits() {
        let mut settings = std::collections::BTreeMap::new();
        settings.insert(
            "seed_seconds".into(),
            hya_plugin_sdk::Value::Number(86400.0),
        );
        assert_eq!(number(&settings, "seed_seconds", 86400).unwrap(), 86400);
        for value in [-1.0, 0.5, 86401.0, f64::NAN] {
            settings.insert("seed_seconds".into(), hya_plugin_sdk::Value::Number(value));
            assert!(number(&settings, "seed_seconds", 86400).is_err());
        }
    }
}
