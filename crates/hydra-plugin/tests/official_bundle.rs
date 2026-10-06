#[path = "../src/bundle.rs"]
mod bundle;

#[test]
fn bundles_portable_plugins_and_only_the_target_native_directory() {
    let root = std::path::Path::new("bundled");
    for os in ["macos", "linux", "windows"] {
        for arch in ["x86_64", "aarch64"] {
            let platform = format!("{os}-{arch}");
            assert_eq!(
                bundle::directories(root, &platform, "gnu"),
                [root.to_path_buf(), root.join("native").join(platform)]
            );
        }
    }
}

#[test]
fn static_musl_hosts_bundle_only_portable_plugins() {
    let root = std::path::Path::new("bundled");
    assert_eq!(
        bundle::directories(root, "linux-x86_64", "musl"),
        [root.to_path_buf()]
    );
}
