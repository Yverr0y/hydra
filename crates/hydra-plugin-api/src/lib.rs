//! The contract between a hydra plugin and the host that runs it.
//!
//! Depends on `serde` only, so a guest build pulls in no networking and no
//! runtime. The semver of this crate is the semver of the plugin API.

pub mod abi;
pub mod error;
pub mod form;
pub mod limits;
pub mod manifest;
pub mod plan;
pub mod request;
pub mod select;

pub use error::{ErrorCode, ErrorReply, PluginError};
pub use form::{Answers, Field, FieldKind, Form, Value};
pub use manifest::{ExecEntry, Manifest, Permissions};
pub use plan::{Assemble, Plan, PlaylistEntry, Ranges, Source, Track, TrackKind};
pub use request::{
    CompleteRequest, EnqueueDecision, Hooks, ProcessRequest, Processed, RefreshRequest, Refreshed,
    RefreshedTrack, Resolve, ResolveRequest,
};
pub use select::{select, AudioPref, Preferences, Selection};
