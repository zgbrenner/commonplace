//! Shared permissions vocabulary. The deterministic evaluation engine lives
//! in `commonspace-permissions`; these types are the language it speaks with
//! the orchestrator, adapters, storage, and UI.

use crate::ids::{PermissionRequestId, SessionId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Classification of every operation the agent can attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationClass {
    /// Read file or folder contents.
    Read,
    /// Create a new file.
    Create,
    /// Modify an existing file's contents.
    Modify,
    /// Rename in place.
    Rename,
    /// Move between folders.
    Move,
    /// Delete (to safe trash unless permanent deletion is explicitly enabled).
    Delete,
    /// Run an executable or installer.
    Execute,
    /// Install a package.
    Install,
    /// Fetch from the network.
    NetworkFetch,
    /// Upload file contents to an external destination.
    Upload,
    /// Send a message or email.
    Send,
    /// Publish, purchase, submit a form, or make another external change.
    Publish,
    /// Access credentials or secrets.
    Secret,
}

/// Risk presented to the user alongside a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// The deterministic engine's verdict for one evaluated operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum PolicyVerdict {
    Allow,
    RequireApproval { reason: String },
    Deny { reason: String },
}

/// A permission request surfaced to the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: PermissionRequestId,
    pub task_id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub operation: OperationClass,
    /// Plain-language description ("Rename 14 files in Documents/Contracts").
    pub summary: String,
    /// Resolved absolute paths involved — what the user actually approves.
    pub paths: Vec<PathBuf>,
    /// Itemized sub-operations for batch requests, when available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    pub risk: RiskLevel,
    /// True when the action cannot be undone; the dialog must warn explicitly.
    pub irreversible: bool,
    pub requested_at: DateTime<Utc>,
}

/// How broadly a user decision applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionScope {
    /// This request only.
    Once,
    /// Remaining identical operations within this task.
    Task,
    /// This workspace, until revoked.
    Workspace,
}

/// The user's answer to a permission request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionDecision {
    Approve { scope: DecisionScope },
    Deny,
}
