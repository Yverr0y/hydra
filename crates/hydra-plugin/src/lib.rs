//! The plugin host: wasm runtime, policy checks, package installer and manager.

pub mod accept;
pub mod exec;
pub mod host;
pub mod http;
pub mod matcher;
pub mod package;
pub mod runtime;

pub mod dirs;
pub mod distribution;
pub mod manager;
pub mod media;
pub mod official;
pub use dirs::hydra_dir;

/// Describes every permission a frontend must display before granting it.
pub fn consent(p: &hya_plugin_api::Permissions) -> Vec<String> {
    let mut lines = Vec::new();
    for host in &p.http {
        lines.push(format!("Connects to {host} through Hydra."));
    }
    for host in &p.sources {
        lines.push(format!("Downloads from {host}."));
    }
    for host in &p.cookies {
        lines.push(format!("Sends your browser cookies for {host}."));
    }
    for exec in &p.exec {
        lines.push(format!(
            "Runs {} with arguments: {}",
            exec.program,
            exec.args.join(" ")
        ));
    }
    if p.data {
        lines.push("Keeps files in its own folder.".into());
    }
    if p.exec_from_data {
        lines.push("Can download programs and run them.".into());
    }
    lines
}

mod secrets;
