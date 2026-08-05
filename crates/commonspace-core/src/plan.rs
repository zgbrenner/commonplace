//! Task plans: what the agent intends to do, surfaced before it does it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A brief plan produced before any multistep or consequential operation.
/// Editable or rejectable by the user while the task holds in
/// `AwaitingApproval`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPlan {
    /// Ordered human-readable steps.
    pub steps: Vec<PlanStep>,
    /// Files and folders that will be accessed (read).
    pub paths_accessed: Vec<PathBuf>,
    /// Files likely to be created or modified.
    pub paths_likely_modified: Vec<PathBuf>,
    /// External services likely to be contacted (provider itself excluded).
    pub external_services: Vec<String>,
    /// Consequential actions that will require approval.
    pub consequential_actions: Vec<String>,
    /// Expected final deliverables.
    pub deliverables: Vec<String>,
    /// True when the plan implies material side effects and therefore must
    /// pass through `AwaitingApproval`.
    pub requires_approval: bool,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl TaskPlan {
    /// A plan with no steps and no side effects (trivial/direct answers).
    pub fn empty() -> Self {
        Self {
            steps: Vec::new(),
            paths_accessed: Vec::new(),
            paths_likely_modified: Vec::new(),
            external_services: Vec::new(),
            consequential_actions: Vec::new(),
            deliverables: Vec::new(),
            requires_approval: false,
        }
    }
}
