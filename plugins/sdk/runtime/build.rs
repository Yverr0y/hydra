use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=HYDRA_PLUGIN_SOURCE");
    let input = PathBuf::from(env::var_os("HYDRA_PLUGIN_SOURCE").expect("set HYDRA_PLUGIN_SOURCE"));
    println!("cargo:rerun-if-changed={}", input.display());
    fs::copy(
        input,
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("plugin.source"),
    )
    .unwrap();
}
