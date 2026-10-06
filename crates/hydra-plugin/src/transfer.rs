//! Native transfer authorization, bounded protocol and transfer lifecycle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fs2::FileExt;
use hya_plugin_api::{ErrorCode, PluginError, Transfer};
use serde::{Deserialize, Serialize};

use crate::manager::Manager;

const MAX_FRAME: usize = 64 * 1024;

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::new(ErrorCode::InvalidPlan, message)
}

/// Checks metadata bounds and portable output paths before loading a native module.
/// # Errors
/// Returns an invalid-plan error for malformed metadata or file descriptions.
pub fn validate(transfer: &Transfer) -> Result<(), PluginError> {
    if transfer.details.as_ref().is_some_and(|details| {
        details.title.is_empty()
            || details.title.len() > 128
            || details.title.chars().any(char::is_control)
            || details.columns.is_empty()
            || details.columns.len() > 8
            || details.columns.iter().any(|label| {
                label.is_empty() || label.len() > 80 || label.chars().any(char::is_control)
            })
    }) {
        return Err(invalid("invalid transfer detail table"));
    }

    if (transfer.output == hya_plugin_api::TransferOutput::File && transfer.files.len() != 1)
        || transfer.engine.is_empty()
        || transfer.engine.contains(['/', '\\'])
        || serde_json::to_vec(&transfer.metadata)
            .map_err(|e| invalid(e.to_string()))?
            .len()
            > 16 * 1024 * 1024
        || transfer.files.is_empty()
        || transfer.files.len() > 4096
        || transfer
            .notice
            .as_ref()
            .is_some_and(|notice| notice.len() > 4096)
    {
        return Err(invalid("invalid or oversized native transfer description"));
    }
    let mut paths = std::collections::HashSet::new();
    let mut indices = std::collections::HashSet::new();
    for file in &transfer.files {
        if file.path.len() > 4096
            || !safe_path(&file.path)
            || !paths.insert(file.path.to_ascii_lowercase())
            || !indices.insert(file.index)
        {
            return Err(invalid("transfer contains unsafe or duplicate file paths"));
        }
    }
    transfer
        .files
        .iter()
        .try_fold(0u64, |sum, f| sum.checked_add(f.size))
        .ok_or_else(|| invalid("transfer size overflows"))?;
    Ok(())
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.contains(['<', '>', '"', '|', '?', '*'])
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}

/// A bounded, leveled native-engine diagnostic.
#[derive(Debug, Clone, Deserialize)]
pub struct LogRecord {
    pub level: String,
    pub message: String,
}

/// Transfer state emitted by a native transfer engine.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub details: Vec<Vec<String>>,
    #[serde(default)]
    pub logs: Vec<LogRecord>,
    pub state: String,
    #[serde(default)]
    pub done: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub download_rate: u64,
    #[serde(default)]
    pub upload_rate: u64,
    #[serde(default)]
    pub peers: u32,
    #[serde(default)]
    pub error: Option<String>,
}

/// Host-selected transfer destination and file selection.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub transfer: Transfer,
    pub destination: PathBuf,
    pub files: Option<Vec<u32>>,
    pub download_limit: u64,
    pub resume_path: PathBuf,
    pub proxy: Option<serde_json::Value>,
    /// Live cap: zero is unlimited; u64::MAX - 1 suspends transfer traffic.
    #[serde(skip)]
    pub control: Option<Arc<std::sync::atomic::AtomicU64>>,
}

/// Encodes the frontend's selected proxy for native engines without guest overrides.
pub fn proxy_config(proxy: &hya_net::Proxy) -> serde_json::Value {
    serde_json::json!({"kind":proxy.kind.as_str(), "host":proxy.host, "port":proxy.port,
        "username":proxy.username, "password":proxy.password})
}

/// Finds the enabled plugin's approved, unchanged transfer engine.
/// # Errors
/// Returns an error if the plugin is disabled, the grant is absent or the executable changed.
pub fn authorize(
    root: PathBuf,
    plugin: &str,
    engine: &str,
) -> Result<crate::native::Module, PluginError> {
    let manager = Manager::open_with_official(root)?;
    let installed = manager
        .list()
        .iter()
        .find(|p| p.manifest.id == plugin && p.enabled)
        .ok_or_else(|| {
            PluginError::new(
                ErrorCode::PermissionDenied,
                "transfer plugin is disabled or removed",
            )
        })?;
    if !installed.grants.native.iter().any(|id| id == engine) {
        return Err(PluginError::new(
            ErrorCode::PermissionDenied,
            "native transfer execution is not granted",
        ));
    }
    let module = installed
        .manifest
        .native_modules
        .iter()
        .find(|module| module.id == engine && module.platform == crate::native::platform())
        .ok_or_else(|| {
            PluginError::new(
                ErrorCode::Unsupported,
                "plugin has no native module for this platform",
            )
        })?;
    let sha256 = installed
        .native_sha256
        .get(&module.module)
        .cloned()
        .ok_or_else(|| {
            PluginError::new(
                ErrorCode::PermissionDenied,
                "native module has no installation hash",
            )
        })?;
    Ok(crate::native::Module {
        path: installed.directory.join(&module.module),
        sha256,
    })
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Runs a native transfer and acknowledges cancellation after it saves resume state.
/// # Errors
/// Returns an error for invalid input, native failures or malformed progress frames.
pub async fn download(
    module: &crate::native::Module,
    request: Request,
    cancel: Arc<AtomicBool>,
    mut update: impl FnMut(&Progress),
) -> Result<Progress, String> {
    validate(&request.transfer).map_err(|e| e.to_string())?;
    if let Some(files) = &request.files {
        if files.is_empty()
            || files
                .iter()
                .any(|index| !request.transfer.files.iter().any(|f| f.index == *index))
        {
            return Err("select at least one valid transfer file".into());
        }
    }
    let module = module.clone();
    let payload = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let aborted = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(aborted.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let mut task = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut lock_name = request.destination.as_os_str().to_owned();
        lock_name.push(".hydra-transfer-lock");
        let lock_path = PathBuf::from(lock_name);
        if let Some(parent) = lock_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "another transfer already owns this destination")?;
        module
            .call_with_limit(
                "download",
                &payload,
                |bytes| {
                    if bytes.len() > MAX_FRAME {
                        return Err("oversized native transfer frame".into());
                    }
                    let progress: Progress = serde_json::from_slice(bytes)
                        .map_err(|e| format!("invalid native transfer frame: {e}"))?;
                    if progress.details.len() > 128
                        || progress.details.iter().any(|row| {
                            row.len()
                                != request
                                    .transfer
                                    .details
                                    .as_ref()
                                    .map_or(0, |details| details.columns.len())
                                || row.iter().any(|cell| {
                                    cell.len() > 256 || cell.chars().any(char::is_control)
                                })
                        })
                    {
                        return Err("invalid native detail rows".into());
                    }
                    if progress.logs.len() > 32
                        || progress.logs.iter().any(|record| {
                            !matches!(record.level.as_str(), "debug" | "info" | "warn" | "error")
                                || record.message.len() > 4096
                        })
                    {
                        return Err("invalid native log record".into());
                    }
                    if let Some(error) = &progress.error {
                        return Err(error.clone());
                    }
                    if progress.done > progress.total
                        || !matches!(
                            progress.state.as_str(),
                            "starting"
                                | "checking"
                                | "downloading"
                                | "seeding"
                                | "complete"
                                | "stopped"
                        )
                    {
                        return Err("invalid native transfer progress".into());
                    }
                    sender
                        .blocking_send(progress)
                        .map_err(|_| "native transfer receiver closed".into())
                },
                || cancel.load(Ordering::Relaxed) || aborted.load(Ordering::Relaxed),
                request.control,
            )
            .map_err(|e| e.to_string())
    });
    let mut latest = None;
    let result = loop {
        tokio::select! {
            progress = receiver.recv() => {
                if let Some(progress) = progress { update(&progress); latest = Some(progress); }
                else { break (&mut task).await; }
            }
            result = &mut task => break result,
        }
    };
    while let Some(progress) = receiver.recv().await {
        update(&progress);
        latest = Some(progress);
    }
    result.map_err(|e| e.to_string())??;
    let latest = latest.ok_or("native transfer returned no progress")?;
    if !matches!(latest.state.as_str(), "complete" | "stopped") {
        return Err("native transfer exited before completing the transfer".into());
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(path: &str) -> Transfer {
        Transfer {
            engine: "example-engine".into(),
            details: None,
            output: hya_plugin_api::TransferOutput::Directory,
            metadata: serde_json::json!({}),
            notice: None,
            files: vec![hya_plugin_api::TransferFile {
                index: 0,
                path: path.into(),
                size: 10,
            }],
        }
    }

    #[test]
    fn detail_headings_are_bounded_and_have_visible_labels() {
        let mut transfer = transfer("payload");
        transfer.details = Some(hya_plugin_api::TransferDetails {
            title: "Peers".into(),
            columns: vec!["Peer".into()],
        });
        validate(&transfer).unwrap();
        for columns in [
            vec![],
            vec!["x".into(); 9],
            vec!["bad\nheading".into()],
            vec!["".into()],
        ] {
            transfer.details.as_mut().unwrap().columns = columns;
            assert!(validate(&transfer).is_err());
        }
    }

    #[test]
    fn rejects_paths_that_escape_or_collide_on_supported_platforms() {
        for path in [
            "../outside",
            "/absolute",
            "dir/../file",
            "dir\\file",
            "C:/file",
            "dir//file",
            "CON.txt",
            "file.",
            "a\0b",
        ] {
            assert!(validate(&transfer(path)).is_err(), "{path}");
        }
        assert!(validate(&transfer("ubuntu/image.iso")).is_ok());
        let mut t = transfer("A/file");
        t.files.push(hya_plugin_api::TransferFile {
            index: 1,
            path: "a/FILE".into(),
            size: 2,
        });
        assert!(validate(&t).is_err());
    }

    #[test]
    fn rejects_invalid_engines_and_overflowing_sizes() {
        let mut t = transfer("file");
        t.engine = "../engine".into();
        assert!(validate(&t).is_err());
        t.engine = "example-engine".into();
        t.files[0].size = u64::MAX;
        t.files.push(hya_plugin_api::TransferFile {
            index: 1,
            path: "other".into(),
            size: 1,
        });
        assert!(validate(&t).is_err());
    }
    #[cfg(unix)]
    fn request(directory: &std::path::Path, mode: &str) -> Request {
        let mut transfer = transfer("file");
        transfer.metadata = serde_json::json!({"mode":mode});
        Request {
            transfer,
            destination: directory.join("output"),
            resume_path: directory.join("resume"),
            files: None,
            download_limit: 0,
            proxy: None,
            control: None,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_transfer_delivers_progress_and_acknowledges_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let module = crate::native::tests::fixture();
        for (cancelled, expected) in [(false, "complete"), (true, "stopped")] {
            let mut bytes = Vec::new();
            let final_state = download(
                &module,
                request(directory.path(), "normal"),
                Arc::new(AtomicBool::new(cancelled)),
                |p| {
                    if p.state == "downloading" {
                        assert_eq!(p.logs[0].level, "info");
                        assert_eq!(p.logs[0].message, "Transfer started");
                    }
                    bytes.push(p.done);
                },
            )
            .await
            .unwrap();
            assert_eq!(final_state.state, expected);
            assert_eq!(bytes, [5, 10]);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_transfer_rejects_broken_protocol_and_empty_selections() {
        let directory = tempfile::tempdir().unwrap();
        let module = crate::native::tests::fixture();
        for mode in [
            "invalid_log",
            "malformed",
            "error_reply",
            "unfinished",
            "no_reply",
            "oversized",
        ] {
            assert!(
                download(
                    &module,
                    request(directory.path(), mode),
                    Arc::new(AtomicBool::new(false)),
                    |_| {}
                )
                .await
                .is_err(),
                "{mode}"
            );
        }
        for files in [vec![], vec![99]] {
            let mut request = request(directory.path(), "normal");
            request.files = Some(files);
            assert!(
                download(&module, request, Arc::new(AtomicBool::new(false)), |_| {})
                    .await
                    .is_err()
            );
        }
    }
}
