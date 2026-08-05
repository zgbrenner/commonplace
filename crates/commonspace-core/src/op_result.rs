//! The structured result every deterministic operation returns. Success is
//! only ever reported after on-disk verification — never because an agent
//! claimed it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Outcome of validating a produced file (re-parse with an independent
/// reader, required-parts check, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ValidationOutcome {
    /// Validation ran and passed.
    Passed,
    /// Validation ran and failed; the operation must not report success.
    Failed { detail: String },
    /// No validator exists for this operation type (e.g. plain directory
    /// listing). Distinct from "passed" — the UI does not show a checkmark.
    NotApplicable,
}

/// Structured result of one deterministic operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationResult {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backups: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub validation: ValidationOutcome,
    /// Plain-language summary for the activity timeline.
    pub user_summary: String,
    /// Technical detail for the developer view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
}

impl OperationResult {
    /// A successful result with a summary and no file changes.
    pub fn ok(user_summary: impl Into<String>) -> Self {
        Self {
            success: true,
            created: Vec::new(),
            modified: Vec::new(),
            backups: Vec::new(),
            warnings: Vec::new(),
            validation: ValidationOutcome::NotApplicable,
            user_summary: user_summary.into(),
            diagnostics: None,
        }
    }

    /// A failed result. `success` is false and validation reflects the failure.
    pub fn failed(user_summary: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            success: false,
            created: Vec::new(),
            modified: Vec::new(),
            backups: Vec::new(),
            warnings: Vec::new(),
            validation: ValidationOutcome::Failed { detail: detail.into() },
            user_summary: user_summary.into(),
            diagnostics: None,
        }
    }
}
