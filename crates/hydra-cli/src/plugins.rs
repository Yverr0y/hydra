//! Plugin management commands and terminal frontend.
use clap::Subcommand;
use hya_plugin::host::Frontend;
use hya_plugin::manager::Manager;
use hya_plugin_api::{Answers, ErrorCode, FieldKind, Form, PluginError, Value};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Options {
    pub preferences: hya_plugin_api::Preferences,
    pub only: Option<String>,
}
impl Options {
    pub fn from_cli(args: &crate::cli::Cli) -> Self {
        Self {
            only: args.plugin.clone(),
            preferences: hya_plugin_api::Preferences {
                audio_only: args.extract_audio,
                audio_format: args.audio_format.clone(),
                playlist_ids: None,
                max_height: args.quality,
                container: Some(args.container.as_str().into()),
                include_files: true,
                audio: match args.audio.as_str() {
                    "none" => hya_plugin_api::AudioPref::None,
                    "best" => hya_plugin_api::AudioPref::Best,
                    id => hya_plugin_api::AudioPref::Id(id.into()),
                },
                subtitle_languages: args.subs.clone(),
                track_ids: args.tracks.clone(),
                transfer_files: None,
            },
        }
    }
}
pub async fn resolve_job(
    job: &crate::download::Job,
) -> Result<Option<(String, hya_plugin_api::Plan)>, String> {
    let request_url = input_url(&job.urls[0])?;
    let mut context = hya_plugin::manager::ResolveContext {
        input_file: ::url::Url::parse(&request_url)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok()),
        only: job.plugin_options.as_ref().and_then(|o| o.only.clone()),
        proxy: match crate::url::Url::parse(&job.urls[0]) {
            Some(url) => {
                crate::url::ProxyPolicy::new(job.proxy.as_deref(), job.no_proxy).for_url(&url)?
            }
            None if !job.no_proxy => job
                .proxy
                .as_deref()
                .map(hya_net::Proxy::parse)
                .transpose()?,
            None => None,
        },
        ..Default::default()
    };
    let cookies = job.cookies.clone();
    let host = crate::url::Url::parse(&job.urls[0])
        .map(|u| u.host)
        .unwrap_or_default();
    let ctl = context.ctl.clone();
    let request = hya_plugin_api::ResolveRequest {
        url: request_url,
        headers: job
            .headers
            .iter()
            .filter_map(|h| h.split_once(':'))
            .filter(|(k, _)| {
                !k.eq_ignore_ascii_case("cookie") && !k.eq_ignore_ascii_case("authorization")
            })
            .map(|(k, v)| (k.trim().into(), v.trim().into()))
            .collect(),
    };
    let mut worker = tokio::task::spawn_blocking(move || {
        if let Some(cookies) = cookies {
            context.cookies = cookies.open(&host, hya_net::cookies::now_secs())?.0;
        }
        let mut manager = Manager::open_with_official(hya_plugin::hydra_dir().join("plugins"))
            .map_err(|e| e.to_string())?;
        let connector =
            hya_plugin::http::connector(context.proxy.as_ref()).map_err(|e| e.to_string())?;
        manager
            .resolve_with_context(|| connector.clone(), request, frontend, context)
            .map_err(|e| e.to_string())
    });
    loop {
        tokio::select! {
            result = &mut worker => return result.map_err(|e|e.to_string())?,
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => if job.cancel.as_ref().is_some_and(|c|c.load(std::sync::atomic::Ordering::Relaxed)) { ctl.cancel(); }
        }
    }
}

fn input_url(address: &str) -> Result<String, String> {
    if !address.contains("://")
        && !address.starts_with("magnet:")
        && std::path::Path::new(address).extension().is_some()
    {
        let path = std::path::absolute(address).map_err(|e| e.to_string())?;
        ::url::Url::from_file_path(path)
            .map_err(|_| "invalid local file path".to_owned())
            .map(String::from)
    } else {
        Ok(address.to_owned())
    }
}

pub async fn finish_job(url: String, path: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let manager = Manager::open_with_official(hya_plugin::hydra_dir().join("plugins"))
            .map_err(|e| e.to_string())?;
        let connector = hya_net::tls::TlsCapableConnector::new().map_err(|e| e.to_string())?;
        let request = hya_plugin_api::CompleteRequest {
            url,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            size: std::fs::metadata(&path).map_err(|e| e.to_string())?.len(),
        };
        manager
            .finish(
                request,
                &path,
                || connector.clone(),
                frontend,
                Default::default(),
            )
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub async fn refresh_plan(
    job: &crate::download::Job,
    plugin: &str,
    plan: hya_plugin_api::Plan,
    track_ids: Vec<String>,
) -> Result<hya_plugin_api::Plan, String> {
    let plugin = plugin.to_string();
    let url = job.urls[0].clone();
    let cookies = job.cookies.clone();
    let context = hya_plugin::manager::ResolveContext {
        proxy: match crate::url::Url::parse(&job.urls[0]) {
            Some(url) => {
                crate::url::ProxyPolicy::new(job.proxy.as_deref(), job.no_proxy).for_url(&url)?
            }
            None => None,
        },
        ..Default::default()
    };
    let ctl = context.ctl.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let mut context = context;
        if let Some(cookies) = cookies {
            let host = crate::url::Url::parse(&url).ok_or("invalid address")?.host;
            context.cookies = cookies.open(&host, hya_net::cookies::now_secs())?.0;
        }
        let mut manager = Manager::open_with_official(hya_plugin::hydra_dir().join("plugins"))
            .map_err(|e| e.to_string())?;
        let connector =
            hya_plugin::http::connector(context.proxy.as_ref()).map_err(|e| e.to_string())?;
        manager
            .refresh_plan(
                &plugin,
                hya_plugin_api::ResolveRequest {
                    url,
                    ..Default::default()
                },
                plan,
                track_ids,
                || connector.clone(),
                frontend,
                context,
            )
            .map_err(|e| e.to_string())
    });
    loop {
        tokio::select! {
            result = &mut worker => return result.map_err(|e|e.to_string())?,
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => if job.cancel.as_ref().is_some_and(|c|c.load(std::sync::atomic::Ordering::Relaxed)) {ctl.cancel();}
        }
    }
}

fn choose<R: std::io::BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    title: &str,
    options: &[String],
    default: usize,
) -> Result<usize, String> {
    writeln!(writer, "\n{title}:").map_err(|e| e.to_string())?;
    for (index, label) in options.iter().enumerate() {
        writeln!(
            writer,
            "  {}. {}",
            index + 1,
            crate::plugin_ui::clean(label)
        )
        .map_err(|e| e.to_string())?;
    }
    loop {
        write!(writer, "Choice [{}] (q to cancel): ", default + 1).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        let mut input = String::new();
        if reader.read_line(&mut input).map_err(|e| e.to_string())? == 0 {
            return Err("input closed".into());
        }
        let input = input.trim();
        if input.is_empty() {
            return Ok(default);
        }
        if input.eq_ignore_ascii_case("q") {
            return Err("track selection cancelled".into());
        }
        if let Ok(index) = input.parse::<usize>() {
            if (1..=options.len()).contains(&index) {
                return Ok(index - 1);
            }
        }
        writeln!(writer, "Enter a number from 1 to {}.", options.len())
            .map_err(|e| e.to_string())?;
    }
}

fn track_label(track: &hya_plugin_api::Track) -> String {
    let mut parts = vec![track.id.clone()];
    if let Some(height) = track.height {
        parts.push(format!("{height}p"));
    }
    if let Some(bitrate) = track.bitrate {
        parts.push(format!("{} kb/s", bitrate / 1000));
    }
    if let Some(codec) = &track.codec {
        parts.push(codec.clone());
    }
    if let Some(container) = &track.container {
        parts.push(container.clone());
    }
    parts.join(" · ")
}

pub async fn interactive_preferences(
    args: &crate::cli::Cli,
    plan: &hya_plugin_api::Plan,
) -> Result<hya_plugin_api::Preferences, String> {
    let mut prefs = Options::from_cli(args).preferences;
    if args.no_input
        || args.quiet
        || !std::io::stdin().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        return Ok(prefs);
    }
    let plan = plan.clone();
    let explicit_video = args.quality.is_some() || !args.tracks.is_empty();
    let explicit_audio = args.audio != "best" || !args.tracks.is_empty();
    tokio::task::spawn_blocking(move || {
        let mut reader = std::io::stdin().lock();
        let mut writer = std::io::stderr().lock();
        let media = !plan.entries.is_empty()
            || plan.tracks.iter().any(|t| {
                matches!(
                    t.kind,
                    hya_plugin_api::TrackKind::Video | hya_plugin_api::TrackKind::Audio
                )
            });
        if !media {
            return Ok(prefs);
        }
        if !prefs.audio_only && !explicit_video && !explicit_audio {
            let mode = choose(
                &mut reader,
                &mut writer,
                "Download mode",
                &[
                    "Video and audio".into(),
                    "Audio only".into(),
                    "Video only".into(),
                ],
                0,
            )?;
            prefs.audio_only = mode == 1;
            if mode == 2 {
                prefs.audio = hya_plugin_api::AudioPref::None;
            }
        }
        if !prefs.audio_only && !explicit_video {
            if plan.entries.is_empty() {
                let mut tracks: Vec<_> = plan
                    .tracks
                    .iter()
                    .filter(|t| t.kind == hya_plugin_api::TrackKind::Video)
                    .collect();
                tracks.sort_by_key(|t| {
                    std::cmp::Reverse((t.height.unwrap_or(0), t.bitrate.unwrap_or(0)))
                });
                if !tracks.is_empty() {
                    let mut labels = vec!["Best".into()];
                    labels.extend(tracks.iter().map(|t| track_label(t)));
                    let index =
                        choose(&mut reader, &mut writer, "Choose video quality", &labels, 0)?;
                    if index > 0 {
                        prefs.track_ids.push(tracks[index - 1].id.clone());
                    }
                }
            } else {
                let labels = ["Best", "2160p", "1440p", "1080p", "720p", "480p"].map(str::to_owned);
                let index = choose(&mut reader, &mut writer, "Choose video quality", &labels, 0)?;
                prefs.max_height = [
                    None,
                    Some(2160),
                    Some(1440),
                    Some(1080),
                    Some(720),
                    Some(480),
                ][index];
            }
        }
        if !explicit_audio
            && prefs.audio != hya_plugin_api::AudioPref::None
            && plan.entries.is_empty()
        {
            let mut tracks: Vec<_> = plan
                .tracks
                .iter()
                .filter(|t| t.kind == hya_plugin_api::TrackKind::Audio)
                .collect();
            tracks.sort_by_key(|t| std::cmp::Reverse(t.bitrate.unwrap_or(0)));
            if !tracks.is_empty() {
                let mut labels = vec!["Best".into()];
                labels.extend(tracks.iter().map(|t| track_label(t)));
                let index = choose(&mut reader, &mut writer, "Choose audio", &labels, 0)?;
                if index > 0 {
                    prefs.audio = hya_plugin_api::AudioPref::Id(tracks[index - 1].id.clone());
                }
            }
        }
        if prefs.audio_only && prefs.audio_format.is_none() {
            let formats = ["Original", "mp3", "m4a", "opus", "flac", "wav"].map(str::to_owned);
            let index = choose(&mut reader, &mut writer, "Choose audio format", &formats, 0)?;
            prefs.audio_format = (index > 0).then(|| formats[index].clone());
        }
        Ok(prefs)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Install or update the signed official plugins bundled with Hydra.
    SyncOfficial {
        /// Fail if this binary was built without official plugin packages.
        #[arg(long)]
        require_bundled: bool,
    },
    List {
        /// Print the complete installed metadata as JSON.
        #[arg(long)]
        json: bool,
    },
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// List catalog updates, or install them after explicit permission consent.
    Update {
        id: Option<String>,
        /// Install the listed updates and grant their displayed permissions.
        #[arg(long)]
        accept_permissions: bool,
        /// Accept a publisher change while applying updates.
        #[arg(long, requires = "accept_permissions")]
        accept_publisher_change: bool,
    },
    /// Execute a development package with its declared capabilities, including exec.
    Test {
        path: PathBuf,
        #[arg(long)]
        url: Option<String>,
    },
    Info {
        id: String,
        /// Emit complete installed metadata as JSON.
        #[arg(long)]
        json: bool,
    },
    Check {
        id: String,
    },
    Logs {
        id: String,
    },
    Config {
        id: String,
        /// Prompt for a secret without putting its value in shell history.
        #[arg(long)]
        secret: String,
    },
    Rollback {
        id: String,
    },
    Grant {
        id: String,
        capability: String,
    },
    Revoke {
        id: String,
        capability: String,
    },
    ClearSession {
        id: String,
    },
    Pack {
        path: PathBuf,
        output: PathBuf,
    },
    Inspect {
        path: PathBuf,
    },
    Install {
        path: String,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        accept_permissions: bool,
        /// Accept the publisher change shown by a refused upgrade.
        #[arg(long)]
        accept_publisher_change: bool,
    },
    Remove {
        id: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Order {
        ids: Vec<String>,
    },
    Set {
        id: String,
        key: String,
        value: String,
    },
    Resolve {
        url: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum IndexCommand {
    List,
    Add {
        url: String,
        #[arg(long)]
        key: Option<String>,
    },
    Rm {
        url: String,
    },
}

pub(crate) fn log_enabled(level: &str, filter: &str) -> bool {
    let rank = |value: &str| match value.to_ascii_lowercase().as_str() {
        "trace" | "debug" | "verbose" => 0,
        "warn" | "warning" => 2,
        "error" => 3,
        _ => 1,
    };
    rank(level) >= rank(filter)
}

pub struct Terminal {
    id: String,
}
impl Frontend for Terminal {
    fn prompt_with_ctl(
        &mut self,
        form: Form,
        ctl: &hya_plugin::runtime::CallCtl,
    ) -> Result<Answers, PluginError> {
        if crate::plugin_ui::active() {
            crate::plugin_ui::prompt(&self.id, form, ctl)
        } else {
            self.prompt(form)
        }
    }
    fn prompt(&mut self, form: Form) -> Result<Answers, PluginError> {
        if crate::plugin_ui::headless() || !std::io::stdin().is_terminal() {
            return Err(PluginError::new(
                ErrorCode::Cancelled,
                format!(
                    "plugin {} needs input; run interactively or configure its settings",
                    self.id
                ),
            ));
        }
        static PROMPT: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = PROMPT
            .lock()
            .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?;
        eprintln!(
            "Plugin {} — {}",
            self.id,
            form.title.as_deref().unwrap_or("Input required")
        );
        let mut answers = Answers::new();
        for field in form.fields {
            if let Some(help) = &field.help {
                eprintln!("{help}");
            }
            if !field.options.is_empty() {
                eprintln!("Options: {}", field.options.join(", "));
            }
            eprint!("{}: ", field.label);
            let _ = std::io::stderr().flush();
            let raw = if field.kind == FieldKind::Secret {
                rpassword::read_password()
                    .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?
            } else {
                let mut line = String::new();
                if std::io::stdin()
                    .read_line(&mut line)
                    .map_err(|e| PluginError::new(ErrorCode::Internal, e.to_string()))?
                    == 0
                {
                    return Err(PluginError::new(ErrorCode::Cancelled, "input closed"));
                }
                line.trim_end().to_string()
            };
            let value = if raw.is_empty() && field.default.is_some() {
                field.default.clone().unwrap()
            } else {
                match field.kind {
                    FieldKind::Bool | FieldKind::Checkbox => Value::Bool(match raw.as_str() {
                        "true" | "yes" | "y" => true,
                        "false" | "no" | "n" => false,
                        _ => {
                            return Err(PluginError::new(
                                ErrorCode::InvalidInput,
                                "enter yes or no",
                            ))
                        }
                    }),
                    FieldKind::Number => Value::Number(raw.parse().map_err(|_| {
                        PluginError::new(ErrorCode::InvalidInput, "enter a number")
                    })?),
                    _ => Value::Text(raw),
                }
            };
            field
                .validate(&value)
                .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e))?;
            answers.insert(field.key, value);
        }
        Ok(answers)
    }
    fn log(&mut self, level: &str, message: &str) {
        let filter = std::env::var("HYDRA_LOG").unwrap_or_else(|_| "info".into());
        if !log_enabled(level, &filter) {
            return;
        }
        if crate::plugin_ui::active() {
            crate::plugin_ui::send(crate::plugin_ui::Event::Log(format!(
                "[{}] {level}: {message}",
                self.id
            )));
        } else {
            eprintln!("[{}] {level}: {message}", self.id);
        }
    }
    fn progress(&mut self, done: u64, total: Option<u64>, note: Option<&str>) {
        let line = format!("[{}] {done}/{total:?} {}", self.id, note.unwrap_or(""));
        if crate::plugin_ui::active() {
            crate::plugin_ui::send(crate::plugin_ui::Event::Log(line));
        } else {
            eprintln!("{line}");
        }
    }
}
pub fn frontend(id: &str) -> Box<dyn Frontend> {
    Box::new(Terminal { id: id.into() })
}

fn list_table(plugins: &[hya_plugin::manager::Installed]) -> String {
    if plugins.is_empty() {
        return "No plugins installed.\n".into();
    }
    let mut rows = vec![[
        "ID".into(),
        "NAME".into(),
        "AUTHOR".into(),
        "SIGNED".into(),
        "VERSION".into(),
    ]];
    rows.extend(plugins.iter().map(|plugin| {
        let manifest = &plugin.manifest;
        [
            crate::plugin_ui::clean(&manifest.id),
            crate::plugin_ui::clean(&manifest.name),
            crate::plugin_ui::clean(manifest.author.as_deref().unwrap_or("—")),
            match plugin.signing {
                hya_plugin::package::Signing::Verified { .. } => "Yes".into(),
                hya_plugin::package::Signing::Unsigned => "No".into(),
            },
            crate::plugin_ui::clean(&manifest.version),
        ]
    }));
    let widths: [usize; 5] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    });
    let mut output = String::new();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            output.push_str(cell);
            if column + 1 < row.len() {
                output.extend(std::iter::repeat_n(
                    ' ',
                    widths[column] - cell.chars().count() + 2,
                ));
            }
        }
        output.push('\n');
    }
    output
}

fn info_text(plugin: &hya_plugin::manager::Installed, color: bool) -> String {
    let manifest = &plugin.manifest;
    let clean = crate::plugin_ui::clean;
    let mut output = format!(
        "Name: {}\nID: {}\nAuthor: {}\nVersion: {}\n",
        clean(&manifest.name),
        clean(&manifest.id),
        clean(manifest.author.as_deref().unwrap_or("—")),
        clean(&manifest.version)
    );
    match &plugin.signing {
        hya_plugin::package::Signing::Unsigned => output.push_str("Signature: Unsigned\n"),
        hya_plugin::package::Signing::Verified { fingerprint } => {
            let status = if color {
                "\x1b[32mSigned (verified)\x1b[0m"
            } else {
                "Signed (verified)"
            };
            output.push_str(&format!(
                "Signature: {status}\nPublisher fingerprint (SHA-256): {}\n",
                clean(fingerprint)
            ));
        }
    }
    output.push_str(&format!(
        "Enabled: {}\nLocation: {}\n",
        plugin.enabled,
        clean(&plugin.directory.display().to_string())
    ));
    output
}

fn install_package(
    manager: &mut Manager,
    prepared: &hya_plugin::distribution::Prepared,
    accept_permissions: bool,
    accept_publisher_change: bool,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let package = Manager::inspect(&prepared.path)?;
    if prepared.remote() && package.manifest.publisher_key.is_none() {
        eprintln!("This package is unsigned; its checksum does not authenticate its publisher.");
    }
    eprintln!(
        "{} ({})\n{}",
        package.manifest.name,
        package.manifest.id,
        serde_json::to_string_pretty(&package.manifest.permissions)?
    );
    if !accept_permissions {
        return Err(
            "review the displayed permissions, then repeat with --accept-permissions".into(),
        );
    }
    let id = manager.install_with_publisher_consent(
        &prepared.path,
        package.manifest.permissions,
        accept_publisher_change,
    )?;
    if let Some(welcome) = package.manifest.welcome {
        eprintln!(
            "\nGetting started — {}\n{}",
            crate::plugin_ui::clean(&package.manifest.name),
            welcome
                .lines()
                .map(crate::plugin_ui::clean)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(id)
}

pub async fn run(command: &Command) -> std::process::ExitCode {
    let command = command.clone();
    let result = tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut manager = Manager::open_with_official(hya_plugin::hydra_dir().join("plugins"))?;
            match command {
                Command::SyncOfficial { require_bundled } => {
                    if require_bundled && !hya_plugin::official::bundled() {
                        return Err("this build contains no official plugins".into());
                    }
                }
                Command::Info { id, json } => {
                    let installed = manager
                        .list()
                        .iter()
                        .find(|p| p.manifest.id == id)
                        .ok_or("unknown plugin")?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(installed)?);
                    } else {
                        let color = std::io::stdout().is_terminal()
                            && std::env::var_os("NO_COLOR").is_none()
                            && std::env::var("TERM").as_deref() != Ok("dumb");
                        print!("{}", info_text(installed, color));
                    }
                }
                Command::Check { id } => manager.check(
                    &id,
                    hya_net::tls::TlsCapableConnector::new()?,
                    frontend(&id),
                )?,
                Command::Logs { id } => print!("{}", manager.logs(&id)?),
                Command::Config { id, secret } => {
                    if !std::io::stdin().is_terminal() {
                        return Err("secret configuration requires an interactive terminal".into());
                    }
                    let value = rpassword::prompt_password(format!("Plugin {id} — {secret}: "))?;
                    manager.set_secret(&id, &secret, value)?;
                    manager.check(
                        &id,
                        hya_net::tls::TlsCapableConnector::new()?,
                        frontend(&id),
                    )?;
                }
                Command::Rollback { id } => manager.rollback(&id)?,
                Command::Grant { id, capability } => manager.permission(&id, &capability, true)?,
                Command::Revoke { id, capability } => {
                    manager.permission(&id, &capability, false)?
                }
                Command::ClearSession { id } => manager.clear_session(&id)?,
                Command::Pack { path, output } => {
                    let bytes = hya_plugin::package::pack(&path)?;
                    std::fs::write(output, bytes)?;
                }
                Command::Index { command } => {
                    let root = hya_plugin::hydra_dir().join("plugins");
                    match command {
                        IndexCommand::List => println!(
                            "{}",
                            serde_json::to_string_pretty(&hya_plugin::distribution::sources(
                                &root
                            )?)?
                        ),
                        IndexCommand::Add { url, key } => println!(
                            "{}",
                            serde_json::to_string_pretty(&hya_plugin::distribution::add(
                                &root,
                                hya_plugin::distribution::IndexSource { url, key }
                            )?)?
                        ),
                        IndexCommand::Rm { url } => hya_plugin::distribution::remove(&root, &url)?,
                    }
                }
                Command::Update {
                    id,
                    accept_permissions,
                    accept_publisher_change,
                } => {
                    let available = hya_plugin::distribution::updates(
                        &hya_plugin::hydra_dir().join("plugins"),
                        manager.list(),
                        id.as_deref(),
                    )?;
                    if accept_permissions {
                        for entry in available {
                            let prepared = entry.prepare()?;
                            println!(
                                "Updated {}",
                                install_package(
                                    &mut manager,
                                    &prepared,
                                    true,
                                    accept_publisher_change
                                )?
                            );
                        }
                    } else {
                        println!("{}", serde_json::to_string_pretty(&available)?);
                    }
                }
                Command::Test { path, url } => {
                    let temp = tempfile::tempdir()?;
                    let mut test = Manager::open(temp.path().into())?;
                    let package = Manager::inspect(&path)?;
                    eprintln!(
                        "Testing with declared capabilities, including exec: {}",
                        serde_json::to_string(&package.manifest.permissions)?
                    );
                    let id = test.install(&path, package.manifest.permissions)?;
                    let connector = hya_net::tls::TlsCapableConnector::new()?;
                    test.check(&id, connector.clone(), frontend(&id))?;
                    if let Some(url) = url {
                        let result = test.resolve(
                            || connector.clone(),
                            hya_plugin_api::ResolveRequest {
                                url,
                                ..Default::default()
                            },
                            frontend,
                        )?;
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    }
                }
                Command::List { json } => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(manager.list())?);
                    } else {
                        print!("{}", list_table(manager.list()));
                    }
                }
                Command::Inspect { path } => println!(
                    "{}",
                    serde_json::to_string_pretty(&Manager::inspect(&path)?.manifest)?
                ),
                Command::Install {
                    path,
                    sha256,
                    accept_permissions,
                    accept_publisher_change,
                } => {
                    let prepared =
                        hya_plugin::distribution::Prepared::new(&path, sha256.as_deref())?;
                    println!(
                        "Installed {}",
                        install_package(
                            &mut manager,
                            &prepared,
                            accept_permissions,
                            accept_publisher_change
                        )?
                    );
                }
                Command::Remove { id } => manager.remove(&id)?,
                Command::Enable { id } => manager.enable(&id, true)?,
                Command::Disable { id } => manager.enable(&id, false)?,
                Command::Order { ids } => manager.order(&ids)?,
                Command::Set { id, key, value } => {
                    let value = serde_json::from_str::<Value>(&value).unwrap_or(Value::Text(value));
                    manager.set(&id, &key, value)?;
                    manager.check(
                        &id,
                        hya_net::tls::TlsCapableConnector::new()?,
                        frontend(&id),
                    )?;
                }
                Command::Resolve { url } => {
                    let connector = hya_net::tls::TlsCapableConnector::new()?;
                    let plan = manager.resolve(
                        || connector.clone(),
                        hya_plugin_api::ResolveRequest {
                            url,
                            ..Default::default()
                        },
                        frontend,
                    )?;
                    println!("{}", serde_json::to_string_pretty(&plan)?);
                }
            }
            Ok(())
        },
    )
    .await;
    match result {
        Ok(Ok(())) => std::process::ExitCode::SUCCESS,
        Ok(Err(e)) => {
            eprintln!("hydra plugin: {e}");
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("hydra plugin worker: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_diagnostics_respect_the_selected_log_level() {
        assert!(!super::log_enabled("debug", "info"));
        assert!(super::log_enabled("debug", "DEBUG"));
        assert!(!super::log_enabled("info", "warn"));
        assert!(super::log_enabled("error", "warn"));
    }

    use super::*;

    #[test]
    fn positional_file_inputs_preserve_spaces_and_leave_urls_and_magnets_unchanged() {
        let path = std::path::absolute("input directory/test.X").unwrap();
        let address = input_url(path.to_str().unwrap()).unwrap();
        assert_eq!(
            ::url::Url::parse(&address).unwrap().to_file_path().unwrap(),
            path
        );
        for address in [
            "https://example.com/file.x",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        ] {
            assert_eq!(input_url(address).unwrap(), address);
        }
    }

    #[test]
    fn catalog_update_arguments_require_explicit_permission_consent() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "hydra",
            "plugin",
            "update",
            "community.video",
            "--accept-permissions",
        ])
        .unwrap();
        assert!(matches!(cli.command,
            Some(crate::cli::Command::Plugin {
                command: Command::Update { id: Some(id), accept_permissions: true, accept_publisher_change: false }
            }) if id == "community.video"));
        assert!(crate::cli::Cli::try_parse_from([
            "hydra",
            "plugin",
            "update",
            "--accept-publisher-change",
        ])
        .is_err());
        let cli = crate::cli::Cli::try_parse_from(["hydra", "plugin", "update"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::cli::Command::Plugin {
                command: Command::Update {
                    accept_permissions: false,
                    ..
                }
            })
        ));
    }

    #[test]
    fn package_updates_require_consent_and_preserve_settings_and_rollback() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let module = wat::parse_str(
            r#"(module
            (memory (export "memory") 1)
            (func (export "hydra_api") (result i32) i32.const 1)
            (func (export "hydra_alloc") (param i32) (result i32) i32.const 0)
            (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const 0))"#,
        )
        .unwrap();
        std::fs::write(source.path().join("plugin.wasm"), module).unwrap();
        let archive = source.path().join("release.hyaplugin");
        let mut manager = Manager::open(root.path().join("plugins")).unwrap();
        for version in ["1.0.0", "1.1.0"] {
            std::fs::write(source.path().join("hydra-plugin.toml"), format!(
                "id='community.video'\nname='Community Video'\nversion='{version}'\napi=1\nmodule='plugin.wasm'\nwelcome='Choose your quality.'\n[[settings]]\nkey='quality'\nlabel='Quality'\ntype='text'\ndefault='best'\n"
            )).unwrap();
            let bytes = hya_plugin::package::pack(source.path()).unwrap();
            std::fs::write(&archive, &bytes).unwrap();
            let prepared = hya_plugin::distribution::Prepared::new(
                archive.to_str().unwrap(),
                Some(&hya_plugin::package::sha256_hex(&bytes)),
            )
            .unwrap();
            assert!(install_package(&mut manager, &prepared, false, false).is_err());
            assert_eq!(
                install_package(&mut manager, &prepared, true, false).unwrap(),
                "community.video"
            );
            if version == "1.0.0" {
                manager
                    .set("community.video", "quality", Value::Text("custom".into()))
                    .unwrap();
            }
        }
        let plugin = &manager.list()[0];
        assert_eq!(plugin.manifest.version, "1.1.0");
        assert_eq!(plugin.settings["quality"], Value::Text("custom".into()));
        assert_eq!(plugin.previous.as_ref().unwrap().manifest.version, "1.0.0");
        manager.rollback("community.video").unwrap();
        assert_eq!(manager.list()[0].manifest.version, "1.0.0");
    }

    #[test]
    fn list_table_shows_verified_signatures_and_unsigned_plugins() {
        let unsigned: hya_plugin::manager::Installed = serde_json::from_value(serde_json::json!({
            "manifest": {"id": "test.cli", "name": "CLI test", "author": "Test Author",
                "version": "1.0.0", "api": 1, "module": "plugin.wasm"},
            "directory": "plugins/test.cli", "dev": false, "enabled": true,
            "grants": {}, "pins": {}
        }))
        .unwrap();
        let mut signed = unsigned.clone();
        signed.manifest.id = "test.signed".into();
        signed.signing = hya_plugin::package::Signing::Verified {
            fingerprint: "verified-key".into(),
        };
        let table = list_table(&[unsigned, signed]);
        let lines: Vec<_> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0].split_whitespace().collect::<Vec<_>>(),
            ["ID", "NAME", "AUTHOR", "SIGNED", "VERSION"]
        );
        assert!(lines[1].contains("No"));
        assert!(lines[2].contains("Yes"));
        for column in ["NAME", "AUTHOR", "SIGNED", "VERSION"] {
            let offset = lines[0].find(column).unwrap();
            assert_ne!(lines[1].as_bytes()[offset], b' ');
            assert_ne!(lines[2].as_bytes()[offset], b' ');
        }
    }

    #[test]
    fn numbered_choices_retry_invalid_input_and_accept_defaults() {
        let options = vec!["720p".into(), "1080p".into()];
        let mut reader = std::io::Cursor::new("9\ninvalid\n2\n");
        let mut writer = Vec::new();
        assert_eq!(
            choose(&mut reader, &mut writer, "Quality", &options, 0).unwrap(),
            1
        );
        assert!(String::from_utf8(writer)
            .unwrap()
            .contains("Enter a number from 1 to 2"));
        assert_eq!(
            choose(
                &mut std::io::Cursor::new("\n"),
                &mut Vec::new(),
                "Quality",
                &options,
                1
            )
            .unwrap(),
            1
        );
        for input in ["q\n", ""] {
            assert!(choose(
                &mut std::io::Cursor::new(input),
                &mut Vec::new(),
                "Quality",
                &options,
                0
            )
            .is_err());
        }
    }
}
