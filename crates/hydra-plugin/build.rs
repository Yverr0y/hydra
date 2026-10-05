fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = root.join("../../Cargo.toml");
    let staged = root.join("product-version.txt");
    println!("cargo:rerun-if-changed={}", workspace.display());
    println!("cargo:rerun-if-changed={}", staged.display());
    let version = std::fs::read_to_string(workspace)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|manifest| {
            manifest
                .get("workspace")?
                .get("package")?
                .get("version")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            std::fs::read_to_string(staged).expect("packaged Hydra product version")
        });
    println!("cargo:rustc-env=HYDRA_PRODUCT_VERSION={}", version.trim());
    println!("cargo:rerun-if-env-changed=HYDRA_OFFICIAL_PLUGIN_DIR");
    let bundle = std::env::var_os("HYDRA_OFFICIAL_PLUGIN_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("../../plugins/bundled"));
    println!("cargo:rerun-if-changed={}", bundle.display());
    let mut packages = Vec::new();
    if bundle.exists() {
        for entry in std::fs::read_dir(&bundle).expect("read official plugin bundle") {
            let path = entry.expect("official plugin entry").path();
            if path.extension().is_some_and(|ext| ext == "hyaplugin") {
                packages.push(std::fs::canonicalize(path).expect("official plugin path"));
            }
        }
    }
    if std::env::var_os("HYDRA_OFFICIAL_PLUGIN_DIR").is_some() {
        assert!(!packages.is_empty(), "official plugin bundle is empty");
    }
    packages.sort();
    let mut source = String::from("pub(crate) const PACKAGES: &[&[u8]] = &[\n");
    for path in packages {
        println!("cargo:rerun-if-changed={}", path.display());
        source.push_str(&format!(
            "include_bytes!({:?}),\n",
            path.to_str().expect("UTF-8 package path")
        ));
    }
    source.push_str("];\n");
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    std::fs::write(output.join("official_plugins.rs"), source)
        .expect("write official plugin bundle");
}
