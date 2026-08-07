//! The task orchestrator: drives one task through the state machine, owns
//! the provider session and the tool server for its lifetime, persists every
//! event, and cleans up after crashes.
//!
//! Tasks run in two phases. The **plan phase** launches a read-only session
//! (no tool server, so neither adapter gets a mutating tool) whose reply must
//! end in a machine-readable plan. A plan with material side effects parks
//! the task in `AwaitingApproval` until the user answers via
//! [`Orchestrator::resolve_plan_decision`]; a harmless plan proceeds straight
//! to the **execution phase**, which stands up the tool server and resumes
//! the provider session (or replays full context when the provider cannot
//! resume). Both phases feed the same event sink, so the channel opened when
//! the task started sees the whole timeline.

use crate::broker::PermissionBroker;
use crate::tools::{ToolContext, ToolServer, ToolServerHandle};
use commonspace_agents::adapter::{AgentAdapter, McpEndpoint, SessionRequest};
use commonspace_agents::process::KillHandle;
use commonspace_core::{
    titles, AgentErrorInfo, AgentEvent, ConversationId, MessageRole, PlanStep, ProviderId, TaskId,
    TaskPlan, TaskState, WorkspaceId,
};
use commonspace_documents::{BackupStore, SafeFs, StagedChange, StagedKind, StagingStore};
use commonspace_permissions::{PathGuard, PolicyEngine, PolicySettings};
use commonspace_storage::{Storage, StorageError};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
    #[error("That plan isn't waiting for an answer anymore.")]
    NoPendingPlan,
}

/// Everything needed to start a task.
#[derive(Clone)]
pub struct StartTask {
    pub conversation_id: ConversationId,
    pub workspace_id: WorkspaceId,
    pub provider: ProviderId,
    pub prompt: String,
    pub model: Option<String>,
    /// Continue a previous provider session, when the provider supports it.
    pub resume: Option<String>,
}

/// The user's answer to a proposed plan, in the frontend's `{"kind": …}`
/// shape.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanDecision {
    /// Begin execution under the plan's approval envelope.
    Start,
    /// End the task without running anything.
    Cancel,
    /// Send feedback back into planning for a corrected plan.
    Revise { feedback: String },
}

/// The live things a task can be holding at any moment. A task moves from
/// its planning session to its execution session, so the kill handle and the
/// tool server are swapped in here as phases change — cancellation always
/// reaches whatever is currently running, through the handle the caller got
/// when the task started.
#[derive(Clone, Default)]
struct TaskControl {
    inner: Arc<tokio::sync::Mutex<ControlState>>,
}

#[derive(Default)]
struct ControlState {
    session: Option<KillHandle>,
    tool_server: Option<ToolServerHandle>,
    cancelled: bool,
}

impl TaskControl {
    /// Install the currently live provider session. A session that arrives
    /// after cancellation is killed on the spot instead of leaking.
    async fn adopt_session(&self, kill: KillHandle) {
        let cancelled = {
            let mut state = self.inner.lock().await;
            if !state.cancelled {
                state.session = Some(kill.clone());
            }
            state.cancelled
        };
        if cancelled {
            kill.kill().await;
        }
    }

    /// Install the execution phase's tool server, same cancellation rule.
    async fn adopt_tool_server(&self, server: ToolServerHandle) {
        let leftover = {
            let mut state = self.inner.lock().await;
            if state.cancelled {
                Some(server)
            } else {
                state.tool_server.replace(server)
            }
        };
        if let Some(server) = leftover {
            server.shutdown().await;
        }
    }

    /// Kill whatever is live and refuse anything that arrives later.
    async fn cancel(&self) {
        let (session, server) = {
            let mut state = self.inner.lock().await;
            state.cancelled = true;
            (state.session.take(), state.tool_server.take())
        };
        if let Some(session) = session {
            session.kill().await;
        }
        if let Some(server) = server {
            server.shutdown().await;
        }
    }
}

/// A running task's control surface.
pub struct TaskHandle {
    pub task_id: TaskId,
    control: TaskControl,
}

impl std::fmt::Debug for TaskHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskHandle")
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

impl TaskHandle {
    /// Cancel the task: terminate the current provider process tree and stop
    /// the tool server. Pending permission requests resolve as abandoned.
    pub async fn cancel(self, broker: &PermissionBroker) {
        self.control.cancel().await;
        broker.abandon_task(&self.task_id);
    }
}

/// A task parked in `AwaitingApproval`: everything needed to act on the
/// user's answer, including the sink the frontend is already listening on.
struct PendingPlan {
    adapter: Arc<dyn AgentAdapter>,
    request: StartTask,
    events: UnboundedSender<AgentEvent>,
    /// The planning session's provider-native id, for resuming into
    /// execution or revision.
    planning_session_id: Option<String>,
    control: TaskControl,
}

/// One planning (or replanning) session about to start.
struct PlanPhase {
    adapter: Arc<dyn AgentAdapter>,
    task_id: TaskId,
    request: StartTask,
    /// The full prompt for this session (planning wrapper or revision).
    prompt: String,
    /// Provider session to resume, when continuing a previous one.
    resume: Option<String>,
    events: UnboundedSender<AgentEvent>,
    control: TaskControl,
    /// True when this session revises an existing plan (`plan.updated`
    /// instead of `plan.created`, and the result always re-enters the gate).
    revision: bool,
}

/// Drives tasks. One instance per application; clones share all state.
#[derive(Clone)]
pub struct Orchestrator {
    storage: Arc<Storage>,
    broker: PermissionBroker,
    backup_root: PathBuf,
    /// Where proposed changes wait until someone reviews them. Kept beside
    /// the backups rather than inside a workspace: a staging area under the
    /// user's own folders would be a place their files could appear without
    /// them having asked for it.
    staging_root: PathBuf,
    /// Where a user's own skills live: skill directories Commonspace manages,
    /// under its data directory rather than in anyone else's namespace.
    ///
    /// Commonspace reads the portable Agent Skills format, but deliberately
    /// does not auto-load `.claude/skills` out of a project folder. A skill is
    /// instructions the model will follow, and a repository someone cloned is
    /// not a place to pick those up silently — the same reason Commonspace
    /// passes `--setting-sources user` to the provider CLI instead of letting
    /// a checked-in settings file configure the run. A project's own skills
    /// live at `<workspace>/.commonspace/skills`, which is somewhere a person
    /// had to deliberately put them.
    skills_root: PathBuf,
    policy_settings: PolicySettings,
    /// Tasks holding in `AwaitingApproval`, keyed by task. In-memory only:
    /// after a restart the sessions are gone, and crash recovery fails these
    /// tasks with an explanation instead.
    pending: Arc<Mutex<HashMap<TaskId, PendingPlan>>>,
}

impl Orchestrator {
    /// Build an orchestrator rooted at the application's data directory.
    /// Backups and staged proposals are separate trees under it: a backup is
    /// a copy of something that already happened, a staged change is
    /// something that has not, and mixing them would make cleaning up either
    /// one risk the other.
    pub fn new(storage: Arc<Storage>, data_dir: PathBuf) -> Self {
        Self {
            storage,
            broker: PermissionBroker::new(),
            backup_root: data_dir.join("backups"),
            staging_root: data_dir.join("staged"),
            skills_root: data_dir.join("skills"),
            policy_settings: PolicySettings::default(),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Where to look for skills, in precedence order: the user's own first,
    /// then each workspace root's `.commonspace/skills`, so a project can
    /// deliberately override a personal skill of the same name.
    fn skill_roots(&self, workspace_roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut roots = vec![self.skills_root.clone()];
        roots.extend(
            workspace_roots
                .iter()
                .map(|root| root.join(".commonspace").join("skills")),
        );
        roots
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

    /// Start a task: persist it, launch the read-only planning session, and
    /// stream normalized events to `events`. Execution follows automatically
    /// for harmless plans, or after the user approves via
    /// [`Orchestrator::resolve_plan_decision`].
    ///
    /// Events are persisted as they flow, so a reload replays the timeline.
    pub async fn start(
        &self,
        adapter: Arc<dyn AgentAdapter>,
        request: StartTask,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<TaskHandle, OrchestratorError> {
        // Fail fast on a workspace with no folders, before any rows exist.
        let roots = self.storage.workspace_roots(&request.workspace_id)?;
        if roots.is_empty() {
            return Err(OrchestratorError::NoWorkspace);
        }

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

        let control = TaskControl::default();
        let prompt = planning_prompt(&request.prompt);
        let resume = request.resume.clone();
        self.start_planning_session(PlanPhase {
            adapter,
            task_id: task.id.clone(),
            request,
            prompt,
            resume,
            events,
            control: control.clone(),
            revision: false,
        })
        .await?;

        Ok(TaskHandle {
            task_id: task.id,
            control,
        })
    }

    /// Launch a planning session and spawn its pump. The session gets no MCP
    /// endpoint — with `mcp: None` both adapters run read-only, so nothing a
    /// planning model does can touch a file — and therefore no tool server
    /// is started for this phase.
    async fn start_planning_session(&self, phase: PlanPhase) -> Result<(), OrchestratorError> {
        let PlanPhase {
            adapter,
            task_id,
            request,
            prompt,
            resume,
            events,
            control,
            revision,
        } = phase;

        let roots = self.storage.workspace_roots(&request.workspace_id)?;
        let cwd = roots
            .first()
            .cloned()
            .ok_or(OrchestratorError::NoWorkspace)?;

        let session_request = SessionRequest {
            task_id: task_id.clone(),
            prompt,
            cwd,
            workspace_roots: roots,
            model: request.model.clone(),
            resume,
            mcp: None,
        };
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = adapter.start_session(session_request, raw_tx).await?;
        control.adopt_session(session.canceller.clone()).await;
        let resumable = adapter.capabilities().supports_resume;
        self.storage
            .record_session(&task_id, request.provider, None, resumable)?;

        let this = self.clone();
        let mut provider_session = session.provider_session_id.clone();
        tokio::spawn(async move {
            let storage = Arc::clone(&this.storage);
            let mut reply = String::new();
            let mut failure: Option<String> = None;
            while let Some(event) = raw_rx.recv().await {
                match &event {
                    // The planning session finishing its reply is not the
                    // task finishing: swallow the provider's terminal event
                    // so the UI never sees a "completed" task that hasn't
                    // run anything yet.
                    AgentEvent::TaskCompleted { .. } => continue,
                    AgentEvent::MessageDelta { text, .. } => reply.push_str(text),
                    AgentEvent::Error { error } => failure = Some(error.message.clone()),
                    _ => {}
                }
                if let Err(error) = storage.append_event(&task_id, &event) {
                    tracing::error!(%error, "failed to persist a task event");
                }
                let _ = events.send(event);
            }

            if !reply.is_empty() {
                let _ = storage.append_message(
                    &request.conversation_id,
                    MessageRole::Assistant,
                    &reply,
                );
            }

            // Persist the provider's session id so execution and plan
            // revision can resume where planning left off.
            let planning_session_id = provider_session.borrow_and_update().clone();
            if let Some(sid) = &planning_session_id {
                let _ = storage.record_session(&task_id, request.provider, Some(sid), resumable);
            }

            if let Some(message) = failure {
                let _ = storage.set_task_summary(&task_id, &message);
                if let Err(error) = storage.transition_task(&task_id, TaskState::Failed) {
                    tracing::warn!(%error, "planning failure could not be recorded");
                }
                return;
            }

            // The task may have been cancelled while the session was winding
            // down; a cancelled task must not be pushed back into the
            // approval flow. (This is also why a killed planning stream never
            // hits the "ended without a result" failure — that default only
            // exists in the execution pump.)
            match storage.get_task(&task_id) {
                Ok(task) if task.state == TaskState::Planning => {}
                _ => return,
            }

            let plan = match plan_from_reply(&reply) {
                Some(plan) => plan,
                None => {
                    let warning = AgentEvent::Warning {
                        message: "Commonspace couldn't read the agent's plan; approval is \
                                  required before anything runs."
                            .into(),
                    };
                    let _ = storage.append_event(&task_id, &warning);
                    let _ = events.send(warning);
                    fallback_plan(&reply)
                }
            };
            if let Err(error) = storage.set_task_plan(&task_id, &plan) {
                tracing::error!(%error, "failed to persist the plan");
            }
            let plan_event = if revision {
                AgentEvent::PlanUpdated { plan: plan.clone() }
            } else {
                AgentEvent::PlanCreated { plan: plan.clone() }
            };
            let _ = storage.append_event(&task_id, &plan_event);
            let _ = events.send(plan_event);

            // A revised plan always returns to the gate — the user asked for
            // changes, so the changed plan goes back to them.
            if revision || plan_needs_gate(&plan) {
                if let Err(error) = storage.transition_task(&task_id, TaskState::AwaitingApproval) {
                    tracing::warn!(%error, "could not park the task for approval");
                    return;
                }
                let mut pending = this.pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.insert(
                    task_id.clone(),
                    PendingPlan {
                        adapter,
                        request,
                        events,
                        planning_session_id,
                        control,
                    },
                );
            } else {
                // A harmless plan proceeds without a gate: the state
                // machine's documented Planning → Running edge.
                if let Err(error) = storage.transition_task(&task_id, TaskState::Running) {
                    tracing::warn!(%error, "could not move a harmless plan into execution");
                    return;
                }
                this.execute_or_fail(
                    adapter,
                    &task_id,
                    &request,
                    planning_session_id,
                    &events,
                    &control,
                )
                .await;
            }
        });
        Ok(())
    }

    /// Run the execution phase; a startup failure becomes the task's terminal
    /// state instead of a spinner that never resolves.
    async fn execute_or_fail(
        &self,
        adapter: Arc<dyn AgentAdapter>,
        task_id: &TaskId,
        request: &StartTask,
        planning_session_id: Option<String>,
        events: &UnboundedSender<AgentEvent>,
        control: &TaskControl,
    ) {
        if let Err(error) = self
            .run_execution_phase(
                adapter,
                task_id,
                request,
                planning_session_id,
                events,
                control,
            )
            .await
        {
            let message = format!("The approved plan could not be started: {error}");
            let event = user_facing_error(message.clone(), Some("Try sending it again.".into()));
            let _ = self.storage.append_event(task_id, &event);
            let _ = events.send(event);
            let _ = self.storage.set_task_summary(task_id, &message);
            if let Err(error) = self.storage.transition_task(task_id, TaskState::Failed) {
                tracing::warn!(%error, "execution failure could not be recorded");
            }
        }
    }

    /// The execution phase: stand up the tool server, launch the provider
    /// with mutation tools and the plan context, and pump events until the
    /// session ends.
    async fn run_execution_phase(
        &self,
        adapter: Arc<dyn AgentAdapter>,
        task_id: &TaskId,
        request: &StartTask,
        planning_session_id: Option<String>,
        events: &UnboundedSender<AgentEvent>,
        control: &TaskControl,
    ) -> Result<(), OrchestratorError> {
        let roots = self.storage.workspace_roots(&request.workspace_id)?;
        let cwd = roots
            .first()
            .cloned()
            .ok_or(OrchestratorError::NoWorkspace)?;

        let guard = PathGuard::new(&roots);
        let backups = BackupStore::new(self.backup_root.join(request.workspace_id.as_ref()));
        // Per task, not per workspace: a proposal belongs to the piece of
        // work that offered it, and abandoning that work should take its
        // unreviewed changes with it rather than leaving them for the next
        // task to inherit.
        let staging = StagingStore::new(self.staging_root.join(task_id.as_ref()));
        // Built per task, not cached: a skill the person just installed
        // should be there for the next thing they ask, without restarting.
        // A dozen tool descriptors and a handful of small Markdown files is
        // not a cost worth a cache-invalidation bug.
        let (capabilities, skill_report) = crate::tools::build_registry(&self.skill_roots(&roots));
        if !skill_report.skipped.is_empty() {
            // Surfaced, not swallowed. A skill that quietly does not exist is
            // the most confusing failure this feature can have, and the user
            // is the only one who can fix the file.
            let _ = events.send(AgentEvent::Warning {
                message: skills_skipped_message(&skill_report),
            });
        }
        let (journal_tx, mut journal_rx) = tokio::sync::mpsc::unbounded_channel();
        let context = Arc::new(ToolContext {
            task_id: task_id.clone(),
            policy: PolicyEngine::new(guard.clone(), self.policy_settings.clone()),
            fs: SafeFs::new(guard, backups),
            staging,
            broker: self.broker.clone(),
            events: events.clone(),
            journal: journal_tx,
            capabilities: Arc::new(capabilities),
        });
        let tool_server = ToolServer::start(context).await?;

        // Persist journaled file operations as they happen, so undo survives
        // a crash mid-task.
        {
            let storage = Arc::clone(&self.storage);
            let task_id = task_id.clone();
            tokio::spawn(async move {
                while let Some(op) = journal_rx.recv().await {
                    if let Err(error) = storage.record_file_operation(Some(&task_id), &op) {
                        tracing::error!(%error, "failed to journal a file operation");
                    }
                }
            });
        }

        let resumable = adapter.capabilities().supports_resume;
        let resume = planning_session_id.filter(|_| resumable);
        let prompt = match &resume {
            Some(_) => APPROVED_PLAN_PROMPT.to_string(),
            // A provider that cannot resume gets a fresh session; replay the
            // request and the approved plan so it has full context.
            None => {
                let plan = self.storage.get_task(task_id)?.plan;
                standalone_execution_prompt(&request.prompt, plan.as_ref())
            }
        };

        let session_request = SessionRequest {
            task_id: task_id.clone(),
            prompt,
            cwd,
            workspace_roots: roots,
            model: request.model.clone(),
            resume,
            mcp: Some(McpEndpoint {
                url: tool_server.url.clone(),
                token: tool_server.token.clone(),
            }),
        };

        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = adapter.start_session(session_request, raw_tx).await?;
        control.adopt_session(session.canceller.clone()).await;
        control.adopt_tool_server(tool_server).await;
        self.storage
            .record_session(task_id, request.provider, None, resumable)?;

        {
            let storage = Arc::clone(&self.storage);
            let task_id = task_id.clone();
            let provider = request.provider;
            let mut provider_session = session.provider_session_id.clone();
            let events = events.clone();
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

                let (state, summary) =
                    terminal.unwrap_or((TaskState::Failed, titles::NO_RESULT_SUMMARY.to_string()));
                let _ = storage.set_task_summary(&task_id, &summary);
                // Ahead of the transition on purpose: the UI reacts to a task
                // becoming Completed, so the better name is already in place
                // by the time it looks.
                refine_conversation_title(&storage, &task_id, state, &summary);
                if let Err(error) = storage.transition_task(&task_id, state) {
                    tracing::warn!(%error, "task ended in an unexpected state");
                }
            });
        }
        Ok(())
    }

    /// Act on the user's answer to a parked plan. Returns the task's new
    /// state and, when execution begins, a fresh handle for the caller to
    /// track so cancellation reaches the execution session.
    pub async fn resolve_plan_decision(
        &self,
        task_id: &TaskId,
        decision: PlanDecision,
    ) -> Result<(TaskState, Option<TaskHandle>), OrchestratorError> {
        let pending = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(task_id)
        };
        let Some(pending) = pending else {
            return Err(OrchestratorError::NoPendingPlan);
        };

        match decision {
            PlanDecision::Start => {
                self.gate_transition(task_id, TaskState::Running)?;
                // What the user approved is a plan that creates, changes and
                // arranges *the files the plan named* — so exactly that stops
                // re-asking. Anything else, including a file elsewhere in the
                // same workspace, still asks; deletes always ask; and the
                // policy engine's hard denials remain unreachable. The stored
                // plan is the durable copy, so a revision the user accepted
                // is the one that governs.
                let roots = self
                    .storage
                    .workspace_roots(&pending.request.workspace_id)?;
                let declared = self
                    .storage
                    .get_task(task_id)
                    .ok()
                    .and_then(|task| task.plan)
                    .map(|plan| plan.paths_likely_modified)
                    .unwrap_or_default();
                self.broker.grant_plan_envelope(task_id, roots, &declared);
                self.execute_or_fail(
                    pending.adapter,
                    task_id,
                    &pending.request,
                    pending.planning_session_id,
                    &pending.events,
                    &pending.control,
                )
                .await;
                let state = self.storage.get_task(task_id)?.state;
                Ok((
                    state,
                    Some(TaskHandle {
                        task_id: task_id.clone(),
                        control: pending.control,
                    }),
                ))
            }
            PlanDecision::Cancel => {
                pending.control.cancel().await;
                self.broker.abandon_task(task_id);
                let state = self.gate_transition(task_id, TaskState::Cancelled)?;
                Ok((state, None))
            }
            PlanDecision::Revise { feedback } => {
                let state = self.gate_transition(task_id, TaskState::Planning)?;
                // The feedback is part of the conversation the user is
                // having; persist it like any other message they sent.
                let _ = self.storage.append_message(
                    &pending.request.conversation_id,
                    MessageRole::User,
                    &feedback,
                );
                let resumable = pending.adapter.capabilities().supports_resume;
                let resume = pending.planning_session_id.clone().filter(|_| resumable);
                let prompt = match &resume {
                    Some(_) => revision_prompt(&feedback),
                    None => {
                        let plan = self.storage.get_task(task_id)?.plan;
                        standalone_revision_prompt(
                            &pending.request.prompt,
                            plan.as_ref(),
                            &feedback,
                        )
                    }
                };
                let events = pending.events.clone();
                let phase = PlanPhase {
                    adapter: pending.adapter,
                    task_id: task_id.clone(),
                    request: pending.request,
                    prompt,
                    resume,
                    events: pending.events,
                    control: pending.control,
                    revision: true,
                };
                if let Err(error) = self.start_planning_session(phase).await {
                    let message = format!("The plan revision could not be started: {error}");
                    let event =
                        user_facing_error(message.clone(), Some("Try sending it again.".into()));
                    let _ = self.storage.append_event(task_id, &event);
                    let _ = events.send(event);
                    let _ = self.storage.set_task_summary(task_id, &message);
                    if let Err(error) = self.storage.transition_task(task_id, TaskState::Failed) {
                        tracing::warn!(%error, "revision failure could not be recorded");
                    }
                    return Err(error);
                }
                Ok((state, None))
            }
        }
    }

    /// Transition a parked task, translating an illegal transition (the task
    /// was cancelled or finished in the meantime) into the plain-language
    /// "no longer waiting" answer.
    fn gate_transition(
        &self,
        task_id: &TaskId,
        next: TaskState,
    ) -> Result<TaskState, OrchestratorError> {
        match self.storage.transition_task(task_id, next) {
            Ok(record) => Ok(record.state),
            Err(StorageError::Transition(_)) => Err(OrchestratorError::NoPendingPlan),
            Err(error) => Err(error.into()),
        }
    }

    /// Undo one journaled file operation, verifying it is still safe.
    /// Everything this task has proposed and nobody has answered yet, each
    /// paired with whether it currently conflicts.
    ///
    /// Conflict is deliberately not a stored column: it is a fact about the
    /// disk at the moment someone looks, not about the proposal. A file the
    /// person edited five minutes ago makes a change conflicted without
    /// anything about the change having altered.
    pub fn staged_changes(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<(StagedChange, bool)>, OrchestratorError> {
        let store = self.staging_store(task_id);
        Ok(self
            .storage
            .staged_changes_for_task(task_id)?
            .into_iter()
            .map(|change| {
                let conflicted = store.is_conflicted(&change);
                (change, conflicted)
            })
            .collect())
    }

    /// Compare a proposal against the file it would replace.
    pub fn staged_diff(
        &self,
        task_id: &TaskId,
        change_id: &str,
    ) -> Result<commonspace_documents::ChangePreview, OrchestratorError> {
        let change = self
            .storage
            .staged_changes_for_task(task_id)?
            .into_iter()
            .find(|change| change.id.as_ref() == change_id)
            .ok_or_else(|| OrchestratorError::UnknownTask(change_id.to_string()))?;
        let store = self.staging_store(task_id);
        Ok(preview_of(&store, &change))
    }

    /// Turn accepted proposals into real writes, oldest first so a rename
    /// that depends on an earlier create still finds its file.
    ///
    /// Each one goes through `SafeFs`, so a change applied here is backed up,
    /// verified on disk and journalled exactly as a direct write always was —
    /// undo keeps working afterwards. A conflicted change fails on its own
    /// and leaves the rest of the set alone; one refusal is not a reason to
    /// abandon the others.
    pub fn apply_staged(
        &self,
        workspace_id: &WorkspaceId,
        task_id: &TaskId,
        change_ids: &[String],
    ) -> Result<Vec<commonspace_core::OperationResult>, OrchestratorError> {
        let roots = self.storage.workspace_roots(workspace_id)?;
        let fs = SafeFs::new(
            PathGuard::new(&roots),
            BackupStore::new(self.backup_root.join(workspace_id.as_ref())),
        );
        let store = self.staging_store(task_id);
        let mut results = Vec::new();
        for change in self.storage.staged_changes_for_task(task_id)? {
            if !change_ids.iter().any(|id| id == change.id.as_ref()) {
                continue;
            }
            match commonspace_documents::staging::apply(&fs, &store, &change) {
                Ok((result, op)) => {
                    self.storage.record_file_operation(Some(task_id), &op)?;
                    self.storage.mark_staged_change_applied(&change.id)?;
                    let _ = store.discard(&change);
                    results.push(result);
                }
                Err(error) => results.push(commonspace_core::OperationResult::failed(
                    format!("{} was not applied.", display_target(&change)),
                    error.to_string(),
                )),
            }
        }
        Ok(results)
    }

    /// Throw proposals away. The user's files are not touched.
    pub fn discard_staged(
        &self,
        task_id: &TaskId,
        change_ids: &[String],
    ) -> Result<(), OrchestratorError> {
        let store = self.staging_store(task_id);
        for change in self.storage.staged_changes_for_task(task_id)? {
            if change_ids.iter().any(|id| id == change.id.as_ref()) {
                let _ = store.discard(&change);
                self.storage.mark_staged_change_discarded(&change.id)?;
            }
        }
        Ok(())
    }

    fn staging_store(&self, task_id: &TaskId) -> StagingStore {
        StagingStore::new(self.staging_root.join(task_id.as_ref()))
    }

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
                self.storage.mark_file_operation_undone(&undone)?;
                Ok(result)
            }
            Err(error) => Ok(commonspace_core::OperationResult::failed(
                "This change could not be undone.",
                error.to_string(),
            )),
        }
    }
}

/// The instruction wrapper around the user's request for the planning
/// session. Kept in one place so tests can assert against the exact text.
fn planning_prompt(user_request: &str) -> String {
    format!(
        "Before doing any work, write a short plan for the request below.\n\
         \n\
         You may investigate first — reading files and listing folders is fine — but do not \
         create, modify, move, or delete anything, and do not contact any external service \
         while planning.\n\
         \n\
         The request:\n\
         {user_request}\n\
         \n\
         End your reply with a fenced ```json code block as the very last thing. The block must \
         contain only one JSON object in exactly this shape:\n\
         \n\
         ```json\n\
         {{\"steps\":[{{\"title\":\"...\",\"detail\":\"...\"}}],\"paths_accessed\":[],\
         \"paths_likely_modified\":[],\"external_services\":[],\"consequential_actions\":[],\
         \"deliverables\":[],\"requires_approval\":true}}\n\
         ```\n\
         \n\
         Rules for the plan:\n\
         - \"requires_approval\" must be true whenever the work would create, modify, move, or \
         delete anything, or contact anything beyond the model itself.\n\
         - Keep it to at most 6 steps, in plain language a non-technical person understands.\n\
         - \"detail\" is optional; leave it out when the title says enough.\n\
         - Do not use markdown inside the JSON strings."
    )
}

/// Sent when the execution phase resumes the planning session.
const APPROVED_PLAN_PROMPT: &str = "The plan you proposed was approved. Carry it out now, \
     exactly as planned; raise anything that materially departs from it.";

/// Execution prompt for providers that cannot resume: a fresh session gets
/// the original request and the approved plan replayed in full.
fn standalone_execution_prompt(original_request: &str, plan: Option<&TaskPlan>) -> String {
    let mut prompt = String::from(
        "The plan below was proposed for the following request and has been approved. Carry it \
         out now, exactly as planned; raise anything that materially departs from it.\n\n\
         The request:\n",
    );
    prompt.push_str(original_request);
    push_plan_json(&mut prompt, plan);
    prompt
}

/// Revision prompt when the planning session can be resumed: the session
/// already holds the request and its own plan.
fn revision_prompt(feedback: &str) -> String {
    format!(
        "{feedback}\n\nRevise your plan accordingly; reply with the corrected plan JSON only, \
         as a fenced ```json block in the same shape as before."
    )
}

/// Revision prompt for a fresh session: replay the request, the current
/// plan, and the feedback, then restate the required shape.
fn standalone_revision_prompt(
    original_request: &str,
    plan: Option<&TaskPlan>,
    feedback: &str,
) -> String {
    let mut prompt = String::from(
        "A plan was proposed for the request below, and the user wants it revised.\n\n\
         The request:\n",
    );
    prompt.push_str(original_request);
    push_plan_json(&mut prompt, plan);
    prompt.push_str("\n\nThe user's feedback:\n");
    prompt.push_str(feedback);
    prompt.push_str(
        "\n\nRevise the plan accordingly; reply with the corrected plan JSON only, as a fenced \
         ```json block in exactly this shape:\n\
         {\"steps\":[{\"title\":\"...\",\"detail\":\"...\"}],\"paths_accessed\":[],\
         \"paths_likely_modified\":[],\"external_services\":[],\"consequential_actions\":[],\
         \"deliverables\":[],\"requires_approval\":true}",
    );
    prompt
}

fn push_plan_json(prompt: &mut String, plan: Option<&TaskPlan>) {
    if let Some(plan) = plan {
        if let Ok(json) = serde_json::to_string_pretty(plan) {
            prompt.push_str("\n\nThe plan:\n```json\n");
            prompt.push_str(&json);
            prompt.push_str("\n```");
        }
    }
}

/// Extract the plan from a planning reply: the *last* fenced ```json block,
/// parsed leniently (missing list fields default to empty). `None` when no
/// such block parses as a plan.
fn plan_from_reply(reply: &str) -> Option<TaskPlan> {
    let start = reply.rfind("```json")?;
    let body = &reply[start + "```json".len()..];
    let end = body.find("```")?;
    serde_json::from_str(body[..end].trim()).ok()
}

/// How much of the raw reply the fallback plan carries as step detail.
const FALLBACK_DETAIL_CHARS: usize = 400;

/// When the reply carries no readable plan, synthesize one that must be
/// approved — unreadable work is never assumed harmless.
fn fallback_plan(reply: &str) -> TaskPlan {
    let trimmed = reply.trim();
    let count = trimmed.chars().count();
    let tail: String = trimmed
        .chars()
        .skip(count.saturating_sub(FALLBACK_DETAIL_CHARS))
        .collect();
    let mut plan = TaskPlan::empty();
    plan.steps.push(PlanStep {
        title: "Carry out your request as described".into(),
        detail: (!tail.is_empty()).then_some(tail),
    });
    plan.requires_approval = true;
    plan
}

/// Whether a plan must pass the approval gate: its own say-so, or any
/// declared side effect — even when the model forgot to set the flag.
fn plan_needs_gate(plan: &TaskPlan) -> bool {
    plan.requires_approval
        || !plan.paths_likely_modified.is_empty()
        || !plan.consequential_actions.is_empty()
        || !plan.external_services.is_empty()
}

/// Let a task that finished well name its conversation. The opening prompt is
/// a guess at what the work is; the summary is what the work turned out to be,
/// and usually the better name — but only for a conversation the user has not
/// named themselves, which `retitle_conversation_if_auto` enforces.
///
/// A task that failed is left alone: nothing it can say about going wrong
/// belongs in the sidebar. Every failure here is cosmetic and swallowed —
/// a title is never worth losing a finished task over.
fn refine_conversation_title(storage: &Storage, task_id: &TaskId, state: TaskState, summary: &str) {
    if state != TaskState::Completed {
        return;
    }
    let Some(title) = titles::from_summary(summary) else {
        return;
    };
    let retitled = storage
        .get_task(task_id)
        .and_then(|task| storage.retitle_conversation_if_auto(&task.conversation_id, &title));
    if let Err(error) = retitled {
        tracing::warn!(%error, "could not name the conversation after its task");
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

/// A proposal's comparison against what is on disk today.
///
/// Office formats are compared through the text Commonspace can extract from
/// them, which is honest but partial — a change to formatting alone produces
/// an identical extraction. The preview carries that caveat so the screen can
/// say so rather than showing an empty diff that reads as "nothing changed".
fn preview_of(store: &StagingStore, change: &StagedChange) -> commonspace_documents::ChangePreview {
    use commonspace_documents::{
        extracted_text_diff, looks_binary, shape_summary, text_diff, ComparisonBasis, DiffBudget,
        SummaryReason,
    };

    let budget = DiffBudget::default();
    let old_bytes = std::fs::read(&change.target).unwrap_or_default();
    let new_bytes = match change.kind {
        // A deletion has no "after", and a relocation leaves the bytes alone;
        // in both cases the file list already says what happens, and a diff
        // of a file against itself would say nothing.
        StagedKind::Delete | StagedKind::Rename | StagedKind::Move => Vec::new(),
        _ => store.staged_bytes(change).unwrap_or_default(),
    };

    if matches!(change.kind, StagedKind::Delete) {
        return shape_summary(
            &old_bytes,
            &[],
            SummaryReason::NotText,
            ComparisonBasis::FullText,
        );
    }
    if looks_binary(&old_bytes) || looks_binary(&new_bytes) {
        // Extraction belongs to the document readers, which need a path and a
        // format; until the studio asks for that, say plainly that this one
        // cannot be shown as text rather than rendering bytes as characters.
        return shape_summary(
            &old_bytes,
            &new_bytes,
            SummaryReason::NotText,
            ComparisonBasis::ExtractedText,
        );
    }
    let (old_text, new_text) = (
        String::from_utf8_lossy(&old_bytes),
        String::from_utf8_lossy(&new_bytes),
    );
    if matches!(change.kind, StagedKind::Create) {
        return text_diff("", &new_text, budget);
    }
    let _ = extracted_text_diff;
    text_diff(&old_text, &new_text, budget)
}

/// What to tell someone when a skill they installed did not load.
///
/// Names the file rather than the count. "1 skill was skipped" sends a person
/// looking; the path and the reason let them go and fix it.
fn skills_skipped_message(report: &commonspace_capabilities::LoadReport) -> String {
    const SHOWN: usize = 3;
    let mut lines: Vec<String> = report
        .skipped
        .iter()
        .take(SHOWN)
        .map(|error| error.to_string())
        .collect();
    if report.skipped.len() > SHOWN {
        lines.push(format!("…and {} more.", report.skipped.len() - SHOWN));
    }
    format!(
        "Some skills could not be used, so they are not available in this task:\n{}",
        lines.join("\n")
    )
}

/// The file a proposal is about, named the way a person would name it.
fn display_target(change: &StagedChange) -> String {
    change
        .target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| change.target.display().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use commonspace_agents::adapter::{AuthInstructions, EventSink, RunningSession};
    use commonspace_agents::AdapterError;
    use commonspace_core::{
        AdapterCapabilities, AuthStatus, HealthReport, InstallStatus, MessageId, OperationClass,
        RiskLevel, SessionId,
    };
    use commonspace_documents::{FileOpKind, FileOperation};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, Orchestrator, WorkspaceId, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws_dir).expect("workspace dir");
        let storage = Arc::new(Storage::open_in_memory().expect("storage"));
        let workspace = storage
            .create_workspace("Test", std::slice::from_ref(&ws_dir))
            .expect("workspace row");
        let orchestrator = Orchestrator::new(Arc::clone(&storage), tmp.path().to_path_buf());
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

    /* ------------------------------------------------ plan-phase parsing */

    #[test]
    fn plan_parse_uses_the_last_fence() {
        let reply = "```json\n{\"steps\":[{\"title\":\"old\"}],\"requires_approval\":false}\n```\n\
                     Wait, better:\n\
                     ```json\n{\"steps\":[{\"title\":\"new\"}],\"requires_approval\":true}\n```\n";
        let plan = plan_from_reply(reply).expect("plan");
        assert_eq!(plan.steps[0].title, "new");
        assert!(plan.requires_approval);
        assert!(plan.paths_likely_modified.is_empty(), "lists default empty");
    }

    #[test]
    fn plan_parse_requires_the_approval_flag() {
        // Missing lists are tolerated; a missing requires_approval is not —
        // the fallback (which always gates) takes over instead.
        assert!(
            plan_from_reply("```json\n{\"steps\":[],\"requires_approval\":true}\n```").is_some()
        );
        assert!(plan_from_reply("```json\n{\"steps\":[]}\n```").is_none());
        assert!(plan_from_reply("no fence at all").is_none());
        assert!(plan_from_reply("```json\nnot json\n```").is_none());
    }

    #[test]
    fn fallback_plan_always_requires_approval_and_truncates() {
        let plan = fallback_plan(&"x".repeat(1000));
        assert!(plan.requires_approval);
        assert!(plan_needs_gate(&plan));
        assert_eq!(plan.steps.len(), 1);
        let detail = plan.steps[0].detail.clone().expect("detail");
        assert_eq!(detail.chars().count(), FALLBACK_DETAIL_CHARS);
    }

    #[test]
    fn gate_triggers_on_declared_side_effects_even_without_the_flag() {
        assert!(!plan_needs_gate(&TaskPlan::empty()));
        let mut plan = TaskPlan::empty();
        plan.paths_likely_modified.push("/ws/a.txt".into());
        assert!(plan_needs_gate(&plan));
        let mut plan = TaskPlan::empty();
        plan.external_services.push("weather.example".into());
        assert!(plan_needs_gate(&plan));
        let mut plan = TaskPlan::empty();
        plan.consequential_actions.push("send an email".into());
        assert!(plan_needs_gate(&plan));
    }

    /* -------------------------------------------------- two-phase flows */

    /// A scripted stand-in for a provider CLI: each session start pops the
    /// next script, streams its events, and ends the stream.
    struct FakeAdapter {
        scripts: std::sync::Mutex<VecDeque<Vec<AgentEvent>>>,
        requests: std::sync::Mutex<Vec<SessionRequest>>,
        sessions: AtomicUsize,
        supports_resume: bool,
    }

    impl FakeAdapter {
        fn scripted(scripts: Vec<Vec<AgentEvent>>) -> Arc<Self> {
            Arc::new(Self {
                scripts: std::sync::Mutex::new(scripts.into()),
                requests: std::sync::Mutex::new(Vec::new()),
                sessions: AtomicUsize::new(0),
                supports_resume: true,
            })
        }

        fn requests(&self) -> Vec<SessionRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// `KillHandle` can only come from a real spawned process, so the fake
    /// spawns one that exits immediately.
    fn noop_kill() -> KillHandle {
        #[cfg(windows)]
        let (shell, args) = (
            PathBuf::from(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())),
            vec!["/c".to_string(), "exit 0".to_string()],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), "exit 0".to_string()],
        );
        commonspace_agents::process::spawn_cli(&shell, &args, &std::env::temp_dir(), &[], None)
            .expect("spawn noop process")
            .kill
    }

    #[async_trait::async_trait]
    impl AgentAdapter for FakeAdapter {
        fn id(&self) -> ProviderId {
            ProviderId::ClaudeCode
        }
        async fn detect(&self) -> InstallStatus {
            InstallStatus::Installed {
                version: "0".into(),
                path: PathBuf::from("fake"),
            }
        }
        async fn auth_status(&self) -> AuthStatus {
            AuthStatus::ApiKey
        }
        fn auth_instructions(&self) -> AuthInstructions {
            AuthInstructions {
                command: "fake".into(),
                args: vec![],
                explanation: String::new(),
            }
        }
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                models: vec![],
                supports_resume: self.supports_resume,
                attachment_types: vec![],
                context_tokens: None,
                supports_permission_bridge: false,
            }
        }
        async fn start_session(
            &self,
            request: SessionRequest,
            events: EventSink,
        ) -> Result<RunningSession, AdapterError> {
            self.requests.lock().unwrap().push(request);
            let script = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
            let n = self.sessions.fetch_add(1, Ordering::SeqCst) + 1;
            let (_sid_tx, sid_rx) = tokio::sync::watch::channel(Some(format!("fake-session-{n}")));
            let done = tokio::spawn(async move {
                for event in script {
                    let _ = events.send(event);
                }
                Ok(())
            });
            Ok(RunningSession {
                session_id: SessionId::generate(),
                provider_session_id: sid_rx,
                canceller: noop_kill(),
                done,
            })
        }
        async fn health(&self) -> HealthReport {
            HealthReport {
                healthy: true,
                checks: vec![],
            }
        }
    }

    const GATED_PLAN: &str = r#"{"steps":[{"title":"Rename the files"}],"paths_accessed":[],"paths_likely_modified":["a.txt"],"external_services":[],"consequential_actions":[],"deliverables":["Renamed files"],"requires_approval":true}"#;
    const HARMLESS_PLAN: &str = r#"{"steps":[{"title":"Read and summarize"}],"paths_accessed":["a.txt"],"paths_likely_modified":[],"external_services":[],"consequential_actions":[],"deliverables":[],"requires_approval":false}"#;
    const REVISED_PLAN: &str = r#"{"steps":[{"title":"Rename only the invoices"}],"paths_accessed":[],"paths_likely_modified":["b.txt"],"external_services":[],"consequential_actions":[],"deliverables":[],"requires_approval":true}"#;

    fn plan_reply(plan_json: &str) -> Vec<AgentEvent> {
        vec![
            AgentEvent::MessageDelta {
                message_id: MessageId("msg_1".into()),
                text: format!("Here's what I'll do.\n```json\n{plan_json}\n```\n"),
            },
            // Providers end successful sessions with task.completed; the
            // plan pump must swallow this one.
            AgentEvent::TaskCompleted {
                summary: "planned".into(),
                usage: None,
            },
        ]
    }

    fn start_request(conversation: &ConversationId, workspace: &WorkspaceId) -> StartTask {
        StartTask {
            conversation_id: conversation.clone(),
            workspace_id: workspace.clone(),
            provider: ProviderId::ClaudeCode,
            prompt: "organize my files".into(),
            model: None,
            resume: None,
        }
    }

    async fn wait_for_state(storage: &Storage, task: &TaskId, state: TaskState) {
        for _ in 0..500 {
            if storage.get_task(task).expect("task").state == state {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "task never reached {state:?}; currently {:?}",
            storage.get_task(task).expect("task").state
        );
    }

    #[tokio::test]
    async fn gated_plan_parks_then_start_runs_it_to_completion() {
        let (_tmp, orchestrator, workspace, dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let adapter = FakeAdapter::scripted(vec![
            plan_reply(GATED_PLAN),
            vec![AgentEvent::TaskCompleted {
                summary: "All done.".into(),
                usage: None,
            }],
        ]);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                Arc::clone(&adapter) as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");
        let task_id = handle.task_id.clone();

        wait_for_state(&storage, &task_id, TaskState::AwaitingApproval).await;

        // The original channel saw the plan and no terminal event.
        let mut saw_plan = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                AgentEvent::PlanCreated { plan } => {
                    saw_plan = true;
                    assert!(plan.requires_approval);
                    assert_eq!(plan.steps[0].title, "Rename the files");
                }
                AgentEvent::TaskCompleted { .. } => {
                    panic!("the planning session must not complete the task")
                }
                _ => {}
            }
        }
        assert!(saw_plan, "plan.created must reach the original channel");
        assert!(storage.get_task(&task_id).expect("task").plan.is_some());

        let (state, rehandle) = orchestrator
            .resolve_plan_decision(&task_id, PlanDecision::Start)
            .await
            .expect("resolve");
        // The fake execution session finishes instantly, so the state read
        // back may already be terminal.
        assert!(matches!(state, TaskState::Running | TaskState::Completed));
        assert!(
            rehandle.is_some(),
            "start must hand back a trackable handle"
        );

        wait_for_state(&storage, &task_id, TaskState::Completed).await;

        // The approval seeded the envelope: an in-root modify no longer asks.
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = orchestrator
            .broker()
            .request(
                crate::broker::Ask {
                    task_id: task_id.clone(),
                    operation: OperationClass::Modify,
                    summary: "Update a file".into(),
                    paths: vec![dir.join("a.txt")],
                    items: vec![],
                    risk: RiskLevel::Medium,
                    irreversible: false,
                },
                &etx,
            )
            .await;
        assert!(outcome.is_allowed(), "{outcome:?}");
        assert!(erx.try_recv().is_err(), "the envelope must not emit an ask");

        // The execution session was wired correctly.
        let requests = adapter.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].mcp.is_none(), "planning must run read-only");
        assert!(requests[0].prompt.contains("organize my files"));
        assert!(requests[0].prompt.contains("requires_approval"));
        assert!(requests[1].mcp.is_some(), "execution needs the tool server");
        assert_eq!(requests[1].resume.as_deref(), Some("fake-session-1"));
        assert_eq!(requests[1].prompt, APPROVED_PLAN_PROMPT);

        // The execution terminal event reached the same channel.
        let mut completed = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AgentEvent::TaskCompleted { .. }) {
                completed = true;
            }
        }
        assert!(completed);
    }

    #[tokio::test]
    async fn cancelling_a_parked_plan_ends_the_task() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let adapter = FakeAdapter::scripted(vec![plan_reply(GATED_PLAN)]);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                adapter as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");
        let task_id = handle.task_id.clone();
        wait_for_state(&storage, &task_id, TaskState::AwaitingApproval).await;

        let (state, rehandle) = orchestrator
            .resolve_plan_decision(&task_id, PlanDecision::Cancel)
            .await
            .expect("resolve");
        assert_eq!(state, TaskState::Cancelled);
        assert!(rehandle.is_none());
        assert_eq!(
            storage.get_task(&task_id).expect("task").state,
            TaskState::Cancelled
        );

        // The parked entry is gone: a second answer is refused plainly.
        let error = orchestrator
            .resolve_plan_decision(&task_id, PlanDecision::Start)
            .await
            .unwrap_err();
        assert!(matches!(error, OrchestratorError::NoPendingPlan));
    }

    #[tokio::test]
    async fn harmless_plan_proceeds_without_the_gate() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let adapter = FakeAdapter::scripted(vec![
            plan_reply(HARMLESS_PLAN),
            vec![AgentEvent::TaskCompleted {
                summary: "Summarized.".into(),
                usage: None,
            }],
        ]);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                Arc::clone(&adapter) as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");

        // No approval step: the task runs straight through to completion.
        wait_for_state(&storage, &handle.task_id, TaskState::Completed).await;
        let requests = adapter.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].resume.as_deref(), Some("fake-session-1"));
    }

    fn conversation_title(storage: &Storage, id: &ConversationId) -> String {
        storage
            .list_conversations(50)
            .expect("conversations")
            .into_iter()
            .find(|c| &c.id == id)
            .expect("the conversation")
            .title
    }

    #[tokio::test]
    async fn a_completed_task_names_its_conversation_after_the_work() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        // The name the opening prompt produced: a guess, and still automatic.
        let conv = storage
            .create_conversation(Some(&workspace), "Organize my files")
            .expect("conversation");
        let adapter = FakeAdapter::scripted(vec![
            plan_reply(HARMLESS_PLAN),
            vec![AgentEvent::TaskCompleted {
                summary: "Sorted 42 downloads into folders by file type.".into(),
                usage: None,
            }],
        ]);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                Arc::clone(&adapter) as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");
        wait_for_state(&storage, &handle.task_id, TaskState::Completed).await;

        assert_eq!(
            conversation_title(&storage, &conv.id),
            "Sorted 42 downloads into folders by file type"
        );
    }

    #[tokio::test]
    async fn a_failed_task_leaves_the_conversation_name_alone() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        let conv = storage
            .create_conversation(Some(&workspace), "Organize my files")
            .expect("conversation");
        // A message that would make a perfectly readable title — the point is
        // that a task which did not finish never gets to write one.
        let adapter = FakeAdapter::scripted(vec![
            plan_reply(HARMLESS_PLAN),
            vec![AgentEvent::Error {
                error: AgentErrorInfo {
                    code: "provider_error".into(),
                    message: "The provider stopped halfway through sorting".into(),
                    recovery: None,
                    transient: false,
                },
            }],
        ]);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                Arc::clone(&adapter) as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");
        wait_for_state(&storage, &handle.task_id, TaskState::Failed).await;

        assert_eq!(conversation_title(&storage, &conv.id), "Organize my files");
    }

    #[tokio::test]
    async fn revision_updates_the_plan_and_reenters_the_gate() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let adapter = FakeAdapter::scripted(vec![plan_reply(GATED_PLAN), plan_reply(REVISED_PLAN)]);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                Arc::clone(&adapter) as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");
        let task_id = handle.task_id.clone();
        wait_for_state(&storage, &task_id, TaskState::AwaitingApproval).await;

        let (state, rehandle) = orchestrator
            .resolve_plan_decision(
                &task_id,
                PlanDecision::Revise {
                    feedback: "Only rename the invoices, leave everything else.".into(),
                },
            )
            .await
            .expect("resolve");
        assert_eq!(state, TaskState::Planning);
        assert!(rehandle.is_none());

        // The revised plan comes back to the gate.
        wait_for_state(&storage, &task_id, TaskState::AwaitingApproval).await;
        let stored = storage
            .get_task(&task_id)
            .expect("task")
            .plan
            .expect("plan");
        assert_eq!(stored.steps[0].title, "Rename only the invoices");

        let mut saw_update = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::PlanUpdated { plan } = event {
                saw_update = true;
                assert_eq!(plan.steps[0].title, "Rename only the invoices");
            }
        }
        assert!(saw_update, "plan.updated must reach the original channel");

        // The revision resumed the planning session with the feedback.
        let requests = adapter.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].resume.as_deref(), Some("fake-session-1"));
        assert!(requests[1].prompt.contains("Only rename the invoices"));
        assert!(requests[1].mcp.is_none(), "revision runs read-only too");

        // Still answerable: cancel cleans up.
        let (state, _) = orchestrator
            .resolve_plan_decision(&task_id, PlanDecision::Cancel)
            .await
            .expect("cancel");
        assert_eq!(state, TaskState::Cancelled);
    }

    #[tokio::test]
    async fn unreadable_plan_synthesizes_a_gated_fallback() {
        let (_tmp, orchestrator, workspace, _dir) = setup();
        let storage = Arc::clone(orchestrator.storage());
        let conv = storage
            .create_conversation(Some(&workspace), "t")
            .expect("conversation");
        let adapter = FakeAdapter::scripted(vec![vec![
            AgentEvent::MessageDelta {
                message_id: MessageId("msg_1".into()),
                text: "I'll just get on with it, no JSON here.".into(),
            },
            AgentEvent::TaskCompleted {
                summary: "planned".into(),
                usage: None,
            },
        ]]);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = orchestrator
            .start(
                adapter as Arc<dyn AgentAdapter>,
                start_request(&conv.id, &workspace),
                tx,
            )
            .await
            .expect("start");
        let task_id = handle.task_id.clone();

        // Unreadable plans always require approval.
        wait_for_state(&storage, &task_id, TaskState::AwaitingApproval).await;
        let plan = storage
            .get_task(&task_id)
            .expect("task")
            .plan
            .expect("plan");
        assert!(plan.requires_approval);
        assert_eq!(plan.steps[0].title, "Carry out your request as described");
        assert!(plan.steps[0]
            .detail
            .as_deref()
            .expect("detail")
            .contains("no JSON here"));

        let mut saw_warning = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Warning { message } = event {
                saw_warning = message.contains("couldn't read the agent's plan");
            }
        }
        assert!(saw_warning, "the user must be told the plan was unreadable");

        let (state, _) = orchestrator
            .resolve_plan_decision(&task_id, PlanDecision::Cancel)
            .await
            .expect("cancel");
        assert_eq!(state, TaskState::Cancelled);
    }

    #[tokio::test]
    async fn resolving_an_unknown_plan_is_a_plain_error() {
        let (_tmp, orchestrator, _workspace, _dir) = setup();
        let error = orchestrator
            .resolve_plan_decision(&TaskId::generate(), PlanDecision::Start)
            .await
            .unwrap_err();
        assert!(matches!(error, OrchestratorError::NoPendingPlan));
        assert_eq!(
            error.to_string(),
            "That plan isn't waiting for an answer anymore."
        );
    }

    #[test]
    fn plan_decision_deserializes_the_frontend_shape() {
        assert_eq!(
            serde_json::from_str::<PlanDecision>(r#"{"kind":"start"}"#).unwrap(),
            PlanDecision::Start
        );
        assert_eq!(
            serde_json::from_str::<PlanDecision>(r#"{"kind":"cancel"}"#).unwrap(),
            PlanDecision::Cancel
        );
        assert_eq!(
            serde_json::from_str::<PlanDecision>(r#"{"kind":"revise","feedback":"less"}"#).unwrap(),
            PlanDecision::Revise {
                feedback: "less".into()
            }
        );
    }
}
