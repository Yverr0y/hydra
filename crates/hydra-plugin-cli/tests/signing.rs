//! Signing exercised through the CLI executable with real publisher keys.
use std::{fs, path::Path, process::Command};

use hya_plugin::package;
use minisign::KeyPair;

fn run(root: &Path, args: &[&str], env: &[(&str, &str)], success: bool) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_hydra-plugin"))
        .current_dir(root)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.success(), success, "{args:?}: {text}");
    text
}

fn fixture(root: &Path) -> Vec<u8> {
    let plugin = root.join("plugin");
    fs::create_dir(&plugin).unwrap();
    fs::write(plugin.join("hydra-plugin.toml"), "id = \"test.signing\"\nname = \"Signing test\"\nauthor = \"Test Author\"\nversion = \"0.1.0\"\napi = 1\nmodule = \"plugin.wasm\"\nhooks = [\"resolve\"]\nclaims = [\"https://example.com/*\"]\n[permissions]\nsources = [\"example.com\"]\n").unwrap();
    fs::write(
        plugin.join("plugin.wasm"),
        wat::parse_str(
            r#"(module
        (memory (export "memory") 1)
        (func (export "hydra_api") (result i32) i32.const 1)
        (func (export "hydra_alloc") (param i32) (result i32) i32.const 0)
        (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const 0))"#,
        )
        .unwrap(),
    )
    .unwrap();
    run(
        root,
        &["pack", "plugin", "--output", "unsigned.hyaplugin"],
        &[],
        true,
    );
    fs::read(root.join("unsigned.hyaplugin")).unwrap()
}

fn save_keys(root: &Path, pair: &KeyPair) -> String {
    let secret = pair.sk.to_box(None).unwrap().into_string();
    fs::write(root.join("publisher.key"), &secret).unwrap();
    fs::write(
        root.join("publisher.pub"),
        pair.pk.to_box().unwrap().into_string(),
    )
    .unwrap();
    secret
}

#[test]
fn file_and_environment_signatures_verify_and_preserve_input() {
    let root = tempfile::tempdir().unwrap();
    let original = fixture(root.path());
    let pair = KeyPair::generate_unencrypted_keypair().unwrap();
    let secret = save_keys(root.path(), &pair);
    for source in ["--secret-key", "--key-env"] {
        let key = if source == "--secret-key" {
            "publisher.key"
        } else {
            "TEST_SIGNING_KEY"
        };
        let text = run(
            root.path(),
            &[
                "sign",
                "unsigned.hyaplugin",
                source,
                key,
                "--public-key",
                "publisher.pub",
                "--output",
                "nested/signed.hyaplugin",
            ],
            &[("TEST_SIGNING_KEY", &secret)],
            true,
        );
        assert!(text.contains("Verified"));
        assert!(!text.contains(&secret));
        let signed = package::open(
            &fs::read(root.path().join("nested/signed.hyaplugin")).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(
            signed.manifest.publisher_key.as_deref(),
            Some(pair.pk.to_base64().as_str())
        );
        assert_eq!(
            signed.module,
            package::open(&original, None).unwrap().module
        );
        assert!(run(
            root.path(),
            &["validate", "nested/signed.hyaplugin"],
            &[],
            true
        )
        .contains("Verified"));
    }
    assert_eq!(
        fs::read(root.path().join("unsigned.hyaplugin")).unwrap(),
        original
    );
    assert!(run(
        root.path(),
        &[
            "sign",
            "nested/signed.hyaplugin",
            "--secret-key",
            "publisher.key",
            "--public-key",
            "publisher.pub",
            "--output",
            "resigned.hyaplugin"
        ],
        &[],
        false
    )
    .contains("already declares"));
    assert!(!root.path().join("resigned.hyaplugin").exists());
}

#[test]
fn signing_errors_preserve_existing_output_and_do_not_expose_keys() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let pair = KeyPair::generate_unencrypted_keypair().unwrap();
    let secret = save_keys(root.path(), &pair);
    fs::write(root.path().join("existing.hyaplugin"), b"keep me").unwrap();
    let args = [
        "sign",
        "unsigned.hyaplugin",
        "--key-env",
        "TEST_SIGNING_KEY",
        "--public-key",
        "publisher.pub",
        "--output",
        "existing.hyaplugin",
    ];
    for value in ["", "malformed key", &secret] {
        if value == secret {
            let other = KeyPair::generate_unencrypted_keypair().unwrap();
            fs::write(
                root.path().join("publisher.pub"),
                other.pk.to_box().unwrap().into_string(),
            )
            .unwrap();
        }
        let text = run(root.path(), &args, &[("TEST_SIGNING_KEY", value)], false);
        assert!(!text.contains(&secret));
        assert_eq!(
            fs::read(root.path().join("existing.hyaplugin")).unwrap(),
            b"keep me"
        );
    }
    for (input, output) in [
        ("unsigned.hyaplugin", "unsigned.hyaplugin"),
        ("plugin", "rejected.hyaplugin"),
        ("missing.hyaplugin", "rejected.hyaplugin"),
    ] {
        run(
            root.path(),
            &[
                "sign",
                input,
                "--secret-key",
                "publisher.key",
                "--public-key",
                "publisher.pub",
                "--output",
                output,
            ],
            &[],
            false,
        );
    }
    run(
        root.path(),
        &[
            "sign",
            "unsigned.hyaplugin",
            "--public-key",
            "publisher.pub",
            "--output",
            "rejected.hyaplugin",
        ],
        &[],
        false,
    );
    run(
        root.path(),
        &[
            "sign",
            "unsigned.hyaplugin",
            "--secret-key",
            "publisher.key",
            "--key-env",
            "TEST_SIGNING_KEY",
            "--public-key",
            "publisher.pub",
            "--output",
            "rejected.hyaplugin",
        ],
        &[],
        false,
    );
    assert!(!root.path().join("rejected.hyaplugin").exists());
}

#[test]
fn encrypted_private_keys_require_the_correct_password_environment() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let pair = KeyPair::generate_encrypted_keypair(Some("test passphrase".into())).unwrap();
    save_keys(root.path(), &pair);
    let args = [
        "sign",
        "unsigned.hyaplugin",
        "--secret-key",
        "publisher.key",
        "--public-key",
        "publisher.pub",
        "--password-env",
        "TEST_KEY_PASSWORD",
        "--output",
        "signed.hyaplugin",
    ];
    for password in ["", "wrong passphrase", "test passphrase"] {
        let text = run(
            root.path(),
            &args,
            &[("TEST_KEY_PASSWORD", password)],
            password == "test passphrase",
        );
        assert!(!text.contains("test passphrase"));
    }
    assert!(run(root.path(), &["validate", "signed.hyaplugin"], &[], true).contains("Verified"));
}
