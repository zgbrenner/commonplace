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

/// Read-only Claude tools enabled in v1. Mutations go through Commonspace's
/// MCP tools ("mcp__commonspace") instead of Write/Edit/Bash.
const ALLOWED_TOOLS: &str = "Read,Glob,Grep,LS,TodoWrite,Task,mcp__commonspace";

/// Explicitly denied tools: mutation, shell, and network. `--allowedTools`
/// only allowlists permissions; this deny list makes the restriction
/// explicit and robust (deny wins in Claude Code's evaluation order).
const DISALLOWED_TOOLS: &str =
    "Bash,PowerShell,Edit,Write,NotebookEdit,WebFetch,WebSearch,KillShell";

pub struct ClaudeCodeAdapter;

#[async_trait::async_trait]
impl AgentAdapter for ClaudeCodeAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::ClaudeCode
    }

    async fn detect(&self) -> InstallStatus {
        probe_version(CLI).await
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
        if let Ok(mut cli) = spawn_cli(&path, &args, &cwd, &[]) {
            let mut output = String::new();
            let collect = async {
                while let Some(line) = cli.stdout_lines.recv().await {
                    output.push_str(&line);
                    output.push('\n');
                }
            };
            if tokio::time::timeout(std::time::Duration::from_secs(20), collect)
                .await
                .is_err()
            {
                cli.kill.kill().await;
            } else if cli.wait().await.ok().flatten() == Some(0) {
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
        if let Some(mcp) = &request.mcp {
            let config = json!({
                "mcpServers": {
                    "commonspace": {
                        "type": "http",
                        "url": mcp.url,
                        "headers": { "Authorization": format!("Bearer {}", mcp.token) }
                    }
                }
            });
            args.push("--mcp-config".into());
            args.push(config.to_string());
            args.push("--strict-mcp-config".into());
        }

        let mut cli =
            spawn_cli(&path, &args, &request.cwd, &[]).map_err(|source| AdapterError::Spawn {
                cli: "Claude Code",
                source,
            })?;

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
                    continue; // non-JSON noise is ignored, kept in raw logs
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

/// Translates Claude Code stream-json lines into normalized [`AgentEvent`]s.
struct Normalizer {
    events: EventSink,
    current_message: Option<MessageId>,
    /// Set when partial deltas stream in, so full assistant text blocks are
    /// not emitted twice.
    saw_deltas: bool,
}

impl Normalizer {
    fn new(events: EventSink) -> Self {
        Self {
            events,
            current_message: None,
            saw_deltas: false,
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
                    self.send(AgentEvent::Error {
                        error: AgentErrorInfo {
                            code: "provider_error".into(),
                            message: summary,
                            recovery: None,
                            transient: false,
                        },
                    });
                } else {
                    self.send(AgentEvent::TaskCompleted { summary, usage });
                }
                true
            }
            _ => false,
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
        assert!(
            matches!(&events[0], AgentEvent::Error { error } if error.message == "credit exhausted")
        );
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
