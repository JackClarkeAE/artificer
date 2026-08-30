//! Rich result and error types for the Artificer API.

use std::collections::BTreeMap;
use std::time::Duration;

use artificer_protocol::{Aabb3, Diagnostic, EntityKind, EntityRef, SnapshotId, TopologyCounts};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The rich result returned from executing a command in a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub step_label: String,
    pub snapshot_id: SnapshotId,
    pub topology: TopologyCounts,
    pub bounds: Option<Aabb3>,
    /// Named entities produced or modified by this operation, indexed by role/identifier.
    pub entities: BTreeMap<String, EntityInfo>,
    pub diagnostics: Vec<Diagnostic>,
    pub elapsed_ms: u64,
    pub summary: String,
}

impl CommandResult {
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms)
    }
}

/// Detailed introspection of an entity produced or selected in the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityInfo {
    pub kind: EntityKind,
    pub entity_ref: EntityRef,
    pub geometry_description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
}

/// Categorization of API errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    InvalidInput,
    SelectorNotFound,
    SelectorAmbiguous,
    KernelError,
    ValidationFailed,
    SessionError,
    ScriptError,
    IoError,
}

/// A structured error with human and AI actionable suggestions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Error)]
#[error("API Error ({code:?}): {message}")]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<EntityInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

impl ApiError {
    #[must_use]
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: None,
            candidates: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    #[must_use]
    pub fn with_candidates(mut self, candidates: Vec<EntityInfo>) -> Self {
        self.candidates = candidates;
        self
    }

    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }
}

impl From<artificer_protocol::KernelError> for ApiError {
    fn from(error: artificer_protocol::KernelError) -> Self {
        Self {
            code: ApiErrorCode::KernelError,
            message: error.to_string(),
            suggestion: None,
            candidates: Vec::new(),
            diagnostics: error.diagnostics,
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        Self::new(ApiErrorCode::IoError, error.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(ApiErrorCode::InvalidInput, error.to_string())
    }
}
