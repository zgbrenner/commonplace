//! Typed Tauri commands. Each one is a thin, auditable entry point: the
//! frontend can call exactly these and nothing else. There is no command that
//! accepts raw SQL, a shell string, or an arbitrary path to write.

use commonspace_core::{
    AgentEvent, Artifact, AuthStatus, ConversationId, InstallStatus, OperationResult,
    PermissionDecision, PermissionRequestId, ProviderId, TaskId, WorkspaceId,
};
use commonspace_runtime::StartTask;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::ipc::Channel;
use tauri::State;

use crate::state::AppState;

/// Errors returned to the frontend. Always a plain-language message plus an
/// optional recovery hint — never a raw Rust `Debug` string.
#[derive(Debug, Serialize)]
pub struct CommandError {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<String>,
}

impl CommandError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            recovery: None,
        }
    }
    pub(crate) fn with_recovery(message: impl Into<String>, recovery: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            recovery: Some(recovery.into()),
        }
    }
}

impl<E: std::fmt::Display> From<E> for CommandError {
    fn from(error: E) -> Self {
        Self::new(error.to_string())
    }
}

type Result<T> = std::result::Result<T, CommandError>;

/* ---------------------------------------------------------- connections */

/// One row of the Connections screen, with an honest billing explanation.
#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    provider: ProviderId,
    display_name: String,
    install: InstallStatus,
    auth: AuthStatus,
    capabilities: commonspace_core::AdapterCapabilities,
    billing_note: String,
    sign_in_command: String,
    sign_in_explanation: String,
}

#[tauri::command]
pub async fn list_connections(state: State<'_, AppState>) -> Result<Vec<ConnectionInfo>> {
    let mut out = Vec::new();
    for adapter in state.adapters() {
        let install = adapter.detect().await;
        let auth = adapter.auth_status().await;
        // An explicit refresh is the freshest information there is; let
        // task starts reuse it instead of probing again per message.
        state.record_auth_status(adapter.id(), &auth);
        let instructions = adapter.auth_instructions();
        out.push(ConnectionInfo {
            provider: adapter.id(),
            display_name: adapter.id().display_name().to_string(),
            billing_note: billing_note(&auth),
            install,
            auth,
            capabilities: adapter.capabilities(),
            sign_in_command: if instructions.args.is_empty() {
                instructions.command.clone()
            } else {
                format!("{} {}", instructions.command, instructions.args.join(" "))
            },
            sign_in_explanation: instructions.explanation,
        });
    }
    Ok(out)
}

/// Plain-language, accurate statement of who pays for what.
fn billing_note(auth: &AuthStatus) -> String {
    match auth {
        AuthStatus::Subscription {
            plan_hint: Some(plan),
        } => format!(
            "Connected through your {plan} subscription. Usage counts against that plan's limits, \
             not against a separate Commonspace charge."
        ),
        AuthStatus::Subscription { plan_hint: None } => {
            "Connected through your provider subscription. Usage counts against that plan's \
             limits, not against a separate Commonspace charge."
                .into()
        }
        AuthStatus::ApiKey => "Connected with an API key. Your provider bills this usage per \
                               token, separately from any subscription."
            .into(),
        AuthStatus::LocalModel => {
            "Running locally. Nothing is sent to a provider and nothing is billed.".into()
        }
        AuthStatus::SignedOut => {
            "Not signed in yet. Sign-in happens in the provider's own tool; Commonspace never \
             sees your credentials."
                .into()
        }
        AuthStatus::NotInstalled => {
            "The provider's official tool isn't installed on this computer yet.".into()
        }
        AuthStatus::Error { detail } => {
            format!("Commonspace couldn't check this connection: {detail}")
        }
    }
}

#[tauri::command]
pub async fn provider_health(
    state: State<'_, AppState>,
    provider: ProviderId,
) -> Result<commonspace_core::HealthReport> {
    let adapter = state
        .adapter(provider)
        .ok_or_else(|| CommandError::new("That provider isn't available in this build."))?;
    Ok(adapter.health().await)
}

#[derive(Debug, Serialize)]
pub struct SignInInstructions {
    command: String,
    explanation: String,
}

#[tauri::command]
pub fn sign_in_instructions(
    state: State<'_, AppState>,
    provider: ProviderId,
) -> Result<SignInInstructions> {
    let adapter = state
        .adapter(provider)
        .ok_or_else(|| CommandError::new("That provider isn't available in this build."))?;
    let instructions = adapter.auth_instructions();
    Ok(SignInInstructions {
        command: if instructions.args.is_empty() {
            instructions.command
        } else {
            format!("{} {}", instructions.command, instructions.args.join(" "))
        },
        explanation: instructions.explanation,
    })
}

/* ----------------------------------------------------------- workspaces */

#[derive(Debug, Serialize)]
pub struct WorkspaceInfo {
    id: String,
    name: String,
    roots: Vec<PathBuf>,
}

#[tauri::command]
pub fn list_workspaces(state: State<'_, AppState>) -> Result<Vec<WorkspaceInfo>> {
    Ok(state
        .storage()
        .list_workspaces()?
        .into_iter()
        .map(|w| WorkspaceInfo {
            id: w.id.0,
            name: w.name,
            roots: w.roots,
        })
        .collect())
}

#[tauri::command]
pub fn create_workspace(
    state: State<'_, AppState>,
    name: String,
    roots: Vec<PathBuf>,
) -> Result<WorkspaceInfo> {
    if roots.is_empty() {
        return Err(CommandError::with_recovery(
            "A workspace needs at least one folder.",
            "Choose a folder Commonspace may work in.",
        ));
    }
    // Folders are authorized by the user through the native picker; store the
    // resolved paths so later scope checks compare like with like.
    let workspace = state.storage().create_workspace(&name, &roots)?;
    Ok(WorkspaceInfo {
        id: workspace.id.0,
        name: workspace.name,
        roots: workspace.roots,
    })
}

#[tauri::command]
pub fn add_workspace_folder(
    state: State<'_, AppState>,
    workspace_id: String,
    root: PathBuf,
) -> Result<()> {
    state
        .storage()
        .add_authorized_root(&WorkspaceId(workspace_id), &root)?;
    Ok(())
}

/* -------------------------------------------------------- conversations */

#[derive(Debug, Serialize)]
pub struct ConversationInfo {
    id: String,
    workspace_id: Option<String>,
    title: String,
    updated_at: String,
}

#[tauri::command]
pub fn list_conversations(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<ConversationInfo>> {
    Ok(state
        .storage()
        .list_conversations(limit.unwrap_or(50))?
        .into_iter()
        .map(|c| ConversationInfo {
            id: c.id.0,
            workspace_id: c.workspace_id.map(|w| w.0),
            title: c.title,
            updated_at: c.updated_at,
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct MessageInfo {
    id: String,
    conversation_id: String,
    role: String,
    content: String,
    created_at: String,
}

#[tauri::command]
pub fn list_messages(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<MessageInfo>> {
    Ok(state
        .storage()
        .list_messages(&ConversationId(conversation_id))?
        .into_iter()
        .map(|m| MessageInfo {
            id: m.id.0,
            conversation_id: m.conversation_id.0,
            role: match m.role {
                commonspace_core::MessageRole::User => "user".into(),
                commonspace_core::MessageRole::Assistant => "assistant".into(),
            },
            content: m.content,
            created_at: m.created_at,
        })
        .collect())
}

/* --------------------------------------------------------------- tasks */

#[derive(Debug, Deserialize)]
pub struct StartTaskArgs {
    conversation_id: Option<String>,
    workspace_id: String,
    provider: ProviderId,
    prompt: String,
    model: Option<String>,
    resume: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartedTask {
    task_id: String,
    conversation_id: String,
}

/// Start a task. Normalized events stream over `on_event` — one channel per
/// task, which the Tauri docs recommend over the global event bus for
/// ordered, high-frequency streams.
#[tauri::command]
pub async fn start_task(
    state: State<'_, AppState>,
    args: StartTaskArgs,
    on_event: Channel<AgentEvent>,
) -> Result<StartedTask> {
    let workspace_id = WorkspaceId(args.workspace_id);
    let conversation = match args.conversation_id {
        Some(id) => ConversationId(id),
        None => {
            let title = title_from_prompt(&args.prompt);
            state
                .storage()
                .create_conversation(Some(&workspace_id), &title)?
                .id
        }
    };

    let adapter = state.adapter(args.provider).ok_or_else(|| {
        CommandError::with_recovery(
            "That provider isn't available in this build.",
            "Choose a different agent in the composer.",
        )
    })?;

    // Served from a short-lived cache: probing spawns the provider CLI, and
    // doing that for every message means seconds of extra latency per send.
    match state.auth_status_cached(adapter).await {
        AuthStatus::Subscription { .. } | AuthStatus::ApiKey | AuthStatus::LocalModel => {}
        AuthStatus::NotInstalled => {
            return Err(CommandError::with_recovery(
                format!(
                    "{} isn't installed on this computer.",
                    args.provider.display_name()
                ),
                "Open Connections to install it.",
            ));
        }
        AuthStatus::SignedOut => {
            return Err(CommandError::with_recovery(
                format!("{} isn't signed in yet.", args.provider.display_name()),
                "Open Connections to sign in with the provider's own tool.",
            ));
        }
        AuthStatus::Error { detail } => {
            return Err(CommandError::new(format!(
                "Commonspace couldn't use {}: {detail}",
                args.provider.display_name()
            )));
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if on_event.send(event).is_err() {
                break; // the window closed; the task continues and is journaled
            }
        }
    });

    let handle = state
        .orchestrator()
        .start(
            adapter,
            StartTask {
                conversation_id: conversation.clone(),
                workspace_id,
                provider: args.provider,
                prompt: args.prompt,
                model: args.model,
                resume: args.resume,
            },
            tx,
        )
        .await?;

    let task_id = handle.task_id.0.clone();
    state.track(handle);
    Ok(StartedTask {
        task_id,
        conversation_id: conversation.0,
    })
}

fn title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();
    let mut title: String = first_line.chars().take(70).collect();
    if first_line.chars().count() > 70 {
        title.push('…');
    }
    if title.is_empty() {
        "New task".into()
    } else {
        title
    }
}

#[tauri::command]
pub async fn cancel_task(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let id = TaskId(task_id);
    if let Some(handle) = state.take_running(&id) {
        handle.cancel(state.orchestrator().broker()).await;
    }
    // Cancelling an already-finished task is not an error from the user's
    // point of view; the state machine simply refuses the transition.
    let _ = state
        .storage()
        .transition_task(&id, commonspace_core::TaskState::Cancelled);
    Ok(())
}

#[tauri::command]
pub fn answer_permission(
    state: State<'_, AppState>,
    request_id: String,
    approve: bool,
    scope: Option<String>,
) -> Result<bool> {
    let decision = if approve {
        let scope = match scope.as_deref() {
            Some("workspace") => commonspace_core::DecisionScope::Workspace,
            Some("task") => commonspace_core::DecisionScope::Task,
            _ => commonspace_core::DecisionScope::Once,
        };
        PermissionDecision::Approve { scope }
    } else {
        PermissionDecision::Deny
    };
    Ok(state
        .orchestrator()
        .broker()
        .respond(&PermissionRequestId(request_id), decision))
}

#[tauri::command]
pub fn list_task_artifacts(state: State<'_, AppState>, task_id: String) -> Result<Vec<Artifact>> {
    Ok(state.storage().list_artifacts(&TaskId(task_id))?)
}

/// Replay a task's persisted events — used when reopening a past task so the
/// timeline looks the same as it did live.
#[tauri::command]
pub fn list_task_events(
    state: State<'_, AppState>,
    task_id: String,
    after_seq: Option<i64>,
) -> Result<Vec<AgentEvent>> {
    Ok(state
        .storage()
        .events_since(&TaskId(task_id), after_seq.unwrap_or(0))?
        .into_iter()
        .map(|(_, event)| event)
        .collect())
}

#[tauri::command]
pub fn undo_file_operation(
    state: State<'_, AppState>,
    workspace_id: String,
    file_operation_id: String,
) -> Result<OperationResult> {
    Ok(state
        .orchestrator()
        .undo(&WorkspaceId(workspace_id), &file_operation_id)?)
}

/* ------------------------------------------------------------ artifacts */

/// Open an artifact in its default application. Only paths Commonspace has
/// journaled as artifacts are accepted, so the frontend cannot use this to
/// launch an arbitrary file.
#[tauri::command]
pub fn open_artifact(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    artifact_id: String,
) -> Result<()> {
    let artifact = find_artifact(&state, &task_id, &artifact_id)?;
    tauri_plugin_opener::open_path(artifact.path.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| {
            let _ = &app;
            CommandError::with_recovery(
                format!("Commonspace couldn't open that file: {e}"),
                "Try opening it from the folder instead.",
            )
        })
}

#[tauri::command]
pub fn reveal_artifact(
    state: State<'_, AppState>,
    task_id: String,
    artifact_id: String,
) -> Result<()> {
    let artifact = find_artifact(&state, &task_id, &artifact_id)?;
    tauri_plugin_opener::reveal_item_in_dir(&artifact.path)
        .map_err(|e| CommandError::new(format!("Commonspace couldn't show that file: {e}")))
}

fn find_artifact(
    state: &State<'_, AppState>,
    task_id: &str,
    artifact_id: &str,
) -> Result<Artifact> {
    state
        .storage()
        .list_artifacts(&TaskId(task_id.to_string()))?
        .into_iter()
        .find(|a| a.id.0 == artifact_id)
        .ok_or_else(|| CommandError::new("That file is no longer listed in this task."))
}

/* ------------------------------------------------------------- settings */

#[tauri::command]
pub fn get_setting(state: State<'_, AppState>, key: String) -> Result<Option<serde_json::Value>> {
    Ok(state.storage().get_setting(&key)?)
}

#[tauri::command]
pub fn set_setting(
    state: State<'_, AppState>,
    key: String,
    value: serde_json::Value,
) -> Result<()> {
    state.storage().set_setting(&key, &value)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_trimmed_and_never_empty() {
        assert_eq!(
            title_from_prompt("Organize my downloads\nplease"),
            "Organize my downloads"
        );
        assert_eq!(title_from_prompt("   "), "New task");
        let long = "a".repeat(200);
        let title = title_from_prompt(&long);
        assert_eq!(title.chars().count(), 71);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn billing_notes_are_specific_and_truthful() {
        let sub = billing_note(&AuthStatus::Subscription {
            plan_hint: Some("Claude Max".into()),
        });
        assert!(sub.contains("Claude Max"));
        assert!(sub.contains("not against a separate Commonspace charge"));

        let api = billing_note(&AuthStatus::ApiKey);
        assert!(api.contains("bills this usage per token"));

        let local = billing_note(&AuthStatus::LocalModel);
        assert!(local.contains("Nothing is sent"));

        // Never claims a subscription where there isn't one.
        let out = billing_note(&AuthStatus::SignedOut);
        assert!(!out.to_lowercase().contains("subscription usage"));
    }
}
