//! Typed storage API. One `Storage` handle per app, internally synchronized.

use crate::migrations::migrate_to_latest;
use chrono::Utc;
use commonspace_core::{
    AgentEvent, Artifact, ArtifactKind, ConversationId, MessageId, MessageRole, ProviderId,
    SessionId, TaskId, TaskPlan, TaskState, WorkspaceId,
};
use commonspace_documents::FileOperation;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("illegal task transition: {0}")]
    Transition(#[from] commonspace_core::TransitionError),
}

type Result<T> = std::result::Result<T, StorageError>;

/// A workspace row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub name: String,
    pub roots: Vec<PathBuf>,
}

/// A conversation row.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRecord {
    pub id: ConversationId,
    pub workspace_id: Option<WorkspaceId>,
    pub title: String,
    pub updated_at: String,
}

/// A message row.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRecord {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub role: MessageRole,
    pub content: String,
    pub created_at: String,
}

/// A task row.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRecord {
    pub id: TaskId,
    pub conversation_id: ConversationId,
    pub workspace_id: Option<WorkspaceId>,
    pub provider: ProviderId,
    pub state: TaskState,
    pub prompt: String,
    pub plan: Option<TaskPlan>,
    pub summary: Option<String>,
}

/// A provider session row (for resume).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub task_id: TaskId,
    pub provider: ProviderId,
    pub provider_session_id: Option<String>,
    pub resumable: bool,
}

/// The storage handle.
pub struct Storage {
    conn: Mutex<Connection>,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn provider_str(p: ProviderId) -> Result<String> {
    Ok(serde_json::to_value(p)?
        .as_str()
        .map(str::to_owned)
        .unwrap_or_default())
}

fn provider_from(s: &str) -> Result<ProviderId> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_owned(),
    ))?)
}

fn state_str(s: TaskState) -> Result<String> {
    Ok(serde_json::to_value(s)?
        .as_str()
        .map(str::to_owned)
        .unwrap_or_default())
}

fn state_from(s: &str) -> Result<TaskState> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_owned(),
    ))?)
}

impl Storage {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// In-memory database for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate_to_latest(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&conn)
    }

    // ---- workspaces ----

    pub fn create_workspace(&self, name: &str, roots: &[PathBuf]) -> Result<WorkspaceRecord> {
        let id = WorkspaceId::generate();
        self.with(|c| {
            c.execute(
                "INSERT INTO workspaces (id, name, created_at) VALUES (?1, ?2, ?3)",
                params![id.0, name, now()],
            )?;
            for root in roots {
                c.execute(
                    "INSERT INTO authorized_roots (workspace_id, path, granted_at)
                     VALUES (?1, ?2, ?3)",
                    params![id.0, root.to_string_lossy(), now()],
                )?;
            }
            Ok(())
        })?;
        Ok(WorkspaceRecord {
            id,
            name: name.to_owned(),
            roots: roots.to_vec(),
        })
    }

    pub fn add_authorized_root(&self, workspace: &WorkspaceId, root: &Path) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO authorized_roots (workspace_id, path, granted_at)
                 VALUES (?1, ?2, ?3)",
                params![workspace.0, root.to_string_lossy(), now()],
            )?;
            Ok(())
        })
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRecord>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT id, name FROM workspaces ORDER BY created_at DESC")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut out = Vec::with_capacity(rows.len());
            for (id, name) in rows {
                let mut stmt = c.prepare(
                    "SELECT path FROM authorized_roots WHERE workspace_id = ?1 ORDER BY id",
                )?;
                let roots = stmt
                    .query_map([&id], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(PathBuf::from)
                    .collect();
                out.push(WorkspaceRecord {
                    id: WorkspaceId(id),
                    name,
                    roots,
                });
            }
            Ok(out)
        })
    }

    pub fn workspace_roots(&self, workspace: &WorkspaceId) -> Result<Vec<PathBuf>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT path FROM authorized_roots WHERE workspace_id = ?1 ORDER BY id")?;
            let roots = stmt
                .query_map([&workspace.0], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            Ok(roots.into_iter().map(PathBuf::from).collect())
        })
    }

    // ---- conversations and messages ----

    pub fn create_conversation(
        &self,
        workspace: Option<&WorkspaceId>,
        title: &str,
    ) -> Result<ConversationRecord> {
        let id = ConversationId::generate();
        let ts = now();
        self.with(|c| {
            c.execute(
                "INSERT INTO conversations (id, workspace_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id.0, workspace.map(|w| &w.0), title, ts],
            )?;
            Ok(())
        })?;
        Ok(ConversationRecord {
            id,
            workspace_id: workspace.cloned(),
            title: title.to_owned(),
            updated_at: ts,
        })
    }

    pub fn list_conversations(&self, limit: usize) -> Result<Vec<ConversationRecord>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, workspace_id, title, updated_at FROM conversations
                 ORDER BY updated_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit as i64], |r| {
                Ok(ConversationRecord {
                    id: ConversationId(r.get(0)?),
                    workspace_id: r.get::<_, Option<String>>(1)?.map(WorkspaceId),
                    title: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn append_message(
        &self,
        conversation: &ConversationId,
        role: MessageRole,
        content: &str,
    ) -> Result<MessageRecord> {
        let id = MessageId::generate();
        let ts = now();
        let role_str = if role == MessageRole::User {
            "user"
        } else {
            "assistant"
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO messages (id, conversation_id, role, content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id.0, conversation.0, role_str, content, ts],
            )?;
            c.execute(
                "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
                params![ts, conversation.0],
            )?;
            Ok(())
        })?;
        Ok(MessageRecord {
            id,
            conversation_id: conversation.clone(),
            role,
            content: content.to_owned(),
            created_at: ts,
        })
    }

    pub fn list_messages(&self, conversation: &ConversationId) -> Result<Vec<MessageRecord>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, role, content, created_at FROM messages
                 WHERE conversation_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([&conversation.0], |r| {
                let role: String = r.get(1)?;
                Ok(MessageRecord {
                    id: MessageId(r.get(0)?),
                    conversation_id: conversation.clone(),
                    role: if role == "user" {
                        MessageRole::User
                    } else {
                        MessageRole::Assistant
                    },
                    content: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // ---- tasks ----

    pub fn create_task(
        &self,
        conversation: &ConversationId,
        workspace: Option<&WorkspaceId>,
        provider: ProviderId,
        prompt: &str,
    ) -> Result<TaskRecord> {
        let id = TaskId::generate();
        let ts = now();
        let provider_s = provider_str(provider)?;
        let state_s = state_str(TaskState::Draft)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO tasks
                 (id, conversation_id, workspace_id, provider, state, prompt,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id.0,
                    conversation.0,
                    workspace.map(|w| &w.0),
                    provider_s,
                    state_s,
                    prompt,
                    ts
                ],
            )?;
            Ok(())
        })?;
        Ok(TaskRecord {
            id,
            conversation_id: conversation.clone(),
            workspace_id: workspace.cloned(),
            provider,
            state: TaskState::Draft,
            prompt: prompt.to_owned(),
            plan: None,
            summary: None,
        })
    }

    pub fn get_task(&self, task: &TaskId) -> Result<TaskRecord> {
        self.with(|c| {
            c.query_row(
                "SELECT id, conversation_id, workspace_id, provider, state, prompt,
                        plan_json, summary
                 FROM tasks WHERE id = ?1",
                [&task.0],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("task {}", task.0)))
            .and_then(|(id, conv, ws, provider, state, prompt, plan, summary)| {
                Ok(TaskRecord {
                    id: TaskId(id),
                    conversation_id: ConversationId(conv),
                    workspace_id: ws.map(WorkspaceId),
                    provider: provider_from(&provider)?,
                    state: state_from(&state)?,
                    prompt,
                    plan: plan.as_deref().map(serde_json::from_str).transpose()?,
                    summary,
                })
            })
        })
    }

    /// Transition a task's state, enforcing the state machine.
    pub fn transition_task(&self, task: &TaskId, next: TaskState) -> Result<TaskRecord> {
        let record = self.get_task(task)?;
        let new_state = record.state.transition_to(next)?;
        let state_s = state_str(new_state)?;
        self.with(|c| {
            c.execute(
                "UPDATE tasks SET state = ?1, updated_at = ?2 WHERE id = ?3",
                params![state_s, now(), task.0],
            )?;
            Ok(())
        })?;
        Ok(TaskRecord {
            state: new_state,
            ..record
        })
    }

    pub fn set_task_plan(&self, task: &TaskId, plan: &TaskPlan) -> Result<()> {
        let json = serde_json::to_string(plan)?;
        self.with(|c| {
            c.execute(
                "UPDATE tasks SET plan_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![json, now(), task.0],
            )?;
            Ok(())
        })
    }

    pub fn set_task_summary(&self, task: &TaskId, summary: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE tasks SET summary = ?1, updated_at = ?2 WHERE id = ?3",
                params![summary, now(), task.0],
            )?;
            Ok(())
        })
    }

    /// Tasks left in a live state by a previous process (crash recovery).
    pub fn stale_running_tasks(&self) -> Result<Vec<TaskId>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM tasks
                 WHERE state IN ('planning','awaiting_approval','running','paused')",
            )?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows.into_iter().map(TaskId).collect())
        })
    }

    /// Force a task to Failed with an explanation, bypassing transition rules
    /// (used only by crash recovery, where the in-memory machine is gone).
    pub fn fail_task_for_recovery(&self, task: &TaskId, reason: &str) -> Result<()> {
        let error = serde_json::json!({
            "code": "interrupted",
            "message": reason,
            "transient": false,
        });
        self.with(|c| {
            c.execute(
                "UPDATE tasks SET state = 'failed', error_json = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![error.to_string(), now(), task.0],
            )?;
            Ok(())
        })
    }

    // ---- task events ----

    pub fn append_event(&self, task: &TaskId, event: &AgentEvent) -> Result<i64> {
        let json = serde_json::to_string(event)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO task_events (task_id, event_json, created_at) VALUES (?1, ?2, ?3)",
                params![task.0, json, now()],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Events after `after_seq` (0 for all), for replay on reconnect.
    pub fn events_since(&self, task: &TaskId, after_seq: i64) -> Result<Vec<(i64, AgentEvent)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, event_json FROM task_events
                 WHERE task_id = ?1 AND seq > ?2 ORDER BY seq",
            )?;
            let rows = stmt
                .query_map(params![task.0, after_seq], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(seq, json)| Ok((seq, serde_json::from_str(&json)?)))
                .collect()
        })
    }

    // ---- provider sessions ----

    pub fn record_session(
        &self,
        task: &TaskId,
        provider: ProviderId,
        provider_session_id: Option<&str>,
        resumable: bool,
    ) -> Result<SessionRecord> {
        let id = SessionId::generate();
        let provider_s = provider_str(provider)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO provider_sessions
                 (id, task_id, provider, provider_session_id, resumable, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.0,
                    task.0,
                    provider_s,
                    provider_session_id,
                    resumable,
                    now()
                ],
            )?;
            Ok(())
        })?;
        Ok(SessionRecord {
            id,
            task_id: task.clone(),
            provider,
            provider_session_id: provider_session_id.map(str::to_owned),
            resumable,
        })
    }

    pub fn latest_session_for_task(&self, task: &TaskId) -> Result<Option<SessionRecord>> {
        self.with(|c| {
            c.query_row(
                "SELECT id, provider, provider_session_id, resumable
                 FROM provider_sessions WHERE task_id = ?1
                 ORDER BY created_at DESC LIMIT 1",
                [&task.0],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?
            .map(|(id, provider, psid, resumable)| {
                Ok(SessionRecord {
                    id: SessionId(id),
                    task_id: task.clone(),
                    provider: provider_from(&provider)?,
                    provider_session_id: psid,
                    resumable,
                })
            })
            .transpose()
        })
    }

    // ---- artifacts ----

    pub fn record_artifact(&self, artifact: &Artifact) -> Result<()> {
        let kind = serde_json::to_value(artifact.kind)?
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO artifacts
                 (id, task_id, kind, path, name, modified_existing, backup_path,
                  file_operation_id, change_summary, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    artifact.id.0,
                    artifact.task_id.0,
                    kind,
                    artifact.path.to_string_lossy(),
                    artifact.name,
                    artifact.modified_existing,
                    artifact
                        .backup_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    artifact.file_operation_id.as_ref().map(|i| &i.0),
                    artifact.change_summary,
                    artifact.created_at.to_rfc3339(),
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_artifacts(&self, task: &TaskId) -> Result<Vec<Artifact>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, kind, path, name, modified_existing, backup_path,
                        file_operation_id, change_summary, created_at
                 FROM artifacts WHERE task_id = ?1 ORDER BY created_at",
            )?;
            let rows = stmt
                .query_map([&task.0], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, bool>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(
                    |(id, kind, path, name, modified, backup, fop, summary, created)| {
                        let kind: ArtifactKind =
                            serde_json::from_value(serde_json::Value::String(kind))?;
                        Ok(Artifact {
                            id: commonspace_core::ArtifactId(id),
                            task_id: task.clone(),
                            kind,
                            path: PathBuf::from(path),
                            name,
                            modified_existing: modified,
                            backup_path: backup.map(PathBuf::from),
                            file_operation_id: fop.map(commonspace_core::FileOperationId),
                            change_summary: summary,
                            created_at: chrono::DateTime::parse_from_rfc3339(&created)
                                .map(|t| t.with_timezone(&Utc))
                                .unwrap_or_else(|_| Utc::now()),
                        })
                    },
                )
                .collect()
        })
    }

    // ---- file operations (undo journal) ----

    pub fn record_file_operation(&self, task: Option<&TaskId>, op: &FileOperation) -> Result<()> {
        let json = serde_json::to_string(op)?;
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO file_operations
                 (id, task_id, op_json, undone, performed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    op.id.0,
                    task.map(|t| &t.0),
                    json,
                    op.undone,
                    op.performed_at.to_rfc3339()
                ],
            )?;
            Ok(())
        })
    }

    pub fn file_operations_for_task(&self, task: &TaskId) -> Result<Vec<FileOperation>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT op_json FROM file_operations WHERE task_id = ?1 ORDER BY performed_at",
            )?;
            let rows = stmt
                .query_map([&task.0], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|json| Ok(serde_json::from_str(&json)?))
                .collect()
        })
    }

    pub fn get_file_operation(&self, id: &str) -> Result<FileOperation> {
        self.with(|c| {
            c.query_row(
                "SELECT op_json FROM file_operations WHERE id = ?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("file operation {id}")))
            .and_then(|json| Ok(serde_json::from_str(&json)?))
        })
    }

    // ---- permission audit ----

    #[allow(clippy::too_many_arguments)]
    pub fn record_permission_decision(
        &self,
        request_id: &str,
        task: &TaskId,
        operation: &str,
        paths: &[PathBuf],
        verdict: &str,
        decision: Option<&str>,
        scope: Option<&str>,
    ) -> Result<()> {
        let paths_json = serde_json::to_string(
            &paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )?;
        self.with(|c| {
            c.execute(
                "INSERT INTO permission_decisions
                 (request_id, task_id, operation, paths_json, verdict, decision, scope, decided_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    request_id,
                    task.0,
                    operation,
                    paths_json,
                    verdict,
                    decision,
                    scope,
                    now()
                ],
            )?;
            Ok(())
        })
    }

    // ---- settings ----

    pub fn get_setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.with(|c| {
            c.query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                [key],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
        })
    }

    pub fn set_setting<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                params![key, json],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonspace_core::PlanStep;

    fn storage() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    #[test]
    fn open_migrate_reopen_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("app").join("commonspace.db");
        {
            let s = Storage::open(&db).unwrap();
            s.create_workspace("Test", &[PathBuf::from("C:/tmp/ws")])
                .unwrap();
        }
        // Reopen: migrations are idempotent, data persists.
        let s = Storage::open(&db).unwrap();
        let ws = s.list_workspaces().unwrap();
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].name, "Test");
        assert_eq!(ws[0].roots.len(), 1);
    }

    #[test]
    fn conversation_message_flow() {
        let s = storage();
        let conv = s.create_conversation(None, "Organize downloads").unwrap();
        s.append_message(&conv.id, MessageRole::User, "please organize")
            .unwrap();
        s.append_message(&conv.id, MessageRole::Assistant, "on it")
            .unwrap();
        let msgs = s.list_messages(&conv.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[1].content, "on it");
        assert_eq!(s.list_conversations(10).unwrap().len(), 1);
    }

    #[test]
    fn task_lifecycle_enforces_state_machine() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "organize files")
            .unwrap();
        assert_eq!(task.state, TaskState::Draft);

        s.transition_task(&task.id, TaskState::Planning).unwrap();
        s.transition_task(&task.id, TaskState::AwaitingApproval)
            .unwrap();
        s.transition_task(&task.id, TaskState::Running).unwrap();
        let done = s.transition_task(&task.id, TaskState::Completed).unwrap();
        assert_eq!(done.state, TaskState::Completed);

        // Illegal: completed -> running
        let err = s.transition_task(&task.id, TaskState::Running).unwrap_err();
        assert!(matches!(err, StorageError::Transition(_)));
    }

    #[test]
    fn plan_round_trips() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::CodexCli, "p")
            .unwrap();
        let mut plan = TaskPlan::empty();
        plan.steps.push(PlanStep {
            title: "Read 12 documents".into(),
            detail: None,
        });
        plan.requires_approval = true;
        s.set_task_plan(&task.id, &plan).unwrap();
        let loaded = s.get_task(&task.id).unwrap();
        assert_eq!(loaded.plan.unwrap(), plan);
    }

    #[test]
    fn events_append_and_replay_in_order() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        for i in 0..5 {
            s.append_event(
                &task.id,
                &AgentEvent::Warning {
                    message: format!("w{i}"),
                },
            )
            .unwrap();
        }
        let all = s.events_since(&task.id, 0).unwrap();
        assert_eq!(all.len(), 5);
        let after = s.events_since(&task.id, all[2].0).unwrap();
        assert_eq!(after.len(), 2);
        match &after[0].1 {
            AgentEvent::Warning { message } => assert_eq!(message, "w3"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sessions_recorded_for_resume() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        assert!(s.latest_session_for_task(&task.id).unwrap().is_none());
        s.record_session(&task.id, ProviderId::ClaudeCode, Some("abc-123"), true)
            .unwrap();
        let latest = s.latest_session_for_task(&task.id).unwrap().unwrap();
        assert_eq!(latest.provider_session_id.as_deref(), Some("abc-123"));
        assert!(latest.resumable);
    }

    #[test]
    fn crash_recovery_finds_stale_tasks() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        s.transition_task(&task.id, TaskState::Planning).unwrap();
        s.transition_task(&task.id, TaskState::Running).unwrap();

        let stale = s.stale_running_tasks().unwrap();
        assert_eq!(stale, vec![task.id.clone()]);
        s.fail_task_for_recovery(&task.id, "Commonspace was closed during this task")
            .unwrap();
        assert!(s.stale_running_tasks().unwrap().is_empty());
        assert_eq!(s.get_task(&task.id).unwrap().state, TaskState::Failed);
    }

    #[test]
    fn file_operations_round_trip() {
        use commonspace_documents::{FileOpKind, FileOperation};
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        let mut op = FileOperation::new(FileOpKind::Modify, PathBuf::from("C:/ws/a.txt"));
        op.backup = Some(PathBuf::from("C:/backups/a.txt"));
        op.hash_before = Some("aa".into());
        op.hash_after = Some("bb".into());
        s.record_file_operation(Some(&task.id), &op).unwrap();
        let ops = s.file_operations_for_task(&task.id).unwrap();
        assert_eq!(ops, vec![op.clone()]);
        assert_eq!(s.get_file_operation(op.id.as_ref()).unwrap(), op);
    }

    #[test]
    fn settings_round_trip() {
        let s = storage();
        s.set_setting("theme", &"dark".to_string()).unwrap();
        assert_eq!(s.get_setting::<String>("theme").unwrap().unwrap(), "dark");
        assert!(s.get_setting::<String>("missing").unwrap().is_none());
        s.set_setting("theme", &"light".to_string()).unwrap();
        assert_eq!(s.get_setting::<String>("theme").unwrap().unwrap(), "light");
    }
}
