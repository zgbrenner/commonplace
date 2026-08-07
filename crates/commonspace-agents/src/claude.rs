//! Claude Code adapter.
//!
//! Headless invocation: `claude -p --input-format stream-json
//! --output-format stream-json --verbose --include-partial-messages`.
//! The prompt is written to stdin as a stream-json user message (never on
//! the command line — Windows length limits, and prompts are user data).
//!
//! Permission model (v1): Claude's own mutating tools are not enabled.
//! `--allowedTools` grants read-only tools plus the Commonspace MCP tool
//! server (`mcp__commonspace`), where every mutation passes the
//! deterministic policy engine and the user's approval UI.
//! `--permission-mode dontAsk` makes non-allowed tool use fail fast instead
//! of hanging on a prompt nobody can see.
//!
//! Configuration comes from Commonspace, never from the workspace. The
//! working directory is a folder the user may have received from anyone —
//! Dropbox, a colleague, a zip — and Claude Code reads `.claude/settings.json`
//! out of it, hooks included, with trust verification off under `-p`. Hooks
//! are shell commands, so a workspace could otherwise run code without ever
//! passing the policy engine. `--setting-sources user` stops project and local
//! settings being discovered at all, and the session settings file this
//! adapter writes turns hooks off outright.
//!
//! Auth: owned entirely by the Claude Code CLI. Commonspace only inspects
//! status non-destructively and, for sign-in, tells the user to run the
//! official `claude` login flow.

use crate::adapter::{
    AdapterError, AgentAdapter, AuthInstructions, EventSink, RunningSession, SessionRequest,
};
use crate::detect::{find_cli, probe_version};
use crate::process::spawn_cli;
use commonspace_core::{
    AdapterCapabilities, AgentErrorInfo, AgentEvent, AuthStatus, HealthCheck, HealthReport,
    InstallStatus, MessageId, MessageRole, ProviderId, SessionId, ToolCallId, ToolStatus,
    UsageInfo,
};
use serde_json::{json, Value};

const CLI: &str = "claude";

/// Environment for every Claude Code invocation Commonspace makes.
///
/// A fresh `claude -p` is spawned per message, and each invocation runs the
/// CLI's own background activity — most visibly its auto-updater, which
/// spawns helper processes. Those helpers are outside Commonspace's process
/// setup, and on Windows some of them briefly open console windows (a
/// long-standing upstream Claude Code issue). They also add startup latency
/// and network traffic to every message. Commonspace has no use for any of
/// it — the app manages its own updates — so it is switched off with the
/// CLI's documented environment variables.
fn cli_quiet_env() -> Vec<(String, String)> {
    vec![
        // No background auto-update. Manual `claude update` still works.
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        // No telemetry or error-reporting traffic; nothing but the model
        // traffic the task needs.
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        ),
    ]
}

/// Read-only Claude tools enabled in v1. Mutations go through Commonspace's
/// MCP tools ("mcp__commonspace") instead of Write/Edit/Bash.
const ALLOWED_TOOLS: &str = "Read,Glob,Grep,LS,TodoWrite,Task,mcp__commonspace";

/// Explicitly denied tools: mutation, shell, and network. `--allowedTools`
/// only allowlists permissions; this deny list makes the restriction
/// explicit and robust (deny wins in Claude Code's evaluation order).
const DISALLOWED_TOOLS: &str =
    "Bash,PowerShell,Edit,Write,NotebookEdit,WebFetch,WebSearch,KillShell";

/// The only settings tier Claude Code may discover for itself.
///
/// The other two — `project` (`<cwd>/.claude/settings.json`) and `local`
/// (`<cwd>/.claude/settings.local.json`) — are read out of the workspace,
/// which is untrusted content. `user` is the CLI's own `~/.claude/settings.json`,
/// which the person at the keyboard owns; taking it away would silently
/// override choices they made in Anthropic's tool, and it is not reachable by
/// a folder someone sent them.
const SETTING_SOURCES: &str = "user";

pub struct ClaudeCodeAdapter;

#[async_trait::async_trait]
impl AgentAdapter for ClaudeCodeAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::ClaudeCode
    }

    async fn detect(&self) -> InstallStatus {
        probe_version(CLI, &cli_quiet_env()).await
    }

    async fn auth_status(&self) -> AuthStatus {
        let Some(path) = find_cli(CLI) else {
            return AuthStatus::NotInstalled;
        };
        if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.is_empty()) {
            return AuthStatus::ApiKey;
        }
        // Primary probe: the CLI's own non-destructive status command.
        let args = vec!["auth".to_string(), "status".to_string()];
        let cwd = std::env::temp_dir();
        if let Ok((code, output)) = crate::process::probe_output(
            &path,
            &args,
            &cwd,
            &cli_quiet_env(),
            std::time::Duration::from_secs(20),
        )
        .await
        {
            if code == Some(0) {
                // `claude auth status` emits JSON:
                // {"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max",…}
                if let Ok(status) = serde_json::from_str::<Value>(&output) {
                    let logged_in = status
                        .get("loggedIn")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !logged_in {
                        return AuthStatus::SignedOut;
                    }
                    let method = status
                        .get("authMethod")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if method.to_lowercase().contains("api") {
                        return AuthStatus::ApiKey;
                    }
                    let plan_hint =
                        status
                            .get("subscriptionType")
                            .and_then(Value::as_str)
                            .map(|s| {
                                let mut c = s.chars();
                                match c.next() {
                                    Some(first) => {
                                        format!("Claude {}{}", first.to_uppercase(), c.as_str())
                                    }
                                    None => "Claude".to_string(),
                                }
                            });
                    return AuthStatus::Subscription { plan_hint };
                }
                return AuthStatus::Subscription { plan_hint: None };
            }
        }
        // Fallback: presence/shape of the CLI's own credential and config
        // files. Contents beyond the minimal status fields are never read
        // into logs or events.
        if let Some(home) = dirs_home() {
            let creds = home.join(".claude").join(".credentials.json");
            if let Ok(raw) = std::fs::read_to_string(&creds) {
                if raw.contains("claudeAiOauth") {
                    return AuthStatus::Subscription { plan_hint: None };
                }
            }
            let config = home.join(".claude.json");
            if let Ok(raw) = std::fs::read_to_string(&config) {
                if raw.contains("\"oauthAccount\"") {
                    return AuthStatus::Subscription { plan_hint: None };
                }
            }
        }
        AuthStatus::SignedOut
    }

    fn auth_instructions(&self) -> AuthInstructions {
        AuthInstructions {
            command: CLI.into(),
            args: vec![],
            explanation: "Sign in with Anthropic's own Claude Code tool. Running `claude` the \
                          first time opens the official sign-in. A Claude Pro or Max subscription \
                          is used if your account has one; otherwise Anthropic bills API usage. \
                          Commonspace never sees or stores these credentials."
                .into(),
        }
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            // The CLI does not expose a model listing command; offer the
            // documented aliases only.
            models: vec![
                "default".into(),
                "sonnet".into(),
                "opus".into(),
                "haiku".into(),
            ],
            supports_resume: true,
            attachment_types: vec![],
            context_tokens: None,
            supports_permission_bridge: true,
        }
    }

    async fn start_session(
        &self,
        request: SessionRequest,
        events: EventSink,
    ) -> Result<RunningSession, AdapterError> {
        let path = find_cli(CLI).ok_or(AdapterError::NotInstalled("Claude Code"))?;

        let mut args: Vec<String> = vec![
            "-p".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--setting-sources".into(),
            SETTING_SOURCES.into(),
            "--allowedTools".into(),
            ALLOWED_TOOLS.into(),
            "--disallowedTools".into(),
            DISALLOWED_TOOLS.into(),
        ];
        for root in &request.workspace_roots {
            if root != &request.cwd {
                args.push("--add-dir".into());
                args.push(root.to_string_lossy().into_owned());
            }
        }
        if let Some(model) = &request.model {
            if model != "default" {
                args.push("--model".into());
                args.push(model.clone());
            }
        }
        if let Some(resume) = &request.resume {
            args.push("--resume".into());
            args.push(resume.clone());
        }
        // Everything Commonspace configures for this run goes in one file
        // rather than on the command line: JSON does not survive Windows
        // `.cmd` argument quoting, and a file keeps the session token out of
        // the process list. Claude Code accepts the same path for both
        // `--settings` and `--mcp-config` — each reads the keys it knows —
        // so there is one file to protect and one file to remove when the
        // session ends.
        let mut config = json!({
            // Hooks are shell commands. `--setting-sources user` already
            // keeps the workspace's own from being discovered; this also
            // covers a hook reaching the CLI by any route Commonspace has
            // not accounted for.
            "disableAllHooks": true,
            "permissions": { "deny": protected_deny_rules() },
        });
        if let Some(mcp) = &request.mcp {
            config["mcpServers"] = json!({
                "commonspace": {
                    "type": "http",
                    "url": mcp.url,
                    "headers": { "Authorization": format!("Bearer {}", mcp.token) }
                }
            });
        }
        let session_config_file = std::env::temp_dir().join(format!(
            "commonspace-claude-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&session_config_file, config.to_string())?;
        restrict_to_owner(&session_config_file);
        args.push("--settings".into());
        args.push(session_config_file.to_string_lossy().into_owned());
        if request.mcp.is_some() {
            args.push("--mcp-config".into());
            args.push(session_config_file.to_string_lossy().into_owned());
            args.push("--strict-mcp-config".into());
        }

        let mut cli = match spawn_cli(&path, &args, &request.cwd, &cli_quiet_env()) {
            Ok(cli) => cli,
            Err(source) => {
                // Nothing is going to consume the file, and it holds the
                // session token.
                let _ = std::fs::remove_file(&session_config_file);
                return Err(AdapterError::Spawn {
                    cli: "Claude Code",
                    source,
                });
            }
        };

        // The prompt goes via stdin as a stream-json user message; closing
        // stdin afterwards makes the CLI exit after its result.
        let user_message = json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": request.prompt }] }
        });
        cli.write_line(&user_message.to_string()).await?;
        cli.close_stdin();

        let session_id = SessionId::generate();
        let (psid_tx, psid_rx) = tokio::sync::watch::channel(None::<String>);
        let kill = cli.kill.clone();
        let stderr_tail = cli.stderr_tail.clone();

        let done = tokio::spawn(async move {
            let mut normalizer = Normalizer::new(events.clone());
            let mut saw_result = false;
            while let Some(line) = cli.stdout_lines.recv().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue; // non-JSON noise is dropped; nothing retains it
                };
                if let Some(sid) = value.get("session_id").and_then(Value::as_str) {
                    let _ = psid_tx.send_if_modified(|cur| {
                        if cur.as_deref() != Some(sid) {
                            *cur = Some(sid.to_string());
                            true
                        } else {
                            false
                        }
                    });
                }
                saw_result |= normalizer.handle(&value);
            }
            let code = cli.wait().await.ok().flatten();
            let _ = std::fs::remove_file(&session_config_file);
            if !saw_result {
                let tail: Vec<String> = {
                    let t = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
                    t.iter().rev().take(5).rev().cloned().collect()
                };
                let _ = events.send(AgentEvent::Error {
                    error: AgentErrorInfo {
                        code: "provider_exited".into(),
                        message: format!("Claude Code ended unexpectedly (exit code {code:?})."),
                        recovery: Some("Check the developer details, then try again.".into()),
                        transient: true,
                    },
                });
                return Err(AdapterError::SessionFailed(format!(
                    "exit {code:?}: {}",
                    tail.join(" | ")
                )));
            }
            Ok(())
        });

        Ok(RunningSession {
            session_id,
            provider_session_id: psid_rx,
            canceller: kill,
            done,
        })
    }

    async fn health(&self) -> HealthReport {
        let install = self.detect().await;
        let auth = self.auth_status().await;
        let checks = vec![
            HealthCheck {
                name: "Installed".into(),
                passed: matches!(install, InstallStatus::Installed { .. }),
                detail: match &install {
                    InstallStatus::Installed { version, path } => {
                        Some(format!("{version} at {}", path.display()))
                    }
                    InstallStatus::Broken { detail } => Some(detail.clone()),
                    InstallStatus::NotInstalled => Some("claude not found on PATH".into()),
                },
            },
            HealthCheck {
                name: "Signed in".into(),
                passed: matches!(auth, AuthStatus::Subscription { .. } | AuthStatus::ApiKey),
                detail: None,
            },
        ];
        HealthReport {
            healthy: checks.iter().all(|c| c.passed),
            checks,
        }
    }
}

/// Claude Code deny rules mirroring the credential stores that
/// `commonspace-permissions` protects.
///
/// Commonspace's policy engine only sees calls to Commonspace's own MCP
/// tools. Claude Code's `Read` and `Grep` go straight to disk, so "read my
/// SSH key and summarise it" is otherwise held back by nothing but the CLI's
/// working-directory rules — and `--add-dir` widens those. Mirroring the same
/// list into the CLI's own configuration closes that route.
///
/// What this is worth, stated plainly: it is enforcement by a cooperating
/// process. It holds for exactly as long as Claude Code keeps honouring its
/// own settings file, which Commonspace cannot verify and the kernel does not
/// help with. It is a second lock on a door that is already locked, not a
/// boundary. Codex has no counterpart at all — `-s read-only` constrains
/// writes, not reads — so this is not something the two adapters share.
///
/// Its reach also stops at file *contents*. Verified against v2.1.224: a
/// `Read(...)` rule blocks `Read` and `Grep`, but `Glob` still returns the
/// names of files inside a denied directory.
///
/// Each store yields four rules, because one name deserves denying in more
/// than one place and in more than one shape: `~/<path>` for the user's real
/// store and `//**/<path>` for a copy that arrives inside a workspace, each
/// as a bare path (`.netrc` is a file) and with `/**` (`.ssh` is a
/// directory). `//` anchors a pattern at the filesystem root; a single
/// leading `/` would be read as relative to the settings file.
fn protected_deny_rules() -> Vec<String> {
    let mut rules = Vec::new();
    for store in commonspace_permissions::credential_store_paths() {
        for anchor in ["~", "//**"] {
            rules.push(format!("Read({anchor}/{store})"));
            rules.push(format!("Read({anchor}/{store}/**)"));
        }
    }
    rules
}

/// Best-effort tightening of a temporary credential file's permissions. On
/// Unix this is owner-only; on Windows the file inherits the user profile's
/// ACL, which is the closest equivalent without shelling out to `icacls`.
fn restrict_to_owner(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(Into::into)
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(Into::into)
    }
}

/// Quota state carried by a Claude Code `rate_limit_event` line.
struct RateLimit {
    /// `allowed`, `allowed_warning`, or `rejected`.
    status: String,
    /// Which window is filling up — `five_hour`, `seven_day`, …
    window: String,
    /// Unix seconds at which that window rolls over, when the CLI says.
    resets_at: Option<i64>,
}

/// Translates Claude Code stream-json lines into normalized [`AgentEvent`]s.
struct Normalizer {
    events: EventSink,
    current_message: Option<MessageId>,
    /// Set when partial deltas stream in, so full assistant text blocks are
    /// not emitted twice.
    saw_deltas: bool,
    /// Latest quota state the CLI reported. Claude Code sends this out of
    /// band, ahead of any failure, so holding on to it is what lets a
    /// terminal error say why the run stopped.
    rate_limit: Option<RateLimit>,
    /// Which warning the user has already been given. The CLI repeats the
    /// same quota line while a window stays over its threshold, and the same
    /// sentence four times is noise, not information.
    warned: Option<(String, Option<i64>)>,
}

impl Normalizer {
    fn new(events: EventSink) -> Self {
        Self {
            events,
            current_message: None,
            saw_deltas: false,
            rate_limit: None,
            warned: None,
        }
    }

    fn send(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    /// Returns true when this line was a terminal `result` event.
    fn handle(&mut self, value: &Value) -> bool {
        match value.get("type").and_then(Value::as_str) {
            Some("system") => false, // init/info; session id captured upstream
            Some("stream_event") => {
                self.handle_stream_event(value.get("event").unwrap_or(&Value::Null));
                false
            }
            Some("assistant") => {
                self.handle_assistant(value.get("message").unwrap_or(&Value::Null));
                false
            }
            Some("user") => {
                self.handle_tool_results(value.get("message").unwrap_or(&Value::Null));
                false
            }
            Some("rate_limit_event") => {
                self.handle_rate_limit(value.get("rate_limit_info").unwrap_or(&Value::Null));
                false
            }
            Some("result") => {
                let summary = value
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("Task finished.")
                    .to_string();
                let usage = value.get("usage").map(|u| UsageInfo {
                    input_tokens: u.get("input_tokens").and_then(Value::as_u64),
                    output_tokens: u.get("output_tokens").and_then(Value::as_u64),
                });
                if value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let error = self.classify_failure(summary);
                    self.send(AgentEvent::Error { error });
                } else {
                    self.send(AgentEvent::TaskCompleted { summary, usage });
                }
                true
            }
            other => {
                // Permissive on purpose: a line type Commonspace has never
                // seen must not end a session. Recording it is what keeps
                // "never seen" from also meaning "never noticed" — this arm
                // is where a new provider event would otherwise vanish.
                tracing::debug!(
                    line_type = other.unwrap_or("<untyped>"),
                    "unhandled Claude Code line type"
                );
                false
            }
        }
    }

    /// Claude Code reports quota on its own line type, whether or not the run
    /// is in trouble. Two things come of that: the user hears a window is
    /// filling up while there is still time to act on it, and a run that
    /// later dies of quota can say so instead of failing anonymously.
    fn handle_rate_limit(&mut self, info: &Value) {
        let str_field = |key: &str| {
            info.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let status = str_field("status");
        let window = str_field("rateLimitType");
        let resets_at = info.get("resetsAt").and_then(Value::as_i64);

        if status == "allowed_warning" {
            let key = (window.clone(), resets_at);
            if self.warned.as_ref() != Some(&key) {
                self.warned = Some(key);
                let limit = window_phrase(&window);
                self.send(AgentEvent::Warning {
                    message: match reset_phrase(resets_at) {
                        Some(when) => format!("Claude is close to its {limit}. It resets {when}."),
                        None => format!("Claude is close to its {limit}."),
                    },
                });
            }
        }

        self.rate_limit = Some(RateLimit {
            status,
            window,
            resets_at,
        });
    }

    /// Distinguishes a run that ran out of quota from one that failed on its
    /// own merits. Both arrive as the same opaque terminal line, so without
    /// this the user is told the same unhelpful thing either way — and told
    /// it is not worth retrying, which for a quota failure is wrong.
    fn classify_failure(&self, message: String) -> AgentErrorInfo {
        let out_of_quota = self
            .rate_limit
            .as_ref()
            .is_some_and(|limit| limit.status == "rejected")
            || {
                // A run can also stop on quota without a `rejected` line
                // preceding it, in which case the CLI's own wording is the
                // only evidence there is.
                let lower = message.to_lowercase();
                lower.contains("usage limit") || lower.contains("rate limit")
            };
        if !out_of_quota {
            return AgentErrorInfo {
                code: "provider_error".into(),
                message,
                recovery: None,
                transient: false,
            };
        }
        let resets_at = self.rate_limit.as_ref().and_then(|limit| limit.resets_at);
        let limit = self
            .rate_limit
            .as_ref()
            .map(|limit| window_phrase(&limit.window))
            .unwrap_or_else(|| "usage limit".to_string());
        AgentErrorInfo {
            code: "provider_rate_limited".into(),
            message,
            recovery: Some(match reset_phrase(resets_at) {
                Some(when) => format!(
                    "This is a {limit}, not a problem with the task. It resets {when} — \
                     the same request should work after that."
                ),
                // Claude Code does not always say when the window rolls over.
                // Naming a time it never gave would be worse than saying so.
                None => format!(
                    "This is a {limit}, not a problem with the task. The same request should \
                     work once it resets."
                ),
            }),
            transient: true,
        }
    }

    fn ensure_message(&mut self) -> MessageId {
        if let Some(id) = &self.current_message {
            return id.clone();
        }
        let id = MessageId::generate();
        self.current_message = Some(id.clone());
        self.send(AgentEvent::MessageStarted {
            message_id: id.clone(),
            role: MessageRole::Assistant,
        });
        id
    }

    fn handle_stream_event(&mut self, event: &Value) {
        if event.get("type").and_then(Value::as_str) == Some("content_block_delta") {
            if let Some(text) = event
                .pointer("/delta/text")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                self.saw_deltas = true;
                let message_id = self.ensure_message();
                self.send(AgentEvent::MessageDelta {
                    message_id,
                    text: text.to_string(),
                });
            }
            if let Some(thinking) = event.pointer("/delta/thinking").and_then(Value::as_str) {
                if !thinking.is_empty() {
                    self.send(AgentEvent::ReasoningSummary {
                        text: thinking.to_string(),
                    });
                }
            }
        }
    }

    fn handle_assistant(&mut self, message: &Value) {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if !self.saw_deltas {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            let message_id = self.ensure_message();
                            self.send(AgentEvent::MessageDelta {
                                message_id,
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("tool_use") => {
                    // A tool call ends the current text message.
                    self.current_message = None;
                    self.saw_deltas = false;
                    let call_id = ToolCallId(
                        block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("tool_unknown")
                            .to_string(),
                    );
                    let tool = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let (title, paths) = humanize_tool(&tool, &input);
                    self.send(AgentEvent::ToolRequested {
                        call_id: call_id.clone(),
                        tool: tool.clone(),
                        title: title.clone(),
                        paths,
                    });
                    self.send(AgentEvent::ToolStarted {
                        call_id,
                        title,
                        detail: None,
                    });
                }
                _ => {}
            }
        }
    }

    fn handle_tool_results(&mut self, message: &Value) {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let call_id = ToolCallId(
                    block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool_unknown")
                        .to_string(),
                );
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.send(AgentEvent::ToolCompleted {
                    call_id,
                    status: if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Succeeded
                    },
                    summary: None,
                });
            }
        }
    }
}

/// Names the quota window the way a person would say it. The CLI's own
/// values (`five_hour`, `seven_day`) read as identifiers; an unfamiliar one
/// still comes out as a sentence rather than as nothing.
fn window_phrase(window: &str) -> String {
    match window.trim() {
        "" => "usage limit".to_string(),
        named => format!("{} usage limit", named.replace('_', "-")),
    }
}

/// The reset moment in the reader's own clock terms. Claude Code gives Unix
/// seconds; "resets at 1786474800" tells a user nothing they can plan around.
fn reset_phrase(resets_at: Option<i64>) -> Option<String> {
    let moment = chrono::DateTime::from_timestamp(resets_at?, 0)?.with_timezone(&chrono::Local);
    Some(moment.format("at %-I:%M %p on %a %-d %b").to_string())
}

/// Human-readable activity line for a tool call, plus involved paths.
fn humanize_tool(tool: &str, input: &Value) -> (String, Vec<std::path::PathBuf>) {
    let path_of =
        |key: &str| -> Option<String> { input.get(key).and_then(Value::as_str).map(str::to_owned) };
    let name_of = |p: &str| -> String {
        std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.to_string())
    };
    match tool {
        "Read" => {
            let p = path_of("file_path").unwrap_or_default();
            (format!("Reading {}", name_of(&p)), vec![p.into()])
        }
        "Glob" => (
            format!(
                "Finding files matching {}",
                input
                    .get("pattern")
                    .and_then(Value::as_str)
                    .unwrap_or("a pattern")
            ),
            vec![],
        ),
        "Grep" => (
            format!(
                "Searching for \"{}\"",
                input
                    .get("pattern")
                    .and_then(Value::as_str)
                    .unwrap_or("a pattern")
            ),
            vec![],
        ),
        "LS" => {
            let p = path_of("path").unwrap_or_default();
            (format!("Looking through {}", name_of(&p)), vec![p.into()])
        }
        "TodoWrite" => ("Updating the task plan".into(), vec![]),
        "Task" => ("Working on a sub-task".into(), vec![]),
        t if t.starts_with("mcp__commonspace__") => {
            let short = t.trim_start_matches("mcp__commonspace__");
            (commonspace_tool_title(short, input), vec![])
        }
        other => (format!("Using {other}"), vec![]),
    }
}

/// Titles for Commonspace's own MCP tools.
fn commonspace_tool_title(tool: &str, input: &Value) -> String {
    let file = input
        .get("path")
        .and_then(Value::as_str)
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string())
        })
        .unwrap_or_default();
    match tool {
        "list_folder" => "Looking through the folder".into(),
        "read_file" => format!("Reading {file}"),
        "create_file" => format!("Creating {file}"),
        "overwrite_file" => format!("Updating {file}"),
        "rename_move" => "Renaming a file".into(),
        "delete_to_trash" => format!("Moving {file} to the trash"),
        "find_duplicates" => "Checking for duplicate files".into(),
        other => format!("Using {}", other.replace('_', " ")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn collect(lines: &[&str]) -> Vec<AgentEvent> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut n = Normalizer::new(tx);
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            n.handle(&v);
        }
        drop(n);
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[test]
    fn text_deltas_stream() {
        let events = collect(&[
            r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"result","subtype":"success","result":"Done.","usage":{"input_tokens":10,"output_tokens":5}}"#,
        ]);
        assert!(matches!(events[0], AgentEvent::MessageStarted { .. }));
        assert!(matches!(&events[1], AgentEvent::MessageDelta { text, .. } if text == "Hel"));
        assert!(matches!(&events[2], AgentEvent::MessageDelta { text, .. } if text == "lo"));
        assert!(
            matches!(&events[3], AgentEvent::TaskCompleted { summary, usage } if summary == "Done." && usage.unwrap().input_tokens == Some(10))
        );
    }

    #[test]
    fn tool_use_lifecycle() {
        let events = collect(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"C:/ws/report.docx"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1"}]}}"#,
        ]);
        match &events[0] {
            AgentEvent::ToolRequested {
                tool, title, paths, ..
            } => {
                assert_eq!(tool, "Read");
                assert_eq!(title, "Reading report.docx");
                assert_eq!(paths.len(), 1);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(events[1], AgentEvent::ToolStarted { .. }));
        assert!(matches!(
            &events[2],
            AgentEvent::ToolCompleted {
                status: ToolStatus::Succeeded,
                ..
            }
        ));
    }

    #[test]
    fn full_text_not_duplicated_after_deltas() {
        let events = collect(&[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"}]}}"#,
            r#"{"type":"result","subtype":"success","result":"Hi"}"#,
        ]);
        let deltas: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::MessageDelta { .. }))
            .collect();
        assert_eq!(deltas.len(), 1, "{events:?}");
    }

    #[test]
    fn error_result_maps_to_error_event() {
        let events = collect(&[
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"credit exhausted"}"#,
        ]);
        let AgentEvent::Error { error } = &events[0] else {
            panic!("unexpected {:?}", events[0]);
        };
        assert_eq!(error.message, "credit exhausted");
        // A failure with no quota evidence behind it must not be advertised
        // as worth retrying.
        assert!(!error.transient);
        assert_eq!(error.code, "provider_error");
        assert!(error.recovery.is_none());
    }

    /// Transcribed verbatim from a live `claude -p` run (v2.1.224), not
    /// invented — the field set differs from the docs in docs/research.md §A.
    const RATE_LIMIT_WARNING: &str = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1786474800,"rateLimitType":"seven_day","utilization":0.8,"isUsingOverage":false,"surpassedThreshold":0.75},"uuid":"a9da9cb2-dca5-45bf-bff0-6e4f01125a78","session_id":"s1"}"#;

    #[test]
    fn approaching_a_limit_warns_once_in_local_terms() {
        let events = collect(&[RATE_LIMIT_WARNING, RATE_LIMIT_WARNING]);
        assert_eq!(events.len(), 1, "{events:?}");
        let AgentEvent::Warning { message } = &events[0] else {
            panic!("unexpected {:?}", events[0]);
        };
        assert!(message.contains("seven-day usage limit"), "{message}");
        // The timezone decides the digits, so only the shape is asserted.
        assert!(message.contains("It resets at "), "{message}");
        assert!(!message.contains("1786474800"), "{message}");
    }

    #[test]
    fn a_run_that_stops_on_quota_says_so_and_says_when() {
        let events = collect(&[
            r#"{"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"five_hour","resetsAt":1771606800,"status":"rejected","isUsingOverage":false,"overageStatus":"rejected"}}"#,
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Request failed"}"#,
        ]);
        // `rejected` is not the warning threshold; the terminal error carries it.
        let AgentEvent::Error { error } = &events[0] else {
            panic!("unexpected {:?}", events[0]);
        };
        assert_eq!(error.code, "provider_rate_limited");
        assert!(error.transient);
        let recovery = error.recovery.as_deref().unwrap_or_default();
        assert!(recovery.contains("five-hour usage limit"), "{recovery}");
        assert!(recovery.contains("It resets at "), "{recovery}");
    }

    /// A quota failure the CLI reports only in prose, with no preceding
    /// `rate_limit_event` to read a reset time out of.
    #[test]
    fn quota_wording_alone_is_enough_to_mark_a_failure_retryable() {
        let events = collect(&[
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Claude AI usage limit reached"}"#,
        ]);
        let AgentEvent::Error { error } = &events[0] else {
            panic!("unexpected {:?}", events[0]);
        };
        assert_eq!(error.code, "provider_rate_limited");
        assert!(error.transient);
        let recovery = error.recovery.as_deref().unwrap_or_default();
        assert!(recovery.contains("once it resets"), "{recovery}");
    }

    #[test]
    fn deny_rules_mirror_every_protected_credential_store() {
        let rules = protected_deny_rules();
        for store in commonspace_permissions::credential_store_paths() {
            for expected in [
                format!("Read(~/{store})"),
                format!("Read(~/{store}/**)"),
                format!("Read(//**/{store})"),
                format!("Read(//**/{store}/**)"),
            ] {
                assert!(rules.contains(&expected), "missing {expected}");
            }
        }
        assert!(rules.contains(&"Read(~/.ssh/**)".to_string()));
        assert!(rules.contains(&"Read(//**/.claude/**)".to_string()));
    }

    #[test]
    fn mcp_tool_titles_are_friendly() {
        let (title, _) = humanize_tool(
            "mcp__commonspace__overwrite_file",
            &serde_json::json!({"path": "C:/ws/notes.md"}),
        );
        assert_eq!(title, "Updating notes.md");
    }
}
