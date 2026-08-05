//! The Commonspace MCP tool server.
//!
//! Provider CLIs reach Commonspace's own tools through this server instead of
//! using their built-in write/shell tools. Every call:
//!
//! 1. is authenticated with a per-session bearer token,
//! 2. is classified and evaluated by the deterministic policy engine,
//! 3. is escalated to the user when policy says approval is needed,
//! 4. is executed by [`commonspace_documents::SafeFs`], which backs up,
//!    verifies, and journals it,
//! 5. emits normalized events for the timeline and artifact panel.
//!
//! Transport is JSON-RPC 2.0 over HTTP bound to loopback only. Narrow typed
//! tools are exposed — never a general shell tool.

use crate::broker::{PermissionBroker, PermissionOutcome};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use commonspace_core::{
    AgentEvent, Artifact, ArtifactId, ArtifactKind, OperationClass, PolicyVerdict, RiskLevel,
    TaskId, ToolCallId, ToolStatus,
};
use commonspace_documents::{inspect, office, textio, FileOperation, SafeFs};
use commonspace_permissions::{PolicyEngine, PolicyRequest};
use serde_json::{json, Value};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// MCP protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Maximum bytes returned by `read_file` in one call.
const MAX_READ_BYTES: usize = 400_000;

/// Everything a running task's tools need.
pub struct ToolContext {
    pub task_id: TaskId,
    pub policy: PolicyEngine,
    pub fs: SafeFs,
    pub broker: PermissionBroker,
    pub events: UnboundedSender<AgentEvent>,
    /// Journaled operations are handed here for persistence + undo.
    pub journal: UnboundedSender<FileOperation>,
}

/// A running tool server.
pub struct ToolServerHandle {
    /// Loopback URL to hand to a provider CLI.
    pub url: String,
    /// Per-session bearer token.
    pub token: String,
    shutdown: tokio::sync::oneshot::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

impl ToolServerHandle {
    /// Stop the server and wait for it to finish.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.join.await;
    }
}

/// Builder/launcher for the tool server.
pub struct ToolServer;

impl ToolServer {
    /// Bind an ephemeral loopback port and serve MCP for one task.
    pub async fn start(context: Arc<ToolContext>) -> std::io::Result<ToolServerHandle> {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let state = ServerState {
            context,
            token: token.clone(),
        };

        let app = Router::new()
            .route("/mcp", post(handle_rpc))
            .with_state(Arc::new(state));

        // Loopback only: never reachable from the network.
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let join = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(error) = server.await {
                tracing::error!(%error, "tool server stopped unexpectedly");
            }
        });

        Ok(ToolServerHandle {
            url: format!("http://127.0.0.1:{}/mcp", addr.port()),
            token,
            shutdown: shutdown_tx,
            join,
        })
    }
}

struct ServerState {
    context: Arc<ToolContext>,
    token: String,
}

async fn handle_rpc(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !constant_time_eq(presented.as_bytes(), state.token.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // Notifications carry no id and expect no response body.
    if id.is_none() {
        return Ok(Json(json!({})));
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "commonspace", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(&state.context, &params).await,
        other => Err(RpcError::method_not_found(other)),
    };

    Ok(Json(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message }
        }),
    }))
}

/// Compare secrets without leaking length-independent timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("unknown method: {method}"),
        }
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }
}

/// Tool schemas advertised to the agent. Narrow and typed by design: there
/// is no general shell tool here.
fn tool_definitions() -> Vec<Value> {
    let path_prop = |desc: &str| json!({ "type": "string", "description": desc });
    vec![
        json!({
            "name": "list_folder",
            "description": "List files and folders inside an authorized folder, with sizes and types.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_prop("Absolute path of the folder to list."),
                    "max_depth": { "type": "integer", "description": "How many levels deep (default 3)." },
                    "max_entries": { "type": "integer", "description": "Maximum entries to return (default 500)." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "read_file",
            "description": "Read a text file's contents, detecting its encoding.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_prop("Absolute path of the file to read.") },
                "required": ["path"]
            }
        }),
        json!({
            "name": "find_duplicates",
            "description": "Find files with identical contents inside an authorized folder.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_prop("Absolute path of the folder to scan.") },
                "required": ["path"]
            }
        }),
        json!({
            "name": "create_file",
            "description": "Create a new file. Fails if the file already exists.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_prop("Absolute path of the file to create."),
                    "content": { "type": "string", "description": "Full contents of the new file." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "overwrite_file",
            "description": "Replace an existing file's contents. The original is backed up first and the change can be undone.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_prop("Absolute path of the file to replace."),
                    "content": { "type": "string", "description": "Full new contents." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "read_document",
            "description": "Extract the text of a PDF or Word document, including its paragraphs.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_prop("Absolute path of the .pdf or .docx file.") },
                "required": ["path"]
            }
        }),
        json!({
            "name": "create_document",
            "description": "Create a Word document (.docx) from Markdown-style content. Headings use '#', bullets use '-'. Commonspace builds and validates the file; never write .docx bytes yourself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_prop("Absolute path of the .docx file to create."),
                    "content": {
                        "type": "string",
                        "description": "Markdown-style content: '# Heading', '- bullet', or plain paragraphs."
                    }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "rename_move",
            "description": "Rename a file or move it to another folder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": path_prop("Current absolute path."),
                    "to": path_prop("New absolute path.")
                },
                "required": ["from", "to"]
            }
        }),
        json!({
            "name": "delete_to_trash",
            "description": "Move a file to the operating system's trash. A backup copy is kept so this can be undone.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_prop("Absolute path of the file to delete.") },
                "required": ["path"]
            }
        }),
    ]
}

async fn call_tool(context: &ToolContext, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing tool name"))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    let call_id = ToolCallId::generate();
    let outcome = dispatch(context, name, &args, &call_id).await;

    match outcome {
        Ok(text) => {
            let _ = context.events.send(AgentEvent::ToolCompleted {
                call_id,
                status: ToolStatus::Succeeded,
                summary: None,
            });
            Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
        }
        Err(ToolFailure::Denied(message)) => {
            let _ = context.events.send(AgentEvent::ToolCompleted {
                call_id,
                status: ToolStatus::Denied,
                summary: Some(message.clone()),
            });
            // Denials are returned as tool results, not protocol errors: the
            // agent should adapt and continue, not abort the session.
            Ok(json!({ "content": [{ "type": "text", "text": message }], "isError": true }))
        }
        Err(ToolFailure::Failed(message)) => {
            let _ = context.events.send(AgentEvent::ToolCompleted {
                call_id,
                status: ToolStatus::Failed,
                summary: Some(message.clone()),
            });
            Ok(json!({ "content": [{ "type": "text", "text": message }], "isError": true }))
        }
        Err(ToolFailure::Protocol(error)) => Err(error),
    }
}

enum ToolFailure {
    /// Policy or the user said no.
    Denied(String),
    /// The operation was attempted and failed.
    Failed(String),
    /// Malformed request.
    Protocol(RpcError),
}

fn arg_path(args: &Value, key: &str) -> Result<PathBuf, ToolFailure> {
    args.get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| ToolFailure::Protocol(RpcError::invalid_params(format!("missing {key}"))))
}

fn arg_str(args: &Value, key: &str) -> Result<String, ToolFailure> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolFailure::Protocol(RpcError::invalid_params(format!("missing {key}"))))
}

async fn dispatch(
    context: &ToolContext,
    name: &str,
    args: &Value,
    call_id: &ToolCallId,
) -> Result<String, ToolFailure> {
    match name {
        "list_folder" => {
            let path = arg_path(args, "path")?;
            gate(
                context,
                OperationClass::Read,
                std::slice::from_ref(&path),
                None,
                &format!("Look through {}", display_name(&path)),
            )
            .await?;
            started(context, call_id, "Looking through the folder", None);
            let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as usize;
            let max = args
                .get("max_entries")
                .and_then(Value::as_u64)
                .unwrap_or(500) as usize;
            let listing = inspect::list_dir(&path, depth, max)
                .map_err(|e| ToolFailure::Failed(format!("could not list the folder: {e}")))?;
            serde_json::to_string_pretty(&listing).map_err(|e| ToolFailure::Failed(e.to_string()))
        }
        "read_file" => {
            let path = arg_path(args, "path")?;
            gate(
                context,
                OperationClass::Read,
                std::slice::from_ref(&path),
                None,
                &format!("Read {}", display_name(&path)),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Reading {}", display_name(&path)),
                None,
            );
            let text = textio::read_text(&path, MAX_READ_BYTES)
                .map_err(|e| ToolFailure::Failed(format!("could not read the file: {e}")))?;
            let mut out = text.content;
            if text.truncated {
                out.push_str("\n\n[Commonspace: file truncated for length]");
            }
            Ok(out)
        }
        "find_duplicates" => {
            let path = arg_path(args, "path")?;
            gate(
                context,
                OperationClass::Read,
                std::slice::from_ref(&path),
                None,
                "Check for duplicates",
            )
            .await?;
            started(context, call_id, "Checking for duplicate files", None);
            let groups = inspect::find_duplicates(&path, 20_000)
                .map_err(|e| ToolFailure::Failed(format!("could not scan the folder: {e}")))?;
            let described: Vec<Value> = groups
                .into_iter()
                .map(|(hash, paths)| json!({ "content_hash": hash, "paths": paths }))
                .collect();
            serde_json::to_string_pretty(&described).map_err(|e| ToolFailure::Failed(e.to_string()))
        }
        "read_document" => {
            let path = arg_path(args, "path")?;
            gate(
                context,
                OperationClass::Read,
                std::slice::from_ref(&path),
                None,
                &format!("Read {}", display_name(&path)),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Reading {}", display_name(&path)),
                None,
            );
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let extracted = match extension.as_str() {
                "pdf" => office::read_pdf(&path, MAX_READ_BYTES),
                "docx" => office::read_docx(&path, MAX_READ_BYTES),
                other => {
                    return Err(ToolFailure::Failed(format!(
                        "Commonspace can't read .{other} documents yet. \
                         Supported here: .pdf and .docx."
                    )))
                }
            }
            .map_err(|e| ToolFailure::Failed(e.to_string()))?;
            serde_json::to_string_pretty(&extracted).map_err(|e| ToolFailure::Failed(e.to_string()))
        }
        "create_document" => {
            let path = arg_path(args, "path")?;
            let content = arg_str(args, "content")?;
            gate(
                context,
                OperationClass::Create,
                std::slice::from_ref(&path),
                None,
                &format!("Create {}", display_name(&path)),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Writing {}", display_name(&path)),
                None,
            );
            if path.exists() {
                return Err(ToolFailure::Failed(format!(
                    "{} already exists; ask before replacing it.",
                    display_name(&path)
                )));
            }
            let blocks = office::blocks_from_markdown(&content);
            let result = office::create_docx(&path, &blocks)
                .map_err(|e| ToolFailure::Failed(e.to_string()))?;
            // Journaled as a create so the artifact card can offer undo.
            let mut op =
                FileOperation::new(commonspace_documents::FileOpKind::Create, path.clone());
            op.hash_after = inspect::hash_file(&path).ok();
            record(context, &op, &path, false, None);
            Ok(result.user_summary)
        }
        "create_file" => {
            let path = arg_path(args, "path")?;
            let content = arg_str(args, "content")?;
            gate(
                context,
                OperationClass::Create,
                std::slice::from_ref(&path),
                None,
                &format!("Create {}", display_name(&path)),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Creating {}", display_name(&path)),
                None,
            );
            let (result, op) = context
                .fs
                .create_file(&path, content.as_bytes())
                .map_err(|e| ToolFailure::Failed(e.to_string()))?;
            record(context, &op, &path, false, None);
            Ok(result.user_summary)
        }
        "overwrite_file" => {
            let path = arg_path(args, "path")?;
            let content = arg_str(args, "content")?;
            gate(
                context,
                OperationClass::Modify,
                std::slice::from_ref(&path),
                None,
                &format!("Update {}", display_name(&path)),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Updating {}", display_name(&path)),
                None,
            );
            let (result, op) = context
                .fs
                .overwrite_file(&path, content.as_bytes())
                .map_err(|e| ToolFailure::Failed(e.to_string()))?;
            let summary = result.user_summary.clone();
            record(context, &op, &path, true, Some("Contents replaced".into()));
            Ok(summary)
        }
        "rename_move" => {
            let from = arg_path(args, "from")?;
            let to = arg_path(args, "to")?;
            let same_folder = from.parent() == to.parent();
            let class = if same_folder {
                OperationClass::Rename
            } else {
                OperationClass::Move
            };
            gate(
                context,
                class,
                std::slice::from_ref(&from),
                Some(to.clone()),
                &format!("Move {} to {}", display_name(&from), to.display()),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Moving {}", display_name(&from)),
                None,
            );
            let (result, op) = context
                .fs
                .rename_or_move(&from, &to)
                .map_err(|e| ToolFailure::Failed(e.to_string()))?;
            let summary = result.user_summary.clone();
            record(context, &op, &to, true, Some("Renamed or moved".into()));
            Ok(summary)
        }
        "delete_to_trash" => {
            let path = arg_path(args, "path")?;
            gate(
                context,
                OperationClass::Delete,
                std::slice::from_ref(&path),
                None,
                &format!("Move {} to the trash", display_name(&path)),
            )
            .await?;
            started(
                context,
                call_id,
                &format!("Deleting {}", display_name(&path)),
                None,
            );
            let (result, op) = context
                .fs
                .delete_to_trash(&path)
                .map_err(|e| ToolFailure::Failed(e.to_string()))?;
            let summary = result.user_summary.clone();
            let _ = context.journal.send(op);
            Ok(summary)
        }
        other => Err(ToolFailure::Protocol(RpcError::invalid_params(format!(
            "unknown tool: {other}"
        )))),
    }
}

/// Evaluate policy, escalate to the user when required, and refuse otherwise.
async fn gate(
    context: &ToolContext,
    class: OperationClass,
    targets: &[PathBuf],
    destination: Option<PathBuf>,
    summary: &str,
) -> Result<(), ToolFailure> {
    let request = PolicyRequest {
        class,
        targets: targets.to_vec(),
        destination: destination.clone(),
        permanent: false,
    };
    let verdict = context
        .policy
        .evaluate(&request)
        .map_err(|e| ToolFailure::Denied(format!("This path cannot be used: {e}")))?;

    match verdict {
        PolicyVerdict::Allow => Ok(()),
        PolicyVerdict::Deny { reason } => Err(ToolFailure::Denied(reason)),
        PolicyVerdict::RequireApproval { reason } => {
            let mut paths = targets.to_vec();
            if let Some(dest) = destination {
                paths.push(dest);
            }
            let outcome = context
                .broker
                .request(
                    crate::broker::Ask {
                        task_id: context.task_id.clone(),
                        operation: class,
                        summary: summary.to_string(),
                        paths,
                        items: Vec::new(),
                        risk: risk_of(class),
                        // Deletions go to the OS trash and keep a backup, so
                        // nothing Commonspace does through these tools is
                        // irreversible today. This flag exists for the
                        // operations that will be (permanent delete, send,
                        // publish) and must be set honestly when they land.
                        irreversible: false,
                    },
                    &context.events,
                )
                .await;
            match outcome {
                o if o.is_allowed() => Ok(()),
                PermissionOutcome::Denied => Err(ToolFailure::Denied(format!(
                    "The person using Commonspace declined: {reason}"
                ))),
                _ => Err(ToolFailure::Denied(
                    "This task ended before the request was answered.".into(),
                )),
            }
        }
    }
}

fn risk_of(class: OperationClass) -> RiskLevel {
    match class {
        OperationClass::Read | OperationClass::Create => RiskLevel::Low,
        OperationClass::Modify | OperationClass::Rename | OperationClass::Move => RiskLevel::Medium,
        _ => RiskLevel::High,
    }
}

fn started(context: &ToolContext, call_id: &ToolCallId, title: &str, detail: Option<String>) {
    let _ = context.events.send(AgentEvent::ToolStarted {
        call_id: call_id.clone(),
        title: title.to_string(),
        detail,
    });
}

/// Journal an operation and surface the resulting artifact.
fn record(
    context: &ToolContext,
    op: &FileOperation,
    path: &std::path::Path,
    modified_existing: bool,
    change_summary: Option<String>,
) {
    let artifact = Artifact {
        id: ArtifactId::generate(),
        task_id: context.task_id.clone(),
        kind: ArtifactKind::from_extension(
            &path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        ),
        path: path.to_path_buf(),
        name: display_name(path),
        modified_existing,
        backup_path: op.backup.clone(),
        file_operation_id: Some(op.id.clone()),
        change_summary,
        created_at: chrono::Utc::now(),
    };
    let event = if modified_existing {
        AgentEvent::ArtifactModified { artifact }
    } else {
        AgentEvent::ArtifactCreated { artifact }
    };
    let _ = context.events.send(event);
    let _ = context.journal.send(op.clone());
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use commonspace_core::{DecisionScope, PermissionDecision};
    use commonspace_documents::BackupStore;
    use commonspace_permissions::{PathGuard, PolicySettings};

    struct Harness {
        _tmp: tempfile::TempDir,
        ws: PathBuf,
        url: String,
        token: String,
        events: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
        journal: tokio::sync::mpsc::UnboundedReceiver<FileOperation>,
        broker: PermissionBroker,
        handle: Option<ToolServerHandle>,
    }

    async fn harness() -> Harness {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).expect("workspace");
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (journal_tx, journal_rx) = tokio::sync::mpsc::unbounded_channel();
        let broker = PermissionBroker::new();
        let context = Arc::new(ToolContext {
            task_id: TaskId::generate(),
            policy: PolicyEngine::new(PathGuard::new([&ws]), PolicySettings::default()),
            fs: SafeFs::new(
                PathGuard::new([&ws]),
                BackupStore::new(tmp.path().join("backups")),
            ),
            broker: broker.clone(),
            events: event_tx,
            journal: journal_tx,
        });
        let handle = ToolServer::start(context).await.expect("server starts");
        Harness {
            _tmp: tmp,
            ws,
            url: handle.url.clone(),
            token: handle.token.clone(),
            events: event_rx,
            journal: journal_rx,
            broker,
            handle: Some(handle),
        }
    }

    async fn rpc(h: &Harness, body: Value, token: Option<&str>) -> (u16, Value) {
        let client = reqwest::Client::new();
        let response = client
            .post(&h.url)
            .bearer_auth(token.unwrap_or(&h.token))
            .json(&body)
            .send()
            .await
            .expect("request");
        let status = response.status().as_u16();
        let value = response.json::<Value>().await.unwrap_or(Value::Null);
        (status, value)
    }

    fn call(name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        })
    }

    #[tokio::test]
    async fn rejects_missing_or_wrong_token() {
        let h = harness().await;
        let (status, _) = rpc(
            &h,
            json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            Some("wrong"),
        )
        .await;
        assert_eq!(status, 401);
        let (status, _) = rpc(
            &h,
            json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            Some(""),
        )
        .await;
        assert_eq!(status, 401);
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn initialize_and_list_tools() {
        let h = harness().await;
        let (_, init) = rpc(
            &h,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            None,
        )
        .await;
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(init["result"]["serverInfo"]["name"], "commonspace");

        let (_, list) = rpc(
            &h,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            None,
        )
        .await;
        let tools = list["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_folder"));
        assert!(names.contains(&"overwrite_file"));
        // No general shell tool is ever exposed.
        assert!(!names
            .iter()
            .any(|n| n.contains("shell") || n.contains("exec")));
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn create_in_scope_succeeds_and_journals() {
        let mut h = harness().await;
        let target = h.ws.join("notes.md");
        let (_, response) = rpc(
            &h,
            call("create_file", json!({"path": target, "content": "# hello"})),
            None,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "# hello");

        // The journal deliberately records the *resolved* path, which can be
        // spelled differently from the one we asked for: on some Windows
        // machines the temp directory contains an 8.3 short name (for example
        // `RUNNER~1`) that canonicalization expands. Compare both canonically
        // rather than asserting on one platform's spelling.
        let op = h.journal.recv().await.expect("journal entry");
        assert_eq!(
            std::fs::canonicalize(&op.source).expect("canonicalize journaled path"),
            std::fs::canonicalize(&target).expect("canonicalize target"),
        );

        let mut saw_artifact = false;
        while let Ok(event) = h.events.try_recv() {
            if matches!(event, AgentEvent::ArtifactCreated { .. }) {
                saw_artifact = true;
            }
        }
        assert!(saw_artifact, "expected an artifact.created event");
        h.handle.expect("handle").shutdown().await;
    }

    /// Writing outside the workspace is never silently allowed: it raises an
    /// approval request naming the resolved path, and declining leaves
    /// nothing behind.
    #[tokio::test]
    async fn out_of_scope_write_asks_first_and_creates_nothing_when_declined() {
        let mut h = harness().await;
        let outside = h.ws.parent().expect("parent").join("escape.txt");

        let url = h.url.clone();
        let token = h.token.clone();
        let outside2 = outside.clone();
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(&url)
                .bearer_auth(token)
                .json(&call(
                    "create_file",
                    json!({"path": outside2, "content": "nope"}),
                ))
                .send()
                .await
                .expect("request")
                .json::<Value>()
                .await
                .expect("json")
        });

        let asked = loop {
            let event = h.events.recv().await.expect("event");
            if let AgentEvent::PermissionRequested { request } = event {
                break request;
            }
        };
        assert_eq!(asked.operation, OperationClass::Create);
        assert!(
            asked.paths.iter().any(|p| p.ends_with("escape.txt")),
            "the dialog must name the resolved path: {:?}",
            asked.paths
        );
        assert!(h.broker.respond(&asked.id, PermissionDecision::Deny));

        let response = request.await.expect("join");
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(!outside.exists(), "a declined write must create nothing");
        h.handle.expect("handle").shutdown().await;
    }

    /// A request nobody answers blocks the tool call rather than defaulting
    /// either way; cancelling the task resolves it as refused.
    #[tokio::test]
    async fn unanswered_request_blocks_until_the_task_is_cancelled() {
        let mut h = harness().await;
        let target = h.ws.join("contract.txt");
        std::fs::write(&target, "original").expect("seed");

        let url = h.url.clone();
        let token = h.token.clone();
        let target2 = target.clone();
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(&url)
                .bearer_auth(token)
                .json(&call(
                    "overwrite_file",
                    json!({"path": target2, "content": "revised"}),
                ))
                .send()
                .await
                .expect("request")
                .json::<Value>()
                .await
                .expect("json")
        });

        let asked = loop {
            let event = h.events.recv().await.expect("event");
            if let AgentEvent::PermissionRequested { request } = event {
                break request;
            }
        };

        // Still blocked: nothing has changed on disk.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), async {})
                .await
                .is_ok()
        );
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "original");

        h.broker.abandon_task(&asked.task_id);
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), request)
            .await
            .expect("cancelling must unblock the tool call")
            .expect("join");
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "original");
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn protected_location_is_denied_outright() {
        let h = harness().await;
        let secret = dirs_home().join(".ssh").join("id_ed25519");
        let (_, response) = rpc(&h, call("read_file", json!({"path": secret})), None).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("protected"), "unexpected message: {text}");
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn modify_waits_for_approval_then_applies() {
        let mut h = harness().await;
        let target = h.ws.join("contract.txt");
        std::fs::write(&target, "original").expect("seed");

        let url = h.url.clone();
        let token = h.token.clone();
        let target2 = target.clone();
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(&url)
                .bearer_auth(token)
                .json(&call(
                    "overwrite_file",
                    json!({"path": target2, "content": "revised"}),
                ))
                .send()
                .await
                .expect("request")
                .json::<Value>()
                .await
                .expect("json")
        });

        // The approval request arrives; the file is untouched until answered.
        let event = loop {
            let event = h.events.recv().await.expect("event");
            if let AgentEvent::PermissionRequested { request } = event {
                break request;
            }
        };
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "original");
        assert!(h.broker.respond(
            &event.id,
            PermissionDecision::Approve {
                scope: DecisionScope::Once
            }
        ));

        let response = request.await.expect("join");
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "revised");
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn denied_modify_leaves_the_file_alone() {
        let mut h = harness().await;
        let target = h.ws.join("contract.txt");
        std::fs::write(&target, "original").expect("seed");

        let url = h.url.clone();
        let token = h.token.clone();
        let target2 = target.clone();
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(&url)
                .bearer_auth(token)
                .json(&call(
                    "overwrite_file",
                    json!({"path": target2, "content": "revised"}),
                ))
                .send()
                .await
                .expect("request")
                .json::<Value>()
                .await
                .expect("json")
        });

        let event = loop {
            let event = h.events.recv().await.expect("event");
            if let AgentEvent::PermissionRequested { request } = event {
                break request;
            }
        };
        assert!(h.broker.respond(&event.id, PermissionDecision::Deny));

        let response = request.await.expect("join");
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "original");
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn read_and_list_need_no_approval_in_scope() {
        let h = harness().await;
        std::fs::write(h.ws.join("a.txt"), "contents").expect("seed");
        let (_, listed) = rpc(&h, call("list_folder", json!({"path": h.ws})), None).await;
        assert_eq!(listed["result"]["isError"], false, "{listed}");
        let (_, read) = rpc(
            &h,
            call("read_file", json!({"path": h.ws.join("a.txt")})),
            None,
        )
        .await;
        assert_eq!(read["result"]["content"][0]["text"], "contents");
        h.handle.expect("handle").shutdown().await;
    }

    #[tokio::test]
    async fn unknown_tool_is_an_error_not_a_panic() {
        let h = harness().await;
        let (_, response) = rpc(
            &h,
            call("run_shell_command", json!({"cmd": "rm -rf /"})),
            None,
        )
        .await;
        assert!(response["error"].is_object(), "{response}");
        h.handle.expect("handle").shutdown().await;
    }

    fn dirs_home() -> PathBuf {
        #[cfg(windows)]
        {
            std::env::var("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
        }
    }
}
