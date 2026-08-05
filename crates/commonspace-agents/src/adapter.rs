//! The provider-independent adapter interface.

use commonspace_core::{
    AdapterCapabilities, AgentEvent, AuthStatus, HealthReport, InstallStatus, ProviderId,
    SessionId, TaskId,
};
use std::path::PathBuf;
use thiserror::Error;

/// Events flow from adapters to the orchestrator through this sink.
pub type EventSink = tokio::sync::mpsc::UnboundedSender<AgentEvent>;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("{0} is not installed")]
    NotInstalled(&'static str),
    #[error("{0} is not signed in")]
    NotAuthenticated(&'static str),
    #[error("failed to launch {cli}: {source}")]
    Spawn {
        cli: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("provider session ended unexpectedly: {0}")]
    SessionFailed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Where the Commonspace tool server is listening for this session.
#[derive(Debug, Clone)]
pub struct McpEndpoint {
    /// Loopback URL, e.g. `http://127.0.0.1:53817/mcp`.
    pub url: String,
    /// Per-session bearer token; the tool server rejects calls without it.
    pub token: String,
}

/// Everything needed to start (or continue) a provider session.
#[derive(Debug, Clone)]
pub struct SessionRequest {
    pub task_id: TaskId,
    pub prompt: String,
    /// Working directory for the CLI (usually the primary workspace root).
    pub cwd: PathBuf,
    /// All authorized roots; passed to the CLI as additional readable dirs.
    pub workspace_roots: Vec<PathBuf>,
    /// Model override, when the user picked one.
    pub model: Option<String>,
    /// Provider-native session id to resume, when continuing.
    pub resume: Option<String>,
    /// Commonspace tool server endpoint (mutations + document tools).
    pub mcp: Option<McpEndpoint>,
}

/// A live provider session.
pub struct RunningSession {
    /// Commonspace's session id (journaled).
    pub session_id: SessionId,
    /// Provider-native session id, filled in once the CLI reports it.
    /// Persisted for resume.
    pub provider_session_id: tokio::sync::watch::Receiver<Option<String>>,
    /// Cancels the session by terminating the full process tree.
    pub canceller: crate::process::KillHandle,
    /// Resolves when the underlying process has fully exited.
    pub done: tokio::task::JoinHandle<Result<(), AdapterError>>,
}

/// One provider integration. Object-safe; the orchestrator holds
/// `Box<dyn AgentAdapter>` per provider.
#[async_trait::async_trait]
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Is the official CLI installed? Never mutates anything.
    async fn detect(&self) -> InstallStatus;

    /// Truthful auth status via non-destructive probes (read-only commands,
    /// config-file presence). Never triggers a login flow.
    async fn auth_status(&self) -> AuthStatus;

    /// Human instructions + command for the official sign-in flow. The user
    /// runs sign-in with the provider's own tooling; Commonspace never
    /// handles the credentials.
    fn auth_instructions(&self) -> AuthInstructions;

    fn capabilities(&self) -> AdapterCapabilities;

    /// Start a new session (or resume, when `request.resume` is set).
    /// Normalized events flow into `events` until the session ends.
    async fn start_session(
        &self,
        request: SessionRequest,
        events: EventSink,
    ) -> Result<RunningSession, AdapterError>;

    /// Health diagnostics for the Connections screen.
    async fn health(&self) -> HealthReport;
}

/// How the user signs in, described truthfully.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuthInstructions {
    /// e.g. "claude" — the command Commonspace can launch in a terminal.
    pub command: String,
    pub args: Vec<String>,
    /// Plain-language explanation shown in the Connections screen,
    /// including what the connection will be billed as.
    pub explanation: String,
}
