//! OpenAI Codex CLI adapter.
//!
//! Headless invocation: `codex exec --json` (JSONL events on stdout), prompt
//! delivered via stdin (`-`), workspace passed with `-C`, sandbox pinned to
//! `read-only` in v1 — mutations flow through Commonspace's MCP tools, where
//! policy and approvals are enforced deterministically.
//!
//! Auth: owned by the Codex CLI (`codex login`). Status is probed with the
//! documented read-only `codex login status` command, which reports whether
//! the account is a ChatGPT subscription or an API key.

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
use serde_json::Value;

const CLI: &str = "codex";

pub struct CodexCliAdapter;

#[async_trait::async_trait]
impl AgentAdapter for CodexCliAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::CodexCli
    }

    async fn detect(&self) -> InstallStatus {
        probe_version(CLI, &[]).await
    }

    async fn auth_status(&self) -> AuthStatus {
        let Some(path) = find_cli(CLI) else {
            return AuthStatus::NotInstalled;
        };
        let args = vec!["login".to_string(), "status".to_string()];
        let cwd = std::env::temp_dir();
        let (code, output) = match crate::process::probe_output(
            &path,
            &args,
            &cwd,
            &[],
            std::time::Duration::from_secs(20),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                return AuthStatus::Error {
                    detail: format!("could not run `codex login status`: {error}"),
                }
            }
        };
        let lower = output.to_lowercase();
        if code == Some(0) {
            if lower.contains("chatgpt") {
                AuthStatus::Subscription {
                    plan_hint: Some("ChatGPT".into()),
                }
            } else if lower.contains("api key") {
                AuthStatus::ApiKey
            } else {
                // Signed in via some method the probe didn't recognize.
                AuthStatus::Subscription { plan_hint: None }
            }
        } else {
            AuthStatus::SignedOut
        }
    }

    fn auth_instructions(&self) -> AuthInstructions {
        AuthInstructions {
            command: CLI.into(),
            args: vec!["login".into()],
            explanation: "Sign in with OpenAI's own Codex CLI. `codex login` opens the official \
                          browser sign-in. A ChatGPT Plus/Pro/Team plan is used if your account \
                          has one; `codex login --api-key` uses API billing instead. Commonspace \
                          never sees or stores these credentials."
                .into(),
        }
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            models: vec!["default".into()],
            supports_resume: true,
            attachment_types: vec!["image/png".into(), "image/jpeg".into()],
            context_tokens: None,
            supports_permission_bridge: false,
        }
    }

    async fn start_session(
        &self,
        request: SessionRequest,
        events: EventSink,
    ) -> Result<RunningSession, AdapterError> {
        let path = find_cli(CLI).ok_or(AdapterError::NotInstalled("Codex CLI"))?;

        let mut args: Vec<String> = vec!["exec".into()];
        if let Some(resume) = &request.resume {
            args.push("resume".into());
            args.push(resume.clone());
        }
        args.extend([
            "--json".into(),
            "--skip-git-repo-check".into(),
            "-s".into(),
            "read-only".into(),
            "-C".into(),
            request.cwd.to_string_lossy().into_owned(),
        ]);
        for root in &request.workspace_roots {
            if root != &request.cwd {
                args.push("--add-dir".into());
                args.push(root.to_string_lossy().into_owned());
            }
        }
        if let Some(model) = &request.model {
            if model != "default" {
                args.push("-m".into());
                args.push(model.clone());
            }
        }
        let mut envs: Vec<(String, String)> = Vec::new();
        if let Some(mcp) = &request.mcp {
            // Loopback HTTP MCP server; the bearer token travels via env,
            // not argv (argv is visible in the process list).
            args.push("-c".into());
            args.push(format!("mcp_servers.commonspace.url=\"{}\"", mcp.url));
            args.push("-c".into());
            args.push(
                "mcp_servers.commonspace.bearer_token_env_var=\"COMMONSPACE_MCP_TOKEN\""
                    .to_string(),
            );
            envs.push(("COMMONSPACE_MCP_TOKEN".into(), mcp.token.clone()));
        }
        // Prompt via stdin.
        args.push("-".into());

        let mut cli =
            spawn_cli(&path, &args, &request.cwd, &envs).map_err(|source| AdapterError::Spawn {
                cli: "Codex CLI",
                source,
            })?;
        cli.write_line(&request.prompt).await?;
        cli.close_stdin();

        let session_id = SessionId::generate();
        let (psid_tx, psid_rx) = tokio::sync::watch::channel(None::<String>);
        let kill = cli.kill.clone();
        let stderr_tail = cli.stderr_tail.clone();

        let done = tokio::spawn(async move {
            let mut normalizer = Normalizer::new(events.clone());
            let mut finished = false;
            while let Some(line) = cli.stdout_lines.recv().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(tid) = value.get("thread_id").and_then(Value::as_str) {
                    let _ = psid_tx.send_if_modified(|cur| {
                        if cur.as_deref() != Some(tid) {
                            *cur = Some(tid.to_string());
                            true
                        } else {
                            false
                        }
                    });
                }
                finished |= normalizer.handle(&value);
            }
            let code = cli.wait().await.ok().flatten();
            if !finished {
                let tail: Vec<String> = {
                    let t = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
                    t.iter().rev().take(5).rev().cloned().collect()
                };
                let _ = events.send(AgentEvent::Error {
                    error: AgentErrorInfo {
                        code: "provider_exited".into(),
                        message: format!("Codex ended unexpectedly (exit code {code:?})."),
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
                    InstallStatus::NotInstalled => Some("codex not found on PATH".into()),
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

/// Translates `codex exec --json` JSONL into normalized [`AgentEvent`]s.
struct Normalizer {
    events: EventSink,
    last_agent_message: Option<String>,
}

impl Normalizer {
    fn new(events: EventSink) -> Self {
        Self {
            events,
            last_agent_message: None,
        }
    }

    fn send(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    /// Returns true on a terminal event (`turn.completed` / `turn.failed`).
    fn handle(&mut self, value: &Value) -> bool {
        match value.get("type").and_then(Value::as_str) {
            Some("item.started") | Some("item.updated") => {
                self.handle_item(value.get("item").unwrap_or(&Value::Null), false);
                false
            }
            Some("item.completed") => {
                self.handle_item(value.get("item").unwrap_or(&Value::Null), true);
                false
            }
            Some("turn.completed") => {
                let usage = value.get("usage").map(|u| UsageInfo {
                    input_tokens: u.get("input_tokens").and_then(Value::as_u64),
                    output_tokens: u.get("output_tokens").and_then(Value::as_u64),
                });
                let summary = self
                    .last_agent_message
                    .take()
                    .unwrap_or_else(|| "Task finished.".to_string());
                self.send(AgentEvent::TaskCompleted { summary, usage });
                true
            }
            Some("turn.failed") | Some("error") => {
                let message = value
                    .pointer("/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex reported an error.")
                    .to_string();
                self.send(AgentEvent::Error {
                    error: AgentErrorInfo {
                        code: "provider_error".into(),
                        message,
                        recovery: None,
                        transient: false,
                    },
                });
                true
            }
            other => {
                // Permissive on purpose: a line type Commonspace has never
                // seen must not end a session. Recording it is what keeps
                // "never seen" from also meaning "never noticed".
                tracing::debug!(
                    line_type = other.unwrap_or("<untyped>"),
                    "unhandled Codex line type"
                );
                false
            }
        }
    }

    fn handle_item(&mut self, item: &Value, completed: bool) {
        let item_type = item
            .get("item_type")
            .or_else(|| item.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let call_id = ToolCallId(
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or("item_unknown")
                .to_string(),
        );
        match item_type {
            "agent_message" => {
                if completed {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        let message_id = MessageId::generate();
                        self.send(AgentEvent::MessageStarted {
                            message_id: message_id.clone(),
                            role: MessageRole::Assistant,
                        });
                        self.send(AgentEvent::MessageDelta {
                            message_id,
                            text: text.to_string(),
                        });
                        self.last_agent_message = Some(text.to_string());
                    }
                }
            }
            "reasoning" => {
                if completed {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        self.send(AgentEvent::ReasoningSummary {
                            text: text.to_string(),
                        });
                    }
                }
            }
            "command_execution" => {
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("a command");
                if completed {
                    let exit = item.get("exit_code").and_then(Value::as_i64);
                    let failed = exit.is_some_and(|c| c != 0)
                        || item.get("status").and_then(Value::as_str) == Some("failed");
                    self.send(AgentEvent::ToolCompleted {
                        call_id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Succeeded
                        },
                        summary: Some(format!("Ran: {command}")),
                    });
                } else {
                    self.send(AgentEvent::ToolStarted {
                        call_id,
                        title: "Running a command".into(),
                        detail: Some(command.to_string()),
                    });
                }
            }
            "mcp_tool_call" => {
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or("a tool");
                let title = format!("Using {}", tool.replace('_', " "));
                if completed {
                    let failed = item.get("status").and_then(Value::as_str) == Some("failed");
                    self.send(AgentEvent::ToolCompleted {
                        call_id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Succeeded
                        },
                        summary: None,
                    });
                } else {
                    self.send(AgentEvent::ToolStarted {
                        call_id,
                        title,
                        detail: None,
                    });
                }
            }
            "file_change" => {
                if completed {
                    let count = item
                        .get("changes")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    self.send(AgentEvent::ToolCompleted {
                        call_id,
                        status: ToolStatus::Succeeded,
                        summary: Some(format!(
                            "Changed {count} file{}",
                            if count == 1 { "" } else { "s" }
                        )),
                    });
                }
            }
            "web_search" => {
                if !completed {
                    self.send(AgentEvent::ToolStarted {
                        call_id,
                        title: "Searching the web".into(),
                        detail: None,
                    });
                } else {
                    self.send(AgentEvent::ToolCompleted {
                        call_id,
                        status: ToolStatus::Succeeded,
                        summary: None,
                    });
                }
            }
            // Codex's own plan updates; surfaced as progress, not a plan
            // (Commonspace plans are extracted at the orchestrator level).
            "todo_list" if completed => {
                self.send(AgentEvent::ToolProgress {
                    call_id,
                    detail: "Updated its checklist".into(),
                });
            }
            _ => {}
        }
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
    fn thread_message_turn_flow() {
        let events = collect(&[
            r#"{"type":"thread.started","thread_id":"t1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","item_type":"agent_message","text":"All organized."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20}}"#,
        ]);
        assert!(matches!(events[0], AgentEvent::MessageStarted { .. }));
        assert!(
            matches!(&events[1], AgentEvent::MessageDelta { text, .. } if text == "All organized.")
        );
        assert!(
            matches!(&events[2], AgentEvent::TaskCompleted { summary, usage } if summary == "All organized." && usage.unwrap().output_tokens == Some(20))
        );
    }

    #[test]
    fn command_execution_lifecycle() {
        let events = collect(&[
            r#"{"type":"item.started","item":{"id":"item_1","item_type":"command_execution","command":"dir"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","item_type":"command_execution","command":"dir","exit_code":0}}"#,
        ]);
        assert!(
            matches!(&events[0], AgentEvent::ToolStarted { title, detail, .. } if title == "Running a command" && detail.as_deref() == Some("dir"))
        );
        assert!(matches!(
            &events[1],
            AgentEvent::ToolCompleted {
                status: ToolStatus::Succeeded,
                ..
            }
        ));
    }

    #[test]
    fn failed_turn_maps_to_error() {
        let events =
            collect(&[r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#]);
        assert!(
            matches!(&events[0], AgentEvent::Error { error } if error.message == "usage limit reached")
        );
    }

    #[test]
    fn unknown_items_are_ignored_not_fatal() {
        let events = collect(&[
            r#"{"type":"item.completed","item":{"id":"x","item_type":"future_thing"}}"#,
            r#"{"type":"turn.completed"}"#,
        ]);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AgentEvent::TaskCompleted { .. }));
    }
}
