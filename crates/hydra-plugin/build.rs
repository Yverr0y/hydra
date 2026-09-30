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
}
