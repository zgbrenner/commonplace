//! The normalized agent event protocol. Every provider adapter translates
//! its CLI's output into exactly these events; the UI and persistence layer
//! never see provider-specific shapes (raw payloads are kept separately for
//! the developer view).

use crate::artifact::Artifact;
use crate::ids::{MessageId, ToolCallId};
use crate::permission::PermissionRequest;
use crate::plan::TaskPlan;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Role of a message in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// Terminal status of one tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

/// Token usage reported by the provider, when available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

/// Structured error carried by `error` events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentErrorInfo {
    /// Stable machine code (e.g. "provider_unavailable", "auth_expired").
    pub code: String,
    /// Plain-language description shown to the user.
    pub message: String,
    /// Suggested recovery action, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
    /// True when retrying may succeed without user action.
    pub transient: bool,
}

/// The normalized event stream.
///
/// Serialized with a dotted `type` tag, e.g.
/// `{"type":"tool.started","call_id":"tool_…","title":"Reading 12 documents"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "message.started")]
    MessageStarted {
        message_id: MessageId,
        role: MessageRole,
    },

    #[serde(rename = "message.delta")]
    MessageDelta { message_id: MessageId, text: String },

    #[serde(rename = "reasoning.summary")]
    ReasoningSummary { text: String },

    #[serde(rename = "plan.created")]
    PlanCreated { plan: TaskPlan },

    #[serde(rename = "plan.updated")]
    PlanUpdated { plan: TaskPlan },

    /// The agent asked to use a tool; policy evaluation happens next.
    #[serde(rename = "tool.requested")]
    ToolRequested {
        call_id: ToolCallId,
        /// Machine-readable tool name (developer view).
        tool: String,
        /// Human-readable intent ("Read report.docx").
        title: String,
        /// Paths involved, resolved, when known.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<PathBuf>,
    },

    #[serde(rename = "tool.started")]
    ToolStarted {
        call_id: ToolCallId,
        /// Human-readable activity ("Reading 12 documents").
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    #[serde(rename = "tool.progress")]
    ToolProgress { call_id: ToolCallId, detail: String },

    #[serde(rename = "tool.completed")]
    ToolCompleted {
        call_id: ToolCallId,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },

    #[serde(rename = "permission.requested")]
    PermissionRequested { request: PermissionRequest },

    #[serde(rename = "artifact.created")]
    ArtifactCreated { artifact: Artifact },

    #[serde(rename = "artifact.modified")]
    ArtifactModified { artifact: Artifact },

    #[serde(rename = "warning")]
    Warning { message: String },

    #[serde(rename = "error")]
    Error { error: AgentErrorInfo },

    #[serde(rename = "task.completed")]
    TaskCompleted {
        /// Result summary in plain language.
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<UsageInfo>,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::ids::*;
    use crate::permission::{OperationClass, RiskLevel};

    #[test]
    fn event_tags_are_dotted() {
        let ev = AgentEvent::ToolStarted {
            call_id: ToolCallId("tool_1".into()),
            title: "Reading 12 documents".into(),
            detail: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "tool.started");
        assert_eq!(v["title"], "Reading 12 documents");
        assert!(v.get("detail").is_none(), "None fields are omitted");
    }

    #[test]
    fn round_trip_permission_request() {
        let ev = AgentEvent::PermissionRequested {
            request: PermissionRequest {
                id: PermissionRequestId::generate(),
                task_id: TaskId::generate(),
                session_id: None,
                operation: OperationClass::Delete,
                summary: "Delete 3 duplicate files".into(),
                paths: vec!["C:/tmp/a.txt".into()],
                items: vec!["a.txt".into()],
                risk: RiskLevel::High,
                irreversible: false,
                requested_at: chrono::Utc::now(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn all_event_names_round_trip() {
        // Guard against tag typos: parse each dotted name back.
        for (ev, tag) in [
            (
                AgentEvent::Warning {
                    message: "w".into(),
                },
                "warning",
            ),
            (
                AgentEvent::TaskCompleted {
                    summary: "done".into(),
                    usage: None,
                },
                "task.completed",
            ),
            (
                AgentEvent::ReasoningSummary { text: "t".into() },
                "reasoning.summary",
            ),
        ] {
            let v = serde_json::to_value(&ev).unwrap();
            assert_eq!(v["type"], tag);
            let back: AgentEvent = serde_json::from_value(v).unwrap();
            assert_eq!(back, ev);
        }
    }
}
