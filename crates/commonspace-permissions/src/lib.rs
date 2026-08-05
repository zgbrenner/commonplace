//! Deterministic permission policy for Commonspace.
//!
//! Design rules:
//! - **Pure evaluation.** The engine performs no filesystem mutation and no
//!   network access; its only IO is path resolution (canonicalization).
//! - **Resolved paths only.** Every decision is made on canonicalized,
//!   symlink-resolved absolute paths — never on the strings an agent sent.
//! - **Deny wins.** For batch operations, the strongest verdict across all
//!   targets applies: `Deny` > `RequireApproval` > `Allow`.
//! - **Protected locations are not configurable.** System directories and
//!   credential stores are denied even inside an authorized root.
//!
//! The LLM is never consulted; every rule here is explicit Rust.

mod path_guard;
mod policy;
mod protected;

pub use path_guard::{PathGuard, PathGuardError, ResolvedPath};
pub use policy::{ModifyPolicy, PolicyEngine, PolicyRequest, PolicySettings};
pub use protected::is_protected_location;
