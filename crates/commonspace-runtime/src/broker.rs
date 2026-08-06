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
use std::path::PathBuf;
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

#[derive(Default)]
struct BrokerState {
    pending: HashMap<PermissionRequestId, oneshot::Sender<PermissionDecision>>,
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
            state.pending.insert(id.clone(), tx);
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
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Seed the plan-approval envelope for a task: creating, modifying,
    /// renaming, and moving files inside `roots` stop re-asking, because the
    /// user just approved a plan that says exactly that work will happen.
    /// Delete stays out on purpose — even under an approved plan, sending
    /// files to the trash is consequential enough to keep asking.
    pub fn grant_plan_envelope(&self, task_id: &TaskId, roots: Vec<PathBuf>) {
        let envelope = PlanEnvelope {
            classes: [
                OperationClass::Create,
                OperationClass::Modify,
                OperationClass::Rename,
                OperationClass::Move,
            ]
            .into_iter()
            .collect(),
            // Canonicalize through the guard so later containment checks
            // compare like with like.
            roots: PathGuard::new(&roots).roots().to_vec(),
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.plan_envelopes.insert(task_id.clone(), envelope);
    }

    /// Abandon every pending request for a task (cancellation, failure).
    pub fn abandon_task(&self, task_id: &TaskId) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.task_grants.remove(task_id);
        state.plan_envelopes.remove(task_id);
        // Dropping the senders resolves the waiters as `Abandoned`.
        state.pending.retain(|_, _| false);
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
        broker.grant_plan_envelope(&task, vec![root]);

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
        broker.grant_plan_envelope(&task, vec![root]);

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
        broker.grant_plan_envelope(&task, vec![root]);

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
        broker.grant_plan_envelope(&task, vec![root]);
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

    #[test]
    fn responding_to_unknown_request_is_false() {
        let broker = PermissionBroker::new();
        assert!(!broker.respond(
            &PermissionRequestId("perm_nope".into()),
            PermissionDecision::Deny
        ));
    }
}
