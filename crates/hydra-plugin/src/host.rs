//! Host-function dispatch shared by the wasm runtime and front ends.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use hya_net::{Connector, CookieJar};
use hya_plugin_api::limits::{MAX_DATA_DIR, MAX_STORAGE_TOTAL, MAX_STORAGE_VALUE};
use hya_plugin_api::{Answers, ErrorCode, Form, PluginError, Value};
use serde::{Deserialize, Serialize};

use crate::exec::{self, Pin};
use crate::http::{self, HttpPolicy, HttpRequest};
use crate::runtime::HostCalls;

/// Front-end callbacks used by prompts and user-visible plugin activity.
pub trait Frontend: Send {
    /// # Errors
    /// Returns a user cancellation or frontend failure.
    fn prompt(&mut self, form: Form) -> Result<Answers, PluginError>;
    /// Prompts while allowing a frontend to observe transfer cancellation.
    fn prompt_with_ctl(
        &mut self,
        form: Form,
        _: &crate::runtime::CallCtl,
    ) -> Result<Answers, PluginError> {
        self.prompt(form)
    }
    fn log(&mut self, level: &str, message: &str);
    fn progress(&mut self, done: u64, total: Option<u64>, note: Option<&str>);
}

struct SilentFrontend;

impl Frontend for SilentFrontend {
    fn prompt(&mut self, _: Form) -> Result<Answers, PluginError> {
        Err(PluginError::new(
            ErrorCode::Cancelled,
            "no frontend is attached",
        ))
    }
    fn log(&mut self, _: &str, _: &str) {}
    fn progress(&mut self, _: u64, _: Option<u64>, _: Option<&str>) {}
}

/// Mutable state exposed to one plugin instance. It is retained by the
/// manager between calls, while each wasm instance remains fresh.
pub struct HostState<C: Connector> {
    pub plugin_id: String,
    pub permissions: hya_plugin_api::Permissions,
    pub http: HttpPolicy,
    pub connector: C,
    pub data_dir: Option<PathBuf>,
    pub settings: BTreeMap<String, Value>,
    pub secrets: BTreeMap<String, Value>,
    pub storage: BTreeMap<String, Vec<u8>>,
    pub pins: BTreeMap<String, Pin>,
    pub exec_entries: Vec<hya_plugin_api::ExecEntry>,
    pub frontend: Box<dyn Frontend>,
}

impl<C: Connector> HostState<C> {
    /// Creates a state with no data directory and a non-interactive frontend.
    pub fn new(
        plugin_id: impl Into<String>,
        permissions: hya_plugin_api::Permissions,
        connector: C,
    ) -> Self {
        let http = HttpPolicy {
            proxy: None,
            http: crate::matcher::HostList::default(),
            cookies: crate::matcher::HostList::default(),
            user_cookies: CookieJar::new(),
            session: CookieJar::new(),
        };
        let exec_entries = permissions.exec.clone();
        Self {
            plugin_id: plugin_id.into(),
            permissions,
            http,
            connector,
            data_dir: None,
            settings: BTreeMap::new(),
            secrets: BTreeMap::new(),
            storage: BTreeMap::new(),
            pins: BTreeMap::new(),
            exec_entries,
            frontend: Box::new(SilentFrontend),
        }
    }

    #[must_use]
    pub fn with_frontend(mut self, frontend: Box<dyn Frontend>) -> Self {
        self.frontend = frontend;
        self
    }

    #[must_use]
    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    fn permission(&self, allowed: bool, name: &str) -> Result<(), PluginError> {
        allowed.then_some(()).ok_or_else(|| {
            PluginError::new(
                ErrorCode::PermissionDenied,
                format!("permission `{name}` is not granted"),
            )
        })
    }
}

/// Wasm ABI dispatcher for the closed host-function set.
pub struct Dispatcher<C: Connector> {
    state: Arc<Mutex<HostState<C>>>,
    ctl: Arc<crate::runtime::CallCtl>,
}

impl<C: Connector> Dispatcher<C> {
    pub fn new(state: HostState<C>) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            ctl: crate::runtime::CallCtl::new(std::time::Duration::from_secs(120)),
        }
    }

    pub fn state(&self) -> Arc<Mutex<HostState<C>>> {
        Arc::clone(&self.state)
    }

    fn dispatch(&mut self, name: &str, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match name {
            "http" => self.http(&mut state, request),
            "platform" => encode(
                &serde_json::json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH}),
            ),
            "program_install" => self.program_install(&mut state, request),
            "exec" => self.exec(&mut state, request),
            "data_read" => self.data_read(&mut state, request),
            "data_write" => self.data_write(&mut state, request),
            "prompt" => self.prompt(&mut state, request),
            "settings" => self.settings(&mut state),
            "storage_get" => self.storage_get(&mut state, request),
            "storage_set" => self.storage_set(&mut state, request),
            "log" => self.log(&mut state, request),
            "progress" => self.progress(&mut state, request),
            _ => Err(PluginError::new(
                ErrorCode::Unsupported,
                format!("unknown host function `{name}`"),
            )),
        }
    }

    fn http(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        state.permission(!state.permissions.http.is_empty(), "http")?;
        if request.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
            let requests: Vec<HttpRequest> = decode(request)?;
            if requests.len() > hya_plugin_api::limits::MAX_BATCH {
                return Err(PluginError::new(
                    ErrorCode::InvalidInput,
                    "too many requests in HTTP batch",
                ));
            }
            let mut replies = Vec::new();
            for chunk in requests.chunks(hya_plugin_api::limits::HTTP_PER_HOST_IN_FLIGHT) {
                self.ctl.check()?;
                let baseline = state.http.clone();
                let connector = &state.connector;
                let results = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk.iter().map(|request| {
                        let mut policy = baseline.clone();
                        scope.spawn(move || {
                            let response = block_on(async {
                                let request = http::perform(connector,&mut policy,request);
                                tokio::pin!(request);
                                loop {tokio::select! {result = &mut request => return result, _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => self.ctl.check()?}}
                            });
                            (response,policy.session)
                        })
                    }).collect();
                    handles
                        .into_iter()
                        .map(|handle| {
                            handle.join().map_err(|_| {
                                PluginError::new(ErrorCode::Internal, "HTTP batch worker failed")
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })?;
                for (response, session) in results {
                    merge_session(&mut state.http.session, &baseline.session, &session);
                    replies.push(response?);
                }
            }
            return encode(&replies);
        }
        let request: HttpRequest = decode(request)?;
        let response = block_on(async {
            let request = http::perform(&state.connector, &mut state.http, &request);
            tokio::pin!(request);
            loop {
                tokio::select! {
                    result = &mut request => return result,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => self.ctl.check()?,
                }
            }
        })?;
        encode(&response)
    }

    fn program_install(
        &self,
        state: &mut HostState<C>,
        request: &[u8],
    ) -> Result<Vec<u8>, PluginError> {
        #[derive(Deserialize)]
        struct Install {
            program: String,
            url: String,
            sha256: String,
        }
        let req: Install = decode(request)?;
        state.permission(
            state.permissions.exec_from_data && state.permissions.data,
            "exec_from_data and data",
        )?;
        state.permission(
            state
                .permissions
                .exec
                .iter()
                .any(|e| e.program == req.program),
            "declared exec program",
        )?;
        if req.program.contains(['/', '\\'])
            || req.program.is_empty()
            || req.program == "."
            || req.program == ".."
            || hya_plugin_api::manifest::is_refused_program(&req.program)
        {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "invalid program name",
            ));
        }
        if state.pins.contains_key(&req.program) {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "program is already pinned; approve changes through plugin permissions",
            ));
        }
        if !req.url.starts_with("https://")
            || req.sha256.len() != 64
            || !req.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "program installation needs HTTPS and SHA256",
            ));
        }
        let root = state
            .data_dir
            .as_ref()
            .ok_or_else(|| PluginError::new(ErrorCode::PermissionDenied, "no data directory"))?
            .clone();
        let remaining = MAX_DATA_DIR.saturating_sub(data_size(&root)?);
        let response = block_on(async {
            let request = HttpRequest {
                method: "GET".into(),
                url: req.url.clone(),
                headers: Default::default(),
                body_b64: None,
            };
            let request = http::perform_bounded(
                &state.connector,
                &mut state.http,
                &request,
                remaining as usize,
            );
            tokio::pin!(request);
            loop {
                tokio::select! {result = &mut request => return result, _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => self.ctl.check()?}
            }
        })?;
        if response.status != 200 {
            return Err(PluginError::new(
                ErrorCode::Network,
                format!("program download returned {}", response.status),
            ));
        }
        let bytes = hya_net::base64::decode(&response.body_b64)
            .ok_or_else(|| PluginError::new(ErrorCode::InvalidReply, "invalid body"))?;
        if crate::package::sha256_hex(&bytes) != req.sha256.to_ascii_lowercase() {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "program checksum mismatch",
            ));
        }
        let filename = if cfg!(windows) && !req.program.ends_with(".exe") {
            format!("{}.exe", req.program)
        } else {
            req.program.clone()
        };
        let path = Self::data_path(state, &format!("bin/{filename}"))?;
        if path.exists() {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "program already exists; approve it through plugin permissions",
            ));
        }
        crate::manager::write_atomic(&path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        }
        let pin = exec::pin(&req.program, &[root.join("bin")])?;
        state.pins.insert(req.program, pin);
        encode(&EmptyReply {})
    }

    fn exec(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let req: ExecRequest = decode(request)?;
        state.permission(!state.permissions.exec.is_empty(), "exec")?;
        let entry = exec::admit(&state.exec_entries, &req.program, &req.args).ok_or_else(|| {
            PluginError::new(
                ErrorCode::PermissionDenied,
                "arguments do not match an exec grant",
            )
        })?;
        let pin = state.pins.get(&entry.program).ok_or_else(|| {
            PluginError::new(
                ErrorCode::ToolMissing,
                format!("`{}` is not pinned", entry.program),
            )
        })?;
        if let Some(root) = &state.data_dir {
            if pin.path.starts_with(root) && !state.permissions.exec_from_data {
                return Err(PluginError::new(
                    ErrorCode::PermissionDenied,
                    "running programs from plugin data requires exec_from_data",
                ));
            }
        }
        let isolated = tempfile::tempdir()
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        let cwd = state.data_dir.as_deref().unwrap_or(isolated.path());
        let mut args = req.args;
        let mut cookie_file = None;
        if args.iter().any(|a| a == exec::COOKIE_FILE) {
            state.permission(!state.permissions.cookies.is_empty(), "cookies")?;
            let mut jar = hya_net::CookieJar::new();
            for cookie in state
                .http
                .user_cookies
                .iter()
                .chain(state.http.session.iter())
            {
                if state
                    .http
                    .cookies
                    .allows_host(cookie.domain.trim_start_matches('.'), None)
                {
                    jar.insert(cookie.clone());
                }
            }
            let (text, _) =
                hya_net::cookies::netscape::render(&jar, true, hya_net::cookies::now_secs());
            let mut file = tempfile::NamedTempFile::new()
                .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
            std::io::Write::write_all(&mut file, text.as_bytes())
                .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
            for arg in &mut args {
                if arg == exec::COOKIE_FILE {
                    *arg = file.path().to_string_lossy().into_owned();
                }
            }
            cookie_file = Some(file);
        }
        for arg in &mut args {
            if arg == exec::DATA_DIR {
                state.permission(state.permissions.data, "data")?;
                *arg = cwd.to_string_lossy().into_owned();
            }
        }
        let secrets = secret_values(state);
        let output = exec::run_with_proxy(
            pin,
            &args,
            cwd,
            &self.ctl,
            &secrets,
            state.http.proxy.as_ref(),
        )?;
        drop(cookie_file);
        encode(&output)
    }

    fn data_path(state: &HostState<C>, path: &str) -> Result<PathBuf, PluginError> {
        state.permission(state.permissions.data, "data")?;
        let root = state.data_dir.as_ref().ok_or_else(|| {
            PluginError::new(ErrorCode::PermissionDenied, "plugin has no data directory")
        })?;
        let relative = Path::new(path);
        if path.is_empty()
            || relative.is_absolute()
            || relative.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "data path escapes the data directory",
            ));
        }
        std::fs::create_dir_all(root)
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        let root = std::fs::canonicalize(root)
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        let path = root.join(relative);
        let mut cursor = root;
        for component in relative.components() {
            cursor.push(component);
            if std::fs::symlink_metadata(&cursor).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(PluginError::new(
                    ErrorCode::PermissionDenied,
                    "symlinks are not allowed in plugin data paths",
                ));
            }
        }
        Ok(path)
    }

    fn data_read(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let req: PathRequest = decode(request)?;
        let path = Self::data_path(state, &req.path)?;
        let meta = std::fs::metadata(&path)
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        if meta.len() > MAX_DATA_DIR {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "data file is too large",
            ));
        }
        let bytes = std::fs::read(path)
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        encode(&BytesReply {
            data_b64: hya_net::base64::encode(&bytes),
        })
    }

    fn data_write(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let req: BytesRequest = decode(request)?;
        let path = Self::data_path(state, &req.path)?;
        let bytes = hya_net::base64::decode(&req.data_b64)
            .ok_or_else(|| PluginError::new(ErrorCode::InvalidInput, "data_b64 is not base64"))?;
        let root = state.data_dir.as_ref().ok_or_else(|| {
            PluginError::new(ErrorCode::PermissionDenied, "plugin has no data directory")
        })?;
        let existing = std::fs::metadata(&path).map_or(0, |m| m.len());
        let total = data_size(root)?
            .saturating_sub(existing)
            .saturating_add(bytes.len() as u64);
        if total > MAX_DATA_DIR {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "data file is too large",
            ));
        }
        crate::manager::write_atomic(&path, &bytes)?;
        encode(&EmptyReply {})
    }

    fn prompt(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let form: Form = decode(request)?;
        let _pause = self.ctl.pause();
        let answers = state.frontend.prompt_with_ctl(form, &self.ctl)?;
        encode(&answers)
    }

    fn settings(&self, state: &mut HostState<C>) -> Result<Vec<u8>, PluginError> {
        let mut all = state.settings.clone();
        all.extend(state.secrets.clone());
        encode(&all)
    }

    fn storage_get(
        &self,
        state: &mut HostState<C>,
        request: &[u8],
    ) -> Result<Vec<u8>, PluginError> {
        let req: KeyRequest = decode(request)?;
        let value = state.storage.get(&req.key).cloned();
        encode(&StorageReply {
            value_b64: value.map(|v| hya_net::base64::encode(&v)),
        })
    }

    fn storage_set(
        &self,
        state: &mut HostState<C>,
        request: &[u8],
    ) -> Result<Vec<u8>, PluginError> {
        let req: StorageSetRequest = decode(request)?;
        let bytes = hya_net::base64::decode(&req.value_b64)
            .ok_or_else(|| PluginError::new(ErrorCode::InvalidInput, "value_b64 is not base64"))?;
        if bytes.len() > MAX_STORAGE_VALUE {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "storage value is too large",
            ));
        }
        let old = state.storage.get(&req.key).map_or(0, Vec::len);
        let total = state.storage.values().map(Vec::len).sum::<usize>() - old + bytes.len();
        if total > MAX_STORAGE_TOTAL {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "storage quota exceeded",
            ));
        }
        state.storage.insert(req.key, bytes);
        encode(&EmptyReply {})
    }

    fn log(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let req: LogRequest = decode(request)?;
        state.frontend.log(
            &req.level,
            &exec::redact(&req.message, &secret_values(state)),
        );
        encode(&EmptyReply {})
    }

    fn progress(&self, state: &mut HostState<C>, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        let req: ProgressRequest = decode(request)?;
        let note = req.note.map(|s| exec::redact(&s, &secret_values(state)));
        state
            .frontend
            .progress(req.done, req.total, note.as_deref());
        encode(&EmptyReply {})
    }
}

impl<C: Connector> HostCalls for Dispatcher<C> {
    fn set_ctl(&mut self, ctl: Arc<crate::runtime::CallCtl>) {
        self.ctl = ctl;
    }
    fn call(&mut self, name: &str, request: &[u8]) -> Result<Vec<u8>, PluginError> {
        self.dispatch(name, request)
    }
}

pub(crate) fn data_size(root: &Path) -> Result<u64, PluginError> {
    let mut total = 0u64;
    for entry in
        std::fs::read_dir(root).map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?
    {
        let entry = entry.map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        let meta = std::fs::symlink_metadata(entry.path())
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        if meta.file_type().is_symlink() {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "symlink in plugin data directory",
            ));
        }
        total = total.saturating_add(if meta.is_dir() {
            data_size(&entry.path())?
        } else {
            meta.len()
        });
    }
    Ok(total)
}

fn secret_values<C: Connector>(state: &HostState<C>) -> Vec<String> {
    state
        .secrets
        .values()
        .filter_map(Value::as_text)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Debug, Deserialize)]
struct PathRequest {
    path: String,
}
#[derive(Debug, Deserialize)]
struct BytesRequest {
    path: String,
    data_b64: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct BytesReply {
    data_b64: String,
}
#[derive(Debug, Deserialize)]
struct KeyRequest {
    key: String,
}
#[derive(Debug, Deserialize)]
struct StorageSetRequest {
    key: String,
    value_b64: String,
}
#[derive(Debug, Deserialize)]
struct ExecRequest {
    program: String,
    args: Vec<String>,
}
#[derive(Debug, Deserialize)]
struct LogRequest {
    level: String,
    message: String,
}
#[derive(Debug, Deserialize)]
struct ProgressRequest {
    done: u64,
    total: Option<u64>,
    note: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
struct StorageReply {
    value_b64: Option<String>,
}
#[derive(Debug, Serialize)]
struct EmptyReply {}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, PluginError> {
    serde_json::from_slice(bytes)
        .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e.to_string()))
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, PluginError> {
    serde_json::to_vec(value).map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(future)
    }
}

fn merge_session(current: &mut CookieJar, baseline: &CookieJar, response: &CookieJar) {
    let same_key = |a: &hya_net::Cookie, b: &hya_net::Cookie| {
        a.name == b.name && a.domain == b.domain && a.path == b.path
    };
    let mut merged = CookieJar::new();
    for cookie in current.iter() {
        let deleted = baseline.iter().any(|old| same_key(old, cookie))
            && !response.iter().any(|new| same_key(new, cookie));
        if !deleted {
            merged.insert(cookie.clone());
        }
    }
    for cookie in response
        .iter()
        .filter(|cookie| !baseline.iter().any(|old| old == *cookie))
    {
        merged.insert(cookie.clone());
    }
    http::bound_session(&mut merged);
    *current = merged;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Front {
        prompts: AtomicUsize,
        logs: Arc<Mutex<Vec<String>>>,
    }

    impl Frontend for Front {
        fn prompt(&mut self, form: Form) -> Result<Answers, PluginError> {
            self.prompts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(form.fields.len(), 1);
            Ok(Answers::from([(
                String::from("answer"),
                Value::Text("yes".into()),
            )]))
        }
        fn log(&mut self, level: &str, message: &str) {
            self.logs.lock().unwrap().push(format!("{level}:{message}"));
        }
        fn progress(&mut self, _: u64, _: Option<u64>, _: Option<&str>) {}
    }

    fn dispatcher() -> Dispatcher<hya_net::TcpConnector> {
        Dispatcher::new(HostState::new(
            "test.plugin",
            hya_plugin_api::Permissions {
                data: true,
                ..Default::default()
            },
            hya_net::TcpConnector,
        ))
    }

    #[test]
    fn program_install_requires_grants_hash_and_an_unpinned_name() {
        use std::io::{Read, Write};
        let root = tempfile::tempdir().unwrap();
        let mut d = dispatcher();
        let state = d.state();
        state.lock().unwrap().data_dir = Some(root.path().into());
        let request = serde_json::json!({"program":"test-tool","url":"https://127.0.0.1/bin","sha256":"0".repeat(64)});
        assert!(d
            .call("program_install", &serde_json::to_vec(&request).unwrap())
            .is_err());
        {
            let mut state = state.lock().unwrap();
            state.permissions.exec_from_data = true;
            state.permissions.exec.push(hya_plugin_api::ExecEntry {
                program: "test-tool".into(),
                args: vec!["--version".into()],
            });
            state.http.http = crate::matcher::HostList::parse(&["127.0.0.1".into()]).unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bytes = b"test program payload";
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if socket.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                socket.write_all(bytes).unwrap();
            }
        });
        let mut request = serde_json::json!({"program":"test-tool","url":format!("https://{address}/bin"),"sha256":"0".repeat(64)});
        assert!(d
            .call("program_install", &serde_json::to_vec(&request).unwrap())
            .is_err());
        assert!(!root.path().join("bin/test-tool").exists());
        request["sha256"] = crate::package::sha256_hex(bytes).into();
        d.call("program_install", &serde_json::to_vec(&request).unwrap())
            .unwrap();
        let pin = state.lock().unwrap().pins["test-tool"].clone();
        assert_eq!(std::fs::read(&pin.path).unwrap(), bytes);
        assert!(d
            .call("program_install", &serde_json::to_vec(&request).unwrap())
            .is_err());
        std::fs::write(&pin.path, b"different executable").unwrap();
        assert!(exec::verify(&pin).is_err());
        server.join().unwrap();
    }
    #[test]
    fn batch_session_merges_updates_and_deletions_without_losing_other_responses() {
        let mut baseline = CookieJar::new();
        baseline.insert(hya_net::Cookie::new("token", "old", "example.com"));
        baseline.insert(hya_net::Cookie::new("keep", "yes", "example.com"));
        let mut current = baseline.clone();
        current.insert(hya_net::Cookie::new("parallel", "new", "example.com"));
        let mut response = CookieJar::new();
        response.insert(hya_net::Cookie::new("keep", "updated", "example.com"));
        merge_session(&mut current, &baseline, &response);
        assert!(!current.iter().any(|c| c.name == "token"));
        assert!(current
            .iter()
            .any(|c| c.name == "keep" && c.value == "updated"));
        assert!(current.iter().any(|c| c.name == "parallel"));
    }
    #[test]
    fn http_batches_enforce_count_and_platform_exposes_only_host_identity() {
        let mut d = dispatcher();
        d.state()
            .lock()
            .unwrap()
            .permissions
            .http
            .push("example.com".into());
        let batch = vec![serde_json::json!({"url":"https://example.com"}); 33];
        assert_eq!(
            d.call("http", &serde_json::to_vec(&batch).unwrap())
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        let platform: serde_json::Value =
            serde_json::from_slice(&d.call("platform", b"{}").unwrap()).unwrap();
        assert_eq!(platform["os"], std::env::consts::OS);
        assert_eq!(platform["arch"], std::env::consts::ARCH);
    }

    #[test]
    fn unknown_functions_and_bad_json_are_typed_errors() {
        let mut d = dispatcher();
        assert_eq!(
            d.call("nope", b"{}").unwrap_err().code,
            ErrorCode::Unsupported
        );
        assert_eq!(
            d.call("storage_get", b"nope").unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }

    #[test]
    fn storage_is_bounded_and_round_trips_bytes() {
        let mut d = dispatcher();
        d.call("storage_set", br#"{"key":"k","value_b64":"aGVsbG8="}"#)
            .unwrap();
        let got: StorageReply =
            serde_json::from_slice(&d.call("storage_get", br#"{"key":"k"}"#).unwrap()).unwrap();
        assert_eq!(got.value_b64.as_deref(), Some("aGVsbG8="));
        assert!(d.call("storage_set", &serde_json::to_vec(&serde_json::json!({"key":"x","value_b64":hya_net::base64::encode(&vec![0; MAX_STORAGE_VALUE + 1])})).unwrap()).is_err());
    }

    #[test]
    fn data_paths_cannot_escape_and_files_round_trip() {
        let dir = std::env::temp_dir().join(format!("hya-dispatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = dispatcher();
        d.state().lock().unwrap().data_dir = Some(dir.clone());
        d.call("data_write", br#"{"path":"a.bin","data_b64":"AQI="}"#)
            .unwrap();
        let got: BytesReply =
            serde_json::from_slice(&d.call("data_read", br#"{"path":"a.bin"}"#).unwrap()).unwrap();
        assert_eq!(got.data_b64, "AQI=");
        assert_eq!(
            d.call("data_read", br#"{"path":"../secret"}"#)
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_include_secrets_only_in_memory() {
        let mut d = dispatcher();
        d.state()
            .lock()
            .unwrap()
            .settings
            .insert("theme".into(), Value::Text("dark".into()));
        d.state()
            .lock()
            .unwrap()
            .secrets
            .insert("token".into(), Value::Text("secret".into()));
        let values: BTreeMap<String, Value> =
            serde_json::from_slice(&d.call("settings", b"{}").unwrap()).unwrap();
        assert_eq!(values["theme"], Value::Text("dark".into()));
        assert_eq!(values["token"], Value::Text("secret".into()));
    }

    #[test]
    fn prompt_log_and_progress_use_the_frontend() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let front = Front {
            logs: logs.clone(),
            ..Default::default()
        };
        let mut d = dispatcher();
        d.state().lock().unwrap().frontend = Box::new(front);
        let form = serde_json::json!({"title":"Q","fields":[{"key":"answer","label":"Answer","type":"text"}]});
        let answers: Answers = serde_json::from_slice(
            &d.call("prompt", &serde_json::to_vec(&form).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(answers["answer"], Value::Text("yes".into()));
        d.call("log", br#"{"level":"info","message":"hello"}"#)
            .unwrap();
        d.call("progress", br#"{"done":1,"total":2,"note":"x"}"#)
            .unwrap();
        assert_eq!(&*logs.lock().unwrap(), &["info:hello"]);
    }

    #[test]
    fn denied_capabilities_do_not_touch_the_filesystem() {
        let mut d = Dispatcher::new(HostState::new(
            "x",
            Default::default(),
            hya_net::TcpConnector,
        ));
        assert_eq!(
            d.call("data_read", br#"{"path":"x"}"#).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            d.call("http", br#"{"url":"https://example.com"}"#)
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            d.call("exec", br#"{"program":"x","args":[]}"#)
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
    }
}
