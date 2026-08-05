//! The task state machine. Transitions are enforced by an explicit table;
//! an illegal transition is an error, never a silent coercion.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lifecycle states of a delegated task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Being composed; nothing has been sent to a provider.
    Draft,
    /// The agent is producing a plan (or answering directly for trivial asks).
    Planning,
    /// A plan with material side effects awaits user approval, editing, or rejection.
    AwaitingApproval,
    /// The agent is executing.
    Running,
    /// Execution suspended by the user; resumable.
    Paused,
    /// Finished; results verified and reported.
    Completed,
    /// Finished unsuccessfully; structured error recorded.
    Failed,
    /// Stopped by the user before completion.
    Cancelled,
    /// A completed/failed/cancelled task whose supported changes were undone.
    RolledBack,
}

impl TaskState {
    /// Whether this state is terminal (no further execution possible;
    /// `RolledBack` may still follow via undo).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::RolledBack
        )
    }

    /// The complete legal-transition table.
    pub fn can_transition_to(self, next: TaskState) -> bool {
        use TaskState::*;
        matches!(
            (self, next),
            (Draft, Planning)
                | (Draft, Cancelled)
                // A plan without material side effects proceeds straight to Running.
                | (Planning, AwaitingApproval)
                | (Planning, Running)
                | (Planning, Failed)
                | (Planning, Cancelled)
                // The user may approve, reject, or send the plan back for revision.
                | (AwaitingApproval, Running)
                | (AwaitingApproval, Planning)
                | (AwaitingApproval, Cancelled)
                | (Running, Paused)
                | (Running, Completed)
                | (Running, Failed)
                | (Running, Cancelled)
                | (Paused, Running)
                | (Paused, Cancelled)
                | (Paused, Failed)
                // Undo of supported changes after the task ended.
                | (Completed, RolledBack)
                | (Failed, RolledBack)
                | (Cancelled, RolledBack)
        )
    }

    /// Validate and perform a transition.
    pub fn transition_to(self, next: TaskState) -> Result<TaskState, TransitionError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(TransitionError {
                from: self,
                to: next,
            })
        }
    }
}

/// An attempted illegal state transition. This indicates a programming error
/// in the orchestrator, not a user-facing condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("illegal task transition: {from:?} -> {to:?}")]
pub struct TransitionError {
    pub from: TaskState,
    pub to: TaskState,
}

#[cfg(test)]
mod tests {
    use super::TaskState::*;
    use super::*;

    #[test]
    fn happy_path_with_approval() {
        let mut s = Draft;
        for next in [Planning, AwaitingApproval, Running, Completed] {
            s = s.transition_to(next).unwrap();
        }
        assert!(s.is_terminal());
    }

    #[test]
    fn no_side_effects_skips_approval() {
        assert!(Planning.can_transition_to(Running));
    }

    #[test]
    fn plan_rejection_and_revision() {
        assert!(AwaitingApproval.can_transition_to(Cancelled));
        assert!(AwaitingApproval.can_transition_to(Planning));
    }

    #[test]
    fn pause_resume() {
        assert!(Running.can_transition_to(Paused));
        assert!(Paused.can_transition_to(Running));
    }

    #[test]
    fn rollback_only_from_terminal() {
        assert!(Completed.can_transition_to(RolledBack));
        assert!(Failed.can_transition_to(RolledBack));
        assert!(Cancelled.can_transition_to(RolledBack));
        assert!(!Running.can_transition_to(RolledBack));
        assert!(!Draft.can_transition_to(RolledBack));
    }

    #[test]
    fn terminal_states_do_not_resume() {
        for terminal in [Completed, Failed, Cancelled, RolledBack] {
            assert!(!terminal.can_transition_to(Running), "{terminal:?}");
            assert!(!terminal.can_transition_to(Planning), "{terminal:?}");
        }
        assert!(!RolledBack.can_transition_to(RolledBack));
    }

    #[test]
    fn illegal_transition_is_error() {
        let err = Draft.transition_to(Completed).unwrap_err();
        assert_eq!(err.from, Draft);
        assert_eq!(err.to, Completed);
    }

    #[test]
    fn serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&AwaitingApproval).unwrap(),
            "\"awaiting_approval\""
        );
    }
}
