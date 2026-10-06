//! `hydra-plugin.toml`: identity, claims, permissions and settings.

use serde::{Deserialize, Serialize};

use crate::form::Field;
use crate::limits::{DEFAULT_MEMORY_MB, MAX_MEMORY_MB};

fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecEntry {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub http: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub cookies: Vec<String>,
    #[serde(default)]
    pub data: bool,
    #[serde(default)]
    pub exec: Vec<ExecEntry>,
    #[serde(default)]
    pub native: Vec<String>,
    #[serde(default)]
    pub exec_from_data: bool,
}

/// A packaged native library for a specific operating system and architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeModule {
    pub id: String,
    /// For example `macos-aarch64`, `linux-x86_64` or `windows-x86_64`.
    pub platform: String,
    pub module: String,
}

/// A file picker offered by an enabled resolver in the Add URL dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAction {
    pub label: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    /// Author display name supplied by the plugin, independent of its signing key.
    #[serde(default)]
    pub author: Option<String>,
    pub version: String,
    pub api: i32,
    #[serde(default)]
    pub min_hydra: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    /// Optional setup instructions shown after installation and in plugin information.
    #[serde(default)]
    pub welcome: Option<String>,
    pub module: String,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default)]
    pub publisher_key: Option<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub claims: Vec<String>,
    #[serde(default)]
    pub input_actions: Vec<InputAction>,
    #[serde(default)]
    pub native_modules: Vec<NativeModule>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub settings: Vec<Field>,
}

impl Manifest {
    /// Checks the fields whose shape the format fixes.
    ///
    /// # Errors
    /// Returns the first violated rule.
    pub fn validate(&self) -> Result<(), String> {
        valid_id(&self.id)?;
        let mut modules = std::collections::HashSet::new();
        if self.native_modules.len() > 8 {
            return Err("too many native modules".into());
        }
        for module in &self.native_modules {
            if module.id.is_empty()
                || module.id.len() > 80
                || !module
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || module.platform.is_empty()
                || !module
                    .platform
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
                || module.module.is_empty()
                || module.module.contains(['/', '\\', '\0'])
                || !matches!(
                    std::path::Path::new(&module.module)
                        .extension()
                        .and_then(|s| s.to_str()),
                    Some("so" | "dylib" | "dll")
                )
                || !modules.insert((&module.id, &module.platform))
            {
                return Err("invalid or duplicate native module".into());
            }
        }
        if self
            .permissions
            .native
            .iter()
            .any(|id| !self.native_modules.iter().any(|module| &module.id == id))
        {
            return Err("native permission names an undeclared module".into());
        }
        if self.input_actions.len() > 8 {
            return Err("too many input actions".into());
        }
        for action in &self.input_actions {
            if action.label.trim().is_empty()
                || action.label.len() > 80
                || action.label.chars().any(char::is_control)
                || action.extensions.is_empty()
                || action.extensions.len() > 16
                || action.extensions.iter().any(|ext| {
                    ext.is_empty()
                        || ext.len() > 16
                        || !ext.bytes().all(|b| b.is_ascii_alphanumeric())
                })
            {
                return Err("invalid input action".into());
            }
        }
        if self
            .author
            .as_ref()
            .is_some_and(|author| author.len() > crate::limits::MAX_STRING_FIELD)
        {
            return Err("author name is too long".into());
        }
        if self
            .welcome
            .as_ref()
            .is_some_and(|text| text.len() > crate::limits::MAX_STRING_FIELD)
        {
            return Err("welcome text is too long".into());
        }
        if self.name.trim().is_empty() {
            return Err("name is empty".into());
        }
        if self.module.is_empty()
            || self.module == ".."
            || self.module.contains(['/', '\\'])
            || self.module.contains('\0')
        {
            return Err(format!("module `{}` must be a bare file name", self.module));
        }
        if self.memory_mb == 0 || self.memory_mb > MAX_MEMORY_MB {
            return Err(format!("memory_mb must be 1..={MAX_MEMORY_MB}"));
        }
        for e in &self.permissions.exec {
            if e.program.is_empty() || e.program.contains(['/', '\\']) {
                return Err(format!(
                    "exec program `{}` must be a name, not a path",
                    e.program
                ));
            }
            if is_refused_program(&e.program) {
                return Err(format!(
                    "exec program `{}` is a shell or interpreter",
                    e.program
                ));
            }
        }
        if self.settings.len() > 128 {
            return Err("too many settings".into());
        }
        for field in &self.settings {
            if field.key.is_empty()
                || field.key.len() > crate::limits::MAX_STRING_FIELD
                || field.label.is_empty()
                || field.options.len() > 128
            {
                return Err("invalid settings field".into());
            }
            if let Some(default) = &field.default {
                field.validate(default)?;
            }
        }
        let mut keys: Vec<&str> = self.settings.iter().map(|s| s.key.as_str()).collect();
        keys.sort_unstable();
        if keys.windows(2).any(|w| w[0] == w[1]) {
            return Err("duplicate settings key".into());
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> Result<(), String> {
    let ok_part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    match id.split_once('.') {
        Some((publisher, name)) if ok_part(publisher) && ok_part(name) => Ok(()),
        _ => Err(format!("id `{id}` must be publisher.name using [a-z0-9-]")),
    }
}

const REFUSED_PROGRAMS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "cmd",
    "powershell",
    "pwsh",
    "python",
    "python3",
    "node",
    "deno",
    "bun",
    "perl",
    "ruby",
    "php",
    "env",
    "xargs",
    "sudo",
    "osascript",
    "wscript",
    "cscript",
];

/// Shells and interpreters, which turn an argument list into arbitrary code.
pub fn is_refused_program(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    lower.ends_with(".cmd") || lower.ends_with(".bat") || REFUSED_PROGRAMS.contains(&stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_content_is_optional_and_bounded() {
        let mut manifest = base();
        manifest.validate().unwrap();
        manifest.welcome = Some("Install the backend, then choose a quality.".into());
        manifest.validate().unwrap();
        assert_eq!(
            serde_json::from_str::<Manifest>(&serde_json::to_string(&manifest).unwrap()).unwrap(),
            manifest
        );
        manifest.welcome = Some("x".repeat(crate::limits::MAX_STRING_FIELD + 1));
        assert!(manifest.validate().unwrap_err().contains("welcome"));
    }

    #[test]
    fn batch_files_are_shell_execution_even_when_named_like_a_tool() {
        for name in ["yt-dlp.cmd", "tool.BAT", "cmd.exe", "PowerShell.EXE"] {
            assert!(is_refused_program(name));
        }
        for name in ["yt-dlp", "yt-dlp.exe", "ffmpeg"] {
            assert!(!is_refused_program(name));
        }
    }

    fn base() -> Manifest {
        Manifest {
            id: "example.direct".into(),
            name: "Direct".into(),
            version: "1.0.0".into(),
            api: 1,
            min_hydra: None,
            license: None,
            homepage: None,
            welcome: None,
            module: "plugin.wasm".into(),
            memory_mb: 64,
            author: None,
            publisher_key: None,
            hooks: vec![],
            claims: vec![],
            input_actions: vec![],
            native_modules: vec![],
            permissions: Permissions::default(),
            settings: vec![],
        }
    }

    #[test]
    fn accepts_a_minimal_manifest() {
        assert!(base().validate().is_ok());
    }

    #[test]
    fn rejects_bad_ids() {
        for id in ["noDot", "a.B", ".x", "x.", "a.b.c", "a b.c", ""] {
            let mut m = base();
            m.id = id.into();
            assert!(m.validate().is_err(), "{id}");
        }
    }

    #[test]
    fn module_must_be_a_bare_name() {
        for module in ["", "..", "a/b.wasm", "a\\b.wasm", "../x.wasm"] {
            let mut m = base();
            m.module = module.into();
            assert!(m.validate().is_err(), "{module}");
        }
    }

    #[test]
    fn memory_bounds() {
        let mut m = base();
        m.memory_mb = 0;
        assert!(m.validate().is_err());
        m.memory_mb = MAX_MEMORY_MB;
        assert!(m.validate().is_ok());
        m.memory_mb = MAX_MEMORY_MB + 1;
        assert!(m.validate().is_err());
    }

    #[test]
    fn exec_refuses_paths_and_interpreters() {
        for program in [
            "/usr/bin/yt-dlp",
            "bin\\x",
            "bash",
            "Python3.exe",
            "PWSH",
            "env",
        ] {
            let mut m = base();
            m.permissions.exec.push(ExecEntry {
                program: program.into(),
                args: vec![],
            });
            assert!(m.validate().is_err(), "{program}");
        }
        let mut m = base();
        m.permissions.exec.push(ExecEntry {
            program: "yt-dlp".into(),
            args: vec!["--version".into()],
        });
        assert!(m.validate().is_ok());
    }

    #[test]
    fn author_metadata_roundtrips_and_has_a_size_limit() {
        let mut manifest = base();
        manifest.author = Some("Hydra Team".into());
        let decoded: Manifest =
            serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();
        assert_eq!(decoded.author.as_deref(), Some("Hydra Team"));
        manifest.author = Some("a".repeat(crate::limits::MAX_STRING_FIELD));
        assert!(manifest.validate().is_ok());
        manifest.author.as_mut().unwrap().push('a');
        assert_eq!(manifest.validate().unwrap_err(), "author name is too long");
    }

    #[test]
    fn json_defaults_fill_in() {
        let m: Manifest = serde_json::from_str(
            r#"{"id":"a.b","name":"n","version":"1","api":1,"module":"m.wasm"}"#,
        )
        .unwrap();
        assert_eq!(m.author, None);
        assert_eq!(m.memory_mb, DEFAULT_MEMORY_MB);
        assert!(!m.permissions.data);
    }
    #[test]
    fn file_actions_are_optional_and_have_bounded_labels_and_filters() {
        let mut manifest = base();
        manifest.input_actions = vec![InputAction {
            label: "Browse X File".into(),
            extensions: vec!["x".into()],
        }];
        manifest.validate().unwrap();
        for extension in ["", "../x", "x.y", "*"] {
            manifest.input_actions[0].extensions = vec![extension.into()];
            assert!(manifest.validate().is_err());
        }
        manifest.input_actions[0].extensions = vec!["x".into()];
        manifest.input_actions[0].label = " ".into();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn native_modules_require_safe_names_unique_platforms_and_declared_grants() {
        let mut manifest = base();
        let module = NativeModule {
            id: "x-engine".into(),
            platform: "linux-x86_64".into(),
            module: "x.so".into(),
        };
        manifest.native_modules.push(module.clone());
        manifest.permissions.native.push("x-engine".into());
        manifest.validate().unwrap();
        manifest.native_modules.push(module);
        assert!(manifest.validate().is_err());
        manifest.native_modules.pop();
        manifest.native_modules[0].module = "../x.so".into();
        assert!(manifest.validate().is_err());
        manifest.native_modules.clear();
        assert!(manifest.validate().is_err());
    }
}
