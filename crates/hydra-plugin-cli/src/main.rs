//! Language-independent authoring tools for Hydra's Wasm plugins.
use std::{
    fs,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hya_plugin::{
    matcher::{HostList, UrlPattern},
    package::{self, Package},
    runtime::Runtime,
};
use serde::{Deserialize, Serialize};

mod signing;

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

#[derive(Parser)]
#[command(version, about = "Create, build, sign and validate Hydra Wasm plugins")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Create a project with a vendored SDK; never overwrite an existing directory.
    Init {
        path: Option<PathBuf>,
        #[arg(long, value_enum)]
        language: Option<Language>,
        /// Human-readable plugin name; spaces and uppercase letters are supported.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        plugin_version: Option<String>,
        #[arg(long)]
        author: Option<String>,
        /// Ask for project metadata even when a directory is provided.
        #[arg(long)]
        interactive: bool,
    },
    /// Compile a project, validate its ABI, and write a checksummed .hyaplugin package.
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Validate and pack an already compiled directory into a .hyaplugin archive.
    Pack {
        path: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Sign a built package and verify its publisher signature.
    Sign(signing::SignArgs),
    /// Validate a package or unpacked directory without granting host capabilities.
    Validate { path: PathBuf },
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Language {
    Rust,
    Python,
    #[value(alias = "javascript")]
    Nodejs,
    C,
    Go,
}
#[derive(Clone, Deserialize, Serialize)]
struct Project {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    language: Language,
    #[serde(default)]
    id: Option<String>,
    #[serde(default = "initial_version")]
    version: String,
    #[serde(default)]
    author: Option<String>,
}
fn initial_version() -> String {
    "0.1.0".into()
}
impl Default for Project {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: None,
            language: Language::Rust,
            id: None,
            version: initial_version(),
            author: None,
        }
    }
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Action::Init {
            path,
            language,
            name,
            id,
            plugin_version,
            author,
            interactive,
        } => {
            let display_name = name
                .or_else(|| {
                    path.as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|s| s.to_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let mut project = Project {
                name: project_name(&display_name),
                display_name: Some(display_name),
                language: language.unwrap_or(Language::Rust),
                id,
                version: plugin_version.unwrap_or_else(initial_version),
                author,
            };
            let path = if interactive || path.is_none() {
                if !io::stdin().is_terminal() {
                    bail!("interactive init requires a terminal; provide a path and metadata flags for scripted use");
                }
                wizard(
                    &mut io::stdin().lock(),
                    &mut io::stdout().lock(),
                    path.as_deref(),
                    &mut project,
                )?
            } else {
                path.context("provide a project directory")?
            };
            println!(
                "Initializing {} ({:?})",
                project.display_name.as_deref().unwrap_or(&project.name),
                project.language
            );
            init(&path, project)?;
            println!("Created {}", path.display());
            println!("Vendored SDK in {}", path.join(".hydra-sdk").display());
            println!("Wrote project metadata, manifest, and language sources");
            println!("Next: hydra-plugin build {}", path.display());
        }
        Action::Build { path, output } => {
            println!("{}", build(&path, output.as_deref())?.display())
        }
        Action::Pack { path, output } => {
            println!("{}", pack(&path, &output)?.display());
        }
        Action::Sign(args) => {
            let output = signing::sign(args)?;
            let package = validate(&output)?;
            println!(
                "Signed: {} {} ({:?})",
                package.manifest.id, package.manifest.version, package.signing
            );
            println!("{}", output.display());
        }
        Action::Validate { path } => {
            let package = validate(&path)?;
            println!(
                "Valid: {} {} (API {}, {:?})",
                package.manifest.id,
                package.manifest.version,
                package.manifest.api,
                package.signing
            );
        }
    }
    Ok(())
}
fn prompt(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    label: &str,
    default: &str,
    validate: impl Fn(&str) -> Result<()>,
) -> Result<String> {
    loop {
        write!(writer, "{label} [{default}]: ")?;
        writer.flush()?;
        let mut input = String::new();
        if reader.read_line(&mut input)? == 0 {
            bail!("initialization cancelled: input closed");
        }
        let value = if input.trim().is_empty() {
            default
        } else {
            input.trim()
        };
        match validate(value) {
            Ok(()) => return Ok(value.to_owned()),
            Err(error) => writeln!(writer, "{error}")?,
        }
    }
}
fn wizard(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    path: Option<&Path>,
    project: &mut Project,
) -> Result<PathBuf> {
    writeln!(
        writer,
        "Create a Hydra plugin (Enter accepts the suggestion)"
    )?;
    let default_name = project.display_name.as_deref().unwrap_or(&project.name);
    let name = prompt(
        reader,
        writer,
        "Name",
        if default_name.is_empty() {
            "My plugin"
        } else {
            default_name
        },
        validate_display_name,
    )?;
    project.name = project_name(&name);
    project.display_name = Some(name);
    let suggested_id = project
        .id
        .clone()
        .unwrap_or_else(|| format!("example.{}", project.name));
    project.id = Some(prompt(reader, writer, "Plugin ID", &suggested_id, |id| {
        let mut candidate = project.clone();
        candidate.id = Some(id.into());
        manifest(&candidate)?.validate().map_err(anyhow::Error::msg)
    })?);
    let suggested_path = path
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.name.clone());
    let directory = prompt(
        reader,
        writer,
        "Directory path and name",
        &suggested_path,
        |value| {
            if Path::new(value).exists() {
                bail!("project directory must not already exist");
            }
            Ok(())
        },
    )?;
    project.version = prompt(reader, writer, "Version", &project.version, |value| {
        semver::Version::parse(value)?;
        Ok(())
    })?;
    let author = prompt(
        reader,
        writer,
        "Author name",
        project.author.as_deref().unwrap_or(""),
        |value| {
            if value.is_empty() {
                bail!("provide the author name");
            }
            if value.len() > hya_plugin_api::limits::MAX_STRING_FIELD {
                bail!("author name is too long");
            }
            Ok(())
        },
    )?;
    project.author = Some(author);
    let default_language = project
        .language
        .to_possible_value()
        .expect("supported language");
    let language = prompt(
        reader,
        writer,
        "Language (go, c, rust, python, nodejs)",
        default_language.get_name(),
        |value| {
            Language::from_str(value, true).map_err(anyhow::Error::msg)?;
            Ok(())
        },
    )?;
    project.language = Language::from_str(&language, true).map_err(anyhow::Error::msg)?;
    Ok(PathBuf::from(directory))
}
fn manifest(project: &Project) -> Result<hya_plugin_api::Manifest> {
    let id = project
        .id
        .clone()
        .unwrap_or_else(|| format!("example.{}", project.name));
    Ok(toml::from_str(&format!(
        "id = {}\nname = {}\nversion = {}\n{}api = 1\nmodule = \"plugin.wasm\"\nhooks = [\"resolve\", \"check\"]\nclaims = [\"https://example.com/*\"]\n[permissions]\nsources = [\"example.com\"]\n",
        toml::Value::String(id), toml::Value::String(project.display_name.clone().unwrap_or_else(|| project.name.clone())),
        toml::Value::String(project.version.clone()),
        project.author.as_ref().map(|author| format!("author = {}\n", toml::Value::String(author.clone()))).unwrap_or_default()
    ))?)
}
fn validate(path: &Path) -> Result<Package> {
    let package = if path.is_dir() {
        package::load_dir(path)?
    } else {
        package::open(&fs::read(path)?, None)?
    };
    let manifest = &package.manifest;
    for pattern in &manifest.claims {
        UrlPattern::parse(pattern).map_err(anyhow::Error::msg)?;
    }
    for patterns in [
        &manifest.permissions.http,
        &manifest.permissions.sources,
        &manifest.permissions.cookies,
    ] {
        HostList::parse(patterns).map_err(anyhow::Error::msg)?;
    }
    Runtime::new().compile(&package.module, manifest.memory_mb)?;
    Ok(package)
}
fn write(root: &Path, name: &str, text: &str) -> Result<()> {
    let path = root.join(name);
    fs::create_dir_all(path.parent().context("missing parent")?)?;
    fs::write(path, text)?;
    Ok(())
}
fn validate_display_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("provide a plugin name");
    }
    if name.len() > hya_plugin_api::limits::MAX_STRING_FIELD {
        bail!("plugin name is too long");
    }
    if name.chars().any(char::is_control) {
        bail!("plugin name must not contain control characters");
    }
    Ok(())
}
fn project_name(display_name: &str) -> String {
    let mut name = String::new();
    let mut separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !name.is_empty() {
                name.push('-');
            }
            name.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if name.is_empty() {
        return "plugin".into();
    }
    if name.as_bytes()[0].is_ascii_digit() {
        name.insert_str(0, "plugin-");
    }
    name
}
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("name must begin with a lowercase letter and contain only a-z, 0-9 and hyphens");
    }
    Ok(())
}
fn init(path: &Path, project: Project) -> Result<()> {
    validate_display_name(project.display_name.as_deref().unwrap_or(&project.name))?;
    validate_name(&project.name)?;
    semver::Version::parse(&project.version)?;
    manifest(&project)?.validate().map_err(anyhow::Error::msg)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(path).context("project directory must not already exist")?;
    let result = scaffold(path, &project);
    if result.is_err() {
        let _ = fs::remove_dir_all(path);
    }
    result
}
fn standalone_sdk_manifest(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace("edition.workspace = true", "edition = \"2021\"")
        .replace(
            "license.workspace = true",
            "license = \"MIT OR Apache-2.0\"",
        )
        .replace(
            "repository.workspace = true",
            "repository = \"https://github.com/ja7ad/hydra\"",
        )
        .replace(
            "homepage.workspace = true",
            "homepage = \"https://hydra.javad.dev\"",
        )
        .replace(
            "serde = { workspace = true }",
            "serde = { version = \"1\", features = [\"derive\"] }",
        )
        .replace("serde_json = { workspace = true }", "serde_json = \"1\"")
        .replace("[lints]\nworkspace = true", "")
}
fn scaffold(path: &Path, project: &Project) -> Result<()> {
    for (name, text) in ASSETS {
        let text = if *name == "crates/hydra-plugin-sdk/Cargo.toml"
            || *name == "crates/hydra-plugin-api/Cargo.toml"
        {
            standalone_sdk_manifest(text)
        } else {
            (*text).to_owned()
        };
        write(path, &format!(".hydra-sdk/{name}"), &text)?;
    }
    write(path, ".hydra-sdk/Cargo.toml", "[workspace]\nmembers = [\"crates/hydra-plugin-api\", \"crates/hydra-plugin-sdk\"]\nresolver = \"2\"\n[workspace.package]\nedition = \"2021\"\nlicense = \"MIT OR Apache-2.0\"\nrepository = \"https://github.com/ja7ad/hydra\"\nhomepage = \"https://hydra.javad.dev\"\n[workspace.dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nserde_json = \"1\"\n[workspace.lints]\n")?;
    write(path, "hydra-project.toml", &toml::to_string(project)?)?;
    write(
        path,
        "hydra-plugin.toml",
        &toml::to_string(&manifest(project)?)?,
    )?;
    let asset = |name: &str| -> Result<&str> {
        ASSETS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .context("SDK template missing")
    };
    match project.language {
        Language::Rust => {
            write(path, "Cargo.toml", &format!("[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n[workspace]\n[lib]\ncrate-type = [\"cdylib\"]\n[dependencies]\nhya-plugin-sdk = {{ path = \".hydra-sdk/crates/hydra-plugin-sdk\" }}\n[profile.release]\nopt-level = \"s\"\nlto = true\nstrip = true\n", project.name, project.version))?;
            write(path, "src/lib.rs", "use hya_plugin_sdk::{Host, Plan, Plugin, Resolve, ResolveRequest, Result, Track};\n#[derive(Default)]\nstruct Resolver;\nimpl Plugin for Resolver {\n    fn resolve(&mut self, host: &Host, req: ResolveRequest) -> Result<Resolve> {\n        let _settings = host.settings()?;\n        Ok(Resolve::Plan(Plan::single(\"Download\", Track::file(\"file\", req.url))))\n    }\n}\nhya_plugin_sdk::export!(Resolver);\n")?;
        }
        Language::Python => write(path, "plugin.py", asset("plugins/sdk/python/example.py")?)?,
        Language::Nodejs => write(path, "plugin.js", asset("plugins/sdk/nodejs/example.js")?)?,
        Language::C => write(path, "plugin.c", asset("plugins/sdk/c/example.c")?)?,
        Language::Go => {
            write(path, "main.go", asset("plugins/sdk/go/example/main.go")?)?;
            write(path, "go.mod", "module plugin.local/resolver\n\ngo 1.24\nrequire hydra.local/sdk v0.0.0\nreplace hydra.local/sdk => ./.hydra-sdk/plugins/sdk/go\n")?;
        }
    }
    write(path, ".gitignore", "/target/\n/plugin.wasm\n/*.hyaplugin\n/.hydra-sdk/plugins/sdk/runtime/target/\n__pycache__/\n")?;
    write(path, "README.md", "# Hydra plugin\n\nEdit the resolver and hydra-plugin.toml claims and permissions.\nRun `hydra-plugin build .` then `hydra-plugin validate NAME.hyaplugin`.\nThe vendored SDK guide is `.hydra-sdk/plugins/sdk/README.md`.\n")?;
    Ok(())
}
fn run(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status().with_context(|| {
        format!("could not run {program}; install the language build prerequisites")
    })?;
    if !status.success() {
        bail!("{program} failed with {status}");
    }
    Ok(())
}
fn build(path: &Path, output: Option<&Path>) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    let project: Project = toml::from_str(&fs::read_to_string(path.join("hydra-project.toml"))?)?;
    validate_name(&project.name)?;
    // Build outputs are fixed by our templates, never shell-expanded manifest commands.
    match project.language {
        Language::Rust => {
            run(Command::new("cargo").current_dir(&path).args([
                "build",
                "--release",
                "--target",
                "wasm32-wasip1",
            ]))?;
            fs::copy(
                path.join(format!(
                    "target/wasm32-wasip1/release/{}.wasm",
                    project.name.replace('-', "_")
                )),
                path.join("plugin.wasm"),
            )?;
        }
        Language::Python => run(
            Command::new(if cfg!(windows) { "python" } else { "python3" })
                .current_dir(&path)
                .args([
                    ".hydra-sdk/plugins/sdk/python/build.py",
                    "plugin.py",
                    "plugin.wasm",
                ]),
        )?,
        Language::Nodejs => run(Command::new("node").current_dir(&path).args([
            ".hydra-sdk/plugins/sdk/nodejs/build.mjs",
            "plugin.js",
            "plugin.wasm",
        ]))?,
        Language::Go => run(Command::new("go")
            .current_dir(&path)
            .env("GOOS", "wasip1")
            .env("GOARCH", "wasm")
            .args(["build", "-buildmode=c-shared", "-o", "plugin.wasm", "."]))?,
        Language::C => {
            let sdk = std::env::var_os("WASI_SDK_PATH")
                .context("set WASI_SDK_PATH to the wasi-sdk directory")?;
            let compiler = Path::new(&sdk).join(if cfg!(windows) {
                "bin/clang.exe"
            } else {
                "bin/clang"
            });
            run(Command::new(compiler)
                .current_dir(&path)
                .args(["--sysroot"])
                .arg(Path::new(&sdk).join("share/wasi-sysroot"))
                .args([
                    "--target=wasm32-wasip1",
                    "-O2",
                    "-mexec-model=reactor",
                    "-I.hydra-sdk/plugins/sdk/c",
                    "plugin.c",
                    ".hydra-sdk/plugins/sdk/c/hydra.c",
                    "-o",
                    "plugin.wasm",
                ]))?;
        }
    }
    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.join(format!("{}.hyaplugin", project.name)));
    pack(&path, &output)
}
fn pack(path: &Path, output: &Path) -> Result<PathBuf> {
    validate(path)?;
    let packed = package::pack(path)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut file, &packed)?;
    file.persist(output)?;
    Ok(output.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_names_preserve_text_and_suggest_safe_project_names() {
        for (display, expected) in [
            ("Youtube test", "youtube-test"),
            ("  Youtube / Test!  ", "youtube-test"),
            ("123 plugin", "plugin-123-plugin"),
            ("افزونه", "plugin"),
            ("my-plugin-1", "my-plugin-1"),
        ] {
            validate_display_name(display).unwrap();
            assert_eq!(project_name(display), expected);
        }
        for name in ["", "  ", "bad\nname", "bad\x1bname"] {
            assert!(validate_display_name(name).is_err());
        }
        assert!(
            validate_display_name(&"a".repeat(hya_plugin_api::limits::MAX_STRING_FIELD)).is_ok()
        );
        assert!(
            validate_display_name(&"a".repeat(hya_plugin_api::limits::MAX_STRING_FIELD + 1))
                .is_err()
        );
        let mut project = Project::default();
        let path = wizard(
            &mut io::Cursor::new("Youtube test\n\n\n\nAuthor\n\n"),
            &mut Vec::new(),
            None,
            &mut project,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("youtube-test"));
        assert_eq!(project.name, "youtube-test");
        assert_eq!(project.display_name.as_deref(), Some("Youtube test"));
        assert_eq!(project.id.as_deref(), Some("example.youtube-test"));
        assert_eq!(manifest(&project).unwrap().name, "Youtube test");
    }

    #[test]
    fn wizard_retries_invalid_metadata_and_accepts_suggestions() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("custom");
        let input = format!(
            "bad\tname\nresolver\nbad id\n\n{}\nbad\n1.2.3\n\nPlugin Author\nunknown\npython\n",
            directory.display()
        );
        let mut project = Project::default();
        let mut output = Vec::new();
        let path = wizard(&mut io::Cursor::new(input), &mut output, None, &mut project).unwrap();
        assert_eq!(path, directory);
        assert_eq!(project.id.as_deref(), Some("example.resolver"));
        assert_eq!(project.version, "1.2.3");
        assert_eq!(project.author.as_deref(), Some("Plugin Author"));
        assert!(matches!(project.language, Language::Python));
        assert!(String::from_utf8(output).unwrap().contains("author name"));
        init(&path, project).unwrap();
        let manifest: hya_plugin_api::Manifest =
            toml::from_str(&fs::read_to_string(path.join("hydra-plugin.toml")).unwrap()).unwrap();
        assert_eq!(manifest.author.as_deref(), Some("Plugin Author"));
        assert_eq!(manifest.version, "1.2.3");
    }

    #[test]
    fn wizard_uses_explicit_defaults_and_retries_an_oversized_author() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("new-plugin");
        let mut project = Project {
            name: "preset".into(),
            display_name: None,
            id: Some("publisher.preset".into()),
            version: "2.3.4".into(),
            author: Some("Default Author".into()),
            language: Language::Go,
        };
        let input = format!(
            "\n\n\n\n{}\n\n\n",
            "a".repeat(hya_plugin_api::limits::MAX_STRING_FIELD + 1)
        );
        let mut output = Vec::new();
        assert_eq!(
            wizard(
                &mut io::Cursor::new(input),
                &mut output,
                Some(&directory),
                &mut project
            )
            .unwrap(),
            directory
        );
        assert_eq!(project.name, "preset");
        assert_eq!(project.id.as_deref(), Some("publisher.preset"));
        assert_eq!(project.version, "2.3.4");
        assert_eq!(project.author.as_deref(), Some("Default Author"));
        assert!(matches!(project.language, Language::Go));
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("author name is too long"));
    }

    #[test]
    fn wizard_preserves_existing_directories_and_cancels_on_eof() {
        let root = tempfile::tempdir().unwrap();
        let mut project = Project::default();
        let input = format!("\n\n{}\n", root.path().display());
        let mut output = Vec::new();
        assert!(wizard(&mut io::Cursor::new(input), &mut output, None, &mut project).is_err());
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("must not already exist"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn vendored_sdk_manifests_are_standalone_with_lf_and_crlf() {
        for name in [
            "crates/hydra-plugin-api/Cargo.toml",
            "crates/hydra-plugin-sdk/Cargo.toml",
        ] {
            let source = ASSETS.iter().find(|(n, _)| *n == name).unwrap().1;
            let source = source.replace("\r\n", "\n");
            for newline in ["\n", "\r\n"] {
                let manifest = standalone_sdk_manifest(&source.replace('\n', newline));
                let manifest: toml::Value = toml::from_str(&manifest).unwrap();
                assert!(manifest.get("lints").is_none(), "{name}: {newline:?}");
                for field in ["edition", "license", "repository", "homepage"] {
                    assert!(manifest["package"][field].is_str(), "{name}: {field}");
                }
                for (_, dependency) in manifest["dependencies"].as_table().unwrap() {
                    assert!(dependency.get("workspace").is_none(), "{name}");
                }
            }
        }
    }

    #[test]
    fn scaffolds_all_languages_without_overwriting_existing_work() {
        let root = tempfile::tempdir().unwrap();
        for language in [
            Language::Rust,
            Language::C,
            Language::Go,
            Language::Python,
            Language::Nodejs,
        ] {
            let path = root.path().join(format!("{language:?}"));
            init(
                &path,
                Project {
                    name: "my-plugin".into(),
                    language,
                    ..Project::default()
                },
            )
            .unwrap();
            let manifest: hya_plugin_api::Manifest =
                toml::from_str(&fs::read_to_string(path.join("hydra-plugin.toml")).unwrap())
                    .unwrap();
            manifest.validate().unwrap();
            assert!(path.join(".hydra-sdk/plugins/sdk/README.md").is_file());
            assert!(init(
                &path,
                Project {
                    name: "other".into(),
                    language,
                    ..Project::default()
                }
            )
            .is_err());
            assert!(fs::read_to_string(path.join("hydra-plugin.toml"))
                .unwrap()
                .contains("my-plugin"));
        }
        assert!(init(
            &root.path().join("unsafe"),
            Project {
                name: "../escape".into(),
                language: Language::Rust,
                ..Project::default()
            }
        )
        .is_err());
        assert!(!root.path().join("unsafe").exists());
    }
    #[test]
    fn validates_reactor_abi_and_rejects_corruption() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample");
        init(
            &path,
            Project {
                name: "sample".into(),
                language: Language::C,
                ..Project::default()
            },
        )
        .unwrap();
        fs::write(
            path.join("plugin.wasm"),
            wat::parse_str(
                r#"(module
            (memory (export "memory") 1)
            (global $ready (mut i32) (i32.const 0))
            (func (export "_initialize") i32.const 1 global.set $ready)
            (func (export "hydra_api") (result i32) global.get $ready)
            (func (export "hydra_alloc") (param i32) (result i32) i32.const 0)
            (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const 0))"#,
            )
            .unwrap(),
        )
        .unwrap();
        validate(&path).unwrap();
        let archive = root.path().join("sample.hyaplugin");
        fs::write(&archive, package::pack(&path).unwrap()).unwrap();
        validate(&archive).unwrap();
        fs::write(&archive, b"not a package").unwrap();
        assert!(validate(&archive).is_err());
        fs::write(
            path.join("plugin.wasm"),
            wat::parse_str("(module)").unwrap(),
        )
        .unwrap();
        assert!(validate(&path).is_err());
    }
}
