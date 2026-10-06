//! What a resolver returns: the download plan.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assemble {
    #[default]
    None,
    Mux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ranges {
    #[default]
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    #[serde(default)]
    pub priority: Option<u32>,
    #[serde(default)]
    pub max_connections: Option<u32>,
}

impl Source {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            priority: None,
            max_connections: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// `algo:hex`, e.g. `sha-256:…`.
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub fps: Option<f32>,
    #[serde(default)]
    pub bitrate: Option<u64>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub auto_generated: bool,
    #[serde(default)]
    pub ranges: Ranges,
    #[serde(default)]
    pub chunk_hint: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub default: bool,
}

impl Track {
    /// A single-source, video-less `file` track.
    pub fn file(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: TrackKind::File,
            sources: vec![Source::new(url)],
            headers: BTreeMap::new(),
            size: None,
            digest: None,
            container: None,
            codec: None,
            height: None,
            fps: None,
            bitrate: None,
            language: None,
            auto_generated: false,
            ranges: Ranges::Allow,
            chunk_hint: None,
            expires_at: None,
            default: true,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

/// One playlist item, resolved when its download starts so signed sources stay fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// One output described by a native transfer engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferFile {
    pub index: u32,
    pub path: String,
    pub size: u64,
}

/// Whether a native transfer publishes one file or a directory of files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferOutput {
    File,
    #[default]
    Directory,
}

/// Plugin-provided headings for a transfer's live detail table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferDetails {
    pub title: String,
    pub columns: Vec<String>,
}

/// Opaque engine parameters and output descriptions for a native transfer engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    #[serde(default)]
    pub details: Option<TransferDetails>,
    pub engine: String,
    #[serde(default)]
    pub output: TransferOutput,
    pub metadata: serde_json::Value,
    pub files: Vec<TransferFile>,
    #[serde(default)]
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub assemble: Assemble,
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<Box<Transfer>>,
    /// An ordered collection of page URLs, mutually exclusive with media tracks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<PlaylistEntry>,
}

impl Plan {
    /// A one-track plan whose id is the track's.
    pub fn single(title: impl Into<String>, track: Track) -> Self {
        Self {
            id: track.id.clone(),
            title: Some(title.into()),
            expires_at: None,
            assemble: Assemble::None,
            tracks: vec![track],
            transfer: None,
            entries: Vec::new(),
        }
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_plan_with_defaults() {
        let plan: Plan = serde_json::from_str(
            r#"{"id":"p","assemble":"mux","tracks":[
                {"id":"v","kind":"video","sources":[{"url":"https://a/x","priority":1}],
                 "height":1080,"ranges":"deny","default":true},
                {"id":"a","kind":"audio","sources":[{"url":"https://a/y"}]}]}"#,
        )
        .unwrap();
        assert_eq!(plan.assemble, Assemble::Mux);
        assert_eq!(plan.tracks[0].ranges, Ranges::Deny);
        assert_eq!(plan.tracks[1].ranges, Ranges::Allow);
        assert!(!plan.tracks[1].default);
        assert!(plan.track("a").is_some());
        assert!(plan.track("zz").is_none());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let t: Track =
            serde_json::from_str(r#"{"id":"v","kind":"file","sources":[],"future_field":42}"#)
                .unwrap();
        assert_eq!(t.id, "v");
    }

    #[test]
    fn single_builds_a_one_track_plan() {
        let p = Plan::single(
            "t",
            Track::file("f", "https://a/b").header("Referer", "https://a"),
        );
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].headers["Referer"], "https://a");
        assert_eq!(p.id, "f");
    }
}
