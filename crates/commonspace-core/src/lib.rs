//! Commonspace core: domain types shared by every crate and the frontend.
//!
//! This crate defines the *contracts* of the system:
//! - stable identifiers ([`ids`])
//! - the normalized agent event protocol ([`events`])
//! - the task state machine ([`task`])
//! - plans ([`plan`]), permissions vocabulary ([`permission`]),
//!   provider/adapter vocabulary ([`provider`]), artifacts ([`artifact`]),
//!   and the structured operation result ([`op_result`]).
//!
//! It also holds the pure logic that has to agree across crates — naming a
//! conversation ([`titles`]) happens in the desktop app and again in the
//! runtime, and both must arrive at the same answer.
//!
//! Everything here serializes with serde using `snake_case` field names and
//! dotted event type tags (e.g. `"tool.started"`). The TypeScript mirror of
//! these types lives in `packages/protocol`; a parity test keeps them in sync.

pub mod artifact;
pub mod error;
pub mod events;
pub mod ids;
pub mod op_result;
pub mod permission;
pub mod plan;
pub mod provider;
pub mod task;
pub mod titles;

pub use artifact::{Artifact, ArtifactKind};
pub use error::CoreError;
pub use events::{AgentErrorInfo, AgentEvent, MessageRole, ToolStatus, UsageInfo};
pub use ids::*;
pub use op_result::{OperationResult, ValidationOutcome};
pub use permission::{
    DecisionScope, OperationClass, PermissionDecision, PermissionRequest, PolicyVerdict, RiskLevel,
};
pub use plan::{PlanStep, TaskPlan};
pub use provider::{
    AdapterCapabilities, AuthMethod, AuthStatus, HealthCheck, HealthReport, InstallStatus,
    ProviderId,
};
pub use task::{TaskState, TransitionError};
