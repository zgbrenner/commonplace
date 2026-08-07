//! Typed Tauri commands. Each one is a thin, auditable entry point: the
//! frontend can call exactly these and nothing else. There is no command that
//! accepts raw SQL, a shell string, or an arbitrary path to write.

use commonspace_core::{
    AgentEvent, Artifact, AuthStatus, ConversationId, InstallStatus, OperationResult,
    PermissionDecision, PermissionRequestId, ProviderId, TaskId, WorkspaceId,
};
use commonspace_runtime::StartTask;
use commonspace_storage::{
    AttachmentKind, AttachmentRecord, NewAttachment, ResumableSession, SearchHit, StorageError,
    TaskRow,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

#[tauri::command]
pub fn rename_conversation(
    state: State<'_, AppState>,
    conversation_id: String,
    title: String,
) -> Result<()> {
    if title.trim().is_empty() {
        return Err(CommandError::with_recovery(
            "A conversation needs a name.",
            "Type a title and try again.",
        ));
    }
    match state
        .storage()
        .rename_conversation(&ConversationId(conversation_id), &title)
    {
        Ok(()) => Ok(()),
        Err(StorageError::NotFound(_)) => {
            Err(CommandError::new("That conversation no longer exists."))
        }
        Err(error) => Err(error.into()),
    }
}

/* --------------------------------------------------------------- search */

#[tauri::command]
pub fn search_history(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<SearchHit>> {
    // An empty box means "no search", not an error — and answering without
    // touching the database keeps search-as-you-type cheap when the user
    // clears the field.
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(30).min(100) as usize;
    Ok(state.storage().search_history(&query, limit)?)
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
    /// Paths the user attached in the composer. Optional so older frontends
    /// (and tests) that omit the field keep working.
    #[serde(default)]
    attachments: Vec<PathBuf>,
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
            let title = commonspace_core::titles::from_prompt(&args.prompt);
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
    match state.auth_status_cached(adapter.as_ref()).await {
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

    // Collect attachment metadata before the provider can touch anything, so
    // the recorded hashes describe what the user handed over, not what the
    // task later made of it. Every step degrades per-file: metadata trouble
    // must never fail the send.
    let workspace_roots = state
        .storage()
        .workspace_roots(&workspace_id)
        .unwrap_or_default();
    let attachments: Vec<NewAttachment> = args
        .attachments
        .iter()
        .map(|path| collect_attachment_metadata(path, &workspace_roots))
        .collect();

    // The attachment list used to be pasted into the prompt by the frontend.
    // The assembly moved here so the paths become data Commonspace can
    // disclose and persist (the attachments table), not prose — while the
    // provider still receives exactly the text it always did.
    let prompt = prompt_with_attachments(&args.prompt, &attachments);

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
                prompt,
                model: args.model,
                resume: args.resume,
            },
            tx,
        )
        .await?;

    // Recorded only after the orchestrator minted the task, so the rows can
    // carry the task id. A recording failure is logged, not returned: the
    // task is already running and must not look failed to the user.
    if !attachments.is_empty() {
        if let Err(error) =
            state
                .storage()
                .record_attachments(&conversation, Some(&handle.task_id), &attachments)
        {
            tracing::warn!(%error, "failed to record attachment metadata");
        }
    }

    let task_id = handle.task_id.0.clone();
    state.track(handle);
    Ok(StartedTask {
        task_id,
        conversation_id: conversation.0,
    })
}

/// Files larger than this are not hashed at send time: hashing means reading
/// the whole file synchronously in the send path, and a multi-gigabyte
/// attachment would stall the message for seconds. Such files (and folders)
/// simply record a null hash.
const MAX_HASHED_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Inspect one attached path and produce its metadata record. Infallible by
/// design: a path that cannot be resolved or read degrades to null fields
/// (and `in_workspace: false` when it matches no root) instead of failing
/// the send.
fn collect_attachment_metadata(path: &Path, workspace_roots: &[PathBuf]) -> NewAttachment {
    // Canonicalize so the recorded path names the real location (symlinks
    // resolved) and the workspace check compares like with like. When
    // canonicalization fails — the path vanished, or permissions — keep the
    // path exactly as given.
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let metadata = std::fs::metadata(&resolved).ok();

    let kind = match &metadata {
        Some(m) if m.is_dir() => AttachmentKind::Folder,
        // An unreadable path is recorded as a file: "file" is the neutral
        // default and every other field is already null.
        _ => AttachmentKind::File,
    };
    let size_bytes = metadata
        .as_ref()
        .filter(|m| m.is_file())
        .map(|m| m.len() as i64);
    let modified_at = metadata
        .as_ref()
        .and_then(|m| m.modified().ok())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());

    let content_hash = match (kind, size_bytes) {
        (AttachmentKind::File, Some(len)) if (len as u64) <= MAX_HASHED_FILE_BYTES => {
            std::fs::read(&resolved).ok().map(|bytes| {
                use sha2::{Digest, Sha256};
                use std::fmt::Write as _;
                let digest = Sha256::digest(&bytes);
                let mut hex = String::with_capacity(digest.len() * 2);
                for byte in digest {
                    let _ = write!(hex, "{byte:02x}");
                }
                hex
            })
        }
        _ => None,
    };

    // The same containment rule the permission layer applies: canonicalized
    // child against canonicalized root, compared component-wise (which
    // `Path::starts_with` is) rather than as a string prefix, so
    // "/ws-evil" never matches root "/ws".
    let in_workspace = workspace_roots.iter().any(|root| {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        resolved.starts_with(&root)
    });

    NewAttachment {
        path: resolved.to_string_lossy().into_owned(),
        kind,
        size_bytes,
        modified_at,
        content_hash,
        in_workspace,
    }
}

/// Append the attached paths to the prompt the provider sees, using exactly
/// the wording the frontend used to embed — so provider behavior does not
/// change now that the frontend sends paths as data instead of prose.
fn prompt_with_attachments(prompt: &str, attachments: &[NewAttachment]) -> String {
    if attachments.is_empty() {
        return prompt.to_owned();
    }
    let mut out = String::from(prompt);
    out.push_str("\n\nFiles and folders I've attached:");
    for attachment in attachments {
        out.push_str("\n- ");
        out.push_str(&attachment.path);
    }
    out
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

/// Answer a plan that is waiting in `awaiting_approval`. Returns the task's
/// new state as its snake_case string (what `taskInfoSchema` uses). On
/// `start`, execution streams into the channel the original `start_task`
/// call opened, and the returned handle is re-tracked so `cancel_task`
/// reaches the execution session.
#[tauri::command]
pub async fn resolve_plan_decision(
    state: State<'_, AppState>,
    task_id: String,
    decision: commonspace_runtime::PlanDecision,
) -> Result<String> {
    let id = TaskId(task_id);
    let (next, handle) = state
        .orchestrator()
        .resolve_plan_decision(&id, decision)
        .await?;
    if let Some(handle) = handle {
        state.track(handle);
    }
    // TaskState serializes to exactly the snake_case strings the frontend's
    // task-state enum expects.
    serde_json::to_value(next)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| CommandError::new("Commonspace couldn't report the task's new state."))
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

/// A conversation's tasks, oldest first, for replaying its history.
#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>, conversation_id: String) -> Result<Vec<TaskRow>> {
    Ok(state
        .storage()
        .list_tasks(&ConversationId(conversation_id))?)
}

/// The provider session a follow-up message can continue, if the
/// conversation's most recent task left a resumable one behind.
#[tauri::command]
pub fn resumable_session(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Option<ResumableSession>> {
    Ok(state
        .storage()
        .resumable_session(&ConversationId(conversation_id))?)
}

/// Everything the user has attached in a conversation — metadata only.
#[tauri::command]
pub fn list_conversation_attachments(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<AttachmentRecord>> {
    Ok(state
        .storage()
        .list_conversation_attachments(&ConversationId(conversation_id))?)
}

/// Undo every file operation a task performed, newest change first, and
/// report one result per operation. Failures are collected, not thrown: the
/// orchestrator already turns "cannot be undone" conditions (missing backup,
/// file changed since, already undone) into failed `OperationResult`s, and
/// any remaining error (say, an operation row that vanished mid-loop) is
/// folded into a failed result too — one stubborn file must never stop the
/// rest of the task from being rolled back.
/// One proposed change, in the shape the studio validates.
///
/// `conflicted` is computed at read time rather than stored: whether a
/// proposal still matches the file it was prepared against is a fact about
/// the disk right now, and the person may have edited that file since.
#[derive(Debug, Serialize)]
pub struct StagedChangeInfo {
    id: String,
    task_id: String,
    kind: &'static str,
    target: String,
    destination: Option<String>,
    summary: String,
    size_after: Option<u64>,
    conflicted: bool,
    staged_at: String,
}

#[tauri::command]
pub fn list_staged_changes(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<StagedChangeInfo>> {
    let task = TaskId(task_id);
    Ok(state
        .orchestrator()
        .staged_changes(&task)?
        .into_iter()
        .map(|(change, conflicted)| StagedChangeInfo {
            id: change.id.as_ref().to_string(),
            task_id: task.0.clone(),
            kind: staged_kind_name(change.kind),
            target: change.target.display().to_string(),
            destination: change.destination.as_ref().map(|p| p.display().to_string()),
            summary: change.summary,
            size_after: change.size_after,
            conflicted,
            staged_at: change.staged_at.to_rfc3339(),
        })
        .collect())
}

/// The wire name for a change's kind. Written out rather than derived so the
/// frontend's union and this list cannot drift apart silently.
fn staged_kind_name(kind: commonspace_documents::StagedKind) -> &'static str {
    use commonspace_documents::StagedKind::*;
    match kind {
        Create => "create",
        Modify => "modify",
        Rename => "rename",
        Move => "move",
        Delete => "delete",
    }
}

#[tauri::command]
pub fn staged_diff(
    state: State<'_, AppState>,
    task_id: String,
    change_id: String,
) -> Result<serde_json::Value> {
    let preview = state
        .orchestrator()
        .staged_diff(&TaskId(task_id), &change_id)?;
    // The caveat is derived rather than stored, and the studio always shows
    // it — for an Office file whose extracted text is unchanged it is the
    // only thing separating "nothing happened" from "the formatting changed".
    let caveat = preview.caveat();
    let mut value = serde_json::to_value(&preview)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "caveat".into(),
            caveat.map(Into::into).unwrap_or(serde_json::Value::Null),
        );
    }
    Ok(value)
}

#[tauri::command]
pub fn apply_staged_changes(
    state: State<'_, AppState>,
    task_id: String,
    change_ids: Vec<String>,
) -> Result<Vec<OperationResult>> {
    // The task already knows which project it belongs to, so the caller is
    // not asked to carry a workspace id it could get wrong. Applying into
    // the wrong project's authorized roots is exactly the mistake the path
    // guard exists to stop, and not offering the chance is better than
    // catching it.
    let task = TaskId(task_id);
    let workspace = state
        .storage()
        .get_task(&task)?
        .workspace_id
        .ok_or_else(|| {
            CommandError::with_recovery(
                "This task isn't attached to a project any more.",
                "Open the project and try the change again.",
            )
        })?;
    Ok(state
        .orchestrator()
        .apply_staged(&workspace, &task, &change_ids)?)
}

#[tauri::command]
pub fn discard_staged_changes(
    state: State<'_, AppState>,
    task_id: String,
    change_ids: Vec<String>,
) -> Result<()> {
    state
        .orchestrator()
        .discard_staged(&TaskId(task_id), &change_ids)?;
    Ok(())
}

#[tauri::command]
pub fn undo_task(
    state: State<'_, AppState>,
    workspace_id: String,
    task_id: String,
) -> Result<Vec<OperationResult>> {
    let workspace = WorkspaceId(workspace_id);
    let operation_ids = state
        .storage()
        .task_file_operation_ids_newest_first(&TaskId(task_id))?;
    let mut results = Vec::with_capacity(operation_ids.len());
    for operation_id in operation_ids {
        match state.orchestrator().undo(&workspace, &operation_id) {
            Ok(result) => results.push(result),
            Err(error) => results.push(OperationResult::failed(
                "This change could not be undone.",
                error.to_string(),
            )),
        }
    }
    Ok(results)
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

/// Open a link from the conversation in the user's default browser.
///
/// Assistant replies contain web links, and the webview must never navigate
/// itself away from the app — so link clicks route through this command. The
/// parse-then-check-scheme step is what makes `file://`, `javascript:`, and
/// custom-scheme launches impossible: only absolute http/https URLs survive.
#[tauri::command]
pub fn open_external_url(url: String) -> Result<()> {
    let parsed = tauri::Url::parse(&url)
        .map_err(|_| CommandError::new("Commonspace only opens web links."))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CommandError::new("Commonspace only opens web links."));
    }
    tauri_plugin_opener::open_url(url, None::<&str>)
        .map_err(|e| CommandError::new(format!("Commonspace couldn't open that link: {e}")))
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

    #[test]
    fn prompt_with_attachments_uses_the_frontend_wording_verbatim() {
        let attachment = |path: &str| NewAttachment {
            path: path.to_owned(),
            kind: AttachmentKind::File,
            size_bytes: None,
            modified_at: None,
            content_hash: None,
            in_workspace: false,
        };
        assert_eq!(
            prompt_with_attachments(
                "Summarize these",
                &[attachment("/home/u/a.pdf"), attachment("/home/u/b")],
            ),
            "Summarize these\n\nFiles and folders I've attached:\n- /home/u/a.pdf\n- /home/u/b"
        );
    }

    #[test]
    fn prompt_without_attachments_is_untouched() {
        assert_eq!(prompt_with_attachments("Hello", &[]), "Hello");
    }

    #[test]
    fn attachment_metadata_for_a_file_includes_size_hash_and_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let file = root.join("hello.txt");
        std::fs::write(&file, "hello world").unwrap();

        let meta = collect_attachment_metadata(&file, std::slice::from_ref(&root));
        assert_eq!(meta.kind, AttachmentKind::File);
        assert_eq!(meta.size_bytes, Some(11));
        assert!(meta.modified_at.is_some());
        // sha256("hello world") — the well-known digest.
        assert_eq!(
            meta.content_hash.as_deref(),
            Some("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9")
        );
        assert!(meta.in_workspace);
        // The recorded path is the canonicalized location of the real file.
        assert!(Path::new(&meta.path).is_absolute());
    }

    #[test]
    fn attachment_metadata_for_a_folder_has_kind_folder_and_no_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("photos");
        std::fs::create_dir(&folder).unwrap();

        // The workspace root is elsewhere, so the folder is out of scope.
        let other_root = tmp.path().join("workspace");
        std::fs::create_dir(&other_root).unwrap();

        let meta = collect_attachment_metadata(&folder, &[other_root]);
        assert_eq!(meta.kind, AttachmentKind::Folder);
        assert_eq!(meta.size_bytes, None);
        assert_eq!(meta.content_hash, None);
        assert!(!meta.in_workspace);
    }

    #[test]
    fn attachment_metadata_for_a_missing_path_degrades_to_nulls() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = collect_attachment_metadata(
            Path::new("/definitely/not/there.bin"),
            &[tmp.path().to_path_buf()],
        );
        assert_eq!(meta.kind, AttachmentKind::File);
        assert_eq!(meta.size_bytes, None);
        assert_eq!(meta.modified_at, None);
        assert_eq!(meta.content_hash, None);
        assert!(!meta.in_workspace);
        // The path is kept as given so the record still names what the user
        // attached.
        assert_eq!(meta.path, "/definitely/not/there.bin");
    }

    #[test]
    fn oversized_files_are_not_hashed() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("huge.bin");
        // A sparse file over the hashing ceiling: cheap to create, and its
        // reported length is what the cutoff checks.
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_HASHED_FILE_BYTES + 1).unwrap();
        drop(f);

        let meta = collect_attachment_metadata(&big, &[tmp.path().to_path_buf()]);
        assert_eq!(meta.kind, AttachmentKind::File);
        assert_eq!(meta.size_bytes, Some((MAX_HASHED_FILE_BYTES + 1) as i64));
        assert_eq!(meta.content_hash, None, "large files must skip hashing");
    }

    #[test]
    fn open_external_url_rejects_everything_but_web_links() {
        // Each of these must fail the scheme gate before any opener call.
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://example.com/file",
            "smb://server/share",
            "not a url at all",
            "/relative/path",
            "example.com/no-scheme",
            "",
        ] {
            let err = open_external_url(bad.to_string()).unwrap_err();
            assert_eq!(
                err.message, "Commonspace only opens web links.",
                "input {bad:?} must be rejected with the plain-language error"
            );
        }
    }
}
