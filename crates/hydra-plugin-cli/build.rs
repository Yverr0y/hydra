use std::{env, fs, path::Path};
fn collect(root: &Path, path: &Path, entries: &mut Vec<(String, String)>) {
    for item in fs::read_dir(path).unwrap() {
        let path = item.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if matches!(name, "target" | "__pycache__" | ".git") {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, entries);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some(
                "rs" | "toml"
                    | "template"
                    | "lock"
                    | "go"
                    | "mod"
                    | "py"
                    | "js"
                    | "mjs"
                    | "c"
                    | "h"
                    | "md"
            )
        ) {
            println!("cargo:rerun-if-changed={}", path.display());
            entries.push((
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/")
                    .trim_end_matches(".template")
                    .to_owned(),
                path.to_str().unwrap().to_owned(),
            ));
        }
    }
}
fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let staged = manifest.join("assets");
    let workspace = manifest.join("../..");
    let root = if workspace.join("plugins/sdk").is_dir() {
        workspace.canonicalize().unwrap()
    } else {
        staged
    };
    let mut entries = Vec::new();
    for dir in [
        "plugins/sdk",
        "crates/hydra-plugin-sdk",
        "crates/hydra-plugin-api",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
        collect(&root, &root.join(dir), &mut entries);
    }
    entries.sort();
    let mut text = "const ASSETS: &[(&str, &str)] = &[\n".to_owned();
    for (name, path) in entries {
        text += &format!("({name:?}, include_str!({path:?})),\n");
    }
    text += "];\n";
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("assets.rs"),
        text,
    )
    .unwrap();
}
