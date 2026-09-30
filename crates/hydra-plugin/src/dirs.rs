//! Shared application directory across plugin hosts.
use std::path::PathBuf;

/// Resolves Hydra's application directory, preserving existing platform defaults.
pub fn hydra_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("HYDRA_CONFIG_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".config");
    base.join("hydra")
}
