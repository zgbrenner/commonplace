//! Versioned schema migrations tracked via SQLite's native
//! `PRAGMA user_version`. Never edit an existing migration; append a new one.
//!
//! (Hand-rolled runner: `rusqlite_migration` currently requires a newer
//! rustc/libsqlite3-sys combination than this workspace pins; the mechanism
//! below is the same `user_version` approach it uses.)

use rusqlite::Connection;

/// Ordered migrations. Index 0 brings the schema to version 1, and so on.
const MIGRATIONS: &[&str] = &[
    V1_INITIAL,
    V2_HISTORY_FTS,
    V3_ATTACHMENTS,
    V4_CONVERSATION_TITLE_AUTO,
    V5_HISTORY_FTS_PREFIX,
    V6_SKILL_PROVENANCE,
    V7_BACKUPS,
];

/// Apply all pending migrations inside transactions.
pub fn migrate_to_latest(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    migrate_to(conn, MIGRATIONS.len())
}

/// The schema version this build knows how to produce.
pub(crate) fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

/// The schema version stamped on `conn`, 0 for a database that has never been
/// migrated.
pub(crate) fn current_version(conn: &Connection) -> Result<i64, rusqlite::Error> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Apply migrations up to `target` (a 1-based schema version). Crate-visible
/// so tests can freeze a database at an older version, fill it with data, and
/// prove the upgrade path — including backfills — against real rows.
pub(crate) fn migrate_to(conn: &mut Connection, target: usize) -> Result<(), rusqlite::Error> {
    let current = current_version(conn)?;
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

/// V3: files and folders the user attached to a message.
///
/// Metadata only — never file contents. `task_id` is nullable and set null on
/// task deletion because an attachment belongs to the conversation first: the
/// user's "what did I hand this conversation?" record must outlive any one
/// task. `content_hash` is null for folders and for files too large to hash
/// at send time; `in_workspace` records whether the path was inside one of
/// the workspace's authorized roots when it was attached.
const V3_ATTACHMENTS: &str = r#"
CREATE TABLE attachments (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    task_id         TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    path            TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('file','folder')),
    size_bytes      INTEGER,
    modified_at     TEXT,
    content_hash    TEXT,
    in_workspace    INTEGER NOT NULL CHECK (in_workspace IN (0,1)),
    created_at      TEXT NOT NULL
) STRICT;
CREATE INDEX idx_attachments_conversation ON attachments(conversation_id, created_at);
"#;

/// V4: whether a conversation's title is still the one Commonspace wrote.
///
/// Titles are generated from the opening prompt and improved from a finished
/// task's summary, but only while nobody has taken them over: renaming a
/// conversation sets this to 0 and nothing overwrites it afterwards. The
/// default of 1 is also the right answer for every row that already exists,
/// since every title written before this column did came from a prompt.
const V4_CONVERSATION_TITLE_AUTO: &str = r#"
ALTER TABLE conversations ADD COLUMN title_auto INTEGER NOT NULL DEFAULT 1;
"#;

/// V5: rebuild `history_fts` with a prefix index and diacritic folding.
///
/// FTS5 creation options are fixed at CREATE time — there is no `ALTER` for
/// them — so changing the tokenizer or adding a prefix index means a new
/// table. That is cheap here: the index is derived data, reconstructible from
/// `conversations` and `messages` exactly as v2's backfill built it, so this
/// rebuilds from the source tables rather than copying the old index across.
/// Rebuilding also repairs any drift the old index had accumulated, which
/// copying would faithfully preserve.
///
/// `prefix='2 3'` stores separate indexes for two- and three-character
/// prefixes, so the short prefixes a search-as-you-type box produces are a
/// lookup rather than a scan of every term in the vocabulary.
///
/// `remove_diacritics 2` folds `café` onto `cafe`. It is the corrected form of
/// the default (`1`), which leaves diacritics attached when a codepoint
/// carries more than one.
///
/// Known limit, stated rather than half-fixed: `unicode61` splits on
/// whitespace and punctuation, and CJK text has neither. An unspaced run of
/// Han characters becomes a single token, so it is findable only by typing the
/// whole run (or, now, a prefix of it). The real fixes are a companion trigram
/// index or a dictionary tokenizer registered through `fts5_api`; the latter
/// needs `unsafe`, which this workspace denies. Neither is in scope here.
const V5_HISTORY_FTS_PREFIX: &str = r#"
DROP TRIGGER conversations_fts_insert;
DROP TRIGGER conversations_fts_update;
DROP TRIGGER conversations_fts_delete;
DROP TRIGGER messages_fts_insert;
DROP TRIGGER messages_fts_delete;
DROP TABLE history_fts;

CREATE VIRTUAL TABLE history_fts USING fts5(
    title,
    body,
    kind            UNINDEXED,
    ref_id          UNINDEXED,
    conversation_id UNINDEXED,
    created_at      UNINDEXED,
    prefix='2 3',
    tokenize="unicode61 remove_diacritics 2"
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
-- cascade delete of messages also fires the message triggers below.
CREATE TRIGGER conversations_fts_delete AFTER DELETE ON conversations BEGIN
    DELETE FROM history_fts WHERE conversation_id = old.id;
END;

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO history_fts (title, body, kind, ref_id, conversation_id, created_at)
    VALUES ('', new.content, 'message', new.id, new.conversation_id, new.created_at);
END;

-- No caller edits a message today, but the index must not depend on that
-- staying true: an unmirrored UPDATE is drift nothing would report.
CREATE TRIGGER messages_fts_update
AFTER UPDATE OF content, conversation_id ON messages BEGIN
    DELETE FROM history_fts WHERE kind = 'message' AND ref_id = old.id;
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

/// V6: which skill a `skills` row actually is.
///
/// A row that names only a path cannot answer "is this the same skill I
/// approved?" after the folder behind it changes. `digest` is the content hash
/// that question needs and the value any later signature would cover;
/// `version` is the author's own name for the revision; `manifest_json` keeps
/// the parsed manifest so a listing does not have to re-read from disk. All
/// three are nullable: rows recorded before this column existed have no
/// honest value to backfill, and inventing one would be worse than admitting
/// the digest is unknown.
const V6_SKILL_PROVENANCE: &str = r#"
ALTER TABLE skills ADD COLUMN version TEXT;
ALTER TABLE skills ADD COLUMN digest TEXT;
ALTER TABLE skills ADD COLUMN manifest_json TEXT;
"#;

/// V7: an enumerable record of the backup copies taken before file edits.
///
/// Until now a backup existed only as a path inside a journaled
/// `FileOperation`'s JSON, which makes "how much disk is this costing?" and
/// "which backups of this file may be dropped?" unanswerable without parsing
/// every operation ever recorded. The indexes match those two questions: by
/// workspace over time for accounting, by source path over time for choosing
/// which copies of one file are redundant.
///
/// The backfill reads the journal rather than the filesystem, so it is exact
/// about what was recorded and silent about what is on disk now: `size_bytes`
/// stays null for historical rows because a migration must not stat thousands
/// of files, and `content_hash` carries `hash_before` — the hash of the
/// content the backup holds. `json_valid` guards the extraction so one
/// unparseable journal row cannot fail the whole upgrade.
///
/// `pruned_at` marks a backup whose file has been deleted while keeping the
/// row, so history still shows a backup was taken and later reclaimed.
const V7_BACKUPS: &str = r#"
CREATE TABLE backups (
    id                TEXT PRIMARY KEY,
    workspace_id      TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
    file_operation_id TEXT REFERENCES file_operations(id) ON DELETE SET NULL,
    source_path       TEXT NOT NULL,
    backup_path       TEXT NOT NULL,
    content_hash      TEXT,
    size_bytes        INTEGER,
    created_at        TEXT NOT NULL,
    pruned_at         TEXT
) STRICT;
CREATE INDEX idx_backups_workspace ON backups(workspace_id, created_at);
CREATE INDEX idx_backups_source ON backups(source_path, created_at);

INSERT INTO backups
    (id, workspace_id, file_operation_id, source_path, backup_path,
     content_hash, size_bytes, created_at, pruned_at)
SELECT 'bak_' || f.id,
       t.workspace_id,
       f.id,
       json_extract(f.op_json, '$.source'),
       json_extract(f.op_json, '$.backup'),
       json_extract(f.op_json, '$.hash_before'),
       NULL,
       COALESCE(json_extract(f.op_json, '$.performed_at'), f.performed_at),
       NULL
FROM file_operations AS f
LEFT JOIN tasks AS t ON t.id = f.task_id
WHERE json_valid(f.op_json)
  AND json_extract(f.op_json, '$.backup') IS NOT NULL
  AND json_extract(f.op_json, '$.source') IS NOT NULL;
"#;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The fingerprint the migration list is expected to produce. Frozen on
    /// purpose: appending a migration changes it and the new value goes in
    /// here with that migration, while *editing* an existing migration changes
    /// it with nothing to justify the change — which is the whole point, since
    /// an edited migration leaves `user_version` untouched and every other
    /// check happy.
    const SCHEMA_FINGERPRINT: &str = "bd27913fa0494099";

    /// A stable digest of every schema object's type, name and DDL. Whitespace
    /// inside the DDL is collapsed so re-indenting a migration string is not
    /// mistaken for a schema change, and `sqlite_master` is read in name order
    /// so the digest does not depend on the order objects were created in.
    ///
    /// FNV-1a rather than a cryptographic hash: nothing here defends against a
    /// crafted collision, only against an accidental edit, and that does not
    /// justify a dependency.
    fn schema_fingerprint(conn: &Connection) -> String {
        let mut stmt = conn
            .prepare(
                "SELECT type, name, COALESCE(sql, '') FROM sqlite_master
                 ORDER BY name, type",
            )
            .unwrap();
        let lines = stmt
            .query_map([], |r| {
                let sql: String = r.get(2)?;
                Ok(format!(
                    "{}|{}|{}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in lines.join("\n").bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    #[test]
    fn migrated_schema_matches_its_frozen_fingerprint() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to_latest(&mut conn).unwrap();
        assert_eq!(
            schema_fingerprint(&conn),
            SCHEMA_FINGERPRINT,
            "the schema changed. If you appended a migration, update \
             SCHEMA_FINGERPRINT in the same commit; if you edited an existing \
             migration, don't — every installed database already ran the old \
             text and will never see the new one."
        );
    }

    /// A database installed fresh and one upgraded step by step must end up
    /// identical. They diverge the moment a schema change is written into an
    /// old migration as well as a new one, or into only one of the two — and
    /// the divergence shows up on users who upgraded, never on the developer
    /// who reinstalls.
    #[test]
    fn a_fresh_database_matches_one_upgraded_one_version_at_a_time() {
        let mut fresh = Connection::open_in_memory().unwrap();
        migrate_to_latest(&mut fresh).unwrap();

        let mut stepped = Connection::open_in_memory().unwrap();
        migrate_to(&mut stepped, 1).unwrap();
        // Real rows from the oldest schema, so every later backfill runs with
        // something to backfill instead of trivially succeeding on no rows.
        stepped
            .execute_batch(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES ('conv_1', 'Old conversation',
                         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                 INSERT INTO messages (id, conversation_id, role, content, created_at)
                 VALUES ('msg_1', 'conv_1', 'user', 'old text', '2026-01-01T00:00:01Z');",
            )
            .unwrap();
        for version in 2..=MIGRATIONS.len() {
            migrate_to(&mut stepped, version).unwrap();
        }

        assert_eq!(current_version(&fresh).unwrap(), latest_version());
        assert_eq!(current_version(&stepped).unwrap(), latest_version());
        assert_eq!(
            schema_fingerprint(&fresh),
            schema_fingerprint(&stepped),
            "a fresh install and an upgraded install disagree about the schema"
        );
    }

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
    fn skill_rows_carry_a_version_and_digest() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO skills (id, name, path, created_at, version, digest, manifest_json)
             VALUES ('skill_1', 'Rename photos', 'C:/skills/rename',
                     '2026-01-01T00:00:00Z', '1.2.0', 'b3:abc', '{\"entry\":\"main\"}');
             -- A skill recorded before v6 has no honest value for any of the
             -- three, and must still be insertable.
             INSERT INTO skills (id, name, path, created_at)
             VALUES ('skill_2', 'Older', 'C:/skills/older', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        let digest: Option<String> = conn
            .query_row("SELECT digest FROM skills WHERE id = 'skill_2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(digest, None);
        let version: String = conn
            .query_row("SELECT version FROM skills WHERE id = 'skill_1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, "1.2.0");
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

        // Editing a message's body moves its index row with it, in place.
        conn.execute(
            "UPDATE messages SET content = 'cancel the hotel' WHERE id = 'msg_1'",
            [],
        )
        .unwrap();
        let body: String = conn
            .query_row(
                "SELECT body FROM history_fts WHERE kind = 'message'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(body, "cancel the hotel");
        assert_eq!(count(&conn), 2);

        // Deleting the conversation clears its row and, via FK cascade on
        // messages, every message row too.
        conn.execute("DELETE FROM conversations WHERE id = 'conv_1'", [])
            .unwrap();
        assert_eq!(count(&conn), 0);
    }
}
