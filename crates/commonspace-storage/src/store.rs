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
    #[error("the database failed its integrity check: {0}")]
    Corrupt(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
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

/// One full-text search result. `kind` is `"conversation"` for a title match
/// (one row per conversation) or `"message"` for a content match (one row per
/// matching message, carrying its parent conversation's title).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SearchHit {
    pub kind: String,
    pub conversation_id: String,
    pub title: String,
    pub snippet: String,
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

/// A task row shaped for conversation replay in the UI. Field names and the
/// snake_case `state` strings serialize exactly as the frontend's
/// `taskInfoSchema` expects, so this struct crosses the IPC boundary as-is.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TaskRow {
    pub id: String,
    pub conversation_id: String,
    pub provider: String,
    pub state: String,
    pub summary: Option<String>,
    /// The human `message` pulled out of the stored error, when one exists.
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The provider session a follow-up message can continue: the newest task in
/// the conversation that left a resumable session id behind. Serializes to
/// the frontend's `resumableSessionSchema`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResumableSession {
    pub provider: String,
    pub provider_session_id: String,
}

/// Whether an attachment is a single file or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    File,
    Folder,
}

impl AttachmentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }
}

/// Metadata for one attachment as collected at send time; `record_attachments`
/// assigns the id and timestamp. Metadata only — file contents never enter
/// the database.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAttachment {
    pub path: String,
    pub kind: AttachmentKind,
    pub size_bytes: Option<i64>,
    pub modified_at: Option<String>,
    pub content_hash: Option<String>,
    pub in_workspace: bool,
}

/// A persisted attachment row. Serializes to the frontend's
/// `attachmentInfoSchema` — note `in_workspace` crosses as a real boolean,
/// not the 0/1 SQLite stores.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AttachmentRecord {
    pub id: String,
    pub conversation_id: String,
    pub task_id: Option<String>,
    pub path: String,
    pub kind: AttachmentKind,
    pub size_bytes: Option<i64>,
    pub modified_at: Option<String>,
    pub content_hash: Option<String>,
    pub in_workspace: bool,
    pub created_at: String,
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

/// Turn raw user input into an FTS5 MATCH expression that can never be a
/// syntax error. Every whitespace-separated term becomes a quoted phrase
/// (embedded quotes doubled, per FTS5 string rules), so operator syntax like
/// `NEAR(`, `*`, or an unbalanced quote reaches the index as literal text to
/// tokenize rather than as query grammar. Quoted phrases joined by spaces are
/// an implicit AND. Returns an empty string for whitespace-only input.
fn fts_match_expression(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pull the human `message` out of a stored `error_json` blob (a serialized
/// `AgentErrorInfo`). Parsed leniently on purpose: a blob written by a
/// different build — or a corrupted row — must degrade to "no message", never
/// fail the whole task listing.
fn error_message_from_json(error_json: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(error_json?).ok()?;
    value.get("message")?.as_str().map(str::to_owned)
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
        // WAL keeps readers unblocked while task-event writes stream in, and
        // its append-only journal survives crashes better than rollback mode.
        // synchronous=NORMAL is the documented safe pairing with WAL: it can
        // never corrupt the database, only lose the last few commits after an
        // OS-level crash, and it avoids an fsync on every transaction.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Surface on-disk corruption at open, where a descriptive error is
        // possible, instead of proceeding and failing confusingly mid-write.
        // quick_check returns the single row "ok" on a healthy database and
        // one description per problem otherwise; the first row is enough to
        // tell the difference and name the fault.
        let verdict: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if verdict != "ok" {
            return Err(StorageError::Corrupt(verdict));
        }
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

    /// Rename a conversation. The title is trimmed, must be non-empty, and is
    /// capped with a visible ellipsis (the same convention `title_from_prompt`
    /// uses for auto-generated titles) rather than silently losing text. The
    /// v2 FTS triggers keep the search index in step with the update.
    pub fn rename_conversation(&self, id: &ConversationId, title: &str) -> Result<()> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err(StorageError::InvalidInput(
                "a conversation title cannot be empty".into(),
            ));
        }
        let mut capped: String = trimmed.chars().take(120).collect();
        if trimmed.chars().count() > 120 {
            capped.push('…');
        }
        self.with(|c| {
            let changed = c.execute(
                "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![capped, now(), id.0],
            )?;
            if changed == 0 {
                return Err(StorageError::NotFound(format!("conversation {}", id.0)));
            }
            Ok(())
        })
    }

    /// Full-text search over conversation titles and message contents.
    /// Results are ranked by FTS5 relevance; snippets are plain text (the UI
    /// renders them as text, so no highlight markers are embedded).
    pub fn search_history(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let expression = fts_match_expression(query);
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        self.with(|c| {
            // Message hits join their parent conversation for its title; the
            // join key works for conversation hits too, since their
            // conversation_id is their own id. The snippet is taken from the
            // column the row kind actually indexes (title for conversations,
            // body for messages) — the other column is always empty.
            let mut stmt = c.prepare(
                "SELECT f.kind,
                        f.conversation_id,
                        COALESCE(conv.title, ''),
                        CASE WHEN f.kind = 'conversation'
                             THEN snippet(history_fts, 0, '', '', '…', 12)
                             ELSE snippet(history_fts, 1, '', '', '…', 12)
                        END,
                        f.created_at
                 FROM history_fts AS f
                 LEFT JOIN conversations AS conv ON conv.id = f.conversation_id
                 WHERE history_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![expression, limit as i64], |r| {
                Ok(SearchHit {
                    kind: r.get(0)?,
                    conversation_id: r.get(1)?,
                    title: r.get(2)?,
                    snippet: r.get(3)?,
                    created_at: r.get(4)?,
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

    /// All tasks in a conversation, oldest first, shaped for replay. The
    /// stored `state` strings already match the frontend's enum, so they pass
    /// through untranslated; the stored error blob is reduced to its human
    /// message here so the frontend never parses provider error JSON.
    pub fn list_tasks(&self, conversation: &ConversationId) -> Result<Vec<TaskRow>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, conversation_id, provider, state, summary, error_json,
                        created_at, updated_at
                 FROM tasks WHERE conversation_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([&conversation.0], |r| {
                Ok(TaskRow {
                    id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    provider: r.get(2)?,
                    state: r.get(3)?,
                    summary: r.get(4)?,
                    error_message: error_message_from_json(
                        r.get::<_, Option<String>>(5)?.as_deref(),
                    ),
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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

    /// The session a follow-up message in this conversation can continue:
    /// the newest task (by creation time) that recorded a session with a
    /// provider session id and the resumable capability, and within that task
    /// the newest such row (the orchestrator records a placeholder row
    /// without an id at start and the real id at completion). None when no
    /// task left anything to continue.
    pub fn resumable_session(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<ResumableSession>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT s.provider, s.provider_session_id
                 FROM provider_sessions AS s
                 JOIN tasks AS t ON t.id = s.task_id
                 WHERE t.conversation_id = ?1
                   AND s.provider_session_id IS NOT NULL
                   AND s.resumable = 1
                 ORDER BY t.created_at DESC, t.id DESC, s.created_at DESC, s.rowid DESC
                 LIMIT 1",
                [&conversation.0],
                |r| {
                    Ok(ResumableSession {
                        provider: r.get(0)?,
                        provider_session_id: r.get(1)?,
                    })
                },
            )
            .optional()?)
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

    /// Ids of a task's journaled operations, newest first — the order a
    /// whole-task undo must apply them in, so later changes are reverted
    /// before the earlier changes they may build on.
    pub fn task_file_operation_ids_newest_first(&self, task: &TaskId) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM file_operations WHERE task_id = ?1
                 ORDER BY performed_at DESC, rowid DESC",
            )?;
            let rows = stmt
                .query_map([&task.0], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    // ---- attachments ----

    /// Persist attachment metadata rows for a message the user just sent.
    /// Ids and the shared timestamp are assigned here.
    pub fn record_attachments(
        &self,
        conversation: &ConversationId,
        task: Option<&TaskId>,
        attachments: &[NewAttachment],
    ) -> Result<Vec<AttachmentRecord>> {
        let ts = now();
        self.with(|c| {
            let mut out = Vec::with_capacity(attachments.len());
            for attachment in attachments {
                let id = format!("att_{}", uuid::Uuid::new_v4().simple());
                c.execute(
                    "INSERT INTO attachments
                     (id, conversation_id, task_id, path, kind, size_bytes,
                      modified_at, content_hash, in_workspace, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id,
                        conversation.0,
                        task.map(|t| &t.0),
                        attachment.path,
                        attachment.kind.as_str(),
                        attachment.size_bytes,
                        attachment.modified_at,
                        attachment.content_hash,
                        attachment.in_workspace,
                        ts
                    ],
                )?;
                out.push(AttachmentRecord {
                    id,
                    conversation_id: conversation.0.clone(),
                    task_id: task.map(|t| t.0.clone()),
                    path: attachment.path.clone(),
                    kind: attachment.kind,
                    size_bytes: attachment.size_bytes,
                    modified_at: attachment.modified_at.clone(),
                    content_hash: attachment.content_hash.clone(),
                    in_workspace: attachment.in_workspace,
                    created_at: ts.clone(),
                });
            }
            Ok(out)
        })
    }

    /// Every attachment ever recorded for a conversation, oldest first.
    /// A batch shares one timestamp, so rowid keeps insertion order within it.
    pub fn list_conversation_attachments(
        &self,
        conversation: &ConversationId,
    ) -> Result<Vec<AttachmentRecord>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, task_id, path, kind, size_bytes, modified_at,
                        content_hash, in_workspace, created_at
                 FROM attachments WHERE conversation_id = ?1
                 ORDER BY created_at, rowid",
            )?;
            let rows = stmt.query_map([&conversation.0], |r| {
                let kind: String = r.get(3)?;
                Ok(AttachmentRecord {
                    id: r.get(0)?,
                    conversation_id: conversation.0.clone(),
                    task_id: r.get(1)?,
                    path: r.get(2)?,
                    // The CHECK constraint admits exactly these two values.
                    kind: if kind == "folder" {
                        AttachmentKind::Folder
                    } else {
                        AttachmentKind::File
                    },
                    size_bytes: r.get(4)?,
                    modified_at: r.get(5)?,
                    content_hash: r.get(6)?,
                    in_workspace: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    // ---- full-text search ----

    #[test]
    fn fts5_is_available_in_bundled_sqlite() {
        // The v2 migration depends on the bundled build shipping FTS5
        // (libsqlite3-sys compiles with -DSQLITE_ENABLE_FTS5); prove it.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(x)")
            .unwrap();
    }

    #[test]
    fn search_finds_message_content() {
        let s = storage();
        let conv = s.create_conversation(None, "Trip planning").unwrap();
        let other = s.create_conversation(None, "Groceries").unwrap();
        s.append_message(
            &conv.id,
            MessageRole::User,
            "book the flamingo hotel near the beach",
        )
        .unwrap();
        s.append_message(&other.id, MessageRole::User, "buy oat milk")
            .unwrap();

        let hits = s.search_history("flamingo", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "message");
        assert_eq!(hits[0].conversation_id, conv.id.0);
        assert_eq!(hits[0].title, "Trip planning");
        assert!(hits[0].snippet.contains("flamingo"));
    }

    #[test]
    fn search_finds_conversation_by_title() {
        let s = storage();
        let conv = s
            .create_conversation(None, "Quarterly budget review")
            .unwrap();
        s.append_message(&conv.id, MessageRole::User, "let's get started")
            .unwrap();

        let hits = s.search_history("quarterly", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "conversation");
        assert_eq!(hits[0].conversation_id, conv.id.0);
        assert_eq!(hits[0].title, "Quarterly budget review");
    }

    #[test]
    fn rename_updates_title_and_search_index() {
        let s = storage();
        let conv = s
            .create_conversation(None, "Zebra migration notes")
            .unwrap();
        s.rename_conversation(&conv.id, "  Aardvark habitat notes  ")
            .unwrap();

        // The stored title is trimmed.
        let listed = s.list_conversations(10).unwrap();
        assert_eq!(listed[0].title, "Aardvark habitat notes");

        // The FTS triggers replaced the old index row: the new title is
        // findable and the old one is gone.
        let hits = s.search_history("aardvark", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "conversation");
        assert_eq!(hits[0].conversation_id, conv.id.0);
        assert!(s.search_history("zebra", 10).unwrap().is_empty());
    }

    #[test]
    fn rename_rejects_empty_caps_length_and_reports_missing() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();

        assert!(matches!(
            s.rename_conversation(&conv.id, "   "),
            Err(StorageError::InvalidInput(_))
        ));

        let long = "x".repeat(200);
        s.rename_conversation(&conv.id, &long).unwrap();
        let title = s.list_conversations(1).unwrap()[0].title.clone();
        assert_eq!(title.chars().count(), 121);
        assert!(title.ends_with('…'));

        assert!(matches!(
            s.rename_conversation(&ConversationId("conv_missing".into()), "x"),
            Err(StorageError::NotFound(_))
        ));
    }

    #[test]
    fn hostile_search_queries_return_ok() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        s.append_message(&conv.id, MessageRole::User, "hello world")
            .unwrap();
        for hostile in [
            "unbalanced ( NEAR",
            "\"",
            "((((",
            "NEAR(hello, world)",
            "hello* -world",
            "term\" OR \"",
            "col:value ^caret",
        ] {
            assert!(
                s.search_history(hostile, 10).is_ok(),
                "query {hostile:?} must not be an FTS/SQL syntax error"
            );
        }
        // Whitespace-only queries do not even reach the index.
        assert!(s.search_history("", 10).unwrap().is_empty());
        assert!(s.search_history("   \t  ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        for i in 0..10 {
            s.append_message(&conv.id, MessageRole::User, &format!("cactus number {i}"))
                .unwrap();
        }
        assert_eq!(s.search_history("cactus", 3).unwrap().len(), 3);
        assert_eq!(s.search_history("cactus", 100).unwrap().len(), 10);
    }

    // ---- tasks for replay ----

    /// Pin a task's timestamps directly. `create_task` stamps "now", which
    /// can collide within a fast test; ordering assertions need distinct,
    /// known values.
    fn set_task_created_at(s: &Storage, task: &TaskId, ts: &str) {
        let conn = s.conn.lock().unwrap();
        conn.execute(
            "UPDATE tasks SET created_at = ?1 WHERE id = ?2",
            params![ts, task.0],
        )
        .unwrap();
    }

    #[test]
    fn list_tasks_orders_by_creation_and_extracts_error_messages() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let ok_task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "first")
            .unwrap();
        let failed_task = s
            .create_task(&conv.id, None, ProviderId::CodexCli, "second")
            .unwrap();
        let garbage_task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "third")
            .unwrap();
        set_task_created_at(&s, &ok_task.id, "2026-01-01T00:00:01Z");
        set_task_created_at(&s, &failed_task.id, "2026-01-01T00:00:02Z");
        set_task_created_at(&s, &garbage_task.id, "2026-01-01T00:00:03Z");

        s.set_task_summary(&ok_task.id, "Organized 4 files")
            .unwrap();

        // A real error_json fixture in the exact shape AgentErrorInfo
        // serializes to (fail_task_for_recovery writes the same shape).
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET error_json = ?1 WHERE id = ?2",
                params![
                    r#"{"code":"provider_unavailable","message":"Claude Code stopped responding.","recovery":"Try sending the message again.","transient":true}"#,
                    failed_task.id.0
                ],
            )
            .unwrap();
            // Garbage must degrade to a null message, not fail the listing.
            conn.execute(
                "UPDATE tasks SET error_json = 'not json at all' WHERE id = ?1",
                [&garbage_task.id.0],
            )
            .unwrap();
        }

        let rows = s.list_tasks(&conv.id).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, ok_task.id.0);
        assert_eq!(rows[0].conversation_id, conv.id.0);
        assert_eq!(rows[0].provider, "claude_code");
        assert_eq!(rows[0].state, "draft");
        assert_eq!(rows[0].summary.as_deref(), Some("Organized 4 files"));
        assert_eq!(rows[0].error_message, None);

        assert_eq!(rows[1].id, failed_task.id.0);
        assert_eq!(
            rows[1].error_message.as_deref(),
            Some("Claude Code stopped responding.")
        );

        assert_eq!(rows[2].id, garbage_task.id.0);
        assert_eq!(rows[2].error_message, None);
    }

    #[test]
    fn list_tasks_reads_the_recovery_error_shape() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        s.transition_task(&task.id, TaskState::Planning).unwrap();
        s.transition_task(&task.id, TaskState::Running).unwrap();
        s.fail_task_for_recovery(&task.id, "Commonspace was closed during this task")
            .unwrap();

        let rows = s.list_tasks(&conv.id).unwrap();
        assert_eq!(rows[0].state, "failed");
        assert_eq!(
            rows[0].error_message.as_deref(),
            Some("Commonspace was closed during this task")
        );
    }

    #[test]
    fn task_rows_serialize_to_the_frontend_contract() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        s.create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        let rows = s.list_tasks(&conv.id).unwrap();
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(json["state"], "draft");
        assert_eq!(json["provider"], "claude_code");
        assert!(json["summary"].is_null());
        assert!(json["error_message"].is_null());
        assert!(json["created_at"].is_string());
    }

    // ---- resumable sessions ----

    #[test]
    fn resumable_session_picks_the_newest_task_that_has_one() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let older = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "one")
            .unwrap();
        let newer = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "two")
            .unwrap();
        set_task_created_at(&s, &older.id, "2026-01-01T00:00:01Z");
        set_task_created_at(&s, &newer.id, "2026-01-01T00:00:02Z");

        // Both tasks got a session id; the newer task's wins.
        s.record_session(&older.id, ProviderId::ClaudeCode, Some("sid-old"), true)
            .unwrap();
        // The orchestrator records a placeholder without an id at task start
        // and the real id at completion; both rows exist in real data.
        s.record_session(&newer.id, ProviderId::ClaudeCode, None, true)
            .unwrap();
        s.record_session(&newer.id, ProviderId::ClaudeCode, Some("sid-new"), true)
            .unwrap();

        let resume = s.resumable_session(&conv.id).unwrap().unwrap();
        assert_eq!(resume.provider, "claude_code");
        assert_eq!(resume.provider_session_id, "sid-new");
    }

    #[test]
    fn resumable_session_falls_back_past_tasks_without_one() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let older = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "one")
            .unwrap();
        let newer = s
            .create_task(&conv.id, None, ProviderId::CodexCli, "two")
            .unwrap();
        set_task_created_at(&s, &older.id, "2026-01-01T00:00:01Z");
        set_task_created_at(&s, &newer.id, "2026-01-01T00:00:02Z");

        s.record_session(&older.id, ProviderId::ClaudeCode, Some("sid-old"), true)
            .unwrap();
        // The newest task never produced a session id (e.g. it crashed).
        s.record_session(&newer.id, ProviderId::CodexCli, None, true)
            .unwrap();

        let resume = s.resumable_session(&conv.id).unwrap().unwrap();
        assert_eq!(resume.provider_session_id, "sid-old");
    }

    #[test]
    fn resumable_session_is_none_when_absent_or_not_resumable() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        assert!(s.resumable_session(&conv.id).unwrap().is_none());

        // A session id from a provider that cannot resume is not offered.
        let task = s
            .create_task(&conv.id, None, ProviderId::CodexCli, "p")
            .unwrap();
        s.record_session(&task.id, ProviderId::CodexCli, Some("sid-x"), false)
            .unwrap();
        assert!(s.resumable_session(&conv.id).unwrap().is_none());

        // Sessions in other conversations never leak in.
        let other = s.create_conversation(None, "other").unwrap();
        let other_task = s
            .create_task(&other.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        s.record_session(&other_task.id, ProviderId::ClaudeCode, Some("sid-y"), true)
            .unwrap();
        assert!(s.resumable_session(&conv.id).unwrap().is_none());
    }

    // ---- file operation ids for whole-task undo ----

    #[test]
    fn file_operation_ids_come_back_newest_first() {
        use commonspace_documents::{FileOpKind, FileOperation};
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        let mut first = FileOperation::new(FileOpKind::Create, PathBuf::from("/ws/a.txt"));
        first.performed_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut second = FileOperation::new(FileOpKind::Create, PathBuf::from("/ws/b.txt"));
        second.performed_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:02Z")
            .unwrap()
            .with_timezone(&Utc);
        s.record_file_operation(Some(&task.id), &first).unwrap();
        s.record_file_operation(Some(&task.id), &second).unwrap();

        let ids = s.task_file_operation_ids_newest_first(&task.id).unwrap();
        assert_eq!(ids, vec![second.id.0.clone(), first.id.0.clone()]);
    }

    // ---- attachments ----

    #[test]
    fn attachments_round_trip_with_booleans_intact() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let task = s
            .create_task(&conv.id, None, ProviderId::ClaudeCode, "p")
            .unwrap();
        let file = NewAttachment {
            path: "/home/user/report.pdf".into(),
            kind: AttachmentKind::File,
            size_bytes: Some(12_345),
            modified_at: Some("2026-01-01T00:00:00+00:00".into()),
            content_hash: Some("abc123".into()),
            in_workspace: true,
        };
        let folder = NewAttachment {
            path: "/mnt/elsewhere/photos".into(),
            kind: AttachmentKind::Folder,
            size_bytes: None,
            modified_at: None,
            content_hash: None,
            in_workspace: false,
        };
        let recorded = s
            .record_attachments(&conv.id, Some(&task.id), &[file.clone(), folder.clone()])
            .unwrap();
        assert_eq!(recorded.len(), 2);
        assert!(recorded[0].id.starts_with("att_"));

        let listed = s.list_conversation_attachments(&conv.id).unwrap();
        assert_eq!(listed, recorded);
        assert_eq!(listed[0].kind, AttachmentKind::File);
        assert!(listed[0].in_workspace);
        assert_eq!(listed[0].task_id.as_deref(), Some(task.id.0.as_str()));
        assert_eq!(listed[1].kind, AttachmentKind::Folder);
        assert!(!listed[1].in_workspace);
        assert_eq!(listed[1].size_bytes, None);
        assert_eq!(listed[1].content_hash, None);

        // The serialized shape carries real booleans and the lowercase kind
        // strings the frontend's attachmentInfoSchema demands.
        let json = serde_json::to_value(&listed).unwrap();
        assert_eq!(json[0]["in_workspace"], serde_json::Value::Bool(true));
        assert_eq!(json[0]["kind"], "file");
        assert_eq!(json[1]["in_workspace"], serde_json::Value::Bool(false));
        assert_eq!(json[1]["kind"], "folder");
    }

    #[test]
    fn attachments_without_a_task_are_allowed() {
        let s = storage();
        let conv = s.create_conversation(None, "t").unwrap();
        let a = NewAttachment {
            path: "/home/user/notes.txt".into(),
            kind: AttachmentKind::File,
            size_bytes: None,
            modified_at: None,
            content_hash: None,
            in_workspace: false,
        };
        s.record_attachments(&conv.id, None, &[a]).unwrap();
        let listed = s.list_conversation_attachments(&conv.id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].task_id, None);
    }

    #[test]
    fn v2_database_upgrades_to_v3() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("commonspace.db");
        {
            // Freeze a populated database at schema v2 — the state installs
            // from before the attachments feature are in.
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            crate::migrations::migrate_to(&mut conn, 2).unwrap();
            conn.execute_batch(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES ('conv_1', 'Old conversation',
                         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
            let missing: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'attachments'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(missing, 0, "attachments must not exist at v2");
        }

        // Reopening applies v3; the new table works against the old rows.
        let s = Storage::open(&db).unwrap();
        let conv = ConversationId("conv_1".into());
        assert!(s.list_conversation_attachments(&conv).unwrap().is_empty());
        s.record_attachments(
            &conv,
            None,
            &[NewAttachment {
                path: "/home/user/a.txt".into(),
                kind: AttachmentKind::File,
                size_bytes: Some(1),
                modified_at: None,
                content_hash: None,
                in_workspace: true,
            }],
        )
        .unwrap();
        assert_eq!(s.list_conversation_attachments(&conv).unwrap().len(), 1);
    }

    #[test]
    fn v1_database_upgrades_to_v2_with_backfill() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("commonspace.db");
        {
            // Build a database frozen at schema v1 and populate it directly —
            // this is the state real pre-FTS installs are in.
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            crate::migrations::migrate_to(&mut conn, 1).unwrap();
            conn.execute_batch(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES ('conv_1', 'Legacy penguin notes',
                         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                 INSERT INTO messages (id, conversation_id, role, content, created_at)
                 VALUES ('msg_1', 'conv_1', 'user', 'remember the walrus password',
                         '2026-01-01T00:00:01Z');",
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 1);
        }

        // Reopening runs the v2 migration; its backfill must index the
        // pre-existing rows without any writes having gone through triggers.
        let s = Storage::open(&db).unwrap();
        let title_hits = s.search_history("penguin", 10).unwrap();
        assert_eq!(title_hits.len(), 1);
        assert_eq!(title_hits[0].kind, "conversation");
        assert_eq!(title_hits[0].conversation_id, "conv_1");

        let message_hits = s.search_history("walrus", 10).unwrap();
        assert_eq!(message_hits.len(), 1);
        assert_eq!(message_hits[0].kind, "message");
        assert_eq!(message_hits[0].conversation_id, "conv_1");
        assert_eq!(message_hits[0].title, "Legacy penguin notes");
        assert!(message_hits[0].snippet.contains("walrus"));
    }
}
