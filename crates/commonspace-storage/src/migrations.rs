//! Versioned schema migrations tracked via SQLite's native
//! `PRAGMA user_version`. Never edit an existing migration; append a new one.
//!
//! (Hand-rolled runner: `rusqlite_migration` currently requires a newer
//! rustc/libsqlite3-sys combination than this workspace pins; the mechanism
//! below is the same `user_version` approach it uses.)

use rusqlite::Connection;

/// Ordered migrations. Index 0 brings the schema to version 1, and so on.
const MIGRATIONS: &[&str] = &[V1_INITIAL];

/// Apply all pending migrations inside transactions.
pub fn migrate_to_latest(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as i64;
    if current > target {
        // A newer app version created this database; refuse to touch it.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "database schema version {current} is newer than this build supports ({target})"
            )),
        ));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as i64 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

/// V1: the full MVP schema.
///
/// Conventions: TEXT primary keys carry the prefixed ids from
/// `commonspace-core` (`task_…`, `conv_…`); timestamps are RFC 3339 UTC TEXT;
/// JSON columns are named `*_json` and hold serde-serialized core types, so
/// evolving those payloads doesn't require schema migrations.
const V1_INITIAL: &str = r#"
CREATE TABLE workspaces (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    settings_json TEXT NOT NULL DEFAULT '{}',
    created_at    TEXT NOT NULL
) STRICT;

CREATE TABLE authorized_roots (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    path         TEXT NOT NULL,
    granted_at   TEXT NOT NULL,
    UNIQUE(workspace_id, path)
) STRICT;

CREATE TABLE conversations (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
    title        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

CREATE TABLE messages (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content         TEXT NOT NULL,
    created_at      TEXT NOT NULL
) STRICT;
CREATE INDEX idx_messages_conversation ON messages(conversation_id, created_at);

CREATE TABLE tasks (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    workspace_id    TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
    provider        TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN (
        'draft','planning','awaiting_approval','running','paused',
        'completed','failed','cancelled','rolled_back')),
    prompt          TEXT NOT NULL,
    plan_json       TEXT,
    summary         TEXT,
    error_json      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;
CREATE INDEX idx_tasks_conversation ON tasks(conversation_id, created_at);
CREATE INDEX idx_tasks_state ON tasks(state);

-- Append-only replay log of normalized AgentEvents per task. `seq` gives a
-- total order for reconnect/replay.
CREATE TABLE task_events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    event_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX idx_task_events_task ON task_events(task_id, seq);

CREATE TABLE providers (
    id          TEXT PRIMARY KEY,
    enabled     INTEGER NOT NULL DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}'
) STRICT;

CREATE TABLE provider_sessions (
    id                  TEXT PRIMARY KEY,
    task_id             TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    provider            TEXT NOT NULL,
    provider_session_id TEXT,
    resumable           INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL
) STRICT;
CREATE INDEX idx_provider_sessions_task ON provider_sessions(task_id);

-- Standing permission grants (user chose "always for this workspace/task").
CREATE TABLE permission_grants (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    task_id      TEXT,
    operation    TEXT NOT NULL,
    path_prefix  TEXT,
    granted_at   TEXT NOT NULL
) STRICT;

-- Full audit trail: every engine verdict and every user decision.
CREATE TABLE permission_decisions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL,
    task_id    TEXT NOT NULL,
    operation  TEXT NOT NULL,
    paths_json TEXT NOT NULL,
    verdict    TEXT NOT NULL,
    decision   TEXT,
    scope      TEXT,
    decided_at TEXT NOT NULL
) STRICT;
CREATE INDEX idx_permission_decisions_task ON permission_decisions(task_id);

CREATE TABLE artifacts (
    id                TEXT PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL,
    path              TEXT NOT NULL,
    name              TEXT NOT NULL,
    modified_existing INTEGER NOT NULL DEFAULT 0,
    backup_path       TEXT,
    file_operation_id TEXT,
    change_summary    TEXT,
    created_at        TEXT NOT NULL
) STRICT;
CREATE INDEX idx_artifacts_task ON artifacts(task_id);

-- Journaled file operations; op_json is the full serialized FileOperation
-- from commonspace-documents, the unit of undo.
CREATE TABLE file_operations (
    id           TEXT PRIMARY KEY,
    task_id      TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    op_json      TEXT NOT NULL,
    undone       INTEGER NOT NULL DEFAULT 0,
    performed_at TEXT NOT NULL
) STRICT;
CREATE INDEX idx_file_operations_task ON file_operations(task_id);

CREATE TABLE skills (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    path       TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE mcp_servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    config_json TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
) STRICT;

CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
) STRICT;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to_latest(&mut conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        // Running again is a no-op.
        migrate_to_latest(&mut conn).unwrap();
    }

    #[test]
    fn newer_db_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        assert!(migrate_to_latest(&mut conn).is_err());
    }
}
