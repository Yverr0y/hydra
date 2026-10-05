//! The one error enum used by plugin replies, host-function replies and the
//! front ends' attribution.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    NotClaimed,
    Unsupported,
    PermissionDenied,
    Network,
    ToolMissing,
    ToolFailed,
    InvalidInput,
    Deadline,
    OutOfFuel,
    OutOfMemory,
    Trap,
    InvalidPlan,
    InvalidReply,
    Cancelled,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotClaimed => "not_claimed",
            Self::Unsupported => "unsupported",
            Self::PermissionDenied => "permission_denied",
            Self::Network => "network",
            Self::ToolMissing => "tool_missing",
            Self::ToolFailed => "tool_failed",
            Self::InvalidInput => "invalid_input",
            Self::Deadline => "deadline",
            Self::OutOfFuel => "out_of_fuel",
            Self::OutOfMemory => "out_of_memory",
            Self::Trap => "trap",
            Self::InvalidPlan => "invalid_plan",
            Self::InvalidReply => "invalid_reply",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
        }
    }

    /// Whether the error is the plugin's own fault, and so feeds the circuit breaker.
    pub fn counts_toward_breaker(self) -> bool {
        matches!(
            self,
            Self::OutOfFuel
                | Self::OutOfMemory
                | Self::Trap
                | Self::InvalidPlan
                | Self::InvalidReply
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub code: ErrorCode,
    pub message: String,
}

impl PluginError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PluginError {}

/// The wire shape of a failed reply: `{"error": {"code": …, "message": …}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorReply {
    pub error: PluginError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plugin_faults_count_toward_the_breaker() {
        let counted: Vec<_> = [
            ErrorCode::NotClaimed,
            ErrorCode::Unsupported,
            ErrorCode::PermissionDenied,
            ErrorCode::Network,
            ErrorCode::ToolMissing,
            ErrorCode::ToolFailed,
            ErrorCode::InvalidInput,
            ErrorCode::Deadline,
            ErrorCode::OutOfFuel,
            ErrorCode::OutOfMemory,
            ErrorCode::Trap,
            ErrorCode::InvalidPlan,
            ErrorCode::InvalidReply,
            ErrorCode::Cancelled,
            ErrorCode::Internal,
        ]
        .into_iter()
        .filter(|c| c.counts_toward_breaker())
        .collect();
        assert_eq!(
            counted,
            [
                ErrorCode::OutOfFuel,
                ErrorCode::OutOfMemory,
                ErrorCode::Trap,
                ErrorCode::InvalidPlan,
                ErrorCode::InvalidReply
            ]
        );
    }

    #[test]
    fn wire_name_matches_as_str() {
        let json = serde_json::to_string(&ErrorCode::PermissionDenied).unwrap();
        assert_eq!(json, "\"permission_denied\"");
        assert_eq!(ErrorCode::PermissionDenied.as_str(), "permission_denied");
    }

    #[test]
    fn error_reply_parses_from_the_documented_shape() {
        let r: ErrorReply =
            serde_json::from_str(r#"{"error":{"code":"deadline","message":"slow"}}"#).unwrap();
        assert_eq!(r.error, PluginError::new(ErrorCode::Deadline, "slow"));
    }

    #[test]
    fn unknown_code_is_rejected_not_defaulted() {
        assert!(
            serde_json::from_str::<ErrorReply>(r#"{"error":{"code":"made_up","message":""}}"#)
                .is_err()
        );
    }
}
