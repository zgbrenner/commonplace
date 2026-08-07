//! Proposed changes, held outside the user's files until someone accepts
//! them.
//!
//! An agent's edit does not land in a document the moment it is decided. It
//! becomes a [`StagedChange`]: the proposed bytes are written into a
//! per-task staging directory, the target's hash is recorded as it was at
//! that moment, and nothing under the user's roots is touched. The UI can
//! then show what would happen — [`StagingStore::staged_bytes`] feeds
//! [`crate::diff`] directly — and only an explicit [`apply`] writes anything.
//!
//! Two rules hold the design together:
//!
//! - **Staging never writes to a user path.** Every byte a staged change
//!   carries lives under the staging root. The only code here that touches
//!   the user's files is [`apply`], and it does so exclusively through
//!   [`SafeFs`], so backup, on-disk hash verification, journaling and undo
//!   work exactly as they do for an unstaged operation. Staging is a gate in
//!   front of that path, not a replacement for it.
//! - **Nothing staged is trusted at apply time.** The target is re-hashed
//!   and compared against [`StagedChange::hash_before`], and the staged
//!   bytes are re-hashed and compared against [`StagedChange::hash_after`].
//!   A file the user edited in the meantime is a [`StagingError::Conflict`],
//!   never an overwrite; a staging file that no longer matches what was
//!   staged is refused rather than written into a document.
//!
//! Scope and protected-location enforcement are not repeated here. Staging a
//! change to an out-of-scope path is harmless — no user file is involved —
//! and [`apply`] refuses it at the moment it would matter, because `SafeFs`
//! checks every path it is given.
//!
//! Staging deals in files. A directory move is not stageable: the conflict
//! check has nothing to hash, and "this folder is as I left it" is not a
//! question a content hash can answer.

use crate::fsops::{FsToolError, SafeFs};
use crate::inspect::hash_file;
use crate::journal::FileOperation;
use chrono::{DateTime, Utc};
use commonspace_core::OperationResult;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Identifies one staged change. Prefixed and UUID-backed, like the ids in
/// [`crate::journal`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StagedChangeId(pub String);

impl StagedChangeId {
    /// Generate a new random id with a readable prefix.
    pub fn generate() -> Self {
        Self(format!("stg_{}", uuid::Uuid::new_v4().simple()))
    }
}

impl std::fmt::Display for StagedChangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for StagedChangeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for StagedChangeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// What a staged change would do to the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagedKind {
    /// Bring a file into existence that does not exist yet.
    Create,
    /// Replace an existing file's contents.
    Modify,
    /// Give a file a different name in the same folder.
    Rename,
    /// Put a file somewhere else.
    Move,
    /// Send a file to the OS trash.
    Delete,
}

impl StagedKind {
    /// A noun for this kind, for sentences a user reads.
    pub fn label(self) -> &'static str {
        match self {
            StagedKind::Create => "new file",
            StagedKind::Modify => "update",
            StagedKind::Rename => "rename",
            StagedKind::Move => "move",
            StagedKind::Delete => "deletion",
        }
    }

    /// Whether this kind proposes new bytes. Renames, moves and deletions
    /// carry none — they relocate or remove content that already exists.
    pub fn carries_content(self) -> bool {
        matches!(self, StagedKind::Create | StagedKind::Modify)
    }
}

/// One proposed change, held outside the user's files until it is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StagedChange {
    pub id: StagedChangeId,
    pub kind: StagedKind,
    /// The user's file this would affect.
    pub target: PathBuf,
    /// Destination for a rename or move; `None` for every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    /// Where the proposed bytes wait. `None` for rename/move/delete, which
    /// carry no new content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_content: Option<PathBuf>,
    /// The target's BLAKE3 hash when this was staged — the conflict check at
    /// apply time compares against it. `None` only for a
    /// [`StagedKind::Create`], where the target did not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_before: Option<String>,
    /// BLAKE3 hash of what the target would hold afterwards. `None` for a
    /// deletion, which leaves nothing behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_after: Option<String>,
    /// Size in bytes of what the target would hold afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_after: Option<u64>,
    /// Plain-language description, e.g. "Update Q3-report.docx".
    pub summary: String,
    pub staged_at: DateTime<Utc>,
}

/// Everything that can go wrong while preparing, inspecting or applying a
/// staged change.
///
/// Deliberately separate from [`FsToolError`]: "this proposal no longer
/// matches the file it was made against" and "the write failed" are different
/// situations, told to the user in different words. The filesystem failures
/// [`apply`] can hit arrive wrapped in [`StagingError::Fs`].
#[derive(Debug, Error)]
pub enum StagingError {
    /// The target is not in the state it was in when the change was staged.
    /// Applying anyway would discard whatever happened in between, which is
    /// the outcome this whole layer exists to prevent.
    #[error("\"{target}\" {detail}")]
    Conflict { target: PathBuf, detail: String },
    #[error("\"{0}\" does not exist")]
    TargetMissing(PathBuf),
    #[error("\"{0}\" already exists")]
    TargetExists(PathBuf),
    #[error("\"{0}\" is a folder; changes are staged for files")]
    NotAFile(PathBuf),
    #[error("a {} carries no proposed content", .kind.label())]
    NoStagedContent { kind: StagedKind },
    #[error("the prepared content for \"{target}\" is no longer in the staging area")]
    StagedContentMissing { target: PathBuf },
    #[error("the prepared content for \"{target}\" cannot be trusted: {detail}")]
    StagedContentCorrupt { target: PathBuf, detail: String },
    #[error("\"{target}\" could not be prepared: {detail}")]
    StagingFailed { target: PathBuf, detail: String },
    #[error(transparent)]
    Fs(#[from] FsToolError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The staging area for one task: proposed bytes, and nothing else.
///
/// Content is **id-addressed** — `<root>/content/<change id>` — rather than
/// content-addressed. Two changes proposing identical bytes then stay
/// independent, so discarding one cannot pull the ground out from under the
/// other; content-addressing would buy deduplication and owe reference
/// counting for it, which is a poor trade for documents a person is reviewing
/// a handful at a time.
///
/// The root's filesystem does not matter. Applying reads the staged bytes and
/// hands them to [`SafeFs`], which backs the original up and verifies the
/// write; it is never a rename into place, so a staging root on another
/// volume costs a copy that was going to happen anyway.
#[derive(Debug, Clone)]
pub struct StagingStore {
    root: PathBuf,
}

impl StagingStore {
    /// Create a store rooted at `root` (created on demand). The caller is
    /// expected to give each task its own root, so [`Self::discard_all`] can
    /// end that task's staging without consulting anything else.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The directory this store owns.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Propose a new file at `target`.
    ///
    /// Refuses immediately if something is already there: the caller asked to
    /// create, and telling them now is better than staging a change that can
    /// only ever conflict.
    pub fn stage_create(
        &self,
        target: &Path,
        contents: &[u8],
    ) -> Result<StagedChange, StagingError> {
        if target.exists() {
            return Err(StagingError::TargetExists(target.to_path_buf()));
        }
        let mut change = self.blank(StagedKind::Create, target);
        change.summary = format!("Create {}", display_name(target));
        self.write_staged_content(&mut change, contents)?;
        Ok(change)
    }

    /// Propose replacing the contents of an existing file.
    pub fn stage_modify(
        &self,
        target: &Path,
        contents: &[u8],
    ) -> Result<StagedChange, StagingError> {
        require_file(target)?;
        let mut change = self.blank(StagedKind::Modify, target);
        change.hash_before = Some(hash_file(target)?);
        change.summary = format!("Update {}", display_name(target));
        self.write_staged_content(&mut change, contents)?;
        Ok(change)
    }

    /// Propose giving a file a different name.
    pub fn stage_rename(&self, from: &Path, to: &Path) -> Result<StagedChange, StagingError> {
        self.stage_relocation(StagedKind::Rename, from, to, "Rename")
    }

    /// Propose putting a file somewhere else.
    pub fn stage_move(&self, from: &Path, to: &Path) -> Result<StagedChange, StagingError> {
        self.stage_relocation(StagedKind::Move, from, to, "Move")
    }

    /// Propose sending a file to the OS trash.
    pub fn stage_delete(&self, target: &Path) -> Result<StagedChange, StagingError> {
        require_file(target)?;
        let mut change = self.blank(StagedKind::Delete, target);
        change.hash_before = Some(hash_file(target)?);
        change.summary = format!("Delete {}", display_name(target));
        Ok(change)
    }

    /// The proposed bytes, for a preview or a diff.
    ///
    /// Verified on the way out against [`StagedChange::hash_after`], so a
    /// staging file that was truncated or tampered with is refused here
    /// rather than shown to the user as though it were the proposal they
    /// approved.
    pub fn staged_bytes(&self, change: &StagedChange) -> Result<Vec<u8>, StagingError> {
        let Some(path) = change.staged_content.as_ref() else {
            return Err(StagingError::NoStagedContent { kind: change.kind });
        };
        // A record can come back from storage with anything in this field;
        // reading outside our own root would be following it somewhere it was
        // never meant to point.
        if !path.starts_with(&self.root) {
            return Err(StagingError::StagedContentCorrupt {
                target: change.target.clone(),
                detail: "it is not in this task's staging area".into(),
            });
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StagingError::StagedContentMissing {
                    target: change.target.clone(),
                })
            }
            Err(e) => return Err(e.into()),
        };
        let Some(expected) = change.hash_after.as_deref() else {
            return Err(StagingError::StagedContentCorrupt {
                target: change.target.clone(),
                detail: "no hash was recorded for it, so it cannot be checked".into(),
            });
        };
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != expected {
            return Err(StagingError::StagedContentCorrupt {
                target: change.target.clone(),
                detail: "it no longer matches what was prepared".into(),
            });
        }
        Ok(bytes)
    }

    /// Has the target changed on disk since this was staged?
    ///
    /// A target that cannot be read counts as conflicted. Applying without a
    /// check is precisely what staging exists to prevent, so "unverifiable"
    /// and "changed" get the same answer.
    pub fn is_conflicted(&self, change: &StagedChange) -> bool {
        check_target_unchanged(change).is_err()
    }

    /// Drop one proposal. The target is not touched — a discarded change
    /// never existed as far as the user's files are concerned.
    pub fn discard(&self, change: &StagedChange) -> Result<(), StagingError> {
        let Some(path) = change.staged_content.as_ref() else {
            return Ok(());
        };
        if !path.starts_with(&self.root) {
            return Err(StagingError::StagedContentCorrupt {
                target: change.target.clone(),
                detail: "it is not in this task's staging area".into(),
            });
        }
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            // Discarding what is already gone is what the caller wanted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Remove the task's whole staging tree.
    pub fn discard_all(&self) -> Result<(), StagingError> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Where this change's bytes live.
    fn content_path(&self, id: &StagedChangeId) -> PathBuf {
        self.root.join("content").join(&id.0)
    }

    /// A change with its identity, kind and target filled in and everything
    /// that depends on the content still empty.
    fn blank(&self, kind: StagedKind, target: &Path) -> StagedChange {
        StagedChange {
            id: StagedChangeId::generate(),
            kind,
            target: target.to_path_buf(),
            destination: None,
            staged_content: None,
            hash_before: None,
            hash_after: None,
            size_after: None,
            summary: String::new(),
            staged_at: Utc::now(),
        }
    }

    /// The shared body of [`Self::stage_rename`] and [`Self::stage_move`],
    /// which differ only in what the user is told they are looking at.
    fn stage_relocation(
        &self,
        kind: StagedKind,
        from: &Path,
        to: &Path,
        verb: &str,
    ) -> Result<StagedChange, StagingError> {
        require_file(from)?;
        if to.exists() {
            return Err(StagingError::TargetExists(to.to_path_buf()));
        }
        let hash = hash_file(from)?;
        let mut change = self.blank(kind, from);
        change.destination = Some(to.to_path_buf());
        // Relocating leaves the bytes alone, so the file afterwards is the
        // file before it — recorded so a caller comparing hashes across the
        // whole set of staged changes does not have to special-case moves.
        change.hash_before = Some(hash.clone());
        change.hash_after = Some(hash);
        change.size_after = std::fs::metadata(from).ok().map(|m| m.len());
        change.summary = format!("{verb} {} to {}", display_name(from), display_name(to));
        Ok(change)
    }

    /// Put the proposed bytes in the staging area and record what they are.
    ///
    /// Read back and verified before the change is handed out: a staging file
    /// that was short-written is a proposal nobody can apply, and finding that
    /// out now costs one read instead of a failed apply later.
    fn write_staged_content(
        &self,
        change: &mut StagedChange,
        contents: &[u8],
    ) -> Result<(), StagingError> {
        let path = self.content_path(&change.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;

        let expected = blake3::hash(contents).to_hex().to_string();
        let actual = hash_file(&path)?;
        if actual != expected {
            let _ = std::fs::remove_file(&path);
            return Err(StagingError::StagingFailed {
                target: change.target.clone(),
                detail: "the prepared content did not read back as written".into(),
            });
        }

        change.staged_content = Some(path);
        change.hash_after = Some(expected);
        change.size_after = Some(contents.len() as u64);
        Ok(())
    }
}

/// Apply one staged change to the user's files, through [`SafeFs`].
///
/// Two checks stand between a proposal and a document, in this order:
///
/// 1. the target is re-hashed and must still match
///    [`StagedChange::hash_before`] — otherwise the file changed since the
///    proposal was made and applying it would silently discard that change;
/// 2. the staged bytes are re-hashed and must still match
///    [`StagedChange::hash_after`].
///
/// Only then does the write happen, and it happens through `SafeFs`, which
/// backs up the original, re-checks scope and protected locations, verifies
/// the result on disk and returns the journal record that powers undo. The
/// staged content is left in place: the lifecycle of a proposal belongs to
/// the caller, and [`StagingStore::discard`] is how it ends.
pub fn apply(
    fs: &SafeFs,
    store: &StagingStore,
    change: &StagedChange,
) -> Result<(OperationResult, FileOperation), StagingError> {
    check_target_unchanged(change)?;
    let outcome =
        match change.kind {
            StagedKind::Create => {
                let bytes = store.staged_bytes(change)?;
                fs.create_file(&change.target, &bytes)?
            }
            StagedKind::Modify => {
                let bytes = store.staged_bytes(change)?;
                fs.overwrite_file(&change.target, &bytes)?
            }
            StagedKind::Rename | StagedKind::Move => {
                let destination = change.destination.as_ref().ok_or_else(|| {
                    StagingError::StagedContentCorrupt {
                        target: change.target.clone(),
                        detail: "no destination was recorded for it".into(),
                    }
                })?;
                fs.rename_or_move(&change.target, destination)?
            }
            StagedKind::Delete => fs.delete_to_trash(&change.target)?,
        };
    Ok(outcome)
}

/// The conflict check: is the target still what it was when this change was
/// staged?
fn check_target_unchanged(change: &StagedChange) -> Result<(), StagingError> {
    let target = &change.target;
    if change.kind == StagedKind::Create {
        return if target.exists() {
            Err(StagingError::Conflict {
                target: target.clone(),
                detail: "already exists; it was created after this new file was prepared".into(),
            })
        } else {
            Ok(())
        };
    }

    if let Some(destination) = &change.destination {
        if destination.exists() {
            return Err(StagingError::Conflict {
                target: destination.clone(),
                detail: "already exists; something was put there after this was prepared".into(),
            });
        }
    }

    let Some(expected) = change.hash_before.as_deref() else {
        return Err(StagingError::Conflict {
            target: target.clone(),
            detail: "was not recorded when this was prepared, so there is nothing to check it \
                     against"
                .into(),
        });
    };
    if !target.is_file() {
        return Err(StagingError::Conflict {
            target: target.clone(),
            detail: "no longer exists".into(),
        });
    }
    if hash_file(target)? != expected {
        return Err(StagingError::Conflict {
            target: target.clone(),
            detail: "has been changed since this was prepared; applying it now would discard that \
                     change"
                .into(),
        });
    }
    Ok(())
}

fn require_file(path: &Path) -> Result<(), StagingError> {
    if path.is_file() {
        return Ok(());
    }
    if path.exists() {
        return Err(StagingError::NotAFile(path.to_path_buf()));
    }
    Err(StagingError::TargetMissing(path.to_path_buf()))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::backup::BackupStore;
    use crate::journal::FileOpKind;
    use commonspace_permissions::PathGuard;

    struct Fixture {
        _tmp: tempfile::TempDir,
        ws: PathBuf,
        fs: SafeFs,
        store: StagingStore,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let fs = SafeFs::new(
            PathGuard::new([&ws]),
            BackupStore::new(tmp.path().join("backups")),
        );
        // The staging root sits beside the workspace, never inside it: a
        // proposal must not be visible to the agent as an ordinary file.
        let store = StagingStore::new(tmp.path().join("staging").join("task_1"));
        Fixture {
            _tmp: tmp,
            ws,
            fs,
            store,
        }
    }

    /// Every path under `root`, relative, sorted — the shape of the user's
    /// folder, for asserting staging left it alone.
    fn tree(root: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn staging_a_create_leaves_the_target_absent() {
        let fx = fixture();
        let target = fx.ws.join("reports").join("Q3 report.md");
        let before = tree(&fx.ws);

        let change = fx.store.stage_create(&target, b"# Q3").unwrap();

        assert!(!target.exists(), "staging must not create the user's file");
        assert_eq!(tree(&fx.ws), before, "the workspace was touched");
        assert_eq!(change.kind, StagedKind::Create);
        assert_eq!(change.hash_before, None);
        assert_eq!(change.size_after, Some(4));
        assert_eq!(change.summary, "Create Q3 report.md");
        let staged = change.staged_content.as_ref().expect("staged content");
        assert!(staged.starts_with(fx.store.root()));
        assert_eq!(std::fs::read(staged).unwrap(), b"# Q3");
    }

    #[test]
    fn staging_a_modify_leaves_the_original_bytes_on_disk() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "original terms").unwrap();
        let before = tree(&fx.ws);

        let change = fx.store.stage_modify(&target, b"revised terms").unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original terms",
            "staging must not write to the user's file"
        );
        assert_eq!(tree(&fx.ws), before);
        assert_eq!(
            change.hash_before,
            Some(blake3::hash(b"original terms").to_hex().to_string())
        );
        assert_eq!(change.summary, "Update contract.md");
        assert!(!fx.store.is_conflicted(&change));
    }

    #[test]
    fn applying_a_create_writes_the_file_and_journals_it() {
        let fx = fixture();
        let target = fx.ws.join("notes").join("naïve 报告.md");
        let change = fx.store.stage_create(&target, "# 报告".as_bytes()).unwrap();

        let (result, op) = apply(&fx.fs, &fx.store, &change).unwrap();

        assert!(result.success);
        assert_eq!(result.created.len(), 1);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "# 报告");
        assert_eq!(op.kind, FileOpKind::Create);
        assert_eq!(op.hash_after, change.hash_after);
        assert_eq!(op.hash_after, Some(hash_file(&target).unwrap()));
        assert_eq!(op.hash_before, None);
    }

    #[test]
    fn applying_a_modify_backs_up_and_journals_both_hashes() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "original terms").unwrap();
        let change = fx.store.stage_modify(&target, b"revised terms").unwrap();

        let (result, op) = apply(&fx.fs, &fx.store, &change).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "revised terms");
        assert_eq!(result.backups.len(), 1, "SafeFs must still take a backup");
        assert_eq!(
            std::fs::read_to_string(&result.backups[0]).unwrap(),
            "original terms"
        );
        assert_eq!(op.kind, FileOpKind::Modify);
        assert_eq!(op.hash_before, change.hash_before);
        assert_eq!(op.hash_after, change.hash_after);

        // The whole point of going through SafeFs: undo still works.
        let (_, undone) = fx.fs.undo(&op).unwrap();
        assert!(undone.undone);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original terms");
    }

    #[test]
    fn applying_after_the_target_changed_underneath_conflicts() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "v1").unwrap();
        let change = fx.store.stage_modify(&target, b"agent's version").unwrap();

        std::fs::write(&target, "the user's own edit").unwrap();
        assert!(fx.store.is_conflicted(&change));

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(matches!(err, StagingError::Conflict { .. }), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "the user's own edit",
            "a conflict must leave the user's file exactly as it was"
        );
    }

    #[test]
    fn applying_a_create_whose_target_appeared_meanwhile_conflicts() {
        let fx = fixture();
        let target = fx.ws.join("summary.md");
        let change = fx.store.stage_create(&target, b"agent's draft").unwrap();

        std::fs::write(&target, "the user got there first").unwrap();
        assert!(fx.store.is_conflicted(&change));

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(matches!(err, StagingError::Conflict { .. }), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "the user got there first"
        );
    }

    #[test]
    fn applying_a_move_whose_destination_filled_up_conflicts() {
        let fx = fixture();
        let src = fx.ws.join("inbox").join("scan_001.pdf");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "pdf-bytes").unwrap();
        let dst = fx.ws.join("archive").join("2026 Contract.pdf");
        let change = fx.store.stage_move(&src, &dst).unwrap();

        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&dst, "something else entirely").unwrap();

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(matches!(err, StagingError::Conflict { .. }), "{err}");
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "something else entirely"
        );
        assert!(src.exists());
    }

    #[test]
    fn staged_bytes_round_trip_unicode_unchanged() {
        let fx = fixture();
        let target = fx.ws.join("résumé.md");
        let contents = "Résumé — 概要\n価格は 750 円です。\nنص عربي\nfamily 👨‍👩‍👧‍👦\n";
        let change = fx.store.stage_create(&target, contents.as_bytes()).unwrap();

        let bytes = fx.store.staged_bytes(&change).unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), contents);
        assert_eq!(change.size_after, Some(contents.len() as u64));
    }

    #[test]
    fn staged_bytes_says_a_relocation_or_deletion_has_none() {
        let fx = fixture();
        let target = fx.ws.join("old draft.txt");
        std::fs::write(&target, "drafty").unwrap();

        let deletion = fx.store.stage_delete(&target).unwrap();
        assert_eq!(deletion.staged_content, None);
        let err = fx.store.staged_bytes(&deletion).unwrap_err();
        assert!(
            matches!(
                err,
                StagingError::NoStagedContent {
                    kind: StagedKind::Delete
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("deletion"), "{err}");

        let renamed = fx
            .store
            .stage_rename(&target, &fx.ws.join("draft (old).txt"))
            .unwrap();
        assert_eq!(renamed.staged_content, None);
        let err = fx.store.staged_bytes(&renamed).unwrap_err();
        assert!(
            matches!(
                err,
                StagingError::NoStagedContent {
                    kind: StagedKind::Rename
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn corrupted_staged_content_is_caught_instead_of_applied() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "original terms").unwrap();
        let change = fx.store.stage_modify(&target, b"revised terms").unwrap();

        // Whatever damages a staging file — a truncated write, a stray
        // process, deliberate tampering — must not reach the document.
        std::fs::write(change.staged_content.as_ref().unwrap(), b"garbage").unwrap();

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(
            matches!(err, StagingError::StagedContentCorrupt { .. }),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original terms");
    }

    #[test]
    fn missing_staged_content_is_reported_plainly() {
        let fx = fixture();
        let target = fx.ws.join("a.txt");
        let change = fx.store.stage_create(&target, b"x").unwrap();
        std::fs::remove_file(change.staged_content.as_ref().unwrap()).unwrap();

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(
            matches!(err, StagingError::StagedContentMissing { .. }),
            "{err}"
        );
        assert!(!target.exists());
    }

    #[test]
    fn staged_content_outside_the_store_is_refused() {
        let fx = fixture();
        let target = fx.ws.join("a.txt");
        std::fs::write(&target, "the user's file").unwrap();
        let mut change = fx.store.stage_create(&fx.ws.join("b.txt"), b"x").unwrap();
        // A record that points at a user file instead of the staging area is
        // not a source of proposed bytes.
        change.staged_content = Some(target.clone());

        let err = fx.store.staged_bytes(&change).unwrap_err();
        assert!(
            matches!(err, StagingError::StagedContentCorrupt { .. }),
            "{err}"
        );
    }

    #[test]
    fn discard_removes_the_proposal_and_nothing_else() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "original terms").unwrap();
        let before = tree(&fx.ws);
        let change = fx.store.stage_modify(&target, b"revised terms").unwrap();
        let staged = change.staged_content.clone().expect("staged content");

        fx.store.discard(&change).unwrap();

        assert!(!staged.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original terms");
        assert_eq!(tree(&fx.ws), before);
        // Discarding twice is not an error; the caller wanted it gone.
        fx.store.discard(&change).unwrap();
    }

    #[test]
    fn discard_all_removes_the_whole_staging_tree() {
        let fx = fixture();
        let a = fx.store.stage_create(&fx.ws.join("a.md"), b"a").unwrap();
        let b = fx.store.stage_create(&fx.ws.join("b.md"), b"b").unwrap();
        assert!(fx.store.root().exists());

        fx.store.discard_all().unwrap();

        assert!(!fx.store.root().exists());
        assert!(!a.staged_content.unwrap().exists());
        assert!(!b.staged_content.unwrap().exists());
        // A cleared store is still usable, and clearing twice is fine.
        fx.store.discard_all().unwrap();
        let c = fx.store.stage_create(&fx.ws.join("c.md"), b"c").unwrap();
        assert!(c.staged_content.unwrap().exists());
    }

    #[test]
    fn a_rename_applies_through_safe_fs() {
        let fx = fixture();
        let src = fx.ws.join("inbox").join("scan_001.pdf");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "pdf-bytes").unwrap();
        let dst = fx.ws.join("inbox").join("2026 Contract.pdf");

        let change = fx.store.stage_rename(&src, &dst).unwrap();
        assert_eq!(change.summary, "Rename scan_001.pdf to 2026 Contract.pdf");
        assert_eq!(change.hash_after, change.hash_before);
        assert_eq!(change.size_after, Some(9));
        assert!(src.exists(), "staging must not move the user's file");

        let (result, op) = apply(&fx.fs, &fx.store, &change).unwrap();
        assert!(result.success);
        assert!(!src.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "pdf-bytes");
        assert_eq!(op.kind, FileOpKind::RenameMove);
        assert_eq!(op.hash_before, change.hash_before);

        let (_, undone) = fx.fs.undo(&op).unwrap();
        assert!(undone.undone);
        assert!(src.exists());
    }

    // `trash` needs a real desktop trash location, which some CI and container
    // environments do not have; the test proves the path when it is available
    // rather than failing on the environment.
    #[test]
    fn a_delete_applies_through_safe_fs() {
        let fx = fixture();
        let target = fx.ws.join("old draft.txt");
        std::fs::write(&target, "drafty").unwrap();

        let change = fx.store.stage_delete(&target).unwrap();
        assert_eq!(change.summary, "Delete old draft.txt");
        assert_eq!(change.hash_after, None);
        assert_eq!(change.size_after, None);
        assert!(target.exists(), "staging must not delete the user's file");

        let (result, op) = match apply(&fx.fs, &fx.store, &change) {
            Ok(v) => v,
            Err(StagingError::Fs(FsToolError::Trash(_, detail))) => {
                eprintln!("skipping: trash unavailable in this environment: {detail}");
                return;
            }
            Err(other) => panic!("unexpected: {other}"),
        };
        assert!(!target.exists());
        assert_eq!(result.backups.len(), 1);
        assert_eq!(op.kind, FileOpKind::DeleteToTrash);
        assert_eq!(op.hash_before, change.hash_before);
    }

    #[test]
    fn a_delete_whose_target_changed_underneath_conflicts() {
        let fx = fixture();
        let target = fx.ws.join("old draft.txt");
        std::fs::write(&target, "drafty").unwrap();
        let change = fx.store.stage_delete(&target).unwrap();

        std::fs::write(&target, "the user just wrote something important").unwrap();

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(matches!(err, StagingError::Conflict { .. }), "{err}");
        assert!(target.exists());
    }

    #[test]
    fn staging_refuses_targets_it_cannot_stand_behind() {
        let fx = fixture();
        let existing = fx.ws.join("a.txt");
        std::fs::write(&existing, "here").unwrap();
        let absent = fx.ws.join("nowhere.txt");
        let folder = fx.ws.join("subfolder");
        std::fs::create_dir_all(&folder).unwrap();

        assert!(matches!(
            fx.store.stage_create(&existing, b"x").unwrap_err(),
            StagingError::TargetExists(_)
        ));
        assert!(matches!(
            fx.store.stage_modify(&absent, b"x").unwrap_err(),
            StagingError::TargetMissing(_)
        ));
        assert!(matches!(
            fx.store.stage_delete(&absent).unwrap_err(),
            StagingError::TargetMissing(_)
        ));
        assert!(matches!(
            fx.store.stage_modify(&folder, b"x").unwrap_err(),
            StagingError::NotAFile(_)
        ));
        assert!(matches!(
            fx.store.stage_rename(&existing, &existing).unwrap_err(),
            StagingError::TargetExists(_)
        ));
    }

    #[test]
    fn applying_out_of_scope_is_still_refused_by_safe_fs() {
        let fx = fixture();
        // Staging a change to a path outside the workspace harms nothing —
        // no user file is involved until apply, which is where scope is
        // enforced.
        let outside = fx.ws.parent().unwrap().join("outside.txt");
        let change = fx.store.stage_create(&outside, b"x").unwrap();

        let err = apply(&fx.fs, &fx.store, &change).unwrap_err();
        assert!(
            matches!(err, StagingError::Fs(FsToolError::OutOfScope(_))),
            "{err}"
        );
        assert!(!outside.exists());
    }

    #[test]
    fn a_staged_change_survives_a_json_round_trip() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "original").unwrap();
        let change = fx.store.stage_modify(&target, b"revised").unwrap();

        let json = serde_json::to_string(&change).unwrap();
        let back: StagedChange = serde_json::from_str(&json).unwrap();
        assert_eq!(back, change);
        assert_eq!(fx.store.staged_bytes(&back).unwrap(), b"revised");
    }

    #[test]
    fn ids_are_prefixed_and_unique() {
        let a = StagedChangeId::generate();
        let b = StagedChangeId::generate();
        assert!(a.0.starts_with("stg_"), "{a}");
        assert_ne!(a, b);
    }
}
