//! Versioned schema migrations tracked via SQLite's native
//! `PRAGMA user_version`. Never edit an existing migration; append a new one.
//!
//! (Hand-rolled runner: `rusqlite_migration` currently requires a newer
//! rustc/libsqlite3-sys combination than this workspace pins; the mechanism
//! below is the same `user_version` approach it uses.)

use rusqlite::Connection;

/// Ordered migrations. Index 0 brings the schema to version 1, and so on.
const MIGRATIONS: &[&str] = &[V1_INITIAL, V2_HISTORY_FTS];

/// Apply all pending migrations inside transactions.
pub fn migrate_to_latest(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    migrate_to(conn, MIGRATIONS.len())
}

/// Apply migrations up to `target` (a 1-based schema version). Crate-visible
/// so tests can freeze a database at an older version, fill it with data, and
/// prove the upgrade path — including backfills — against real rows.
pub(crate) fn migrate_to(conn: &mut Connection, target: usize) -> Result<(), rusqlite::Error> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = target as i64;
    if current > target {
        // A newer app version created this database; refuse to touch it.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "database schema version {current} is newer than this build supports ({target})"
            )),
        ));
    }
    for (index, sql) in MIGRATIONS
        .iter()
        .enumerate()
        .take(target as usize)
        .skip(current as usize)
    {
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

/// V2: full-text search over conversation history.
///
/// `history_fts` is a standalone FTS5 table with one row per conversation
/// (kind 'conversation', title indexed) and one row per message (kind
/// 'message', body indexed). A title match therefore surfaces exactly one
/// conversation hit while each matching message is its own hit — the
/// deduplication the search UI wants falls out of the schema.
///
/// The index is kept in sync by SQL triggers rather than application code:
/// triggers run inside the same transaction as the write they mirror, so the
/// index can never drift from the tables the way a forgotten call site in app
/// code could. The final INSERTs backfill databases upgrading from v1, which
/// already contain history.
const V2_HISTORY_FTS: &str = r#"
CREATE VIRTUAL TABLE history_fts USING fts5(
    title,
    body,
    kind            UNINDEXED,
    ref_id          UNINDEXED,
    conversation_id UNINDEXED,
    created_at      UNINDEXED,
    tokenize='unicode61'
);

CREATE TRIGGER conversations_fts_insert AFTER INSERT ON conversations BEGIN
    INSERT INTO history_fts (title, body, kind, ref_id, conversation_id, created_at)
    VALUES (new.title, '', 'conversation', new.id, new.id, new.created_at);
END;

CREATE TRIGGER conversations_fts_update AFTER UPDATE OF title ON conversations BEGIN
    DELETE FROM history_fts WHERE kind = 'conversation' AND ref_id = old.id;
    INSERT INTO history_fts (title, body, kind, ref_id, conversation_id, created_at)
    VALUES (new.title, '', 'conversation', new.id, new.id, new.created_at);
END;

-- One sweep by conversation_id removes the conversation's own row and every
-- message row under it, so the index stays correct whether or not the FK
-- cascade delete of messages also fires the message trigger below.
CREATE TRIGGER conversations_fts_delete AFTER DELETE ON conversations BEGIN
    DELETE FROM history_fts WHERE conversation_id = old.id;
END;

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO history_fts (title, body, kind, ref_id, conversation_id, created_at)
    VALUES ('', new.content, 'message', new.id, new.conversation_id, new.created_at);
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM history_fts WHERE kind = 'message' AND ref_id = old.id;
END;

INSERT INTO history_fts (title, body, kind, ref_id, conversation_id, created_at)
SELECT title, '', 'conversation', id, id, created_at FROM conversations;

INSERT INTO history_fts (title, body, kind, ref_id, conversation_id, created_at)
SELECT '', content, 'message', id, conversation_id, created_at FROM messages;
"#;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    #[test]
    fn migrations_can_stop_at_an_older_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to(&mut conn, 1).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
        // v2's FTS table must not exist yet.
        let fts: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'history_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[test]
    fn fts_triggers_track_inserts_updates_and_cascade_deletes() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO conversations (id, title, created_at, updated_at)
             VALUES ('conv_1', 'Trip planning', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO messages (id, conversation_id, role, content, created_at)
             VALUES ('msg_1', 'conv_1', 'user', 'book the hotel', '2026-01-01T00:00:01Z');",
        )
        .unwrap();
        let count = |conn: &Connection| -> i64 {
            conn.query_row("SELECT count(*) FROM history_fts", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count(&conn), 2);

        // A title edit replaces the conversation's index row in place.
        conn.execute(
            "UPDATE conversations SET title = 'Beach holiday' WHERE id = 'conv_1'",
            [],
        )
        .unwrap();
        let title: String = conn
            .query_row(
                "SELECT title FROM history_fts WHERE kind = 'conversation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Beach holiday");
        assert_eq!(count(&conn), 2);

        // Deleting the conversation clears its row and, via FK cascade on
        // messages, every message row too.
        conn.execute("DELETE FROM conversations WHERE id = 'conv_1'", [])
            .unwrap();
        assert_eq!(count(&conn), 0);
    }
}
