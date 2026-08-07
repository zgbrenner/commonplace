//! The end-to-end vertical slice, run against a real provider CLI.
//!
//! This is the test that proves Commonspace actually works rather than
//! merely compiling: it authorizes a folder, starts a real agent session,
//! lets the agent discover and call Commonspace's MCP tools, answers the
//! permission request the policy engine raises, and then verifies on disk
//! that the file was written, journaled, and can be undone.
//!
//! `#[ignore]` by default because it spawns the user's installed and
//! authenticated CLI and consumes a small amount of real subscription usage:
//!
//! ```text
//! cargo test -p commonspace-runtime --test vertical_slice -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use commonspace_agents::{AgentAdapter, ClaudeCodeAdapter};
use commonspace_core::{
    AgentEvent, AuthStatus, DecisionScope, InstallStatus, PermissionDecision, ProviderId, TaskId,
    TaskState,
};
use commonspace_runtime::{Orchestrator, PlanDecision, StartTask};
use commonspace_storage::Storage;
use std::sync::Arc;
use std::time::Duration;

/// A small messy folder, like a real user's Downloads.
fn seed_workspace(dir: &std::path::Path) {
    std::fs::write(
        dir.join("invoice_2026_03.txt"),
        "Acme Ltd — 1,240.00 EUR — 2026-03-11",
    )
    .unwrap();
    std::fs::write(
        dir.join("invoice_2026_04.txt"),
        "Acme Ltd — 980.00 EUR — 2026-04-09",
    )
    .unwrap();
    std::fs::write(dir.join("notes.md"), "# Notes\n\nCall the supplier back.").unwrap();
}

struct Outcome {
    events: Vec<AgentEvent>,
    permission_summaries: Vec<String>,
}

/// Stand in for the user pressing "Start" on the plan card. Parking happens
/// just after `plan.created` is emitted, so retry briefly; a harmless plan
/// that auto-proceeded (or already finished) is detected via the task state.
async fn approve_plan_when_parked(orchestrator: &Orchestrator, task_id: &TaskId) {
    for _ in 0..100 {
        if orchestrator
            .resolve_plan_decision(task_id, PlanDecision::Start)
            .await
            .is_ok()
        {
            return;
        }
        let state = orchestrator.storage().get_task(task_id).map(|t| t.state);
        if matches!(
            state,
            Ok(TaskState::Running | TaskState::Completed | TaskState::Failed)
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Run one task to completion, approving its plan and every permission
/// request, and recording what was asked.
async fn run_task(
    orchestrator: &Orchestrator,
    adapter: Arc<dyn AgentAdapter>,
    request: StartTask,
) -> Outcome {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = orchestrator
        .start(adapter, request, tx)
        .await
        .expect("task should start");
    let task_id = handle.task_id.clone();

    let broker = orchestrator.broker().clone();
    let mut events = Vec::new();
    let mut permission_summaries = Vec::new();

    let collect = async {
        while let Some(event) = rx.recv().await {
            if let AgentEvent::PermissionRequested { request } = &event {
                permission_summaries.push(request.summary.clone());
                // Stand in for the user clicking "Allow for this task".
                broker.respond(
                    &request.id,
                    PermissionDecision::Approve {
                        scope: DecisionScope::Task,
                    },
                );
            }
            let approve_plan = matches!(&event, AgentEvent::PlanCreated { .. });
            let terminal = matches!(
                event,
                AgentEvent::TaskCompleted { .. } | AgentEvent::Error { .. }
            );
            events.push(event);
            if approve_plan {
                approve_plan_when_parked(orchestrator, &task_id).await;
            }
            if terminal {
                break;
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(420), collect)
        .await
        .expect("the task should finish within 7 minutes");

    handle.cancel(orchestrator.broker()).await;
    Outcome {
        events,
        permission_summaries,
    }
}

#[tokio::test]
#[ignore = "requires installed + authenticated Claude Code; uses real subscription usage"]
async fn organize_a_folder_end_to_end() {
    let adapter: Arc<dyn AgentAdapter> = Arc::new(ClaudeCodeAdapter);
    assert!(
        matches!(adapter.detect().await, InstallStatus::Installed { .. }),
        "Claude Code must be installed for this test"
    );
    assert!(
        matches!(
            adapter.auth_status().await,
            AuthStatus::Subscription { .. } | AuthStatus::ApiKey
        ),
        "Claude Code must be signed in for this test"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let ws_dir = tmp.path().join("Documents");
    std::fs::create_dir_all(&ws_dir).expect("workspace dir");
    seed_workspace(&ws_dir);

    let storage = Arc::new(Storage::open_in_memory().expect("storage"));
    let orchestrator = Orchestrator::new(Arc::clone(&storage), tmp.path().to_path_buf());
    let workspace = storage
        .create_workspace("Documents", std::slice::from_ref(&ws_dir))
        .expect("workspace");
    let conversation = storage
        .create_conversation(Some(&workspace.id), "Index the folder")
        .expect("conversation");

    // ---- 1. the agent reads the folder and creates a file ----------------

    let index_path = ws_dir.join("INDEX.md");
    let outcome = run_task(
        &orchestrator,
        Arc::clone(&adapter),
        StartTask {
            conversation_id: conversation.id.clone(),
            workspace_id: workspace.id.clone(),
            provider: ProviderId::ClaudeCode,
            prompt: format!(
                "Use only the tools from the `commonspace` MCP server. \
                 First call list_folder on {}. Then call create_file to write a file at {} \
                 whose contents are a Markdown bullet list of the names of the files you found. \
                 Do not use any other tools, and do not ask me questions.",
                ws_dir.display(),
                index_path.display()
            ),
            model: Some("sonnet".into()),
            resume: None,
        },
    )
    .await;

    let errors: Vec<&AgentEvent> = outcome
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { .. }))
        .collect();
    assert!(errors.is_empty(), "the task reported errors: {errors:?}");

    // The file exists, was written by Commonspace's own verified tool, and
    // actually mentions what the agent found.
    assert!(
        index_path.is_file(),
        "INDEX.md was not created. Events: {:#?}",
        outcome.events
    );
    let contents = std::fs::read_to_string(&index_path).expect("read INDEX.md");
    assert!(
        contents.contains("invoice_2026_03") && contents.contains("notes.md"),
        "INDEX.md does not list the folder's files: {contents:?}"
    );

    // The timeline carried human-readable activity, not raw tool names.
    let activity: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { title, .. } => Some(title.clone()),
            _ => None,
        })
        .collect();
    assert!(
        activity.iter().any(|t| t.contains("Creating INDEX.md")),
        "expected a readable 'Creating INDEX.md' step, got {activity:?}"
    );

    // An artifact was surfaced for the file.
    let artifact = outcome
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ArtifactCreated { artifact } => Some(artifact.clone()),
            _ => None,
        })
        .expect("an artifact.created event for INDEX.md");
    assert_eq!(artifact.name, "INDEX.md");
    assert!(!artifact.modified_existing);

    // ---- 2. the change is journaled and undoable -------------------------

    let file_operation_id = artifact
        .file_operation_id
        .clone()
        .expect("the artifact should carry an undo record");
    let result = orchestrator
        .undo(&workspace.id, file_operation_id.as_ref())
        .expect("undo should run");
    assert!(result.success, "undo failed: {result:?}");
    assert!(!index_path.exists(), "undo should have removed INDEX.md");

    // ---- 3. the task and its history persisted ---------------------------

    let tasks_events_persisted = storage
        .events_since(
            &storage
                .list_conversations(10)
                .expect("conversations")
                .first()
                .map(|_| ())
                .map(|_| {
                    // The task id is on the artifact; use it directly.
                    artifact.task_id.clone()
                })
                .expect("a conversation exists"),
            0,
        )
        .expect("events");
    assert!(
        !tasks_events_persisted.is_empty(),
        "the task's events should be replayable from storage"
    );
    // The terminal state is written once the provider's stream closes, which
    // happens just after the last event reaches the UI; wait for it to settle
    // rather than racing that hand-off.
    let mut state = storage.get_task(&artifact.task_id).expect("task row").state;
    for _ in 0..100 {
        if state.is_terminal() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        state = storage.get_task(&artifact.task_id).expect("task row").state;
    }
    assert_eq!(state, TaskState::Completed, "task should have completed");

    eprintln!(
        "vertical slice ok — {} events, permission asked for: {:?}",
        outcome.events.len(),
        outcome.permission_summaries
    );
}

/// Declining the approval must leave the workspace untouched.
///
/// Deleting is used as the probe because an approved plan's envelope covers
/// in-workspace modifies, while deletes always ask. This exercises the gate
/// against a live model, so it depends on the agent actually attempting the
/// delete; the deterministic guarantee is covered by
/// `tools::tests::denied_modify_leaves_the_file_alone`.
#[tokio::test]
#[ignore = "requires installed + authenticated Claude Code; uses real subscription usage"]
async fn declining_an_approval_changes_nothing() {
    let adapter: Arc<dyn AgentAdapter> = Arc::new(ClaudeCodeAdapter);
    if !matches!(adapter.detect().await, InstallStatus::Installed { .. }) {
        eprintln!("skipping: Claude Code is not installed");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let ws_dir = tmp.path().join("Documents");
    std::fs::create_dir_all(&ws_dir).expect("workspace dir");
    std::fs::write(
        ws_dir.join("notes.md"),
        "# Notes

- Call the supplier back.
",
    )
    .expect("seed");

    let storage = Arc::new(Storage::open_in_memory().expect("storage"));
    let orchestrator = Orchestrator::new(Arc::clone(&storage), tmp.path().to_path_buf());
    let workspace = storage
        .create_workspace("Documents", std::slice::from_ref(&ws_dir))
        .expect("workspace");
    let conversation = storage
        .create_conversation(Some(&workspace.id), "Try to edit")
        .expect("conversation");

    let target = ws_dir.join("notes.md");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = orchestrator
        .start(
            Arc::clone(&adapter),
            StartTask {
                conversation_id: conversation.id,
                workspace_id: workspace.id,
                provider: ProviderId::ClaudeCode,
                // Every argument is spelled out so the agent makes the call
                // immediately instead of stopping to ask; what is under test
                // is Commonspace's approval gate, not the model's
                // willingness.
                prompt: format!(
                    "Call the `delete_to_trash` tool from the `commonspace` MCP server exactly \
                     once, with path={}. Every argument you need is in this message, so make \
                     the call immediately without asking me anything.",
                    target.display()
                ),
                model: Some("sonnet".into()),
                resume: None,
            },
            tx,
        )
        .await
        .expect("task should start");
    let task_id = handle.task_id.clone();

    let broker = orchestrator.broker().clone();
    let mut declined = false;
    let mut seen = Vec::new();
    let collect = async {
        while let Some(event) = rx.recv().await {
            if let AgentEvent::PermissionRequested { request } = &event {
                declined = true;
                broker.respond(&request.id, PermissionDecision::Deny);
            }
            let approve_plan = matches!(&event, AgentEvent::PlanCreated { .. });
            let terminal = matches!(
                event,
                AgentEvent::TaskCompleted { .. } | AgentEvent::Error { .. }
            );
            seen.push(event);
            if approve_plan {
                // The plan is approved — the point under test is that the
                // delete itself still asks, and declining it changes nothing.
                approve_plan_when_parked(&orchestrator, &task_id).await;
            }
            if terminal {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(420), collect)
        .await
        .expect("the task should finish within 7 minutes");
    handle.cancel(orchestrator.broker()).await;

    assert!(
        declined,
        "deleting a file must raise a permission request. Events: {seen:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "# Notes

- Call the supplier back.
",
        "declining must leave the file untouched"
    );
}
