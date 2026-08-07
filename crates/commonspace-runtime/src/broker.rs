//! The permission broker: turns a policy verdict of "ask the user" into a
//! `permission.requested` event and blocks the requesting tool call until
//! the user answers (or the task is cancelled).
//!
//! Remembered decisions live here too: approving "for this task" or "for
//! this workspace" means later identical operations are not re-asked. Denials
//! are never remembered — a denial applies to that request only, so the user
//! is never silently locked out of a later, differently-scoped attempt.

use commonspace_core::{
    AgentEvent, DecisionScope, OperationClass, PermissionDecision, PermissionRequest,
    PermissionRequestId, RiskLevel, TaskId,
};
use commonspace_permissions::PathGuard;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// What happened to a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOutcome {
    /// Allowed by policy without asking.
    AutoAllowed,
    /// The user approved.
    Approved,
    /// The user denied.
    Denied,
    /// Denied by policy; the user was never asked.
    PolicyDenied,
    /// The task ended (cancelled or failed) before the user answered.
    Abandoned,
}

impl PermissionOutcome {
    pub fn is_allowed(self) -> bool {
        matches!(
            self,
            PermissionOutcome::AutoAllowed | PermissionOutcome::Approved
        )
    }
}

/// One thing to ask the user about.
#[derive(Debug, Clone)]
pub struct Ask {
    pub task_id: TaskId,
    pub operation: OperationClass,
    /// Plain-language description shown in the dialog.
    pub summary: String,
    /// Resolved paths involved — what the user is actually approving.
    pub paths: Vec<PathBuf>,
    /// Itemized sub-operations for batch requests.
    pub items: Vec<String>,
    pub risk: RiskLevel,
    /// True when the action cannot be undone; the dialog warns explicitly.
    pub irreversible: bool,
}

/// A remembered approval: an operation class, optionally narrowed to paths
/// under a prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Grant {
    operation: OperationClass,
    scope_key: String,
}

/// The blanket permission a user gives by approving a task's plan: the
/// covered classes of work, on paths inside the workspace's authorized
/// roots, proceed without re-asking. Delete is deliberately never part of an
/// envelope — sending files to the trash is consequential enough to keep
/// asking every time. The policy engine's hard denials (protected paths,
/// secrets, permanent deletion) are evaluated before the broker is ever
/// reached, so no envelope can cover them.
#[derive(Debug, Clone)]
struct PlanEnvelope {
    classes: HashSet<OperationClass>,
    /// Canonicalized workspace roots the envelope covers.
    roots: Vec<PathBuf>,
}

/// A request the user has not answered yet, and the task it belongs to.
struct PendingRequest {
    task_id: TaskId,
    responder: oneshot::Sender<PermissionDecision>,
}

#[derive(Default)]
struct BrokerState {
    /// Requests awaiting an answer, each remembering the task that raised it
    /// so one task's cancellation cannot abandon another's open dialog.
    pending: HashMap<PermissionRequestId, PendingRequest>,
    /// Grants remembered for the lifetime of one task.
    task_grants: HashMap<TaskId, HashSet<Grant>>,
    /// Grants remembered for a workspace (in-memory; the storage layer
    /// persists the durable copy).
    workspace_grants: HashSet<Grant>,
    /// Per-task blanket approvals seeded when a plan is approved.
    plan_envelopes: HashMap<TaskId, PlanEnvelope>,
}

/// Shared broker handle.
#[derive(Clone, Default)]
pub struct PermissionBroker {
    state: Arc<Mutex<BrokerState>>,
}

impl PermissionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the user (unless a remembered grant already covers it) and wait.
    ///
    /// `events` is the sink the `permission.requested` event goes to; the UI
    /// answers via [`PermissionBroker::respond`].
    pub async fn request(
        &self,
        ask: Ask,
        events: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> PermissionOutcome {
        let Ask {
            task_id,
            operation,
            summary,
            paths,
            items,
            risk,
            irreversible,
        } = ask;
        let task_id = &task_id;

        // The plan-approval envelope comes first: work the user already
        // approved as a plan proceeds without another question. Containment
        // is resolved through the same PathGuard rules the policy engine
        // uses, so symlinks and `..` cannot stretch the envelope, and any
        // path that fails to resolve simply falls through to a real ask.
        let envelope = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.plan_envelopes.get(task_id).cloned()
        };
        if let Some(envelope) = envelope {
            if envelope.classes.contains(&operation)
                && !paths.is_empty()
                && envelope_covers(&envelope, &paths)
            {
                return PermissionOutcome::Approved;
            }
        }

        let grant = Grant {
            operation,
            scope_key: scope_key(&paths),
        };
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.workspace_grants.contains(&grant)
                || state
                    .task_grants
                    .get(task_id)
                    .is_some_and(|grants| grants.contains(&grant))
            {
                return PermissionOutcome::Approved;
            }
        }

        let id = PermissionRequestId::generate();
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.pending.insert(
                id.clone(),
                PendingRequest {
                    task_id: task_id.clone(),
                    responder: tx,
                },
            );
        }

        let request = PermissionRequest {
            id: id.clone(),
            task_id: task_id.clone(),
            session_id: None,
            operation,
            summary,
            paths,
            items,
            risk,
            irreversible,
            requested_at: chrono::Utc::now(),
        };
        if events
            .send(AgentEvent::PermissionRequested { request })
            .is_err()
        {
            self.forget(&id);
            return PermissionOutcome::Abandoned;
        }

        match rx.await {
            Ok(PermissionDecision::Approve { scope }) => {
                self.remember(task_id, grant, scope);
                PermissionOutcome::Approved
            }
            Ok(PermissionDecision::Deny) => PermissionOutcome::Denied,
            // Sender dropped: the task ended before the user answered.
            Err(_) => PermissionOutcome::Abandoned,
        }
    }

    /// Deliver the user's answer. Returns false if the request is unknown
    /// (already answered, or the task ended).
    pub fn respond(&self, id: &PermissionRequestId, decision: PermissionDecision) -> bool {
        let sender = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.pending.remove(id)
        };
        match sender {
            Some(entry) => entry.responder.send(decision).is_ok(),
            None => false,
        }
    }

    /// Seed the plan-approval envelope for a task: creating, modifying,
    /// renaming, and moving files stop re-asking, because the user just
    /// approved a plan that says exactly that work will happen. Delete stays
    /// out on purpose — even under an approved plan, sending files to the
    /// trash is consequential enough to keep asking.
    ///
    /// The envelope covers **the paths the plan declared**, not the whole
    /// workspace. That is the difference between the file list in the plan
    /// card being decoration and being the thing the user is actually
    /// approving: a plan that says it will rewrite two documents does not
    /// silently authorize rewriting forty. Declared paths are resolved and
    /// kept only where they land inside `roots`, so a plan cannot widen its
    /// own reach beyond what the workspace already allows.
    ///
    /// Under-declaring is safe and over-narrowing is safe: work outside the
    /// declared paths falls through to an ordinary permission question, so
    /// the worst case is one more prompt, never a blocked task. A plan that
    /// declares nothing gets no envelope at all — nothing specific was shown
    /// to the user, so nothing specific was approved.
    pub fn grant_plan_envelope(&self, task_id: &TaskId, roots: Vec<PathBuf>, declared: &[PathBuf]) {
        let scopes = declared_scopes(&roots, declared);
        if scopes.is_empty() {
            return;
        }
        let envelope = PlanEnvelope {
            classes: [
                OperationClass::Create,
                OperationClass::Modify,
                OperationClass::Rename,
                OperationClass::Move,
            ]
            .into_iter()
            .collect(),
            roots: scopes,
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.plan_envelopes.insert(task_id.clone(), envelope);
    }

    /// Abandon every pending request for a task (cancellation, failure).
    pub fn abandon_task(&self, task_id: &TaskId) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.task_grants.remove(task_id);
        state.plan_envelopes.remove(task_id);
        // Dropping a sender resolves its waiter as `Abandoned` — but only
        // this task's. With more than one task in flight, clearing the whole
        // map would silently abandon another task's open approval dialog.
        state.pending.retain(|_, entry| &entry.task_id != task_id);
    }

    /// Number of requests currently awaiting an answer.
    pub fn pending_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }

    fn remember(&self, task_id: &TaskId, grant: Grant, scope: DecisionScope) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match scope {
            DecisionScope::Once => {}
            DecisionScope::Task => {
                state
                    .task_grants
                    .entry(task_id.clone())
                    .or_default()
                    .insert(grant);
            }
            DecisionScope::Workspace => {
                state.workspace_grants.insert(grant);
            }
        }
    }

    fn forget(&self, id: &PermissionRequestId) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.remove(id);
    }
}

/// Whether every requested path resolves inside one of the envelope's roots.
/// Unresolvable paths (relative, vanished ancestors, Windows hazards) are
/// never covered — falling through to a real question is the safe direction.
/// The paths an approved plan's envelope actually covers: each declared
/// path, resolved, and kept only if it lands inside an authorized root.
///
/// A declared folder covers its subtree; a declared file covers itself,
/// including one that does not exist yet, because "create the summary" is
/// the most ordinary thing a plan can promise (`soft_canonicalize` resolves
/// through existing ancestors and appends the missing tail lexically, so a
/// symlinked parent still cannot smuggle the path out of scope).
///
/// Models write relative paths often enough that dropping them would leave
/// most plans with an empty envelope, so a relative path is tried against
/// each authorized root. Every candidate that survives is inside a root the
/// user authorized and matches the name the plan card displayed.
fn declared_scopes(roots: &[PathBuf], declared: &[PathBuf]) -> Vec<PathBuf> {
    let authorized = PathGuard::new(roots);
    let mut scopes: Vec<PathBuf> = Vec::new();
    let mut keep = |candidate: &Path| {
        if let Ok(resolved) = authorized.resolve(candidate) {
            if resolved.in_scope() && !scopes.contains(&resolved.resolved) {
                scopes.push(resolved.resolved);
            }
        }
    };

    for path in declared {
        if path.is_absolute() {
            keep(path);
        } else {
            for root in authorized.roots() {
                keep(&root.join(path));
            }
        }
    }
    scopes
}

fn envelope_covers(envelope: &PlanEnvelope, paths: &[PathBuf]) -> bool {
    let guard = PathGuard::new(&envelope.roots);
    paths
        .iter()
        .all(|path| guard.resolve(path).map(|r| r.in_scope()).unwrap_or(false))
}

/// Grant key for a set of paths: the common parent folder, so approving
/// "rename files in Documents/Contracts" doesn't silently cover a different
/// folder. Empty path sets get a distinct key so non-path operations
/// (uploads, sends) can't be covered by a filesystem grant.
fn scope_key(paths: &[PathBuf]) -> String {
    match paths.split_first() {
        None => "<no-paths>".to_string(),
        Some((first, rest)) => {
            let mut common: PathBuf = first
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| first.clone());
            for path in rest {
                let parent = path.parent().unwrap_or(path);
                while !parent.starts_with(&common) {
                    match common.parent() {
                        Some(up) => common = up.to_path_buf(),
                        None => break,
                    }
                }
            }
            common.to_string_lossy().into_owned()
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sink() -> (
        tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ) {
        tokio::sync::mpsc::unbounded_channel()
    }

    async fn ask(
        broker: &PermissionBroker,
        task: &TaskId,
        paths: Vec<PathBuf>,
        answer: Option<PermissionDecision>,
    ) -> PermissionOutcome {
        ask_class(broker, task, OperationClass::Delete, paths, answer).await
    }

    async fn ask_class(
        broker: &PermissionBroker,
        task: &TaskId,
        operation: OperationClass,
        paths: Vec<PathBuf>,
        answer: Option<PermissionDecision>,
    ) -> PermissionOutcome {
        let (tx, mut rx) = sink();
        let broker2 = broker.clone();
        let handle = tokio::spawn({
            let task = task.clone();
            async move {
                broker2
                    .request(
                        Ask {
                            task_id: task,
                            operation,
                            summary: "Do something".into(),
                            paths,
                            items: vec![],
                            risk: RiskLevel::High,
                            irreversible: false,
                        },
                        &tx,
                    )
                    .await
            }
        });
        if let Some(decision) = answer {
            let event = rx.recv().await.expect("permission event");
            let AgentEvent::PermissionRequested { request } = event else {
                panic!("unexpected event: {event:?}");
            };
            // The waiter registers before emitting, so this always lands.
            assert!(broker.respond(&request.id, decision));
        }
        handle.await.expect("join")
    }

    #[tokio::test]
    async fn approval_and_denial_round_trip() {
        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        let approved = ask(
            &broker,
            &task,
            vec!["C:/ws/a.txt".into()],
            Some(PermissionDecision::Approve {
                scope: DecisionScope::Once,
            }),
        )
        .await;
        assert_eq!(approved, PermissionOutcome::Approved);

        let denied = ask(
            &broker,
            &task,
            vec!["C:/ws/b.txt".into()],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(denied, PermissionOutcome::Denied);
        assert_eq!(broker.pending_count(), 0);
    }

    #[tokio::test]
    async fn once_scope_is_not_remembered() {
        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        ask(
            &broker,
            &task,
            vec!["C:/ws/a.txt".into()],
            Some(PermissionDecision::Approve {
                scope: DecisionScope::Once,
            }),
        )
        .await;
        // A second identical request must ask again: answering it proves the
        // event was emitted rather than auto-approved.
        let second = ask(
            &broker,
            &task,
            vec!["C:/ws/a.txt".into()],
            Some(PermissionDecision::Approve {
                scope: DecisionScope::Once,
            }),
        )
        .await;
        assert_eq!(second, PermissionOutcome::Approved);
    }

    #[tokio::test]
    async fn task_scope_is_remembered_for_same_folder_only() {
        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        ask(
            &broker,
            &task,
            vec!["C:/ws/docs/a.txt".into()],
            Some(PermissionDecision::Approve {
                scope: DecisionScope::Task,
            }),
        )
        .await;

        // Same folder: auto-approved without emitting an event.
        let (tx, mut rx) = sink();
        let outcome = broker
            .request(
                Ask {
                    task_id: task.clone(),
                    operation: OperationClass::Delete,
                    summary: "Delete files".into(),
                    paths: vec!["C:/ws/docs/b.txt".into()],
                    items: vec![],
                    risk: RiskLevel::High,
                    irreversible: false,
                },
                &tx,
            )
            .await;
        assert_eq!(outcome, PermissionOutcome::Approved);
        assert!(
            rx.try_recv().is_err(),
            "must not re-ask for the same folder"
        );

        // Different folder: must ask again.
        let elsewhere = ask(
            &broker,
            &task,
            vec!["C:/ws/other/c.txt".into()],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(elsewhere, PermissionOutcome::Denied);
    }

    /// Cancelling one task must not abandon another task's open dialog.
    ///
    /// The pending map used to be cleared wholesale, which was invisible
    /// while only one task could run at a time and would have become a
    /// trust-destroying bug the moment a queue allowed two.
    #[tokio::test]
    async fn abandoning_one_task_leaves_another_tasks_question_standing() {
        let broker = PermissionBroker::new();
        let task_a = TaskId::generate();
        let task_b = TaskId::generate();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // Two questions in flight, one per task. Neither is answered.
        let ask_of = |task: TaskId, path: &str| {
            let broker = broker.clone();
            let tx = tx.clone();
            let path = PathBuf::from(path);
            tokio::spawn(async move {
                broker
                    .request(
                        Ask {
                            task_id: task,
                            operation: OperationClass::Delete,
                            summary: "Delete a file".into(),
                            paths: vec![path],
                            items: vec![],
                            risk: RiskLevel::High,
                            irreversible: true,
                        },
                        &tx,
                    )
                    .await
            })
        };
        let a = ask_of(task_a.clone(), "C:/ws/a.txt");
        let b = ask_of(task_b.clone(), "C:/ws/b.txt");

        // Both requests have reached the broker before either is abandoned.
        let mut ids = Vec::new();
        for _ in 0..2 {
            if let Some(AgentEvent::PermissionRequested { request }) = rx.recv().await {
                ids.push((request.task_id.clone(), request.id));
            }
        }
        assert_eq!(broker.pending_count(), 2);

        broker.abandon_task(&task_a);

        assert_eq!(
            broker.pending_count(),
            1,
            "only task A's question is withdrawn"
        );
        assert_eq!(a.await.expect("join"), PermissionOutcome::Abandoned);

        // Task B's question is still answerable, and its answer lands.
        let (_, b_id) = ids
            .iter()
            .find(|(task, _)| task == &task_b)
            .cloned()
            .expect("task B asked");
        assert!(broker.respond(&b_id, PermissionDecision::Deny));
        assert_eq!(b.await.expect("join"), PermissionOutcome::Denied);
    }

    #[tokio::test]
    async fn task_grant_does_not_leak_to_another_task() {
        let broker = PermissionBroker::new();
        let task_a = TaskId::generate();
        let task_b = TaskId::generate();
        ask(
            &broker,
            &task_a,
            vec!["C:/ws/docs/a.txt".into()],
            Some(PermissionDecision::Approve {
                scope: DecisionScope::Task,
            }),
        )
        .await;
        let other = ask(
            &broker,
            &task_b,
            vec!["C:/ws/docs/b.txt".into()],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(other, PermissionOutcome::Denied);
    }

    #[tokio::test]
    async fn dropped_sink_abandons_request() {
        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        let (tx, rx) = sink();
        drop(rx);
        let outcome = broker
            .request(
                Ask {
                    task_id: task.clone(),
                    operation: OperationClass::Delete,
                    summary: "Delete".into(),
                    paths: vec!["C:/ws/a.txt".into()],
                    items: vec![],
                    risk: RiskLevel::High,
                    irreversible: true,
                },
                &tx,
            )
            .await;
        assert_eq!(outcome, PermissionOutcome::Abandoned);
        assert_eq!(broker.pending_count(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_task_abandons_waiters() {
        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        let (tx, mut rx) = sink();
        let broker2 = broker.clone();
        let task2 = task.clone();
        let waiter = tokio::spawn(async move {
            broker2
                .request(
                    Ask {
                        task_id: task2,
                        operation: OperationClass::Delete,
                        summary: "Delete".into(),
                        paths: vec!["C:/ws/a.txt".into()],
                        items: vec![],
                        risk: RiskLevel::High,
                        irreversible: true,
                    },
                    &tx,
                )
                .await
        });
        let _ = rx.recv().await;
        broker.abandon_task(&task);
        assert_eq!(waiter.await.expect("join"), PermissionOutcome::Abandoned);
    }

    /// An approved plan's envelope answers in-root create/modify/rename/move
    /// silently — no `permission.requested` event ever reaches the UI.
    #[tokio::test]
    async fn envelope_approves_in_root_modify_without_asking() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("notes.md");
        std::fs::write(&target, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root.clone()], &[root]);

        let (tx, mut rx) = sink();
        let outcome = broker
            .request(
                Ask {
                    task_id: task,
                    operation: OperationClass::Modify,
                    summary: "Update notes.md".into(),
                    paths: vec![target],
                    items: vec![],
                    risk: RiskLevel::Medium,
                    irreversible: false,
                },
                &tx,
            )
            .await;
        assert_eq!(outcome, PermissionOutcome::Approved);
        assert!(rx.try_recv().is_err(), "the envelope must not emit an ask");
    }

    #[tokio::test]
    async fn envelope_never_covers_delete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("old.txt");
        std::fs::write(&target, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root.clone()], &[root]);

        let outcome = ask_class(
            &broker,
            &task,
            OperationClass::Delete,
            vec![target],
            Some(PermissionDecision::Deny),
        )
        .await;
        // The question was asked (the helper answered it) and the answer
        // stood — the envelope did not swallow the delete.
        assert_eq!(outcome, PermissionOutcome::Denied);
    }

    #[tokio::test]
    async fn envelope_does_not_cover_paths_outside_its_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let outside = tmp.path().join("elsewhere.txt");
        std::fs::write(&outside, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root.clone()], &[root]);

        let outcome = ask_class(
            &broker,
            &task,
            OperationClass::Modify,
            vec![outside],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(outcome, PermissionOutcome::Denied);
    }

    #[tokio::test]
    async fn abandoning_a_task_clears_its_envelope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("notes.md");
        std::fs::write(&target, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root.clone()], &[root]);
        broker.abandon_task(&task);

        let outcome = ask_class(
            &broker,
            &task,
            OperationClass::Modify,
            vec![target],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(outcome, PermissionOutcome::Denied, "envelope must be gone");
    }

    /// The point of the whole change: approving a plan that named two files
    /// does not authorize rewriting a third one sitting beside them.
    #[tokio::test]
    async fn envelope_covers_only_the_paths_the_plan_declared() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let declared = root.join("report.docx");
        let undeclared = root.join("payroll.xlsx");
        std::fs::write(&declared, "x").expect("seed");
        std::fs::write(&undeclared, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root], std::slice::from_ref(&declared));

        let (tx, mut rx) = sink();
        let covered = broker
            .request(
                Ask {
                    task_id: task.clone(),
                    operation: OperationClass::Modify,
                    summary: "Update the report".into(),
                    paths: vec![declared],
                    items: vec![],
                    risk: RiskLevel::Medium,
                    irreversible: false,
                },
                &tx,
            )
            .await;
        assert_eq!(covered, PermissionOutcome::Approved);
        assert!(rx.try_recv().is_err(), "a declared file is not re-asked");

        // The file the plan never mentioned still asks, even though it sits
        // in the same authorized folder.
        let outcome = ask_class(
            &broker,
            &task,
            OperationClass::Modify,
            vec![undeclared],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(outcome, PermissionOutcome::Denied);
    }

    /// A declared folder covers what is inside it — otherwise "tidy this
    /// folder" would ask once per file, which is the prompt fatigue the
    /// envelope exists to prevent.
    #[tokio::test]
    async fn a_declared_folder_covers_its_contents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        let invoices = root.join("Invoices");
        std::fs::create_dir_all(&invoices).expect("root");
        let inside = invoices.join("2026-03.pdf");
        std::fs::write(&inside, "x").expect("seed");
        let outside = root.join("contract.pdf");
        std::fs::write(&outside, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root], &[invoices]);

        let (tx, mut rx) = sink();
        let covered = broker
            .request(
                Ask {
                    task_id: task.clone(),
                    operation: OperationClass::Rename,
                    summary: "Rename an invoice".into(),
                    paths: vec![inside],
                    items: vec![],
                    risk: RiskLevel::Medium,
                    irreversible: false,
                },
                &tx,
            )
            .await;
        assert_eq!(covered, PermissionOutcome::Approved);
        assert!(rx.try_recv().is_err());

        let sibling = ask_class(
            &broker,
            &task,
            OperationClass::Rename,
            vec![outside],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(sibling, PermissionOutcome::Denied);
    }

    /// A plan that names a file it is about to create still covers it — the
    /// most ordinary promise a plan makes is "I will write you a summary".
    #[tokio::test]
    async fn a_declared_file_that_does_not_exist_yet_is_covered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("summary.md");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root], std::slice::from_ref(&target));

        let (tx, mut rx) = sink();
        let outcome = broker
            .request(
                Ask {
                    task_id: task,
                    operation: OperationClass::Create,
                    summary: "Write the summary".into(),
                    paths: vec![target],
                    items: vec![],
                    risk: RiskLevel::Medium,
                    irreversible: false,
                },
                &tx,
            )
            .await;
        assert_eq!(outcome, PermissionOutcome::Approved);
        assert!(rx.try_recv().is_err());
    }

    /// A plan that declared nothing gets no envelope: nothing specific was
    /// shown to the user, so nothing specific was approved.
    #[tokio::test]
    async fn a_plan_declaring_no_paths_grants_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("notes.md");
        std::fs::write(&target, "x").expect("seed");

        let broker = PermissionBroker::new();
        let task = TaskId::generate();
        broker.grant_plan_envelope(&task, vec![root], &[]);

        let outcome = ask_class(
            &broker,
            &task,
            OperationClass::Modify,
            vec![target],
            Some(PermissionDecision::Deny),
        )
        .await;
        assert_eq!(outcome, PermissionOutcome::Denied);
    }

    /// A declared path outside the authorized roots is dropped rather than
    /// honoured — a plan cannot widen its own reach past the workspace.
    #[test]
    fn declared_paths_outside_the_roots_are_dropped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        let escape = tmp.path().join("elsewhere.txt");
        std::fs::write(&escape, "x").expect("seed");

        let scopes = declared_scopes(std::slice::from_ref(&root), &[escape, root.join("ok.md")]);
        assert_eq!(scopes.len(), 1, "only the in-root path survives");
        assert!(scopes[0].ends_with("ok.md"));
    }

    /// Models write relative paths often enough that dropping them would
    /// leave most plans with an empty envelope.
    #[test]
    fn a_relative_declared_path_is_resolved_against_the_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");

        let scopes = declared_scopes(&[root], &[PathBuf::from("report.docx")]);
        assert_eq!(scopes.len(), 1);
        assert!(scopes[0].ends_with("report.docx"));
    }

    #[test]
    fn responding_to_unknown_request_is_false() {
        let broker = PermissionBroker::new();
        assert!(!broker.respond(
            &PermissionRequestId("perm_nope".into()),
            PermissionDecision::Deny
        ));
    }
}
