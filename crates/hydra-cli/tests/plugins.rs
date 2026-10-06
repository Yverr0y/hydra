//! Black-box plugin lifecycle and transfer through the shipped command line.
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn run(root: &Path, args: &[&str], success: bool) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_hydra"))
        .args(args)
        .env("HYDRA_CONFIG_DIR", root.join("profile"))
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn fixture(path: &Path, plan: serde_json::Value) {
    let reply = serde_json::to_string(&serde_json::json!({"plan":plan})).unwrap();
    let data: String = reply.bytes().map(|b| format!("\\{b:02x}")).collect();
    let module = wat::parse_str(format!(r#"(module
      (memory (export "memory") 4)
      (global $heap (mut i32) (i32.const 10000))
      (data (i32.const 0) "null")
      (data (i32.const 128) "{data}")
      (func (export "hydra_api") (result i32) i32.const 1)
      (func (export "hydra_alloc") (param $n i32) (result i32)
        (local $p i32) global.get $heap local.set $p global.get $heap local.get $n i32.add global.set $heap local.get $p)
      (func (export "hydra_call") (param i32 i32 i32 i32) (result i64)
        local.get 1 i32.const 5 i32.eq
        if (result i64) i64.const 4 else i64.const {} end))"#,hya_plugin_api::abi::pack(128,reply.len() as u32))).unwrap();
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("plugin.wasm"), module).unwrap();
    std::fs::write(
        path.join("hydra-plugin.toml"),
        r#"id="test.cli"
name="CLI test"
author="Test Author"
version="1.0.0"
api=1
module="plugin.wasm"
hooks=["resolve","check"]
claims=["https://example.com/*"]
[permissions]
sources=["127.0.0.1"]
[[settings]]
key="enabled"
label="Enabled"
type="boolean"
default=false
[[settings]]
key="quality"
label="Quality"
type="dropdown"
options=["best","small"]
default="best"
"#,
    )
    .unwrap();
}
#[test]
fn cli_package_consent_settings_sidecars_permissions_and_removal() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            };
            std::thread::spawn(move || {
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                loop {
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while !request.ends_with(b"\r\n\r\n") {
                        if socket.read(&mut byte).unwrap_or(0) == 0 {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    if request.is_empty() {
                        break;
                    }
                    let text = String::from_utf8_lossy(&request);
                    assert!(!text.to_lowercase().contains("\r\nrange:"));
                    let body = if text.contains("/sub ") {
                        b"WEBVTT\n".as_slice()
                    } else {
                        b"plugin payload\n".as_slice()
                    };
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    if !text.starts_with("HEAD ") {
                        let _ = socket.write_all(body);
                    }
                    if text.to_ascii_lowercase().contains("connection: close") {
                        break;
                    }
                }
            });
        }
    });
    let package = root.path().join("package");
    fixture(
        &package,
        serde_json::json!({"id":"fixture","title":"Plugin file","tracks":[
            {"id":"file","kind":"file","container":"txt","ranges":"deny","sources":[{"url":format!("http://{address}/file")}]},
            {"id":"sub","kind":"subtitle","language":"en","container":"vtt","ranges":"deny","sources":[{"url":format!("http://{address}/sub")}]}
        ]}),
    );
    run(root.path(), &["plugin", "inspect", "package"], true);
    run(
        root.path(),
        &["plugin", "pack", "package", "test.hyaplugin"],
        true,
    );
    run(root.path(), &["plugin", "install", "test.hyaplugin"], false);
    assert_eq!(
        run(root.path(), &["plugin", "list", "--json"], true).trim(),
        "[]"
    );
    run(
        root.path(),
        &[
            "plugin",
            "install",
            "test.hyaplugin",
            "--accept-permissions",
        ],
        true,
    );
    let table = run(root.path(), &["plugin", "list"], true);
    assert_eq!(table.lines().count(), 2);
    for value in [
        "NAME",
        "ID",
        "AUTHOR",
        "SIGNED",
        "No",
        "VERSION",
        "CLI test",
        "test.cli",
        "Test Author",
        "1.0.0",
    ] {
        assert!(table.contains(value), "{table}");
    }
    let listed: serde_json::Value =
        serde_json::from_str(&run(root.path(), &["plugin", "list", "--json"], true)).unwrap();
    assert_eq!(listed[0]["manifest"]["author"], "Test Author");
    run(root.path(), &["plugin", "check", "test.cli"], true);
    run(
        root.path(),
        &["plugin", "set", "test.cli", "enabled", "true"],
        true,
    );
    run(
        root.path(),
        &["plugin", "set", "test.cli", "quality", "invalid"],
        false,
    );
    let info: serde_json::Value = serde_json::from_str(&run(
        root.path(),
        &["plugin", "info", "test.cli", "--json"],
        true,
    ))
    .unwrap();
    assert_eq!(info["settings"]["enabled"], true);
    run(root.path(), &["plugin", "logs", "test.cli"], true);
    run(root.path(), &["plugin", "order", "test.cli"], true);
    run(
        root.path(),
        &["plugin", "resolve", "https://example.com/file"],
        true,
    );
    run(
        root.path(),
        &[
            "--no-proxy",
            "--subs",
            "en",
            "-O",
            "result.txt",
            "https://example.com/file",
        ],
        true,
    );
    assert_eq!(
        std::fs::read(root.path().join("result.txt")).unwrap(),
        b"plugin payload\n"
    );
    assert_eq!(
        std::fs::read(root.path().join("result.txt.track-1.vtt")).unwrap(),
        b"WEBVTT\n"
    );
    run(
        root.path(),
        &["--no-proxy", "-O", "result.txt", "https://example.com/file"],
        false,
    );
    run(
        root.path(),
        &["plugin", "revoke", "test.cli", "sources:127.0.0.1"],
        true,
    );
    run(
        root.path(),
        &["plugin", "resolve", "https://example.com/file"],
        false,
    );
    run(
        root.path(),
        &["plugin", "grant", "test.cli", "sources:127.0.0.1"],
        true,
    );
    run(root.path(), &["plugin", "disable", "test.cli"], true);
    run(
        root.path(),
        &[
            "--plugin",
            "test.cli",
            "--list-tracks",
            "https://example.com/file",
        ],
        false,
    );
    run(root.path(), &["plugin", "enable", "test.cli"], true);
    run(root.path(), &["plugin", "clear-session", "test.cli"], true);
    run(root.path(), &["plugin", "remove", "test.cli"], true);
    assert_eq!(
        run(root.path(), &["plugin", "list", "--json"], true).trim(),
        "[]"
    );
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}

#[test]
fn forbidden_source_refreshes_once_and_downloads_new_transport() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let old = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let old_count = old.clone();
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            };
            let old_count = old_count.clone();
            std::thread::spawn(move || {
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                loop {
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while !request.ends_with(b"\r\n\r\n") {
                        if socket.read(&mut byte).unwrap_or(0) == 0 {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    if request.is_empty() {
                        break;
                    }
                    let text = String::from_utf8_lossy(&request);
                    assert!(!text.to_lowercase().contains("\r\nrange:"));
                    let expired = text.contains("/old ");
                    if expired {
                        old_count.fetch_add(1, Ordering::Relaxed);
                    }
                    let body = if expired {
                        b"".as_slice()
                    } else {
                        b"refreshed bytes".as_slice()
                    };
                    write!(
                        socket,
                        "HTTP/1.1 {}\r\nContent-Length: {}\r\n\r\n",
                        if expired { "403 Forbidden" } else { "200 OK" },
                        body.len()
                    )
                    .unwrap();
                    if !text.starts_with("HEAD ") {
                        let _ = socket.write_all(body);
                    }
                }
            });
        }
    });
    let package = root.path().join("package");
    let plan = serde_json::json!({"id":"same","tracks":[{"id":"file","kind":"file","ranges":"deny","sources":[{"url":format!("http://{address}/old")}]}]});
    fixture(&package, plan.clone());
    let manifest = package.join("hydra-plugin.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("\"resolve\",\"check\"", "\"resolve\",\"check\",\"refresh\""),
    )
    .unwrap();
    let resolve = serde_json::to_vec(&serde_json::json!({"plan":plan})).unwrap();
    let refresh=serde_json::to_vec(&serde_json::json!({"tracks":[{"id":"file","sources":[{"url":format!("http://{address}/new")}]}]})).unwrap();
    let encode = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>()
    };
    let module=wat::parse_str(format!(r#"(module
        (memory (export "memory") 4)
        (data (i32.const 128) "{}") (data (i32.const 4096) "{}")
        (global $heap (mut i32) (i32.const 10000))
        (func (export "hydra_api") (result i32) i32.const 1)
        (func (export "hydra_alloc") (param $n i32) (result i32) (local $p i32) global.get $heap local.set $p global.get $heap local.get $n i32.add global.set $heap local.get $p)
        (func (export "hydra_call") (param $method i32) (param i32 i32 i32) (result i64)
            local.get $method i32.const 2 i32.add i32.load8_u i32.const 102 i32.eq
            if (result i64) i64.const {} else i64.const {} end))"#,encode(&resolve),encode(&refresh),hya_plugin_api::abi::pack(4096,refresh.len() as u32),hya_plugin_api::abi::pack(128,resolve.len() as u32))).unwrap();
    std::fs::write(package.join("plugin.wasm"), module).unwrap();
    run(
        root.path(),
        &["plugin", "install", "package", "--accept-permissions"],
        true,
    );
    run(
        root.path(),
        &["--no-proxy", "-O", "fresh.bin", "https://example.com/file"],
        true,
    );
    assert_eq!(
        std::fs::read(root.path().join("fresh.bin")).unwrap(),
        b"refreshed bytes"
    );
    assert!(old.load(Ordering::Relaxed) > 0);
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".hydra-plugin-")));
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}

#[test]
fn plugin_table_handles_empty_profiles_missing_authors_and_control_characters() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        run(root.path(), &["plugin", "list"], true).trim(),
        "No plugins installed."
    );
    let package = root.path().join("package");
    fixture(&package, serde_json::json!({"id":"fixture","tracks":[]}));
    let path = package.join("hydra-plugin.toml");
    let manifest = std::fs::read_to_string(&path)
        .unwrap()
        .replace("author=\"Test Author\"\n", "")
        .replace("CLI test", "CLI\\u001b test");
    std::fs::write(path, manifest).unwrap();
    run(
        root.path(),
        &["plugin", "install", "package", "--accept-permissions"],
        true,
    );
    let table = run(root.path(), &["plugin", "list"], true);
    assert!(table.contains("CLI test"));
    assert!(table.contains('—'));
    assert!(!table.contains('\x1b'));
}

#[test]
fn official_sync_accepts_source_builds_and_reports_missing_release_bundle() {
    let root = tempfile::tempdir().unwrap();
    run(root.path(), &["plugin", "sync-official"], true);
    run(
        root.path(),
        &["plugin", "sync-official", "--require-bundled"],
        hya_plugin::official::bundled(),
    );
}

#[test]
fn default_catalog_is_listed_and_protected_while_mirrors_remain_removable() {
    let root = tempfile::tempdir().unwrap();
    let default = hya_plugin::distribution::DEFAULT_INDEX_URL;
    let listed = run(root.path(), &["plugin", "index", "list"], true);
    let sources: Vec<hya_plugin::distribution::IndexSource> =
        serde_json::from_str(&listed).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].url, default);
    run(root.path(), &["plugin", "index", "rm", default], false);
    run(
        root.path(),
        &["plugin", "index", "add", default, "--key", "other-key"],
        false,
    );
    let mirror = "https://example.com/plugins.json";
    std::fs::write(
        root.path().join("profile/plugins/indexes.json"),
        serde_json::to_vec(&[hya_plugin::distribution::IndexSource {
            url: mirror.into(),
            key: None,
        }])
        .unwrap(),
    )
    .unwrap();
    let listed = run(root.path(), &["plugin", "index", "list"], true);
    let sources: Vec<hya_plugin::distribution::IndexSource> =
        serde_json::from_str(&listed).unwrap();
    assert_eq!(sources.len(), 2);
    run(root.path(), &["plugin", "index", "rm", mirror], true);
    let listed = run(root.path(), &["plugin", "index", "list"], true);
    let sources: Vec<hya_plugin::distribution::IndexSource> =
        serde_json::from_str(&listed).unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].is_default());
}

#[test]
#[cfg(debug_assertions)]
fn real_cli_installs_and_updates_from_an_isolated_http_catalog() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/plugins/catalog-e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args(["--skip-build", "--cli", env!("CARGO_BIN_EXE_hydra")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS: real CLI HTTP install"));
}
