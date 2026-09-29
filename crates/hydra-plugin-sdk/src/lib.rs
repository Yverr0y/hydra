//! Guest SDK for Hydra resolver plugins. Compile with `wasm32-wasip1`.
pub use hya_plugin_api::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
pub use serde_json;
use std::collections::BTreeMap;

/// A host or resolver result.
pub type Result<T> = std::result::Result<T, PluginError>;

/// Implements hooks evaluated in a fresh guest instance for each call.
pub trait Plugin: Default {
    /// Decides whether a claimed address may enter the queue.
    fn enqueue(&mut self, _: &Host, _: ResolveRequest) -> Result<EnqueueDecision> {
        Ok(EnqueueDecision::default())
    }
    /// Observes a completed transfer under the plugin's existing capabilities.
    fn complete(&mut self, _: &Host, _: CompleteRequest) -> Result<()> {
        Ok(())
    }
    /// Optionally transforms an isolated file copy into the supplied output path.
    fn process(&mut self, _: &Host, _: ProcessRequest) -> Result<Processed> {
        Ok(Processed::default())
    }
    /// Resolves an address into tracks or declines the address.
    fn resolve(&mut self, host: &Host, req: ResolveRequest) -> Result<Resolve>;
    /// Checks optional backend prerequisites.
    fn check(&mut self, _: &Host) -> Result<()> {
        Ok(())
    }
    /// Resolves again while preserving selected plan and track identities.
    fn refresh(&mut self, host: &Host, req: RefreshRequest) -> Result<Refreshed> {
        let Resolve::Plan(plan) = self.resolve(
            host,
            ResolveRequest {
                url: req.url,
                ..Default::default()
            },
        )?
        else {
            return Err(PluginError::new(
                ErrorCode::NotClaimed,
                "address is no longer claimed",
            ));
        };
        if plan.id != req.plan_id {
            return Err(PluginError::new(
                ErrorCode::InvalidPlan,
                "plan identity changed",
            ));
        }
        let mut tracks = Vec::new();
        for id in req.track_ids {
            let t = plan.track(&id).ok_or_else(|| {
                PluginError::new(ErrorCode::InvalidPlan, format!("track {id} disappeared"))
            })?;
            tracks.push(RefreshedTrack {
                id,
                sources: t.sources.clone(),
                headers: t.headers.clone(),
                size: t.size,
                digest: t.digest.clone(),
                container: t.container.clone(),
                codec: t.codec.clone(),
                expires_at: t.expires_at,
            });
        }
        Ok(Refreshed { tracks })
    }
}

/// Policy-checked host capabilities.
pub struct Host;
/// Bounded output from an approved executable.
#[derive(Debug, Deserialize)]
pub struct ExecOutput {
    /// Process exit code.
    pub status: i32,
    /// Standard output encoded as base64.
    pub stdout_b64: String,
    /// Redacted standard error.
    pub stderr: String,
}
impl Host {
    /// Calls a capability through the host's policy dispatcher.
    pub fn call<T: DeserializeOwned>(&self, name: &str, request: &impl Serialize) -> Result<T> {
        let bytes = serde_json::to_vec(request).map_err(invalid)?;
        #[cfg(target_arch = "wasm32")]
        let reply = {
            #[link(wasm_import_module = "hydra")]
            extern "C" {
                fn hydra_host_call(np: i32, nl: i32, rp: i32, rl: i32) -> i64;
            }
            // SAFETY: input buffers stay live for the host call; the host copies them.
            let packed = unsafe {
                hydra_host_call(
                    name.as_ptr() as i32,
                    name.len() as i32,
                    bytes.as_ptr() as i32,
                    bytes.len() as i32,
                )
            };
            let (ptr, len) = abi::unpack(packed);
            // SAFETY: the host allocated this exact buffer through hydra_alloc.
            unsafe { Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize) }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (name, bytes);
            Err(PluginError::new(
                ErrorCode::Unsupported,
                "host calls require wasm32",
            ))
        }
        #[cfg(target_arch = "wasm32")]
        decode(&reply)
    }
    /// Runs an approved executable template.
    pub fn exec(&self, program: &str, args: &[String]) -> Result<ExecOutput> {
        self.call("exec", &serde_json::json!({"program":program,"args":args}))
    }
    /// Reads the plugin settings with defaults applied.
    pub fn settings(&self) -> Result<BTreeMap<String, Value>> {
        self.call("settings", &serde_json::json!({}))
    }
    /// Requests typed input from the active frontend.
    pub fn prompt(&self, form: &Form) -> Result<Answers> {
        self.call("prompt", form)
    }
    /// Writes a redacted message to the plugin log.
    pub fn log(&self, level: &str, message: &str) -> Result<serde_json::Value> {
        self.call("log", &serde_json::json!({"level":level,"message":message}))
    }
}
fn invalid(e: impl std::fmt::Display) -> PluginError {
    PluginError::new(ErrorCode::InvalidReply, e.to_string())
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(invalid)?;
    if value.get("error").is_some() {
        return Err(serde_json::from_value::<ErrorReply>(value)
            .map_err(invalid)?
            .error);
    }
    serde_json::from_value(value).map_err(invalid)
}

/// Dispatches a method, also used by native author tests.
pub fn dispatch<P: Plugin>(method: &str, bytes: &[u8]) -> Vec<u8> {
    let mut plugin = P::default();
    let result = (|| -> Result<serde_json::Value> {
        match method {
            "resolve" => {
                serde_json::to_value(plugin.resolve(&Host, decode(bytes)?)?).map_err(invalid)
            }
            "enqueue" => {
                serde_json::to_value(plugin.enqueue(&Host, decode(bytes)?)?).map_err(invalid)
            }
            "complete" => {
                plugin.complete(&Host, decode(bytes)?)?;
                Ok(serde_json::Value::Null)
            }
            "process" => {
                serde_json::to_value(plugin.process(&Host, decode(bytes)?)?).map_err(invalid)
            }
            "refresh" => {
                serde_json::to_value(plugin.refresh(&Host, decode(bytes)?)?).map_err(invalid)
            }
            "check" => {
                plugin.check(&Host)?;
                Ok(serde_json::json!({}))
            }
            "hooks" => Ok(
                serde_json::json!({"api":abi::API_MAJOR,"hooks":["resolve","refresh","check","enqueue","complete","process"]}),
            ),
            _ => Err(PluginError::new(ErrorCode::Unsupported, "unknown method")),
        }
    })();
    match result {
        Ok(value) => serde_json::to_vec(&value).expect("JSON value"),
        Err(error) => serde_json::to_vec(&ErrorReply { error }).expect("error reply"),
    }
}

/// Exports the Hydra API 1 memory and hook interface.
#[macro_export]
macro_rules! export {
    ($plugin:ty) => {
        #[no_mangle]
        pub extern "C" fn hydra_api() -> i32 {
            $crate::abi::API_MAJOR
        }
        #[no_mangle]
        pub extern "C" fn hydra_alloc(len: i32) -> i32 {
            let bytes = vec![0u8; len as u32 as usize].into_boxed_slice();
            Box::into_raw(bytes) as *mut u8 as i32
        }
        #[no_mangle]
        pub unsafe extern "C" fn hydra_call(mp: i32, ml: i32, rp: i32, rl: i32) -> i64 {
            // SAFETY: the host passes buffers from hydra_alloc, consumed exactly once.
            let method = unsafe { Vec::from_raw_parts(mp as *mut u8, ml as usize, ml as usize) };
            // SAFETY: request follows the same allocator ownership contract.
            let request = unsafe { Vec::from_raw_parts(rp as *mut u8, rl as usize, rl as usize) };
            let reply =
                $crate::dispatch::<$plugin>(std::str::from_utf8(&method).unwrap_or(""), &request)
                    .into_boxed_slice();
            let len = reply.len();
            $crate::abi::pack(Box::into_raw(reply) as *mut u8 as u32, len as u32)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Resolver;
    impl Plugin for Resolver {
        fn resolve(&mut self, _: &Host, request: ResolveRequest) -> Result<Resolve> {
            if request.url == "decline" {
                return Ok(Resolve::NotClaimed);
            }
            Ok(Resolve::Plan(Plan::single(
                "Download",
                Track::file("file", request.url),
            )))
        }
    }
    #[test]
    fn native_author_can_dispatch_hooks_and_refresh_transport() {
        let request = br#"{"url":"https://example.com/file"}"#;
        let Resolve::Plan(plan) =
            decode::<Resolve>(&dispatch::<Resolver>("resolve", request)).unwrap()
        else {
            panic!("plan")
        };
        let refresh = serde_json::to_vec(&RefreshRequest {
            url: "https://example.com/new".into(),
            plan_id: plan.id.clone(),
            track_ids: vec!["file".into()],
        })
        .unwrap();
        let result: Refreshed = decode(&dispatch::<Resolver>("refresh", &refresh)).unwrap();
        assert_eq!(result.tracks[0].sources[0].url, "https://example.com/new");
        for (url, id, tracks, code) in [
            (
                "decline",
                plan.id.as_str(),
                vec!["file"],
                ErrorCode::NotClaimed,
            ),
            (
                "https://example.com/new",
                "different",
                vec!["file"],
                ErrorCode::InvalidPlan,
            ),
            (
                "https://example.com/new",
                plan.id.as_str(),
                vec!["missing"],
                ErrorCode::InvalidPlan,
            ),
        ] {
            let request = serde_json::to_vec(&RefreshRequest {
                url: url.into(),
                plan_id: id.into(),
                track_ids: tracks.into_iter().map(String::from).collect(),
            })
            .unwrap();
            assert_eq!(
                decode::<Refreshed>(&dispatch::<Resolver>("refresh", &request))
                    .unwrap_err()
                    .code,
                code
            );
        }
        assert!(
            decode::<EnqueueDecision>(&dispatch::<Resolver>("enqueue", request))
                .unwrap()
                .allow
        );
        let completed = br#"{"url":"https://example.com/file","name":"file","size":10}"#;
        decode::<()>(&dispatch::<Resolver>("complete", completed)).unwrap();
        let process = br#"{"job":{"url":"https://example.com/file","name":"file","size":10},"input":"a","output":"b"}"#;
        assert!(
            !decode::<Processed>(&dispatch::<Resolver>("process", process))
                .unwrap()
                .changed
        );
        for method in ["hooks", "check"] {
            decode::<serde_json::Value>(&dispatch::<Resolver>(method, b"{}")).unwrap();
        }
        assert_eq!(
            decode::<Resolve>(&dispatch::<Resolver>("unknown", request))
                .unwrap_err()
                .code,
            ErrorCode::Unsupported
        );
        assert_eq!(
            decode::<Resolve>(&dispatch::<Resolver>("resolve", b"bad json"))
                .unwrap_err()
                .code,
            ErrorCode::InvalidReply
        );
    }
    #[test]
    fn native_host_calls_return_explicit_wasm_requirement() {
        assert_eq!(Host.settings().unwrap_err().code, ErrorCode::Unsupported);
        assert_eq!(
            Host.exec("tool", &[]).unwrap_err().code,
            ErrorCode::Unsupported
        );
        let form: Form = serde_json::from_str(r#"{"fields":[]}"#).unwrap();
        assert_eq!(Host.prompt(&form).unwrap_err().code, ErrorCode::Unsupported);
        assert_eq!(
            Host.log("info", "text").unwrap_err().code,
            ErrorCode::Unsupported
        );
    }
}
