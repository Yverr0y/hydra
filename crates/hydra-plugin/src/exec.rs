//! The `exec` host function: argument templates, program pinning and a
//! bounded, deadline-killed child process.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_plugin_api::limits::MAX_EXEC_OUTPUT;
use hya_plugin_api::manifest::is_refused_program;
use hya_plugin_api::{ErrorCode, ExecEntry, PluginError};
use serde::{Deserialize, Serialize};

use crate::package::sha256_hex;
use crate::runtime::CallCtl;

pub const COOKIE_FILE: &str = "{cookie_file}";
pub const DATA_DIR: &str = "{data}";
const STDERR_TAIL: usize = 2048;
const KEPT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SYSTEMROOT",
];

/// A resolved program as it was when the user approved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub sha256: String,
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn candidates(name: &str) -> Vec<String> {
    if cfg!(windows) && !name.contains('.') {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    }
}

/// Finds `name` in `dirs`, in order.
pub fn find_program(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .flat_map(|d| candidates(name).into_iter().map(move |c| d.join(c)))
        .find(|p| is_executable(p))
}

/// Directories searched for a program: an explicit setting, `PATH`, then the
/// well-known tool locations.
pub fn search_dirs(setting_path: Option<&Path>) -> Vec<PathBuf> {
    search_dirs_with_path(setting_path, std::env::var_os("PATH").as_deref())
}

fn search_dirs_with_path(setting_path: Option<&Path>, path: Option<&OsStr>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = setting_path {
        match p.parent() {
            Some(parent) if p.is_file() => dirs.push(parent.to_path_buf()),
            _ => dirs.push(p.to_path_buf()),
        }
    }
    if let Some(path) = path {
        dirs.extend(std::env::split_paths(&path));
    }
    for extra in [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/usr/bin",
        "/snap/bin",
    ] {
        dirs.push(PathBuf::from(extra));
    }
    dirs
}

/// Resolves and pins a program for a grant.
///
/// # Errors
/// `tool_missing` when it cannot be found or read.
pub fn pin(name: &str, dirs: &[PathBuf]) -> Result<Pin, PluginError> {
    if is_refused_program(name) {
        return Err(PluginError::new(
            ErrorCode::PermissionDenied,
            format!("`{name}` is a shell or interpreter"),
        ));
    }
    let path = find_program(name, dirs).ok_or_else(|| {
        PluginError::new(ErrorCode::ToolMissing, format!("`{name}` was not found"))
    })?;
    pin_path(path)
}

fn pin_path(path: PathBuf) -> Result<Pin, PluginError> {
    let missing = |e: std::io::Error| {
        PluginError::new(ErrorCode::ToolMissing, format!("{}: {e}", path.display()))
    };
    let meta = std::fs::metadata(&path).map_err(missing)?;
    let bytes = std::fs::read(&path).map_err(missing)?;
    Ok(Pin {
        size: meta.len(),
        mtime: mtime_of(&meta),
        sha256: sha256_hex(&bytes),
        path,
    })
}

/// Confirms the file on disk is still the one that was approved.
///
/// # Errors
/// `tool_missing` when it is gone or its hash changed.
pub fn verify(pin: &Pin) -> Result<(), PluginError> {
    let gone = |e: std::io::Error| {
        PluginError::new(
            ErrorCode::ToolMissing,
            format!("{}: {e}", pin.path.display()),
        )
    };
    let bytes = std::fs::read(&pin.path).map_err(gone)?;
    if sha256_hex(&bytes) == pin.sha256 {
        return Ok(());
    }
    Err(PluginError::new(
        ErrorCode::ToolMissing,
        format!(
            "`{}` changed since you approved it — approve again",
            pin.path.display()
        ),
    ))
}

/// Whether `args` satisfies `template`: literals match themselves, `*` one
/// argument not starting with `-`, `**` the rest.
pub fn matches_template(template: &[String], args: &[String]) -> bool {
    let mut i = 0;
    for (t, tpl) in template.iter().enumerate() {
        match tpl.as_str() {
            "**" => return t == template.len() - 1,
            "*" => match args.get(i) {
                Some(a) if !a.starts_with('-') => i += 1,
                _ => return false,
            },
            lit => {
                if args.get(i).map(String::as_str) != Some(lit) {
                    return false;
                }
                i += 1;
            }
        }
    }
    i == args.len()
}

/// Finds the first declared entry for `program` whose template admits `args`.
pub fn admit<'a>(
    entries: &'a [ExecEntry],
    program: &str,
    args: &[String],
) -> Option<&'a ExecEntry> {
    entries
        .iter()
        .find(|e| e.program == program && matches_template(&e.args, args))
}

/// Captured result of a finished child.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExecOutput {
    pub status: i32,
    pub stdout_b64: String,
    pub stderr: String,
}

fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let mut over = false;
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = MAX_EXEC_OUTPUT.saturating_sub(out.len());
                    if n > room {
                        over = true;
                    }
                    out.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
        (out, over)
    })
}

/// Runs a pinned program with a scrubbed environment, killing it at the
/// call's deadline or on cancel.
///
/// # Errors
/// `tool_missing`, `tool_failed` (with a stderr tail), `deadline`, `cancelled`.
pub fn run(
    pin: &Pin,
    args: &[String],
    cwd: &Path,
    ctl: &Arc<CallCtl>,
    secrets: &[String],
) -> Result<ExecOutput, PluginError> {
    run_with_proxy(pin, args, cwd, ctl, secrets, None)
}

pub(crate) fn run_with_proxy(
    pin: &Pin,
    args: &[String],
    cwd: &Path,
    ctl: &Arc<CallCtl>,
    secrets: &[String],
    proxy: Option<&hya_net::Proxy>,
) -> Result<ExecOutput, PluginError> {
    verify(pin)?;
    let mut cmd = Command::new(&pin.path);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in KEPT_ENV {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    // Finder's PATH can omit Homebrew even when the approved tool was found there.
    let path = std::env::join_paths(search_dirs(None))
        .map_err(|e| PluginError::new(ErrorCode::Internal, format!("tool PATH: {e}")))?;
    cmd.env("PATH", path);
    let mut redactions = secrets.to_vec();
    if let Some(proxy) = proxy {
        let mut address = url::Url::parse(&format!(
            "{}://{}:{}",
            proxy.kind.as_str(),
            proxy.host,
            proxy.port
        ))
        .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e.to_string()))?;
        if let Some(user) = &proxy.username {
            let _ = address.set_username(user);
        }
        if let Some(password) = &proxy.password {
            let _ = address.set_password(Some(password));
            redactions.push(password.clone());
        }
        let address = address.to_string();
        for key in ["http_proxy", "https_proxy", "all_proxy"] {
            cmd.env(key, &address);
        }
        redactions.push(address);
    }
    let secrets = &redactions;
    let mut child = cmd.spawn().map_err(|e| {
        PluginError::new(
            ErrorCode::ToolMissing,
            format!("{}: {e}", pin.path.display()),
        )
    })?;
    let out = drain(child.stdout.take().expect("piped"));
    let err = drain(child.stderr.take().expect("piped"));

    let status = loop {
        if let Err(e) = ctl.check() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = (out.join(), err.join());
            return Err(e);
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                return Err(PluginError::new(ErrorCode::Internal, format!("wait: {e}")));
            }
        }
    };
    let (stdout, over_out) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    if over_out {
        return Err(PluginError::new(
            ErrorCode::ToolFailed,
            "program output is over 16 MiB",
        ));
    }
    let mut stderr = redact(&String::from_utf8_lossy(&stderr), secrets);
    if stderr.len() > STDERR_TAIL {
        let cut = stderr.len() - STDERR_TAIL;
        let cut = (cut..stderr.len())
            .find(|&i| stderr.is_char_boundary(i))
            .unwrap_or(cut);
        stderr = stderr[cut..].to_string();
    }
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        return Err(PluginError::new(
            ErrorCode::ToolFailed,
            format!("exited with {code}: {}", stderr.trim()),
        ));
    }
    let _ = SystemTime::now();
    Ok(ExecOutput {
        status: code,
        stdout_b64: hya_net::base64::encode(&redact_bytes(&stdout, secrets)),
        stderr,
    })
}

/// Replaces every secret's exact value with `***`.
pub fn redact(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(text.to_string(), |t, s| t.replace(s.as_str(), "***"))
}

fn redact_bytes(bytes: &[u8], secrets: &[String]) -> Vec<u8> {
    if secrets.iter().all(String::is_empty) {
        return bytes.to_vec();
    }
    redact(&String::from_utf8_lossy(bytes), secrets).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn finder_path_includes_homebrew_for_child_runtime_discovery() {
        let inherited = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let dirs = search_dirs_with_path(None, Some(&inherited));
        assert_eq!(
            &dirs[..2],
            &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        assert!(std::env::join_paths(&dirs).is_ok());
    }

    #[test]
    fn absent_path_still_searches_fallback_tool_folders() {
        let dirs = search_dirs_with_path(None, None);
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        assert!(std::env::join_paths(&dirs).is_ok());
    }

    #[test]
    fn templates_match_element_for_element() {
        assert!(matches_template(
            &s(&["-J", "--no-download", "*"]),
            &s(&["-J", "--no-download", "https://x"])
        ));
        assert!(!matches_template(&s(&["-J", "*"]), &s(&["-J"])));
        assert!(!matches_template(&s(&["-J", "*"]), &s(&["-J", "a", "b"])));
        assert!(!matches_template(&s(&["-J", "*"]), &s(&["-J", "-evil"])));
        assert!(!matches_template(&s(&["-J"]), &s(&["--other"])));
        assert!(matches_template(&s(&[]), &s(&[])));
        assert!(!matches_template(&s(&[]), &s(&["x"])));
    }

    #[test]
    fn double_star_takes_the_rest_but_only_last() {
        assert!(matches_template(&s(&["a", "**"]), &s(&["a"])));
        assert!(matches_template(&s(&["a", "**"]), &s(&["a", "-x", "y"])));
        assert!(!matches_template(&s(&["**", "a"]), &s(&["a"])));
    }

    #[test]
    fn admit_picks_the_entry_by_program_and_template() {
        let entries = vec![
            ExecEntry {
                program: "tool".into(),
                args: s(&["--version"]),
            },
            ExecEntry {
                program: "tool".into(),
                args: s(&["get", "*"]),
            },
        ];
        assert!(admit(&entries, "tool", &s(&["get", "u"])).is_some());
        assert!(admit(&entries, "tool", &s(&["rm", "u"])).is_none());
        assert!(admit(&entries, "other", &s(&["--version"])).is_none());
    }

    #[test]
    fn redaction_replaces_exact_values() {
        assert_eq!(redact("pw=hunter2 ok", &s(&["hunter2"])), "pw=*** ok");
        assert_eq!(redact("abc", &s(&[""])), "abc");
    }

    #[test]
    fn refused_programs_are_never_pinned() {
        assert_eq!(
            pin("bash", &search_dirs(None)).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            pin("definitely-not-a-real-tool-xyz", &search_dirs(None))
                .unwrap_err()
                .code,
            ErrorCode::ToolMissing
        );
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }

        fn tmp(tag: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("hya-exec-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        fn ctl(secs: u64) -> Arc<CallCtl> {
            CallCtl::new(Duration::from_secs(secs))
        }

        #[test]
        fn runs_captures_and_scrubs_the_environment() {
            let d = tmp("run");
            script(
                &d,
                "t",
                "echo out; echo err >&2; echo \"s=$SECRET_VAR h=$HOME\"",
            );
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            std::env::set_var("SECRET_VAR", "leak");
            let out = run(&pinned, &[], &d, &ctl(10), &[]).unwrap();
            let body =
                String::from_utf8(hya_net::base64::decode(&out.stdout_b64).unwrap()).unwrap();
            assert!(body.contains("out"));
            assert!(body.contains("s= "), "{body}");
            assert_eq!(out.stderr.trim(), "err");
        }

        #[test]
        fn child_path_keeps_inherited_priority_and_adds_tool_folders() {
            let dir = tmp("child-path");
            script(&dir, "t", "printf '%s' \"$PATH\"");
            let pinned = pin("t", std::slice::from_ref(&dir)).unwrap();
            let output = run(&pinned, &[], &dir, &ctl(10), &[]).unwrap();
            let path =
                String::from_utf8(hya_net::base64::decode(&output.stdout_b64).unwrap()).unwrap();
            let dirs: Vec<_> = std::env::split_paths(&path).collect();
            if let Some(inherited) = std::env::var_os("PATH") {
                let inherited: Vec<_> = std::env::split_paths(&inherited).collect();
                assert!(dirs.starts_with(&inherited));
            }
            assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
            assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        }

        #[test]
        fn explicit_proxy_reaches_backend_and_credentials_are_redacted() {
            let dir = tmp("proxy");
            script(&dir,"t","test -n \"$https_proxy\" || exit 2; echo \"$https_proxy\" >&2; echo \"$http_proxy\"");
            let pinned = pin("t", std::slice::from_ref(&dir)).unwrap();
            let proxy = hya_net::Proxy::parse("http://user:private@127.0.0.1:1234").unwrap();
            let output = run_with_proxy(&pinned, &[], &dir, &ctl(10), &[], Some(&proxy)).unwrap();
            assert!(!output.stderr.contains("private"));
            assert!(
                !String::from_utf8(hya_net::base64::decode(&output.stdout_b64).unwrap())
                    .unwrap()
                    .contains("private")
            );
        }
        #[test]
        fn nonzero_exit_is_tool_failed_with_a_redacted_tail() {
            let d = tmp("fail");
            script(&d, "t", "echo oops-sekret >&2; exit 3");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let e = run(&pinned, &[], &d, &ctl(10), &["sekret".into()]).unwrap_err();
            assert_eq!(e.code, ErrorCode::ToolFailed);
            assert!(
                e.message.contains("exited with 3") && e.message.contains("oops-***"),
                "{}",
                e.message
            );
        }

        #[test]
        fn a_child_past_the_deadline_is_killed() {
            let d = tmp("slow");
            script(&d, "t", "sleep 30");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let started = std::time::Instant::now();
            let e = run(
                &pinned,
                &[],
                &d,
                &CallCtl::new(Duration::from_millis(150)),
                &[],
            )
            .unwrap_err();
            assert_eq!(e.code, ErrorCode::Deadline);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn cancel_kills_the_child() {
            let d = tmp("cancel");
            script(&d, "t", "sleep 30");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            let c = ctl(60);
            let c2 = c.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                c2.cancel();
            });
            assert_eq!(
                run(&pinned, &[], &d, &c, &[]).unwrap_err().code,
                ErrorCode::Cancelled
            );
        }

        #[test]
        fn a_changed_binary_is_refused_until_repinned() {
            let d = tmp("pin");
            let p = script(&d, "t", "echo one");
            let pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            verify(&pinned).unwrap();
            script(&d, "t", "echo two-two-two");
            let e = run(&pinned, &[], &d, &ctl(10), &[]).unwrap_err();
            assert_eq!(e.code, ErrorCode::ToolMissing);
            assert!(e.message.contains("approve again"));
            let again = pin_path(p).unwrap();
            assert!(run(&again, &[], &d, &ctl(10), &[]).is_ok());
        }

        #[test]
        fn a_touched_but_identical_binary_still_passes() {
            let d = tmp("touch");
            let p = script(&d, "t", "echo one");
            let mut pinned = pin("t", std::slice::from_ref(&d)).unwrap();
            pinned.mtime += 100;
            verify(&pinned).unwrap();
            pinned.sha256 = "0".repeat(64);
            assert!(verify(&pinned).is_err());
            std::fs::remove_file(p).unwrap();
            assert_eq!(verify(&pinned).unwrap_err().code, ErrorCode::ToolMissing);
        }

        #[test]
        fn search_prefers_the_setting_directory() {
            let d = tmp("search");
            script(&d, "mytool", "true");
            let dirs = search_dirs(Some(&d));
            assert_eq!(find_program("mytool", &dirs), Some(d.join("mytool")));
            assert_eq!(search_dirs(Some(&d.join("mytool")))[0], d);
        }
    }
}
