//! The file-operation journal: one record per mutation, carrying everything
//! needed to verify, audit, and (where possible) undo it. Persisted by the
//! storage layer; produced and consumed here.

use chrono::{DateTime, Utc};
use commonspace_core::FileOperationId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Kinds of journaled mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOpKind {
    /// A new file was created.
    Create,
    /// An existing file's contents were replaced (backup taken first).
    Modify,
    /// A file was renamed or moved (content unchanged).
    RenameMove,
    /// A file was deleted to the OS trash (backup copy taken first, so undo
    /// does not depend on platform trash-restore APIs).
    DeleteToTrash,
}

/// One journaled file operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileOperation {
    pub id: FileOperationId,
    pub kind: FileOpKind,
    /// The path the operation applied to (pre-operation path for moves).
    pub source: PathBuf,
    /// Destination for renames/moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    /// Backup copy of the original, when one was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    /// BLAKE3 hash of the file before the operation, when it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_before: Option<String>,
    /// BLAKE3 hash of the file after the operation, when it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_after: Option<String>,
    pub performed_at: DateTime<Utc>,
    /// Set once the operation has been undone; an undone record is never
    /// undone twice.
    pub undone: bool,
}

impl FileOperation {
    pub fn new(kind: FileOpKind, source: PathBuf) -> Self {
        Self {
            id: FileOperationId::generate(),
            kind,
            source,
            destination: None,
            backup: None,
            hash_before: None,
            hash_after: None,
            performed_at: Utc::now(),
            undone: false,
        }
    }

    /// Whether this operation supports undo at all (independent of whether
    /// the file has since changed — that's checked at undo time).
    pub fn supports_undo(&self) -> bool {
        match self.kind {
            FileOpKind::Create => true,
            FileOpKind::Modify => self.backup.is_some(),
            FileOpKind::RenameMove => self.destination.is_some(),
            FileOpKind::DeleteToTrash => self.backup.is_some(),
        }
    }
}
