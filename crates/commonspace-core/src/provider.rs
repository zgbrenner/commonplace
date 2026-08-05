//! Provider/adapter vocabulary: who the agent is, how it's installed,
//! how it's authenticated, and what it can do. The adapter implementations
//! live in `commonspace-agents`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Supported providers, in integration priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    ClaudeCode,
    CodexCli,
    GeminiCli,
    OpenCode,
    ApiCompatible,
    LocalModel,
}

impl ProviderId {
    /// Display name using the provider's own product name, accurately and
    /// without implying endorsement.
    pub fn display_name(self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "Claude Code",
            ProviderId::CodexCli => "Codex CLI",
            ProviderId::GeminiCli => "Gemini CLI",
            ProviderId::OpenCode => "OpenCode",
            ProviderId::ApiCompatible => "API provider",
            ProviderId::LocalModel => "Local model",
        }
    }
}

/// Whether the official CLI/tooling is present on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InstallStatus {
    Installed {
        version: String,
        path: PathBuf,
    },
    NotInstalled,
    /// Present but unusable (wrong version, broken install, …).
    Broken {
        detail: String,
    },
}

/// Truthful authentication state. The Connections screen renders these
/// directly; the UI never guesses or embellishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AuthStatus {
    NotInstalled,
    SignedOut,
    /// Connected through a consumer subscription the provider officially
    /// supports for this tooling.
    Subscription {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_hint: Option<String>,
    },
    /// Connected with an API key; usage is billed by the provider.
    ApiKey,
    /// Running locally; nothing leaves the machine.
    LocalModel,
    Error {
        detail: String,
    },
}

/// How a provider connection is paid for / powered — shown to the user with
/// an honest cost explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Subscription,
    ApiBilling,
    LocalInference,
    Unsupported,
}

/// Static and discovered capabilities of an adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterCapabilities {
    /// Models available for selection, when discoverable. Empty when the
    /// provider does not expose a listing.
    pub models: Vec<String>,
    /// Whether provider-side session continuation is supported.
    pub supports_resume: bool,
    /// Attachment MIME types the provider accepts, when documented.
    pub attachment_types: Vec<String>,
    /// Context window in tokens, when known. `None` means unknown — the UI
    /// says "unknown", it does not invent a number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// Whether the CLI supports routing permission prompts to a host app.
    pub supports_permission_bridge: bool,
}

/// Result of an adapter health check, for diagnostics UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthReport {
    pub healthy: bool,
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
