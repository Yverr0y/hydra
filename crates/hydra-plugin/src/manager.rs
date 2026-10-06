//! Persistent plugin configuration and policy-checked resolver dispatch.
use fs2::FileExt;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_net::Connector;
use hya_plugin_api::{
    ErrorCode, Manifest, Permissions, PluginError, Resolve, ResolveRequest, Value,
};
use serde::{Deserialize, Serialize};

use crate::accept::{self, Clamps};
use crate::exec::{self, Pin};
use crate::host::{Dispatcher, Frontend, HostState};
use crate::matcher::{HostList, UrlPattern};
use crate::package::{self, Package};
use crate::runtime::{CallCtl, Runtime};

/// Per-call user context, never supplied by the guest.
#[derive(Clone)]
pub struct ResolveContext {
    pub ctl: std::sync::Arc<CallCtl>,
    pub cookies: hya_net::CookieJar,
    pub only: Option<String>,
    /// Explicit file input selected by the frontend, scoped to this call.
    pub input_file: Option<PathBuf>,
    /// User-selected proxy, never supplied by a plugin.
    pub proxy: Option<hya_net::Proxy>,
}
impl Default for ResolveContext {
    fn default() -> Self {
        Self {
            ctl: CallCtl::new(Duration::from_secs(300)),
            cookies: hya_net::CookieJar::new(),
            only: None,
            input_file: None,
            proxy: None,
        }
    }
}

/// An installed plugin and its explicit grants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub manifest: Manifest,
    pub directory: PathBuf,
    pub dev: bool,
    /// Signature verified when this package was installed; development folders are unsigned.
    #[serde(default)]
    pub signing: package::Signing,
    pub enabled: bool,
    pub grants: Permissions,
    pub pins: BTreeMap<String, Pin>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub module_sha256: String,
    #[serde(default)]
    pub native_sha256: BTreeMap<String, String>,
    #[serde(default)]
    pub previous: Option<Box<Installed>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    plugins: Vec<Installed>,
}

/// Shared CLI and desktop plugin manager. Call from a blocking worker.
pub struct Manager {
    root: PathBuf,
    state: State,
    runtime: Runtime,
    _lock: std::fs::File,
}

fn error(e: impl std::fmt::Display) -> PluginError {
    PluginError::new(ErrorCode::Internal, e.to_string())
}

impl Manager {
    /// Opens the installed list without silently discarding damaged state.
    pub fn open(root: PathBuf) -> Result<Self, PluginError> {
        std::fs::create_dir_all(&root).map_err(error)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("state.lock"))
            .map_err(error)?;
        lock.lock_exclusive().map_err(error)?;
        let state = match std::fs::read_to_string(root.join("state.toml")) {
            Ok(text) => toml::from_str(&text).map_err(error)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(error(e)),
        };
        Ok(Self {
            root,
            state,
            runtime: Runtime::new(),
            _lock: lock,
        })
    }
    /// Opens a CLI or GUI profile and installs newer bundled official plugins.
    ///
    /// Removed plugins, development overrides and other publishers are preserved.
    /// # Errors
    /// Returns an error for damaged state, invalid signatures or installation failure.
    pub fn open_with_official(root: PathBuf) -> Result<Self, PluginError> {
        let mut manager = Self::open(root)?;
        manager.sync_official(crate::official::PACKAGES, crate::official::KEY)?;
        Ok(manager)
    }

    fn sync_official(&mut self, archives: &[&[u8]], key: &str) -> Result<(), PluginError> {
        if archives.is_empty() {
            return Ok(());
        }
        let path = self.root.join("official.json");
        let mut seen: BTreeMap<String, String> = read_json(&path)?;
        let key = key
            .lines()
            .find(|line| !line.starts_with("untrusted comment:"))
            .unwrap_or("")
            .trim();
        for bytes in archives {
            let hash = package::sha256_hex(bytes);
            if seen.values().any(|previous| previous == &hash) {
                continue;
            }
            let package = package::open(bytes, None).map_err(error)?;
            if package.manifest.publisher_key.as_deref() != Some(key)
                || !matches!(package.signing, package::Signing::Verified { .. })
            {
                return Err(PluginError::new(
                    ErrorCode::PermissionDenied,
                    "official plugin is not signed by Hydra",
                ));
            }
            let id = &package.manifest.id;
            let previous = self.list().iter().find(|plugin| &plugin.manifest.id == id);
            let should_install = match previous {
                Some(old) => {
                    !old.dev
                        && old.manifest.publisher_key.as_deref() == Some(key)
                        && matches!(old.signing, package::Signing::Verified { .. })
                        && semver::Version::parse(&package.manifest.version).map_err(error)?
                            > semver::Version::parse(&old.manifest.version).map_err(error)?
                }
                None => !seen.contains_key(id),
            };
            if crate::distribution::compatible(
                package.manifest.api,
                package.manifest.min_hydra.as_deref(),
            )
            .is_err()
            {
                continue;
            }
            if should_install {
                let enabled = previous.is_none_or(|old| old.enabled);
                let mut file = tempfile::NamedTempFile::new().map_err(error)?;
                file.write_all(bytes).map_err(error)?;
                let installed = self.install(file.path(), package.manifest.permissions.clone())?;
                if !enabled {
                    self.enable(&installed, false)?;
                }
            }
            seen.insert(id.clone(), hash);
            write_atomic(&path, &serde_json::to_vec(&seen).map_err(error)?)?;
        }
        Ok(())
    }

    /// Lists plugins in resolver precedence order.
    pub fn list(&self) -> &[Installed] {
        &self.state.plugins
    }
    fn save(&self) -> Result<(), PluginError> {
        std::fs::create_dir_all(&self.root).map_err(error)?;
        let bytes = toml::to_string_pretty(&self.state).map_err(error)?;
        write_atomic(&self.root.join("state.toml"), bytes.as_bytes())
    }
    /// Verifies a local package or development directory before consent.
    pub fn inspect(path: &Path) -> Result<Package, PluginError> {
        if path.is_dir() {
            package::load_dir(path).map_err(error)
        } else {
            package::open(&std::fs::read(path).map_err(error)?, None).map_err(error)
        }
    }
    /// Installs a verified package after the frontend has obtained consent.
    pub fn install(&mut self, path: &Path, grants: Permissions) -> Result<String, PluginError> {
        self.install_with_publisher_consent(path, grants, false)
    }
    /// Installs after explicit acceptance of a publisher-key change, if requested.
    pub fn install_with_publisher_consent(
        &mut self,
        path: &Path,
        grants: Permissions,
        accept_publisher_change: bool,
    ) -> Result<String, PluginError> {
        let package = Self::inspect(path)?;
        crate::distribution::compatible(
            package.manifest.api,
            package.manifest.min_hydra.as_deref(),
        )?;
        if let Some(old) = self
            .list()
            .iter()
            .find(|p| p.manifest.id == package.manifest.id)
        {
            if old.manifest.publisher_key != package.manifest.publisher_key
                && !accept_publisher_change
            {
                let key = |key: &Option<String>| {
                    key.as_deref()
                        .map(package::key_fingerprint)
                        .unwrap_or_else(|| "unsigned".into())
                };
                return Err(PluginError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "publisher changed from {} to {}; explicit publisher consent is required",
                        key(&old.manifest.publisher_key),
                        key(&package.manifest.publisher_key)
                    ),
                ));
            }
        }
        if grants != package.manifest.permissions {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "install requires consent to the displayed permissions",
            ));
        }
        for id in &grants.native {
            if !package
                .manifest
                .native_modules
                .iter()
                .any(|module| module.id == *id && module.platform == crate::native::platform())
            {
                return Err(PluginError::new(
                    ErrorCode::Unsupported,
                    format!(
                        "native module {id} is not available for {}",
                        crate::native::platform()
                    ),
                ));
            }
        }
        for pattern in &package.manifest.claims {
            UrlPattern::parse(pattern).map_err(error)?;
        }
        for hosts in [&grants.http, &grants.sources, &grants.cookies] {
            HostList::parse(hosts).map_err(error)?;
        }
        self.runtime
            .compile(&package.module, package.manifest.memory_mb)
            .map_err(error)?;
        let id = package.manifest.id.clone();
        let previous = self
            .state
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .cloned();
        let mut pins = BTreeMap::new();
        for entry in &grants.exec {
            let mut dirs = exec::search_dirs(None);
            if grants.exec_from_data {
                dirs.insert(0, self.root.join(&id).join("data/bin"));
            }
            match exec::pin(&entry.program, &dirs) {
                Ok(pin) => {
                    pins.insert(entry.program.clone(), pin);
                }
                Err(e) if e.code == ErrorCode::ToolMissing => {}
                Err(e) => return Err(e),
            }
        }
        let base = self.root.join(&id);
        std::fs::create_dir_all(base.join("data")).map_err(error)?;
        let dev = path.is_dir();
        let directory = if dev {
            std::fs::canonicalize(path).map_err(error)?
        } else {
            let dir = base.join("versions").join(&package.archive_sha256);
            std::fs::create_dir_all(base.join("versions")).map_err(error)?;
            if !dir.exists() {
                let staging = tempfile::tempdir_in(base.join("versions")).map_err(error)?;
                package.write_into(staging.path()).map_err(error)?;
                std::fs::rename(staging.path(), &dir).map_err(error)?;
            }
            dir
        };
        let mut settings: BTreeMap<String, Value> = package
            .manifest
            .settings
            .iter()
            .filter(|f| f.kind != hya_plugin_api::FieldKind::Secret)
            .filter_map(|f| f.default.clone().map(|v| (f.key.clone(), v)))
            .collect();
        if let Some(old) = &previous {
            for field in package
                .manifest
                .settings
                .iter()
                .filter(|f| f.kind != hya_plugin_api::FieldKind::Secret)
            {
                if let Some(value) = old
                    .settings
                    .get(&field.key)
                    .filter(|v| field.validate(v).is_ok())
                {
                    settings.insert(field.key.clone(), value.clone());
                }
            }
        }
        let position = self
            .state
            .plugins
            .iter()
            .position(|p| p.manifest.id == id)
            .unwrap_or(self.state.plugins.len());
        self.state.plugins.retain(|p| p.manifest.id != id);
        let previous = previous.map(|mut p| {
            p.previous = None;
            Box::new(p)
        });
        let native_sha256 = package
            .manifest
            .native_modules
            .iter()
            .map(|module| {
                (
                    module.module.clone(),
                    package::sha256_hex(&package.entries[&module.module]),
                )
            })
            .collect();
        self.state.plugins.insert(
            position,
            Installed {
                manifest: package.manifest,
                directory,
                dev,
                signing: package.signing,
                enabled: true,
                grants,
                pins,
                settings,
                failures: 0,
                module_sha256: package::sha256_hex(&package.module),
                native_sha256,
                previous,
            },
        );
        self.save()?;
        Ok(id)
    }
    fn installed_mut(&mut self, id: &str) -> Result<&mut Installed, PluginError> {
        self.state
            .plugins
            .iter_mut()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| {
                PluginError::new(ErrorCode::InvalidInput, format!("unknown plugin {id}"))
            })
    }
    /// Enables or disables a plugin; enabling resets its circuit breaker.
    pub fn enable(&mut self, id: &str, enabled: bool) -> Result<(), PluginError> {
        let p = self.installed_mut(id)?;
        p.enabled = enabled;
        if enabled {
            p.failures = 0;
        }
        self.save()
    }
    /// Removes configuration; retained data can be removed separately.
    pub fn remove(&mut self, id: &str) -> Result<(), PluginError> {
        self.installed_mut(id)?;
        self.state.plugins.retain(|p| p.manifest.id != id);
        self.save()
    }
    /// Restores the previous package and grants while preserving user settings and data.
    pub fn rollback(&mut self, id: &str) -> Result<(), PluginError> {
        let current = self.installed_mut(id)?;
        let mut previous = current
            .previous
            .take()
            .ok_or_else(|| PluginError::new(ErrorCode::InvalidInput, "no previous version"))?;
        previous.settings = current.settings.clone();
        let mut replaced = std::mem::replace(current, *previous);
        replaced.previous = None;
        current.previous = Some(Box::new(replaced));
        current.failures = 0;
        self.save()
    }
    /// Changes a capability grant; exec grants pin the current executable explicitly.
    pub fn permission(
        &mut self,
        id: &str,
        capability: &str,
        grant: bool,
    ) -> Result<(), PluginError> {
        let private_bin = self.root.join(id).join("data/bin");
        let p = self.installed_mut(id)?;
        let (kind, value) = capability.split_once(':').unwrap_or((capability, ""));
        match kind {
            "http" | "sources" | "cookies" => {
                let (declared,granted) = match kind { "http" => (&p.manifest.permissions.http,&mut p.grants.http), "sources" => (&p.manifest.permissions.sources,&mut p.grants.sources), _ => (&p.manifest.permissions.cookies,&mut p.grants.cookies) };
                if !declared.iter().any(|h| h == value) { return Err(PluginError::new(ErrorCode::PermissionDenied,"capability is not declared")); }
                granted.retain(|h| h != value); if grant { granted.push(value.into()); }
            }
            "exec_from_data" => {if grant && !p.manifest.permissions.exec_from_data {return Err(PluginError::new(ErrorCode::PermissionDenied,"exec_from_data is not declared"));}p.grants.exec_from_data=grant;}
            "data" => { if grant && !p.manifest.permissions.data { return Err(PluginError::new(ErrorCode::PermissionDenied,"data is not declared")); } p.grants.data = grant; }
            "native" => {
                if !p.manifest.permissions.native.iter().any(|id| id == value) {
                    return Err(PluginError::new(ErrorCode::PermissionDenied, "native module is not declared"));
                }
                p.grants.native.retain(|id| id != value);
                if grant { p.grants.native.push(value.into()); }
            }
            "exec" => {
                let entries: Vec<_> = p.manifest.permissions.exec.iter().filter(|e| e.program == value).cloned().collect();
                if entries.is_empty() { return Err(PluginError::new(ErrorCode::PermissionDenied,"program is not declared")); }
                if grant { let key = format!("{}_path",value.replace('-',"_")); let setting = p.settings.get(&key).and_then(Value::as_text).map(Path::new); let mut dirs = exec::search_dirs(setting); if p.grants.exec_from_data {dirs.insert(0,private_bin.clone());} p.pins.insert(value.into(),exec::pin(value,&dirs)?); }
                else { p.pins.remove(value); }
                p.grants.exec.retain(|e| e.program != value); if grant { p.grants.exec.extend(entries); }
            }
            _ => return Err(PluginError::new(ErrorCode::InvalidInput,"unknown capability; use http:HOST, sources:HOST, cookies:HOST, exec:PROGRAM or data")),
        }
        self.save()
    }
    /// Clears only the plugin-held session cookies.
    pub fn clear_session(&mut self, id: &str) -> Result<(), PluginError> {
        self.installed_mut(id)?;
        write_atomic(&self.root.join(id).join("session.jar"), b"")
    }
    /// Returns whether an enabled resolver claims an address.
    pub fn claims(&self, url: &str) -> bool {
        self.list()
            .iter()
            .any(|p| p.enabled && crate::matcher::claims_input(&p.manifest, url))
    }
    /// Sets the complete resolver order, rejecting missing or duplicate IDs.
    pub fn order(&mut self, ids: &[String]) -> Result<(), PluginError> {
        let mut reordered = Vec::new();
        for id in ids {
            if reordered.iter().any(|p: &Installed| &p.manifest.id == id) {
                return Err(PluginError::new(
                    ErrorCode::InvalidInput,
                    "duplicate plugin in order",
                ));
            }
            reordered.push(self.installed_mut(id)?.clone());
        }
        if reordered.len() != self.state.plugins.len() {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "order must include every plugin",
            ));
        }
        self.state.plugins = reordered;
        self.save()
    }
    /// Stores a declared non-secret setting after validating its type.
    pub fn set(&mut self, id: &str, key: &str, value: Value) -> Result<(), PluginError> {
        let p = self.installed_mut(id)?;
        let field = p
            .manifest
            .settings
            .iter()
            .find(|f| f.key == key)
            .ok_or_else(|| PluginError::new(ErrorCode::InvalidInput, "unknown setting"))?;
        if field.kind == hya_plugin_api::FieldKind::Secret {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "secret settings require protected storage",
            ));
        }
        field
            .validate(&value)
            .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e))?;
        p.settings.insert(key.into(), value);
        self.save()
    }
    fn invoke<C: Connector + 'static>(
        &self,
        p: &Installed,
        connector: C,
        frontend: Box<dyn Frontend>,
        context: &ResolveContext,
        method: &str,
        request: &[u8],
    ) -> Result<serde_json::Value, PluginError> {
        let package = package::load_dir(&p.directory).map_err(error)?;
        // A dev rebuild may change code, but must never silently expand consent.
        if package.manifest != p.manifest
            || (!p.dev && package::sha256_hex(&package.module) != p.module_sha256)
        {
            return Err(PluginError::new(
                ErrorCode::PermissionDenied,
                "manifest changed; reinstall to review permissions",
            ));
        }
        let compiled = self
            .runtime
            .compile(&package.module, p.manifest.memory_mb)
            .map_err(error)?;
        let mut state = HostState::new(&p.manifest.id, p.grants.clone(), connector).with_frontend(
            Box::new(LoggedFrontend {
                inner: frontend,
                path: self.root.join(&p.manifest.id).join("log.txt"),
            }),
        );
        state.http.http = HostList::parse(&p.grants.http).map_err(error)?;
        state.http.cookies = HostList::parse(&p.grants.cookies).map_err(error)?;
        state.http.user_cookies = context.cookies.clone();
        state.http.proxy = context.proxy.clone();
        state.input_file = context.input_file.clone();
        state.native_modules = package
            .manifest
            .native_modules
            .iter()
            .filter(|module| {
                module.platform == crate::native::platform() && p.grants.native.contains(&module.id)
            })
            .map(|module| {
                (
                    module.id.clone(),
                    crate::native::Module {
                        path: p.directory.join(&module.module),
                        sha256: p
                            .native_sha256
                            .get(&module.module)
                            .cloned()
                            .unwrap_or_default(),
                    },
                )
            })
            .collect();
        if p.grants.data {
            state.data_dir = Some(self.root.join(&p.manifest.id).join("data"));
        }
        state.pins = read_json(&self.root.join(&p.manifest.id).join("pins.json"))?;
        state.pins.extend(p.pins.clone());
        if !p.grants.data || !p.grants.exec_from_data {
            let data = std::fs::canonicalize(self.root.join(&p.manifest.id).join("data"))
                .map_err(error)?;
            state.pins.retain(|_, pin| !pin.path.starts_with(&data));
        }
        state.settings = p.settings.clone();
        let base = self.root.join(&p.manifest.id);
        state.secrets = crate::secrets::read(&base.join("secrets.bin"))?;
        state.secrets.retain(|key, value| {
            p.manifest.settings.iter().any(|f| {
                f.key == *key
                    && f.kind == hya_plugin_api::FieldKind::Secret
                    && f.validate(value).is_ok()
            })
        });
        state.storage = read_json(&base.join("storage.json"))?;
        if let Ok(jar) = std::fs::read_to_string(base.join("session.jar")) {
            state.http.session = hya_net::cookies::netscape::parse(&jar).0;
        }
        let dispatcher = Dispatcher::new(state);
        let shared = dispatcher.state();
        let outcome = self.runtime.call(
            &compiled,
            Box::new(dispatcher),
            &context.ctl,
            method,
            request,
        );
        let state = shared.lock().map_err(error)?;
        write_atomic(
            &base.join("pins.json"),
            &serde_json::to_vec(&state.pins).map_err(error)?,
        )?;
        write_atomic(
            &base.join("storage.json"),
            &serde_json::to_vec(&state.storage).map_err(error)?,
        )?;
        let (jar, _) = hya_net::cookies::netscape::render(
            &state.http.session,
            true,
            hya_net::cookies::now_secs(),
        );
        write_atomic(&base.join("session.jar"), jar.as_bytes())?;
        if !outcome.output.is_empty() {
            let mut output = outcome.output;
            for value in state
                .secrets
                .values()
                .filter_map(Value::as_text)
                .filter(|s| !s.is_empty())
            {
                output = output.replace(value, "[redacted]");
            }
            append_log(&base.join("log.txt"), &output)?;
        }
        outcome.result.map_err(|mut e| {
            for value in state
                .secrets
                .values()
                .filter_map(Value::as_text)
                .filter(|s| !s.is_empty())
            {
                e.message = e.message.replace(value, "[redacted]");
            }
            e
        })
    }
    /// Runs declared processors on isolated copies, then sends completion hooks.
    pub fn finish<C: Connector + 'static>(
        &self,
        request: hya_plugin_api::CompleteRequest,
        path: &Path,
        connector: impl Fn() -> C,
        frontend: impl Fn(&str) -> Box<dyn Frontend>,
        context: ResolveContext,
    ) -> Result<(), PluginError> {
        for plugin in self.list().iter().filter(|p| {
            p.enabled
                && p.manifest.claims.iter().any(|pattern| {
                    UrlPattern::parse(pattern).is_ok_and(|pattern| pattern.matches(&request.url))
                })
        }) {
            let result = (|| {
                if plugin.manifest.hooks.iter().any(|h| h == "process") {
                    if !plugin.grants.data {
                        return Err(PluginError::new(
                            ErrorCode::PermissionDenied,
                            "processing files needs the data grant",
                        ));
                    }
                    let size = std::fs::metadata(path).map_err(error)?.len();
                    let root = self.root.join(&plugin.manifest.id).join("data");
                    if crate::host::data_size(&root)?.saturating_add(size.saturating_mul(2))
                        > hya_plugin_api::limits::MAX_DATA_DIR
                    {
                        return Err(PluginError::new(
                            ErrorCode::InvalidInput,
                            "processing copy exceeds plugin data quota",
                        ));
                    }
                    let scratch = tempfile::Builder::new()
                        .prefix("job-")
                        .tempdir_in(&root)
                        .map_err(error)?;
                    let extension = path
                        .extension()
                        .and_then(|s| s.to_str())
                        .filter(|s| s.bytes().all(|b| b.is_ascii_alphanumeric()))
                        .unwrap_or("bin");
                    let input = scratch.path().join(format!("input.{extension}"));
                    let output = scratch.path().join(format!("output.{extension}"));
                    std::fs::copy(path, &input).map_err(error)?;
                    let process = hya_plugin_api::ProcessRequest {
                        job: request.clone(),
                        input: input
                            .strip_prefix(&root)
                            .map_err(error)?
                            .to_string_lossy()
                            .replace('\\', "/"),
                        output: output
                            .strip_prefix(&root)
                            .map_err(error)?
                            .to_string_lossy()
                            .replace('\\', "/"),
                    };
                    let reply = self.invoke(
                        plugin,
                        connector(),
                        frontend(&plugin.manifest.id),
                        &context,
                        "process",
                        &serde_json::to_vec(&process).map_err(error)?,
                    )?;
                    let reply: hya_plugin_api::Processed =
                        serde_json::from_value(reply).map_err(error)?;
                    if reply.changed {
                        let metadata = std::fs::symlink_metadata(&output).map_err(error)?;
                        if !metadata.is_file()
                            || metadata.len() > hya_plugin_api::limits::MAX_DATA_DIR
                            || !std::fs::canonicalize(&output)
                                .map_err(error)?
                                .starts_with(std::fs::canonicalize(scratch.path()).map_err(error)?)
                        {
                            return Err(PluginError::new(
                                ErrorCode::InvalidReply,
                                "processor output is outside its isolated directory",
                            ));
                        }
                        let mut staged = tempfile::NamedTempFile::new_in(
                            path.parent().unwrap_or_else(|| Path::new(".")),
                        )
                        .map_err(error)?;
                        std::io::copy(
                            &mut std::fs::File::open(&output).map_err(error)?,
                            &mut staged,
                        )
                        .map_err(error)?;
                        staged.as_file().sync_all().map_err(error)?;
                        staged.persist(path).map_err(error)?;
                    }
                }
                if plugin.manifest.hooks.iter().any(|h| h == "complete") {
                    let mut request = request.clone();
                    request.size = std::fs::metadata(path).map_err(error)?.len();
                    self.invoke(
                        plugin,
                        connector(),
                        frontend(&plugin.manifest.id),
                        &context,
                        "complete",
                        &serde_json::to_vec(&request).map_err(error)?,
                    )?;
                }
                Ok(())
            })();
            result.map_err(|mut e: PluginError| {
                e.message = format!("{}: {}", plugin.manifest.id, e.message);
                e
            })?;
        }
        Ok(())
    }

    /// Refreshes transport or resolves again, preserving the requested plan and track identities.
    pub fn refresh_plan<C: Connector + 'static>(
        &mut self,
        id: &str,
        request: ResolveRequest,
        mut plan: hya_plugin_api::Plan,
        track_ids: Vec<String>,
        connector: impl Fn() -> C,
        frontend: impl Fn(&str) -> Box<dyn Frontend>,
        mut context: ResolveContext,
    ) -> Result<hya_plugin_api::Plan, PluginError> {
        context.only = Some(id.into());
        let refresh = self.refresh(
            id,
            hya_plugin_api::RefreshRequest {
                url: request.url.clone(),
                plan_id: plan.id.clone(),
                track_ids: track_ids.clone(),
            },
            connector(),
            frontend(id),
            context.clone(),
        );
        match refresh {
            Ok(reply) => {
                for track in reply.tracks {
                    let previous = plan
                        .tracks
                        .iter_mut()
                        .find(|t| t.id == track.id)
                        .ok_or_else(|| {
                            PluginError::new(ErrorCode::InvalidPlan, "unknown refreshed track")
                        })?;
                    previous.sources = track.sources;
                    previous.headers = track.headers;
                    previous.size = track.size;
                    previous.digest = track.digest;
                    previous.container = track.container;
                    previous.codec = track.codec;
                    previous.expires_at = track.expires_at;
                }
                plan.expires_at = None;
                Ok(plan)
            }
            Err(error) if error.code == ErrorCode::Cancelled => Err(error),
            Err(_) => {
                let (_, next) = self
                    .resolve_with_context(connector, request, frontend, context)?
                    .ok_or_else(|| {
                        PluginError::new(
                            ErrorCode::NotClaimed,
                            "plugin no longer claims this address",
                        )
                    })?;
                if plan.id != next.id {
                    return Err(PluginError::new(
                        ErrorCode::InvalidPlan,
                        "plan identity changed",
                    ));
                }
                for id in track_ids {
                    let track = next
                        .track(&id)
                        .ok_or_else(|| {
                            PluginError::new(
                                ErrorCode::InvalidPlan,
                                format!("track {id} disappeared"),
                            )
                        })?
                        .clone();
                    let previous =
                        plan.tracks.iter_mut().find(|t| t.id == id).ok_or_else(|| {
                            PluginError::new(ErrorCode::InvalidInput, "unknown selected track")
                        })?;
                    *previous = track;
                }
                plan.expires_at = next.expires_at;
                Ok(plan)
            }
        }
    }

    /// Refreshes selected tracks through the same capability and plan validation.
    pub fn refresh<C: Connector + 'static>(
        &self,
        id: &str,
        request: hya_plugin_api::RefreshRequest,
        connector: C,
        frontend: Box<dyn Frontend>,
        context: ResolveContext,
    ) -> Result<hya_plugin_api::Refreshed, PluginError> {
        let p = self
            .list()
            .iter()
            .find(|p| p.manifest.id == id && p.enabled)
            .ok_or_else(|| error("plugin is unavailable"))?;
        if !p.manifest.hooks.iter().any(|h| h == "refresh") {
            return Err(PluginError::new(
                ErrorCode::Unsupported,
                "plugin has no refresh hook",
            ));
        }
        if request.track_ids.is_empty()
            || request.track_ids.len() > hya_plugin_api::limits::MAX_TRACKS
        {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "invalid refresh track count",
            ));
        }
        let value = self.invoke(
            p,
            connector,
            frontend,
            &context,
            "refresh",
            &serde_json::to_vec(&request).map_err(error)?,
        )?;
        let mut reply: hya_plugin_api::Refreshed = serde_json::from_value(value)
            .map_err(|e| PluginError::new(ErrorCode::InvalidReply, e.to_string()))?;
        let expected: std::collections::BTreeSet<_> = request.track_ids.iter().collect();
        let actual: std::collections::BTreeSet<_> = reply.tracks.iter().map(|t| &t.id).collect();
        if expected != actual || actual.len() != reply.tracks.len() {
            return Err(PluginError::new(
                ErrorCode::InvalidPlan,
                "refresh must return each requested track exactly once",
            ));
        }
        let allowed = HostList::parse(&p.grants.sources).map_err(error)?;
        for track in &mut reply.tracks {
            let mut candidate = hya_plugin_api::Track::file(&track.id, "");
            candidate.sources = track.sources.clone();
            candidate.headers = track.headers.clone();
            candidate.size = track.size;
            candidate.digest = track.digest.clone();
            candidate.codec = track.codec.clone();
            candidate.container = track.container.clone();
            let mut plan = hya_plugin_api::Plan::single(&request.plan_id, candidate);
            accept::accept(
                &mut plan,
                &allowed,
                Clamps {
                    max_connections: 16,
                },
            )?;
            track.sources = plan.tracks.remove(0).sources;
        }
        Ok(reply)
    }

    /// Runs the plugin's declared prerequisite check using current grants and settings.
    pub fn check<C: Connector + 'static>(
        &self,
        id: &str,
        connector: C,
        frontend: Box<dyn Frontend>,
    ) -> Result<(), PluginError> {
        let p = self
            .list()
            .iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| error("unknown plugin"))?;
        if p.manifest.hooks.iter().any(|hook| hook == "check") {
            self.invoke(
                p,
                connector,
                frontend,
                &ResolveContext::default(),
                "check",
                b"{}",
            )?;
        }
        Ok(())
    }
    /// Reads the bounded, redacted log for an installed plugin.
    pub fn logs(&self, id: &str) -> Result<String, PluginError> {
        if !self.list().iter().any(|p| p.manifest.id == id) {
            return Err(error("unknown plugin"));
        }
        match std::fs::read_to_string(self.root.join(id).join("log.txt")) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(error(e)),
        }
    }
    /// Persists a declared secret separately from public plugin configuration.
    pub fn set_secret(&mut self, id: &str, key: &str, value: String) -> Result<(), PluginError> {
        let p = self.installed_mut(id)?;
        let field = p
            .manifest
            .settings
            .iter()
            .find(|f| f.key == key && f.kind == hya_plugin_api::FieldKind::Secret)
            .ok_or_else(|| error("setting is not a declared secret"))?;
        field.validate(&Value::Text(value.clone())).map_err(error)?;
        let path = self.root.join(id).join("secrets.bin");
        let mut secrets = crate::secrets::read(&path)?;
        secrets.insert(key.into(), Value::Text(value));
        crate::secrets::write(&path, &secrets)
    }
    /// Resolves through enabled matching plugins and accepts every returned source.
    pub fn resolve<C: Connector + 'static>(
        &mut self,
        connector: impl Fn() -> C,
        request: ResolveRequest,
        frontend: impl Fn(&str) -> Box<dyn Frontend>,
    ) -> Result<Option<(String, hya_plugin_api::Plan)>, PluginError> {
        self.resolve_with_context(connector, request, frontend, ResolveContext::default())
    }
    /// Resolves with caller cancellation, cookies and an optional explicit plugin.
    pub fn resolve_with_context<C: Connector + 'static>(
        &mut self,
        connector: impl Fn() -> C,
        request: ResolveRequest,
        frontend: impl Fn(&str) -> Box<dyn Frontend>,
        context: ResolveContext,
    ) -> Result<Option<(String, hya_plugin_api::Plan)>, PluginError> {
        let mut last_error = None;
        for i in 0..self.state.plugins.len() {
            let p = self.state.plugins[i].clone();
            if !p.enabled
                || context.only.as_ref().is_some_and(|id| id != &p.manifest.id)
                || !crate::matcher::claims_input(&p.manifest, &request.url)
            {
                continue;
            }
            let result: Result<Option<hya_plugin_api::Plan>, PluginError> = (|| {
                if p.manifest.hooks.iter().any(|h| h == "enqueue") {
                    let reply = self.invoke(
                        &p,
                        connector(),
                        frontend(&p.manifest.id),
                        &context,
                        "enqueue",
                        &serde_json::to_vec(&request).map_err(error)?,
                    )?;
                    let decision: hya_plugin_api::EnqueueDecision =
                        serde_json::from_value(reply).map_err(error)?;
                    if !decision.allow {
                        return Err(PluginError::new(
                            ErrorCode::Cancelled,
                            decision
                                .reason
                                .unwrap_or_else(|| "job declined by plugin rule".into()),
                        ));
                    }
                }
                if !p.manifest.hooks.iter().any(|h| h == "resolve") {
                    return Ok(None);
                }
                let value = self.invoke(
                    &p,
                    connector(),
                    frontend(&p.manifest.id),
                    &context,
                    "resolve",
                    &serde_json::to_vec(&request).map_err(error)?,
                )?;
                let reply: Resolve = serde_json::from_value(value)
                    .map_err(|e| PluginError::new(ErrorCode::InvalidReply, e.to_string()))?;
                match reply {
                    Resolve::NotClaimed => Ok(None),
                    Resolve::Plan(mut plan) => {
                        accept::accept(
                            &mut plan,
                            &HostList::parse(&p.grants.sources).map_err(error)?,
                            Clamps {
                                max_connections: 16,
                            },
                        )?;
                        Ok(Some(plan))
                    }
                }
            })();
            match result {
                Ok(plan) => {
                    self.state.plugins[i].failures = 0;
                    self.save()?;
                    if let Some(plan) = plan {
                        return Ok(Some((p.manifest.id, plan)));
                    }
                }
                Err(mut e) => {
                    if e.code.counts_toward_breaker() {
                        let installed = &mut self.state.plugins[i];
                        installed.failures += 1;
                        if installed.failures >= 3 {
                            installed.enabled = false;
                        }
                        self.save()?;
                    }
                    e.message = format!("{}: {}", p.manifest.id, e.message);
                    if context.only.is_some() || e.code == ErrorCode::Cancelled {
                        return Err(e);
                    }
                    last_error = Some(e);
                }
            }
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        if let Some(id) = context.only {
            return Err(PluginError::new(
                ErrorCode::NotClaimed,
                format!("plugin {id} is unavailable or did not claim this address"),
            ));
        }
        Ok(None)
    }
}

struct LoggedFrontend {
    inner: Box<dyn Frontend>,
    path: PathBuf,
}
impl Frontend for LoggedFrontend {
    fn prompt(
        &mut self,
        form: hya_plugin_api::Form,
    ) -> Result<hya_plugin_api::Answers, PluginError> {
        self.inner.prompt(form)
    }
    fn prompt_with_ctl(
        &mut self,
        form: hya_plugin_api::Form,
        ctl: &CallCtl,
    ) -> Result<hya_plugin_api::Answers, PluginError> {
        self.inner.prompt_with_ctl(form, ctl)
    }
    fn log(&mut self, level: &str, message: &str) {
        let _ = append_log(&self.path, &format!("[{level}] {message}"));
        self.inner.log(level, message);
    }
    fn progress(&mut self, done: u64, total: Option<u64>, note: Option<&str>) {
        self.inner.progress(done, total, note);
    }
}
fn append_log(path: &Path, message: &str) -> Result<(), PluginError> {
    const CAP: usize = 1024 * 1024;
    let mut bytes = std::fs::read(path).unwrap_or_default();
    bytes.extend_from_slice(message.as_bytes());
    bytes.push(b'\n');
    if bytes.len() > CAP {
        let mut offset = bytes.len() - CAP;
        while offset < bytes.len() && bytes[offset] & 0xc0 == 0x80 {
            offset += 1;
        }
        bytes.drain(..offset);
    }
    write_atomic(path, &bytes)
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, PluginError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(error(e)),
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PluginError> {
    let parent = path.parent().ok_or_else(|| error("file has no parent"))?;
    std::fs::create_dir_all(parent).map_err(error)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
    file.write_all(bytes).map_err(error)?;
    file.as_file().sync_all().map_err(error)?;
    file.persist(path).map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(dir: &Path, reply: &str) {
        let encoded: String = reply
            .as_bytes()
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect();
        let wasm = wat::parse_str(format!(r#"(module
          (memory (export "memory") 2)
          (global $heap (mut i32) (i32.const 4096))
          (data (i32.const 128) "{encoded}")
          (func (export "hydra_api") (result i32) i32.const 1)
          (func (export "hydra_alloc") (param $n i32) (result i32)
            (local $p i32) global.get $heap local.set $p global.get $heap local.get $n i32.add global.set $heap local.get $p)
          (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const {}))"#,hya_plugin_api::abi::pack(128,reply.len() as u32))).unwrap();
        std::fs::write(dir.join("plugin.wasm"), wasm).unwrap();
        std::fs::write(
            dir.join("hydra-plugin.toml"),
            include_str!("../../../examples/plugins/direct/hydra-plugin.toml"),
        )
        .unwrap();
    }
    fn official_package(pair: &minisign::KeyPair, version: &str, extra: &str) -> Vec<u8> {
        official_package_with_native(pair, version, extra, false)
    }

    fn official_package_with_native(
        pair: &minisign::KeyPair,
        version: &str,
        extra: &str,
        native: bool,
    ) -> Vec<u8> {
        let directory = tempfile::tempdir().unwrap();
        fixture(directory.path(), r#"{"skip":true}"#);
        let manifest_path = directory.path().join(package::MANIFEST_NAME);
        let manifest = std::fs::read_to_string(&manifest_path)
            .unwrap()
            .replace("0.1.0", version);
        let mut manifest = format!("publisher_key = {:?}\n{extra}\n{manifest}\n[[settings]]\nkey='quality'\nlabel='Quality'\ntype='text'\ndefault='best'\n", pair.pk.to_base64());
        if native {
            manifest = manifest.replace("[permissions]", "[permissions]\nnative=['test-engine']");
            manifest.push_str(&format!(
                "\n[[native_modules]]\nid='test-engine'\nplatform='{}'\nmodule='test-native.so'\n",
                crate::native::platform()
            ));
        }
        std::fs::write(&manifest_path, manifest).unwrap();
        let mut entries = BTreeMap::new();
        for name in [package::MANIFEST_NAME, "plugin.wasm"] {
            entries.insert(name, std::fs::read(directory.path().join(name)).unwrap());
        }
        if native {
            entries.insert(
                "test-native.so",
                format!("native fixture {version}").into_bytes(),
            );
        }
        let sums = entries
            .iter()
            .map(|(name, data)| format!("{}  {name}\n", package::sha256_hex(data)))
            .collect::<String>();
        let signature = minisign::sign(
            Some(&pair.pk),
            &pair.sk,
            std::io::Cursor::new(sums.as_bytes()),
            None,
            None,
        )
        .unwrap();
        entries.insert(package::SUMS_NAME, sums.into_bytes());
        entries.insert(package::SIGNATURE_NAME, signature.to_string().into_bytes());
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    #[test]
    fn official_native_sync_installs_and_updates_while_preserving_transfer_data() {
        let root = tempfile::tempdir().unwrap();
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let first = official_package_with_native(&pair, "1.0.0", "", true);
        let second = official_package_with_native(&pair, "1.1.0", "", true);
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager
            .sync_official(&[&first], &pair.pk.to_base64())
            .unwrap();
        assert_eq!(manager.list()[0].grants.native, ["test-engine"]);
        let old_hash = manager.list()[0].native_sha256["test-native.so"].clone();
        manager.enable("example.direct", false).unwrap();
        manager
            .set("example.direct", "quality", Value::Text("small".into()))
            .unwrap();
        let checkpoint = root.path().join("example.direct/data/torrent.resume");
        std::fs::write(&checkpoint, "saved checkpoint").unwrap();
        manager
            .sync_official(&[&second], &pair.pk.to_base64())
            .unwrap();
        let installed = &manager.list()[0];
        assert_eq!(installed.manifest.version, "1.1.0");
        assert_ne!(installed.native_sha256["test-native.so"], old_hash);
        assert!(!installed.enabled);
        assert_eq!(installed.settings["quality"], Value::Text("small".into()));
        assert_eq!(
            std::fs::read_to_string(checkpoint).unwrap(),
            "saved checkpoint"
        );
    }

    #[test]
    fn official_sync_installs_once_and_updates_without_losing_user_state() {
        let root = tempfile::tempdir().unwrap();
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let key = pair.pk.to_base64();
        let first = official_package(&pair, "1.0.0", "");
        let second = official_package(&pair, "1.1.0", "");
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.sync_official(&[&first], &key).unwrap();
        assert!(manager.list()[0].enabled);
        assert!(!manager.list()[0].dev);
        assert!(matches!(
            manager.list()[0].signing,
            package::Signing::Verified { .. }
        ));
        manager
            .set("example.direct", "quality", Value::Text("small".into()))
            .unwrap();
        manager.enable("example.direct", false).unwrap();
        let data = root.path().join("example.direct/data/saved");
        std::fs::write(&data, "retained").unwrap();
        manager.sync_official(&[&first], &key).unwrap();
        assert!(manager.list()[0].previous.is_none());
        manager.sync_official(&[&second], &key).unwrap();
        assert_eq!(manager.list()[0].manifest.version, "1.1.0");
        assert!(!manager.list()[0].enabled);
        assert_eq!(
            manager.list()[0].settings["quality"],
            Value::Text("small".into())
        );
        assert_eq!(std::fs::read_to_string(data).unwrap(), "retained");
        assert_eq!(
            manager.list()[0]
                .previous
                .as_ref()
                .unwrap()
                .manifest
                .version,
            "1.0.0"
        );
        drop(manager);
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.sync_official(&[&second], &key).unwrap();
        assert!(!manager.list()[0].enabled);
        manager.sync_official(&[&first], &key).unwrap();
        assert_eq!(manager.list()[0].manifest.version, "1.1.0");
        manager.remove("example.direct").unwrap();
        let third = official_package(&pair, "1.2.0", "");
        manager.sync_official(&[&third], &key).unwrap();
        assert!(manager.list().is_empty());
    }

    #[test]
    fn official_sync_preserves_other_publishers_and_development_overrides() {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let key = pair.pk.to_base64();
        let official = official_package(&pair, "1.1.0", "");
        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let custom = official_package(&other, "1.0.0", "");
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("custom.hyaplugin");
        std::fs::write(&source, custom).unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager
            .install(
                &source,
                Manager::inspect(&source).unwrap().manifest.permissions,
            )
            .unwrap();
        manager.sync_official(&[&official], &key).unwrap();
        assert_eq!(
            manager.list()[0].manifest.publisher_key.as_deref(),
            Some(other.pk.to_base64().as_str())
        );
        manager.remove("example.direct").unwrap();
        fixture(directory.path(), r#"{"skip":true}"#);
        manager
            .install(
                directory.path(),
                Manager::inspect(directory.path())
                    .unwrap()
                    .manifest
                    .permissions,
            )
            .unwrap();
        let newer = official_package(&pair, "2.0.0", "");
        manager.sync_official(&[&newer], &key).unwrap();
        assert!(manager.list()[0].dev);
    }

    #[test]
    fn official_sync_rejects_untrusted_packages_and_corrupt_tracking() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        assert!(manager.sync_official(&[], "").is_ok());
        assert!(manager.sync_official(&[b"broken"], "").is_err());
        let directory = tempfile::tempdir().unwrap();
        fixture(directory.path(), r#"{"skip":true}"#);
        let unsigned = package::pack(directory.path()).unwrap();
        assert!(manager.sync_official(&[&unsigned], "").is_err());
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signed = official_package(&pair, "1.0.0", "");
        assert!(manager
            .sync_official(&[&signed], crate::official::KEY)
            .is_err());
        assert!(manager.list().is_empty());
        let future = official_package(&pair, "1.0.0", "min_hydra='999.0.0'");
        manager
            .sync_official(&[&future], &pair.pk.to_base64())
            .unwrap();
        assert!(manager.list().is_empty());
        std::fs::write(root.path().join("official.json"), "broken").unwrap();
        assert!(manager
            .sync_official(&[&signed], &pair.pk.to_base64())
            .is_err());
    }

    struct Quiet;
    impl Frontend for Quiet {
        fn prompt(
            &mut self,
            _: hya_plugin_api::Form,
        ) -> Result<hya_plugin_api::Answers, PluginError> {
            unreachable!()
        }
        fn log(&mut self, _: &str, _: &str) {}
        fn progress(&mut self, _: u64, _: Option<u64>, _: Option<&str>) {}
    }
    fn resolve(
        manager: &mut Manager,
    ) -> Result<Option<(String, hya_plugin_api::Plan)>, PluginError> {
        manager.resolve(
            || hya_net::TcpConnector,
            ResolveRequest {
                url: "https://example.com/file".into(),
                ..Default::default()
            },
            |_| Box::new(Quiet),
        )
    }
    #[test]
    fn consent_persistence_disable_order_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            r#"{"plan":{"id":"p","tracks":[{"id":"f","kind":"file","sources":[{"url":"https://example.com/file"}]}]}}"#,
        );
        let mut m = Manager::open(root.path().to_path_buf()).unwrap();
        assert!(m.install(dir.path(), Permissions::default()).is_err());
        let grants = Manager::inspect(dir.path()).unwrap().manifest.permissions;
        m.install(dir.path(), grants).unwrap();
        assert!(resolve(&mut m).unwrap().is_some());
        assert!(m.order(&[]).is_err());
        m.order(&["example.direct".into()]).unwrap();
        m.enable("example.direct", false).unwrap();
        assert!(resolve(&mut m).unwrap().is_none());
        drop(m);
        let mut m = Manager::open(root.path().to_path_buf()).unwrap();
        assert!(!m.list()[0].enabled);
        m.enable("example.direct", true).unwrap();
        assert!(resolve(&mut m).unwrap().is_some());
        m.remove("example.direct").unwrap();
        assert!(m.list().is_empty());
        assert!(m.remove("example.direct").is_err());
    }
    #[test]
    fn forbidden_source_trips_breaker_and_manifest_change_requires_consent() {
        let dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            r#"{"plan":{"id":"p","tracks":[{"id":"f","kind":"file","sources":[{"url":"https://evil.test/file"}]}]}}"#,
        );
        let mut m = Manager::open(root.path().to_path_buf()).unwrap();
        let grants = Manager::inspect(dir.path()).unwrap().manifest.permissions;
        m.install(dir.path(), grants).unwrap();
        for _ in 0..3 {
            assert_eq!(resolve(&mut m).unwrap_err().code, ErrorCode::InvalidPlan);
        }
        assert!(!m.list()[0].enabled);
        m.enable("example.direct", true).unwrap();
        let path = dir.path().join("hydra-plugin.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("version = \"0.1.0\"", "version = \"0.2.0\"");
        std::fs::write(path, text).unwrap();
        assert_eq!(
            resolve(&mut m).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(m.list()[0].failures, 0);
    }
    #[test]
    fn secret_settings_stay_out_of_state_and_errors_and_logs_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            r#"{"error":{"code":"invalid_input","message":"test-secret-value"}}"#,
        );
        let path = dir.path().join("hydra-plugin.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[[settings]]\nkey = \"token\"\nlabel = \"Token\"\ntype = \"secret\"\n");
        std::fs::write(path, text).unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        let permissions = Manager::inspect(dir.path()).unwrap().manifest.permissions;
        manager.install(dir.path(), permissions).unwrap();
        assert!(manager
            .set("example.direct", "token", Value::Text("public".into()))
            .is_err());
        manager
            .set_secret("example.direct", "token", "test-secret-value".into())
            .unwrap();
        assert!(!std::fs::read_to_string(root.path().join("state.toml"))
            .unwrap()
            .contains("test-secret-value"));
        assert_eq!(
            resolve(&mut manager).unwrap_err().message,
            "example.direct: [redacted]"
        );
        assert!(manager
            .set_secret("example.direct", "undeclared", "secret".into())
            .is_err());
        let log = root.path().join("example.direct/log.txt");
        append_log(&log, &"🦀".repeat(300_000)).unwrap();
        append_log(&log, "latest").unwrap();
        let text = manager.logs("example.direct").unwrap();
        assert!(text.len() <= 1024 * 1024);
        assert!(text.ends_with("latest\n"));
    }
    #[test]
    fn refresh_rejects_missing_duplicate_and_forbidden_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for (reply, valid) in [
            (
                r#"{"tracks":[{"id":"f","sources":[{"url":"https://example.com/new"}]}]}"#,
                true,
            ),
            (r#"{"tracks":[]}"#, false),
            (
                r#"{"tracks":[{"id":"f","sources":[{"url":"https://evil.test/new"}]}]}"#,
                false,
            ),
            (
                r#"{"tracks":[{"id":"f","sources":[]},{"id":"f","sources":[]}]}"#,
                false,
            ),
        ] {
            fixture(dir.path(), reply);
            let path = dir.path().join("hydra-plugin.toml");
            let text = std::fs::read_to_string(&path).unwrap().replace(
                "hooks = [\"resolve\"]",
                "hooks = [\"resolve\", \"refresh\"]",
            );
            std::fs::write(path, text).unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            let permissions = Manager::inspect(dir.path()).unwrap().manifest.permissions;
            manager.install(dir.path(), permissions).unwrap();
            let result = manager.refresh(
                "example.direct",
                hya_plugin_api::RefreshRequest {
                    url: "https://example.com/file".into(),
                    plan_id: "p".into(),
                    track_ids: vec!["f".into()],
                },
                hya_net::TcpConnector,
                Box::new(Quiet),
                ResolveContext::default(),
            );
            assert_eq!(result.is_ok(), valid, "{result:?}");
        }
    }
    #[test]
    fn processing_replaces_only_a_valid_isolated_output_and_preserves_input_on_failure() {
        struct Writer {
            root: PathBuf,
            write: bool,
        }
        impl Frontend for Writer {
            fn prompt(
                &mut self,
                _: hya_plugin_api::Form,
            ) -> Result<hya_plugin_api::Answers, PluginError> {
                unreachable!()
            }
            fn log(&mut self, _: &str, _: &str) {
                if !self.write {
                    return;
                }
                for entry in std::fs::read_dir(&self.root).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_name().to_string_lossy().starts_with("job-") {
                        let mut bytes = std::fs::read(entry.path().join("input.txt")).unwrap();
                        bytes.extend_from_slice(b" processed");
                        std::fs::write(entry.path().join("output.txt"), bytes).unwrap();
                    }
                }
            }
            fn progress(&mut self, _: u64, _: Option<u64>, _: Option<&str>) {}
        }
        for write in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = tempfile::tempdir().unwrap();
            fixture(dir.path(), r#"{"changed":true}"#);
            let text = std::fs::read_to_string(dir.path().join("hydra-plugin.toml"))
                .unwrap()
                .replace(
                    "hooks = [\"resolve\", \"refresh\", \"check\"]",
                    "hooks = [\"process\", \"complete\"]",
                )
                .replace("[permissions]", "[permissions]\ndata = true");
            std::fs::write(dir.path().join("hydra-plugin.toml"), text).unwrap();
            let log = r#"{"level":"info","message":"process fixture"}"#;
            let encoded: String = log.bytes().map(|b| format!("\\{b:02x}")).collect();
            let reply = r#"{"changed":true}"#;
            let encoded_reply: String = reply.bytes().map(|b| format!("\\{b:02x}")).collect();
            let wasm = wat::parse_str(format!(r#"(module
                (import "hydra" "hydra_host_call" (func $host (param i32 i32 i32 i32) (result i64)))
                (memory (export "memory") 2)
                (data (i32.const 0) "log") (data (i32.const 16) "{encoded}") (data (i32.const 128) "{encoded_reply}")
                (global $heap (mut i32) (i32.const 4096))
                (func (export "hydra_api") (result i32) i32.const 1)
                (func (export "hydra_alloc") (param $n i32) (result i32) (local $p i32) global.get $heap local.set $p global.get $heap local.get $n i32.add global.set $heap local.get $p)
                (func (export "hydra_call") (param i32 i32 i32 i32) (result i64)
                  i32.const 0 i32.const 3 i32.const 16 i32.const {} call $host drop i64.const {}))"#,log.len(),hya_plugin_api::abi::pack(128,reply.len() as u32))).unwrap();
            std::fs::write(dir.path().join("plugin.wasm"), wasm).unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            let grants = Manager::inspect(dir.path()).unwrap().manifest.permissions;
            manager.install(dir.path(), grants).unwrap();
            let output = root.path().join("download.txt");
            std::fs::write(&output, b"original").unwrap();
            let request = hya_plugin_api::CompleteRequest {
                url: "https://example.com/file".into(),
                name: "download.txt".into(),
                size: 8,
            };
            let result = manager.finish(
                request,
                &output,
                || hya_net::TcpConnector,
                |_| {
                    Box::new(Writer {
                        root: root.path().join("example.direct/data"),
                        write,
                    })
                },
                Default::default(),
            );
            assert_eq!(result.is_ok(), write, "{result:?}");
            assert_eq!(
                std::fs::read(&output).unwrap(),
                if write {
                    b"original processed".as_slice()
                } else {
                    b"original".as_slice()
                }
            );
            assert_eq!(
                std::fs::read_dir(root.path().join("example.direct/data"))
                    .unwrap()
                    .count(),
                0
            );
            assert!(manager
                .logs("example.direct")
                .unwrap()
                .contains("process fixture"));
        }
    }
    #[test]
    fn broken_state_is_not_treated_as_an_empty_installation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("state.toml"), "[broken").unwrap();
        assert!(Manager::open(root.path().to_path_buf()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn native_installation_pins_code_and_revoking_permission_blocks_execution() {
        let directory = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fixture(directory.path(), r#""not_claimed""#);
        let path = directory.path().join(package::MANIFEST_NAME);
        let mut manifest = std::fs::read_to_string(&path).unwrap();
        manifest = manifest.replace("[permissions]", "[permissions]\nnative=['test-engine']");
        manifest += &format!(
            "\n[[native_modules]]\nid='test-engine'\nplatform='{}'\nmodule='test.so'\n",
            crate::native::platform()
        );
        std::fs::write(&path, manifest).unwrap();
        std::fs::copy(
            crate::native::tests::fixture().path,
            directory.path().join("test.so"),
        )
        .unwrap();
        let grants = package::load_dir(directory.path())
            .unwrap()
            .manifest
            .permissions;
        let mut manager = Manager::open(root.path().to_owned()).unwrap();
        let id = manager.install(directory.path(), grants).unwrap();
        assert_eq!(manager.list()[0].native_sha256.len(), 1);
        drop(manager);
        let module =
            crate::transfer::authorize(root.path().to_owned(), &id, "test-engine").unwrap();
        module.call("inspect", b"{}", |_| Ok(()), || false).unwrap();
        std::fs::write(directory.path().join("test.so"), b"changed").unwrap();
        assert_eq!(
            module
                .call("inspect", b"{}", |_| Ok(()), || false)
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        let mut manager = Manager::open(root.path().to_owned()).unwrap();
        manager
            .permission(&id, "native:test-engine", false)
            .unwrap();
        drop(manager);
        assert_eq!(
            crate::transfer::authorize(root.path().to_owned(), &id, "test-engine")
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
    }
}
