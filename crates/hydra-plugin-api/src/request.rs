//! Requests and replies for the plugin's exported methods.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::plan::{Plan, Source};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub url: String,
    /// Headers the address arrived with, such as a browser extension's `Referer`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolve {
    NotClaimed,
    Plan(Plan),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub url: String,
    pub plan_id: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshedTrack {
    pub id: String,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Refreshed {
    pub tracks: Vec<RefreshedTrack>,
}

/// Reply of the `hooks` method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hooks {
    pub api: i32,
    pub hooks: Vec<String>,
}

/// A rule's decision before a claimed address is enqueued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueDecision {
    pub allow: bool,
    #[serde(default)]
    pub reason: Option<String>,
}
impl Default for EnqueueDecision {
    fn default() -> Self {
        Self {
            allow: true,
            reason: None,
        }
    }
}
/// A completed job, without browser credentials or access to arbitrary host files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub url: String,
    pub name: String,
    pub size: u64,
}
/// Private input/output paths relative to the plugin's data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRequest {
    pub job: CompleteRequest,
    pub input: String,
    pub output: String,
}
/// Whether the post-processor produced the requested output file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Processed {
    #[serde(default)]
    pub changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_reply_shapes() {
        assert_eq!(
            serde_json::from_str::<Resolve>(r#""not_claimed""#).unwrap(),
            Resolve::NotClaimed
        );
        let r: Resolve = serde_json::from_str(r#"{"plan":{"id":"p","tracks":[]}}"#).unwrap();
        assert!(matches!(r, Resolve::Plan(_)));
    }

    #[test]
    fn hooks_reply_parses() {
        let h: Hooks = serde_json::from_str(r#"{"api":1,"hooks":["resolve"]}"#).unwrap();
        assert_eq!(h.hooks, ["resolve"]);
    }
}
