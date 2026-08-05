//! Golden fixtures for the IPC contract.
//!
//! This test serializes one representative value of every type that crosses
//! the Tauri boundary and compares the result against a committed JSON file.
//! The TypeScript side (`apps/desktop/src/lib/protocol.test.ts`) validates
//! that same file against its Zod schemas.
//!
//! Together the two make drift impossible to merge silently: change a Rust
//! type without updating `packages/protocol`, and one of them fails.
//!
//! To accept an intentional change:
//!
//! ```text
//! UPDATE_PROTOCOL_FIXTURES=1 cargo test -p commonspace-core --test protocol_fixtures
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use commonspace_core::*;
use serde_json::{json, Value};
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol-samples.json")
}

/// Fixed ids and timestamps so the fixture is stable across runs.
const TASK: &str = "task_0000000000000000000000000000fixt";
const CONV: &str = "conv_0000000000000000000000000000fixt";
const WHEN: &str = "2026-08-05T12:00:00Z";

fn timestamp() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(WHEN)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn sample_plan() -> TaskPlan {
    TaskPlan {
        steps: vec![PlanStep {
            title: "Read 12 documents".into(),
            detail: Some("Contracts folder".into()),
        }],
        paths_accessed: vec!["C:/ws/contracts".into()],
        paths_likely_modified: vec!["C:/ws/summary.docx".into()],
        external_services: vec![],
        consequential_actions: vec!["Create summary.docx".into()],
        deliverables: vec!["A summary document".into()],
        requires_approval: true,
    }
}

fn sample_artifact() -> Artifact {
    Artifact {
        id: ArtifactId("art_0000000000000000000000000000fixt".into()),
        task_id: TaskId(TASK.into()),
        kind: ArtifactKind::Docx,
        path: "C:/ws/summary.docx".into(),
        name: "summary.docx".into(),
        modified_existing: false,
        backup_path: None,
        file_operation_id: Some(FileOperationId(
            "fop_0000000000000000000000000000fixt".into(),
        )),
        change_summary: Some("Created from 12 contracts".into()),
        created_at: timestamp(),
    }
}

fn sample_permission_request() -> PermissionRequest {
    PermissionRequest {
        id: PermissionRequestId("perm_000000000000000000000000000fixt".into()),
        task_id: TaskId(TASK.into()),
        session_id: None,
        operation: OperationClass::Delete,
        summary: "Move 3 duplicate files to the trash".into(),
        paths: vec!["C:/ws/copy of a.txt".into()],
        items: vec!["copy of a.txt".into()],
        risk: RiskLevel::High,
        irreversible: false,
        requested_at: timestamp(),
    }
}

/// Every sample, keyed by the name the TypeScript test looks for.
fn samples() -> Value {
    json!({
        "events": [
            AgentEvent::MessageStarted {
                message_id: MessageId("msg_0000000000000000000000000000fixt".into()),
                role: MessageRole::Assistant,
            },
            AgentEvent::MessageDelta {
                message_id: MessageId("msg_0000000000000000000000000000fixt".into()),
                text: "Hello".into(),
            },
            AgentEvent::ReasoningSummary { text: "Considering the folder".into() },
            AgentEvent::PlanCreated { plan: sample_plan() },
            AgentEvent::PlanUpdated { plan: sample_plan() },
            AgentEvent::ToolRequested {
                call_id: ToolCallId("tool_000000000000000000000000000fixt".into()),
                tool: "Read".into(),
                title: "Reading report.docx".into(),
                paths: vec!["C:/ws/report.docx".into()],
            },
            AgentEvent::ToolStarted {
                call_id: ToolCallId("tool_000000000000000000000000000fixt".into()),
                title: "Reading 12 documents".into(),
                detail: None,
            },
            AgentEvent::ToolProgress {
                call_id: ToolCallId("tool_000000000000000000000000000fixt".into()),
                detail: "8 of 12".into(),
            },
            AgentEvent::ToolCompleted {
                call_id: ToolCallId("tool_000000000000000000000000000fixt".into()),
                status: ToolStatus::Succeeded,
                summary: Some("Read 12 documents".into()),
            },
            AgentEvent::PermissionRequested { request: sample_permission_request() },
            AgentEvent::ArtifactCreated { artifact: sample_artifact() },
            AgentEvent::ArtifactModified { artifact: sample_artifact() },
            AgentEvent::Warning { message: "One file could not be read".into() },
            AgentEvent::Error {
                error: AgentErrorInfo {
                    code: "provider_exited".into(),
                    message: "Claude Code ended unexpectedly.".into(),
                    recovery: Some("Check the developer details, then try again.".into()),
                    transient: true,
                },
            },
            AgentEvent::TaskCompleted {
                summary: "Created summary.docx from 12 contracts.".into(),
                usage: Some(UsageInfo { input_tokens: Some(1200), output_tokens: Some(340) }),
            },
        ],
        "install_status": [
            InstallStatus::Installed { version: "2.1.222".into(), path: "C:/npm/claude.cmd".into() },
            InstallStatus::NotInstalled,
            InstallStatus::Broken { detail: "timed out".into() },
        ],
        "auth_status": [
            AuthStatus::NotInstalled,
            AuthStatus::SignedOut,
            AuthStatus::Subscription { plan_hint: Some("Claude Max".into()) },
            AuthStatus::Subscription { plan_hint: None },
            AuthStatus::ApiKey,
            AuthStatus::LocalModel,
            AuthStatus::Error { detail: "could not run the CLI".into() },
        ],
        "provider_ids": [
            ProviderId::ClaudeCode,
            ProviderId::CodexCli,
            ProviderId::GeminiCli,
            ProviderId::OpenCode,
            ProviderId::ApiCompatible,
            ProviderId::LocalModel,
        ],
        "task_states": [
            TaskState::Draft,
            TaskState::Planning,
            TaskState::AwaitingApproval,
            TaskState::Running,
            TaskState::Paused,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::RolledBack,
        ],
        "operation_classes": [
            OperationClass::Read,
            OperationClass::Create,
            OperationClass::Modify,
            OperationClass::Rename,
            OperationClass::Move,
            OperationClass::Delete,
            OperationClass::Execute,
            OperationClass::Install,
            OperationClass::NetworkFetch,
            OperationClass::Upload,
            OperationClass::Send,
            OperationClass::Publish,
            OperationClass::Secret,
        ],
        "artifact_kinds": [
            ArtifactKind::Docx, ArtifactKind::Xlsx, ArtifactKind::Pptx, ArtifactKind::Pdf,
            ArtifactKind::Markdown, ArtifactKind::Text, ArtifactKind::Image,
            ArtifactKind::CodeDiff, ArtifactKind::Other,
        ],
        "capabilities": AdapterCapabilities {
            models: vec!["default".into(), "sonnet".into()],
            supports_resume: true,
            attachment_types: vec![],
            context_tokens: None,
            supports_permission_bridge: true,
        },
        "health_report": HealthReport {
            healthy: false,
            checks: vec![
                HealthCheck { name: "Installed".into(), passed: true, detail: Some("2.1.222".into()) },
                HealthCheck { name: "Signed in".into(), passed: false, detail: None },
            ],
        },
        "operation_results": [
            OperationResult {
                success: true,
                created: vec!["C:/ws/summary.docx".into()],
                modified: vec![],
                backups: vec![],
                warnings: vec![],
                validation: ValidationOutcome::Passed,
                user_summary: "Created summary.docx".into(),
                diagnostics: None,
            },
            OperationResult::failed("This change could not be undone.", "the file changed since"),
            OperationResult::ok("Looked through the folder"),
        ],
        "permission_request": sample_permission_request(),
        "plan": sample_plan(),
        "artifact": sample_artifact(),
        "conversation_id": CONV,
    })
}

#[test]
fn protocol_fixtures_match_the_committed_file() {
    let actual = serde_json::to_string_pretty(&samples()).unwrap() + "\n";
    let path = fixture_path();

    if std::env::var("UPDATE_PROTOCOL_FIXTURES").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        eprintln!("updated {}", path.display());
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}.\nRun with UPDATE_PROTOCOL_FIXTURES=1 to create it.",
            path.display()
        )
    });

    // Compare parsed JSON so line endings can't cause a spurious failure on
    // Windows checkouts.
    let actual_value: Value = serde_json::from_str(&actual).unwrap();
    let expected_value: Value = serde_json::from_str(&expected).unwrap();
    assert_eq!(
        actual_value, expected_value,
        "\nThe IPC contract changed. If that was intentional, update \
         packages/protocol/src/index.ts to match, then re-run with \
         UPDATE_PROTOCOL_FIXTURES=1.\n"
    );
}
