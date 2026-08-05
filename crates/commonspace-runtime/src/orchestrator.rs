//! The task orchestrator: drives one task through the state machine, owns
//! the provider session and the tool server for its lifetime, persists every
//! event, and cleans up after crashes.

use crate::broker::PermissionBroker;
use crate::tools::{ToolContext, ToolServer, ToolServerHandle};
use commonspace_agents::adapter::{AgentAdapter, McpEndpoint, SessionRequest};
use commonspace_core::{
    AgentErrorInfo, AgentEvent, ConversationId, MessageRole, ProviderId, TaskId, TaskPlan,
    TaskState, WorkspaceId,
};
use commonspace_documents::{BackupStore, SafeFs};
use commonspace_permissions::{PathGuard, PolicyEngine, PolicySettings};
use commonspace_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Adapter(#[from] commonspace_agents::AdapterError),
    #[error("could not start the tool server: {0}")]
    ToolServer(#[from] std::io::Error),
    #[error("no workspace is selected for this task")]
    NoWorkspace,
    #[error("unknown task: {0}")]
    UnknownTask(String),
}

/// Everything needed to start a task.
pub struct StartTask {
    pub conversation_id: ConversationId,
    pub workspace_id: WorkspaceId,
    pub provider: ProviderId,
    pub prompt: String,
    pub model: Option<String>,
    /// Continue a previous provider session, when the provider supports it.
    pub resume: Option<String>,
}

/// A running task's control surface.
pub struct TaskHandle {
    pub task_id: TaskId,
    canceller: commonspace_agents::process::KillHandle,
    tool_server: Option<ToolServerHandle>,
}

impl TaskHandle {
    /// Cancel the task: terminate the provider process tree and stop the
    /// tool server. Pending permission requests resolve as abandoned.
    pub async fn cancel(mut self, broker: &PermissionBroker) {
        self.canceller.kill().await;
        broker.abandon_task(&self.task_id);
        if let Some(server) = self.tool_server.take() {
            server.shutdown().await;
        }
    }
}

/// Drives tasks. One instance per application.
pub struct Orchestrator {
    storage: Arc<Storage>,
    broker: PermissionBroker,
    backup_root: PathBuf,
    policy_settings: PolicySettings,
}

impl Orchestrator {
    pub fn new(storage: Arc<Storage>, backup_root: PathBuf) -> Self {
        Self {
            storage,
            broker: PermissionBroker::new(),
            backup_root,
            policy_settings: PolicySettings::default(),
        }
    }

    pub fn broker(&self) -> &PermissionBroker {
        &self.broker
    }

    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }

    /// Reconcile state left behind by a previous process. Any task still
    /// marked live could not have survived the restart: its child process is
    /// gone, so it is failed with an explanation the user can act on.
    pub fn recover_after_restart(&self) -> Result<Vec<TaskId>, OrchestratorError> {
        let stale = self.storage.stale_running_tasks()?;
        for task in &stale {
            self.storage.fail_task_for_recovery(
                task,
                "Commonspace closed while this task was running, so it was stopped. \
                 Any changes already made are listed below and can be undone.",
            )?;
        }
        Ok(stale)
    }

    /// Start a task: persist it, stand up its tool server, launch the
    /// provider, and stream normalized events to `events`.
    ///
    /// Events are persisted as they flow, so a reload replays the timeline.
    pub async fn start(
        &self,
        adapter: &dyn AgentAdapter,
        request: StartTask,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<TaskHandle, OrchestratorError> {
        let roots = self.storage.workspace_roots(&request.workspace_id)?;
        let cwd = roots
            .first()
            .cloned()
            .ok_or(OrchestratorError::NoWorkspace)?;

        let task = self.storage.create_task(
            &request.conversation_id,
            Some(&request.workspace_id),
            request.provider,
            &request.prompt,
        )?;
        self.storage.append_message(
            &request.conversation_id,
            MessageRole::User,
            &request.prompt,
        )?;
        self.storage
            .transition_task(&task.id, TaskState::Planning)?;

        let guard = PathGuard::new(&roots);
        let backups = BackupStore::new(self.backup_root.join(request.workspace_id.as_ref()));
        let (journal_tx, mut journal_rx) = tokio::sync::mpsc::unbounded_channel();
        let context = Arc::new(ToolContext {
            task_id: task.id.clone(),
            policy: PolicyEngine::new(guard.clone(), self.policy_settings.clone()),
            fs: SafeFs::new(guard, backups),
            broker: self.broker.clone(),
            events: events.clone(),
            journal: journal_tx,
        });
        let tool_server = ToolServer::start(context).await?;

        // Persist journaled file operations as they happen, so undo survives
        // a crash mid-task.
        {
            let storage = Arc::clone(&self.storage);
            let task_id = task.id.clone();
            tokio::spawn(async move {
                while let Some(op) = journal_rx.recv().await {
                    if let Err(error) = storage.record_file_operation(Some(&task_id), &op) {
                        tracing::error!(%error, "failed to journal a file operation");
                    }
                }
            });
        }

        let session_request = SessionRequest {
            task_id: task.id.clone(),
            prompt: request.prompt.clone(),
            cwd,
            workspace_roots: roots,
            model: request.model.clone(),
            resume: request.resume.clone(),
            mcp: Some(McpEndpoint {
                url: tool_server.url.clone(),
                token: tool_server.token.clone(),
            }),
        };

        // Intercept the adapter's events for persistence and state changes.
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = adapter.start_session(session_request, raw_tx).await?;
        let canceller = session.canceller.clone();

        self.storage.record_session(
            &task.id,
            request.provider,
            None,
            adapter.capabilities().supports_resume,
        )?;

        self.storage.transition_task(&task.id, TaskState::Running)?;

        {
            let storage = Arc::clone(&self.storage);
            let task_id = task.id.clone();
            let provider = request.provider;
            let mut provider_session = session.provider_session_id.clone();
            let resumable = adapter.capabilities().supports_resume;
            tokio::spawn(async move {
                let mut assistant_text = String::new();
                let mut terminal = None;
                while let Some(event) = raw_rx.recv().await {
                    if let Err(error) = storage.append_event(&task_id, &event) {
                        tracing::error!(%error, "failed to persist a task event");
                    }
                    match &event {
                        AgentEvent::MessageDelta { text, .. } => assistant_text.push_str(text),
                        AgentEvent::PlanCreated { plan } | AgentEvent::PlanUpdated { plan } => {
                            let _ = storage.set_task_plan(&task_id, plan);
                        }
                        AgentEvent::TaskCompleted { summary, .. } => {
                            terminal = Some((TaskState::Completed, summary.clone()));
                        }
                        AgentEvent::Error { error } => {
                            terminal = Some((TaskState::Failed, error.message.clone()));
                        }
                        _ => {}
                    }
                    let _ = events.send(event);
                }

                if !assistant_text.is_empty() {
                    if let Ok(task) = storage.get_task(&task_id) {
                        let _ = storage.append_message(
                            &task.conversation_id,
                            MessageRole::Assistant,
                            &assistant_text,
                        );
                    }
                }

                // Persist the provider's session id for continuation.
                let latest = provider_session.borrow_and_update().clone();
                if let Some(sid) = latest {
                    let _ = storage.record_session(&task_id, provider, Some(&sid), resumable);
                }

                let (state, summary) = terminal.unwrap_or((
                    TaskState::Failed,
                    "The task ended without a result.".to_string(),
                ));
                let _ = storage.set_task_summary(&task_id, &summary);
                if let Err(error) = storage.transition_task(&task_id, state) {
                    tracing::warn!(%error, "task ended in an unexpected state");
                }
            });
        }

        Ok(TaskHandle {
            task_id: task.id,
            canceller,
            tool_server: Some(tool_server),
        })
    }

    /// Undo one journaled file operation, verifying it is still safe.
    pub fn undo(
        &self,
        workspace_id: &WorkspaceId,
        file_operation_id: &str,
    ) -> Result<commonspace_core::OperationResult, OrchestratorError> {
        let roots = self.storage.workspace_roots(workspace_id)?;
        let fs = SafeFs::new(
            PathGuard::new(&roots),
            BackupStore::new(self.backup_root.join(workspace_id.as_ref())),
        );
        let op = self.storage.get_file_operation(file_operation_id)?;
        match fs.undo(&op) {
            Ok((result, undone)) => {
                self.storage.record_file_operation(None, &undone)?;
                Ok(result)
            }
            Err(error) => Ok(commonspace_core::OperationResult::failed(
                "This change could not be undone.",
                error.to_string(),
            )),
        }
    }

    /// Record a plan the user has approved or rejected.
    pub fn resolve_plan(
        &self,
        task_id: &TaskId,
        plan: &TaskPlan,
        approved: bool,
    ) -> Result<TaskState, OrchestratorError> {
        self.storage.set_task_plan(task_id, plan)?;
        let next = if approved {
            TaskState::Running
        } else {
            TaskState::Cancelled
        };
        Ok(self.storage.transition_task(task_id, next)?.state)
    }
}

/// A structured error suitable for surfacing to the user.
pub fn user_facing_error(message: impl Into<String>, recovery: Option<String>) -> AgentEvent {
    AgentEvent::Error {
        error: AgentErrorInfo {
            code: "commonspace_error".into(),
            message: message.into(),
            recovery,
            transient: false,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use commonspace_documents::{FileOpKind, FileOperation};

    fn setup() -> (tempfile::TempDir, Orchestrator, WorkspaceId, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws_dir).expect("workspace dir");
        let storage = Arc::new(Storage::open_in_memory().expect("storage"));
        let workspace = storage
            .create_workspace("Test", std::slice::from_ref(&ws_dir))
            .expect("workspace row");
        let orchestrator = Orchestrator::new(Arc::clone(&storage), tmp.path().join("backups"));
        (tmp, orchestrator, workspace.id, ws_dir)
    }

    #[test]
    fn recovery_fails_tasks_left_running() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = orchestrator.storage();
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let task = storage
            .create_task(
                &conv.id,
                Some(&workspace),
                ProviderId::ClaudeCode,
                "organize",
            )
            .expect("task");
        storage
            .transition_task(&task.id, TaskState::Planning)
            .expect("planning");
        storage
            .transition_task(&task.id, TaskState::Running)
            .expect("running");

        let recovered = orchestrator.recover_after_restart().expect("recovery");
        assert_eq!(recovered, vec![task.id.clone()]);
        assert_eq!(
            storage.get_task(&task.id).expect("task").state,
            TaskState::Failed
        );
        // Idempotent: a second pass finds nothing.
        assert!(orchestrator
            .recover_after_restart()
            .expect("recovery")
            .is_empty());
    }

    #[test]
    fn undo_restores_a_modified_file() {
        let (_tmp, orchestrator, workspace, dir) = setup();
        let storage = orchestrator.storage();
        let target = dir.join("notes.md");
        std::fs::write(&target, "original").expect("seed");

        let backups = BackupStore::new(orchestrator.backup_root.join(workspace.as_ref()));
        let fs = SafeFs::new(PathGuard::new(std::slice::from_ref(&dir)), backups);
        let (_, op) = fs.overwrite_file(&target, b"changed").expect("overwrite");
        storage.record_file_operation(None, &op).expect("journal");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "changed");

        let result = orchestrator.undo(&workspace, op.id.as_ref()).expect("undo");
        assert!(result.success, "{result:?}");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "original");
    }

    #[test]
    fn undo_refuses_when_the_file_changed_since() {
        let (_tmp, orchestrator, workspace, dir) = setup();
        let storage = orchestrator.storage();
        let target = dir.join("notes.md");
        std::fs::write(&target, "original").expect("seed");
        let backups = BackupStore::new(orchestrator.backup_root.join(workspace.as_ref()));
        let fs = SafeFs::new(PathGuard::new(std::slice::from_ref(&dir)), backups);
        let (_, op) = fs.overwrite_file(&target, b"changed").expect("overwrite");
        storage.record_file_operation(None, &op).expect("journal");

        std::fs::write(&target, "the user edited this afterwards").expect("user edit");
        let result = orchestrator
            .undo(&workspace, op.id.as_ref())
            .expect("undo call");
        assert!(!result.success);
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "the user edited this afterwards"
        );
    }

    #[test]
    fn undo_of_unknown_operation_is_an_error_not_a_panic() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let error = orchestrator
            .undo(&workspace, "fop_nonexistent")
            .unwrap_err();
        assert!(matches!(
            error,
            OrchestratorError::Storage(StorageError::NotFound(_))
        ));
    }

    #[test]
    fn plan_rejection_cancels_the_task() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = orchestrator.storage();
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let task = storage
            .create_task(&conv.id, Some(&workspace), ProviderId::CodexCli, "organize")
            .expect("task");
        storage
            .transition_task(&task.id, TaskState::Planning)
            .expect("planning");
        storage
            .transition_task(&task.id, TaskState::AwaitingApproval)
            .expect("awaiting");

        let mut plan = TaskPlan::empty();
        plan.requires_approval = true;
        let state = orchestrator
            .resolve_plan(&task.id, &plan, false)
            .expect("resolve");
        assert_eq!(state, TaskState::Cancelled);
    }

    #[test]
    fn journaled_operations_are_listed_per_task() {
        let (_tmp, orchestrator, workspace, dir) = setup();
        let storage = orchestrator.storage();
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let task = storage
            .create_task(&conv.id, Some(&workspace), ProviderId::ClaudeCode, "p")
            .expect("task");
        let op = FileOperation::new(FileOpKind::Create, dir.join("a.txt"));
        storage
            .record_file_operation(Some(&task.id), &op)
            .expect("journal");
        let ops = storage.file_operations_for_task(&task.id).expect("list");
        assert_eq!(ops.len(), 1);
        assert!(ops[0].supports_undo());
    }
}
