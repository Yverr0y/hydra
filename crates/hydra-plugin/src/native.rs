//! Permission-checked native library loading and the synchronous version-one ABI.

use std::ffi::c_void;
use std::path::PathBuf;

use hya_plugin_api::{ErrorCode, PluginError};

/// A library pinned to the bytes approved during plugin installation.
#[derive(Debug, Clone)]
pub struct Module {
    pub path: PathBuf,
    pub sha256: String,
}

/// The native module platform key used in plugin manifests.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

type Emit = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32;
type Poll = unsafe extern "C" fn(*mut c_void, *mut u64) -> i32;
type Entry =
    unsafe extern "C" fn(*const u8, usize, *const u8, usize, Emit, Poll, *mut c_void) -> i32;

struct Context<E, P> {
    emit: E,
    poll: P,
    error: Option<String>,
    limit: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

unsafe extern "C" fn emit<E, P>(context: *mut c_void, bytes: *const u8, len: usize) -> i32
where
    E: FnMut(&[u8]) -> Result<(), String>,
    P: FnMut() -> bool,
{
    // SAFETY: call passes this live Context; the ABI permits callbacks only during that call.
    let context = unsafe { &mut *context.cast::<Context<E, P>>() };
    if context.error.is_some() {
        return 1;
    }
    if bytes.is_null() || len > hya_plugin_api::limits::MAX_PAYLOAD {
        context.error = Some("native reply is null or too large".into());
        return 1;
    }
    // SAFETY: approved native modules promise a readable buffer of len bytes until emit returns.
    let bytes = unsafe { std::slice::from_raw_parts(bytes, len) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (context.emit)(bytes))) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            context.error = Some(error);
            1
        }
        Err(_) => {
            context.error = Some("native reply callback panicked".into());
            1
        }
    }
}

unsafe extern "C" fn poll<E, P>(context: *mut c_void, limit: *mut u64) -> i32
where
    E: FnMut(&[u8]) -> Result<(), String>,
    P: FnMut() -> bool,
{
    // SAFETY: call passes this live Context and callbacks are synchronous on the calling thread.
    let context = unsafe { &mut *context.cast::<Context<E, P>>() };
    if context.error.is_some() || limit.is_null() {
        return 1;
    }
    // SAFETY: native ABI 1 supplies a writable, aligned u64 for the duration of poll.
    unsafe {
        *limit = context.limit.as_ref().map_or(u64::MAX, |limit| {
            limit.load(std::sync::atomic::Ordering::Relaxed)
        });
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (context.poll)())) {
        Ok(cancelled) => i32::from(cancelled),
        Err(_) => {
            context.error = Some("native cancellation callback panicked".into());
            1
        }
    }
}

impl Module {
    /// Calls a native module after verifying its installation hash.
    /// # Errors
    /// Returns an error for changed code, missing ABI symbols or rejected callbacks.
    pub fn call(
        &self,
        method: &str,
        request: &[u8],
        emit_reply: impl FnMut(&[u8]) -> Result<(), String>,
        cancelled: impl FnMut() -> bool,
    ) -> Result<(), PluginError> {
        self.call_with_limit(method, request, emit_reply, cancelled, None)
    }

    pub(crate) fn call_with_limit(
        &self,
        method: &str,
        request: &[u8],
        emit_reply: impl FnMut(&[u8]) -> Result<(), String>,
        cancelled: impl FnMut() -> bool,
        limit: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> Result<(), PluginError> {
        let error = |message: String| PluginError::new(ErrorCode::Internal, message);
        let bytes = std::fs::read(&self.path).map_err(|e| error(e.to_string()))?;
        if crate::package::sha256_hex(&bytes) != self.sha256 {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "native module changed; reinstall it to review the new code",
            ));
        }
        // SAFETY: the user granted native execution; package hashes pin this library and its initializers.
        let library =
            unsafe { libloading::Library::new(&self.path) }.map_err(|e| error(e.to_string()))?;
        // SAFETY: native ABI 1 requires this exact C signature and synchronous callbacks.
        let entry: libloading::Symbol<'_, Entry> =
            unsafe { library.get(b"hydra_native_v1\0") }.map_err(|e| error(e.to_string()))?;
        let mut context = Context {
            emit: emit_reply,
            poll: cancelled,
            error: None,
            limit,
        };
        let status = invoke(*entry, method, request, &mut context);
        if let Some(message) = context.error {
            return Err(error(message));
        }
        if status != 0 {
            return Err(error(format!("native module failed with status {status}")));
        }
        Ok(())
    }
}

fn invoke<E, P>(entry: Entry, method: &str, request: &[u8], context: &mut Context<E, P>) -> i32
where
    E: FnMut(&[u8]) -> Result<(), String>,
    P: FnMut() -> bool,
{
    // SAFETY: buffers and Context remain live throughout the synchronous call; callbacks match ABI 1.
    unsafe {
        entry(
            method.as_ptr(),
            method.len(),
            request.as_ptr(),
            request.len(),
            emit::<E, P>,
            poll::<E, P>,
            (context as *mut Context<E, P>).cast(),
        )
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn fixture() -> Module {
        static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let directory = DIRECTORY.get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("fixture.so");
            let status = std::process::Command::new("cc")
                .args(if cfg!(target_os = "macos") {
                    vec!["-dynamiclib"]
                } else {
                    vec!["-shared", "-fPIC"]
                })
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/native.c"
                ))
                .arg("-o")
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success());
            directory
        });
        let path = directory.path().join("fixture.so");
        Module {
            sha256: crate::package::sha256_hex(&std::fs::read(&path).unwrap()),
            path,
        }
    }

    #[test]
    fn native_calls_keep_borrowed_json_and_callback_state_alive() {
        let mut reply = Vec::new();
        fixture()
            .call(
                "inspect",
                br#"{"value":42}"#,
                |bytes| {
                    reply = bytes.to_vec();
                    Ok(())
                },
                || false,
            )
            .unwrap();
        assert_eq!(reply, br#"{"value":42}"#);
    }

    #[test]
    fn changed_code_and_failed_calls_are_refused() {
        let mut module = fixture();
        module.sha256 = "0".repeat(64);
        assert_eq!(
            module
                .call("inspect", b"{}", |_| Ok(()), || false)
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        assert!(fixture()
            .call("fail", b"{}", |_| Ok(()), || false)
            .unwrap_err()
            .message
            .contains('7'));
    }

    #[test]
    fn null_oversized_replies_and_callback_panics_do_not_cross_the_abi() {
        for request in [
            br#"{"null_reply":true}"#.as_slice(),
            br#"{"oversized":true}"#,
        ] {
            assert!(fixture()
                .call("inspect", request, |_| Ok(()), || false)
                .is_err());
        }
        assert!(fixture()
            .call("inspect", b"{}", |_| panic!("callback"), || false)
            .is_err());
        assert!(fixture()
            .call("download", b"{}", |_| Ok(()), || panic!("poll"))
            .is_err());
    }
}
