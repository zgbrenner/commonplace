//! Provider adapters for Commonspace.
//!
//! Every provider is integrated through [`AgentAdapter`]: detection,
//! truthful auth status, session lifecycle, normalized events, cancellation.
//! No provider-specific logic exists outside this crate.
//!
//! Mutation model: adapters configure each CLI so that *reading* happens
//! through the provider's own scoped tools, while every mutation flows
//! through Commonspace's MCP tool server — where the deterministic policy
//! engine and the user's approval UI live. The provider's own write/shell
//! tools are not enabled in v1.

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod detect;
pub mod process;
pub mod sandbox;

pub use adapter::{
    AdapterError, AgentAdapter, EventSink, McpEndpoint, RunningSession, SessionRequest,
};
pub use claude::ClaudeCodeAdapter;
pub use codex::CodexCliAdapter;
