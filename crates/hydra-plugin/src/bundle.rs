//! Build-time selection of portable and target-specific official packages.

use std::path::{Path, PathBuf};

pub(crate) fn directories(root: &Path, platform: &str, target_env: &str) -> Vec<PathBuf> {
    let mut directories = vec![root.to_path_buf()];
    if target_env != "musl" {
        directories.push(root.join("native").join(platform));
    }
    directories
}
