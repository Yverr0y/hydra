//! Authoring commands exercised through the executable with isolated projects.
use std::path::Path;
use std::process::Command;

fn run(root: &Path, args: &[&str], success: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hydra-plugin"));
    command.current_dir(root).args(args);
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        // Wasm guests have no LLVM profiler runtime; coverage belongs to the native tool.
        for variable in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            command.env_remove(variable);
        }
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn init_pack_validate_and_reject_invalid_input() {
    let root = tempfile::tempdir().unwrap();
    for language in ["rust", "python", "nodejs", "c", "go"] {
        run(
            root.path(),
            &["init", language, "--language", language],
            true,
        );
        run(root.path(), &["init", language], false);
    }
    run(root.path(), &["init"], false);
    run(root.path(), &["init", "bad", "--name", ""], false);
    run(root.path(), &["init", "bad", "--id", "invalid"], false);
    run(
        root.path(),
        &["init", "bad", "--plugin-version", "invalid"],
        false,
    );
    run(
        root.path(),
        &[
            "init",
            "custom",
            "--id",
            "publisher.custom",
            "--plugin-version",
            "1.2.3",
            "--author",
            "Plugin Author",
        ],
        true,
    );
    let metadata = std::fs::read_to_string(root.path().join("custom/hydra-plugin.toml")).unwrap();
    assert!(metadata.contains("publisher.custom"));
    assert!(metadata.contains("Plugin Author"));
    assert!(metadata.contains("1.2.3"));
    assert!(
        std::fs::read_to_string(root.path().join("custom/Cargo.toml"))
            .unwrap()
            .contains("version = \"1.2.3\"")
    );
    assert!(!root.path().join("bad").exists());
    let module = wat::parse_str(
        r#"(module
        (memory (export "memory") 1)
        (func (export "hydra_api") (result i32) i32.const 1)
        (func (export "hydra_alloc") (param i32) (result i32) i32.const 0)
        (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const 0))"#,
    )
    .unwrap();
    std::fs::write(root.path().join("c/plugin.wasm"), module).unwrap();
    run(root.path(), &["validate", "c"], true);
    run(
        root.path(),
        &["pack", "c", "--output", "sample.hyaplugin"],
        true,
    );
    assert!(run(root.path(), &["validate", "sample.hyaplugin"], true).contains("example.c"));
    std::fs::write(root.path().join("sample.hyaplugin"), b"corrupt").unwrap();
    run(root.path(), &["validate", "sample.hyaplugin"], false);
    std::fs::write(
        root.path().join("c/plugin.wasm"),
        wat::parse_str("(module)").unwrap(),
    )
    .unwrap();
    run(
        root.path(),
        &["pack", "c", "--output", "rejected.hyaplugin"],
        false,
    );
    assert!(!root.path().join("rejected.hyaplugin").exists());
}

#[test]
#[ignore = "requires rustup target add wasm32-wasip1; exercised by release verification"]
fn fresh_rust_project_builds_and_packages_without_repository_sdk() {
    let root = tempfile::tempdir().unwrap();
    run(
        root.path(),
        &["init", "resolver", "--name", "Youtube test"],
        true,
    );
    let manifest = std::fs::read_to_string(root.path().join("resolver/hydra-plugin.toml")).unwrap();
    assert!(manifest.contains("name = \"Youtube test\""));
    assert!(manifest.contains("example.youtube-test"));
    run(
        root.path(),
        &["build", "resolver", "--output", "built.hyaplugin"],
        true,
    );
    assert!(
        run(root.path(), &["validate", "built.hyaplugin"], true).contains("example.youtube-test")
    );
    run(
        root.path(),
        &["pack", "resolver", "--output", "packed.hyaplugin"],
        true,
    );
    assert_eq!(
        std::fs::read(root.path().join("built.hyaplugin")).unwrap(),
        std::fs::read(root.path().join("packed.hyaplugin")).unwrap()
    );
}
