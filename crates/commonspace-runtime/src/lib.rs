//! Task orchestration and the Commonspace MCP tool server.
//!
//! This crate is where an agent's *intent* becomes an *effect*:
//!
//! ```text
//! agent CLI ──MCP──► tool server ──► policy engine ──► approval (if needed)
//!                                          │
//!                                          ▼
//!                                    SafeFs (verified, backed up, journaled)
//!                                          │
//!                                          ▼
//!                              normalized events ──► UI + storage
//! ```
//!
//! Nothing here trusts the agent: the policy engine decides, the user
//! approves what the policy says needs approving, and the filesystem layer
//! verifies the result before anything is reported as done.

pub mod broker;
pub mod orchestrator;
pub mod tools;

pub use broker::{Ask, PermissionBroker, PermissionOutcome};
pub use orchestrator::{Orchestrator, OrchestratorError, PlanDecision, StartTask, TaskHandle};
pub use tools::{ToolServer, ToolServerHandle};
