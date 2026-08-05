//! Safe, verified, journaled file mutations.
//!
//! Every operation:
//! 1. resolves paths through the workspace [`PathGuard`] and refuses
//!    out-of-scope or protected targets (defense-in-depth — policy was
//!    already evaluated upstream);
//! 2. backs up originals before changing them;
//! 3. verifies the on-disk result (existence + BLAKE3 content hash) before
//!    reporting success;
//! 4. returns an [`OperationResult`] plus a journaled [`FileOperation`]
//!    whose inverse powers undo.

use crate::backup::BackupStore;
use crate::inspect::hash_file;
use crate::journal::{FileOpKind, FileOperation};
use commonspace_core::{OperationResult, ValidationOutcome};
use commonspace_permissions::{is_protected_location, PathGuard, PathGuardError};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FsToolError {
    #[error(transparent)]
    Guard(#[from] PathGuardError),
    #[error("\"{0}\" is outside the workspace's authorized folders")]
    OutOfScope(PathBuf),
    #[error("\"{0}\" is a protected system or credential location")]
    Protected(PathBuf),
    #[error("\"{0}\" already exists")]
    AlreadyExists(PathBuf),
    #[error("\"{0}\" does not exist")]
    NotFound(PathBuf),
    #[error("verification failed after {action}: {detail}")]
    VerificationFailed {
        action: &'static str,
        detail: String,
    },
    #[error("undo is not possible: {0}")]
    UndoUnavailable(String),
    #[error("could not move \"{0}\" to the trash: {1}")]
    Trash(PathBuf, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Safe filesystem operations scoped to one workspace.
pub struct SafeFs {
    guard: PathGuard,
    backups: BackupStore,
}

impl SafeFs {
    pub fn new(guard: PathGuard, backups: BackupStore) -> Self {
        Self { guard, backups }
    }

    /// Resolve + enforce scope and protection. All mutating entry points go
    /// through here.
    fn checked(&self, path: &Path) -> Result<PathBuf, FsToolError> {
        let resolved = self.guard.resolve(path)?;
        if is_protected_location(&resolved.resolved) {
            return Err(FsToolError::Protected(resolved.resolved));
        }
        if !resolved.in_scope() {
            return Err(FsToolError::OutOfScope(resolved.resolved));
        }
        Ok(resolved.resolved)
    }

    /// Create a new file with the given contents. Fails if the file exists.
    pub fn create_file(
        &self,
        path: &Path,
        contents: &[u8],
    ) -> Result<(OperationResult, FileOperation), FsToolError> {
        let target = self.checked(path)?;
        if target.exists() {
            return Err(FsToolError::AlreadyExists(target));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic-ish create: write a temp sibling, then rename into place.
        let tmp = temp_sibling(&target);
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, &target).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;

        let expected = blake3::hash(contents).to_hex().to_string();
        let actual = hash_file(&target)?;
        if actual != expected {
            return Err(FsToolError::VerificationFailed {
                action: "create",
                detail: format!("content hash mismatch for {}", target.display()),
            });
        }

        let mut op = FileOperation::new(FileOpKind::Create, target.clone());
        op.hash_after = Some(actual);

        let mut result = OperationResult::ok(format!("Created {}", display_name(&target)));
        result.created.push(target);
        result.validation = ValidationOutcome::Passed;
        Ok((result, op))
    }

    /// Replace an existing file's contents. A backup is always taken first.
    ///
    /// The write itself is in-place (not atomic) because Windows cannot
    /// rename over an open/existing file reliably; the backup plus post-write
    /// hash verification is the recovery guarantee.
    pub fn overwrite_file(
        &self,
        path: &Path,
        contents: &[u8],
    ) -> Result<(OperationResult, FileOperation), FsToolError> {
        let target = self.checked(path)?;
        if !target.is_file() {
            return Err(FsToolError::NotFound(target));
        }
        let hash_before = hash_file(&target)?;
        let backup = self.backups.backup(&target)?;

        std::fs::write(&target, contents)?;

        let expected = blake3::hash(contents).to_hex().to_string();
        let actual = hash_file(&target)?;
        if actual != expected {
            return Err(FsToolError::VerificationFailed {
                action: "modify",
                detail: format!(
                    "content hash mismatch for {}; original preserved at {}",
                    target.display(),
                    backup.display()
                ),
            });
        }

        let mut op = FileOperation::new(FileOpKind::Modify, target.clone());
        op.backup = Some(backup.clone());
        op.hash_before = Some(hash_before);
        op.hash_after = Some(actual);

        let mut result = OperationResult::ok(format!("Updated {}", display_name(&target)));
        result.modified.push(target);
        result.backups.push(backup);
        result.validation = ValidationOutcome::Passed;
        Ok((result, op))
    }

    /// Rename or move a file. Content must be unchanged afterwards.
    pub fn rename_or_move(
        &self,
        from: &Path,
        to: &Path,
    ) -> Result<(OperationResult, FileOperation), FsToolError> {
        let src = self.checked(from)?;
        let dst = self.checked(to)?;
        if !src.exists() {
            return Err(FsToolError::NotFound(src));
        }
        if dst.exists() {
            return Err(FsToolError::AlreadyExists(dst));
        }
        let hash_before = if src.is_file() {
            Some(hash_file(&src)?)
        } else {
            None
        };
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&src, &dst)?;

        if let Some(expected) = &hash_before {
            let actual = hash_file(&dst)?;
            if &actual != expected {
                return Err(FsToolError::VerificationFailed {
                    action: "move",
                    detail: format!("content changed while moving to {}", dst.display()),
                });
            }
        } else if !dst.exists() {
            return Err(FsToolError::VerificationFailed {
                action: "move",
                detail: format!("{} missing after move", dst.display()),
            });
        }

        let mut op = FileOperation::new(FileOpKind::RenameMove, src.clone());
        op.destination = Some(dst.clone());
        op.hash_before = hash_before.clone();
        op.hash_after = hash_before;

        let mut result = OperationResult::ok(format!(
            "Moved {} to {}",
            display_name(&src),
            display_name(&dst)
        ));
        result.modified.push(dst);
        result.validation = ValidationOutcome::Passed;
        Ok((result, op))
    }

    /// Delete a file to the OS trash. A backup copy is taken first so undo
    /// never depends on platform trash-restore APIs.
    pub fn delete_to_trash(
        &self,
        path: &Path,
    ) -> Result<(OperationResult, FileOperation), FsToolError> {
        let target = self.checked(path)?;
        if !target.is_file() {
            return Err(FsToolError::NotFound(target));
        }
        let hash_before = hash_file(&target)?;
        let backup = self.backups.backup(&target)?;

        trash::delete(&target).map_err(|e| FsToolError::Trash(target.clone(), e.to_string()))?;

        if target.exists() {
            return Err(FsToolError::VerificationFailed {
                action: "delete",
                detail: format!("{} still present after trashing", target.display()),
            });
        }

        let mut op = FileOperation::new(FileOpKind::DeleteToTrash, target.clone());
        op.backup = Some(backup.clone());
        op.hash_before = Some(hash_before);

        let mut result =
            OperationResult::ok(format!("Moved {} to the trash", display_name(&target)));
        result.backups.push(backup);
        result.validation = ValidationOutcome::Passed;
        Ok((result, op))
    }

    /// Undo a journaled operation, verifying it is still safely reversible.
    /// Returns the updated journal record (marked `undone`).
    pub fn undo(
        &self,
        op: &FileOperation,
    ) -> Result<(OperationResult, FileOperation), FsToolError> {
        if op.undone {
            return Err(FsToolError::UndoUnavailable(
                "this change was already undone".into(),
            ));
        }
        let mut record = op.clone();
        let result =
            match op.kind {
                FileOpKind::Create => {
                    let target = self.checked(&op.source)?;
                    verify_unchanged(&target, op.hash_after.as_deref(), "created file")?;
                    trash::delete(&target)
                        .map_err(|e| FsToolError::Trash(target.clone(), e.to_string()))?;
                    OperationResult::ok(format!(
                        "Removed {} (moved to the trash)",
                        display_name(&target)
                    ))
                }
                FileOpKind::Modify => {
                    let target = self.checked(&op.source)?;
                    let backup = op.backup.as_ref().ok_or_else(|| {
                        FsToolError::UndoUnavailable("no backup was recorded".into())
                    })?;
                    verify_unchanged(&target, op.hash_after.as_deref(), "modified file")?;
                    std::fs::copy(backup, &target)?;
                    let restored = hash_file(&target)?;
                    if Some(restored.as_str()) != op.hash_before.as_deref() {
                        return Err(FsToolError::VerificationFailed {
                            action: "undo",
                            detail: "restored content does not match the original".into(),
                        });
                    }
                    OperationResult::ok(format!(
                        "Restored the previous version of {}",
                        display_name(&target)
                    ))
                }
                FileOpKind::RenameMove => {
                    let dest = op.destination.as_ref().ok_or_else(|| {
                        FsToolError::UndoUnavailable("no destination was recorded".into())
                    })?;
                    let current = self.checked(dest)?;
                    let original = self.checked(&op.source)?;
                    verify_unchanged(&current, op.hash_after.as_deref(), "moved file")?;
                    if original.exists() {
                        return Err(FsToolError::UndoUnavailable(format!(
                            "a new file now exists at {}",
                            original.display()
                        )));
                    }
                    std::fs::rename(&current, &original)?;
                    OperationResult::ok(format!("Moved {} back", display_name(&original)))
                }
                FileOpKind::DeleteToTrash => {
                    let target = self.checked(&op.source)?;
                    let backup = op.backup.as_ref().ok_or_else(|| {
                        FsToolError::UndoUnavailable("no backup was recorded".into())
                    })?;
                    if target.exists() {
                        return Err(FsToolError::UndoUnavailable(format!(
                            "a new file now exists at {}",
                            target.display()
                        )));
                    }
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(backup, &target)?;
                    let restored = hash_file(&target)?;
                    if Some(restored.as_str()) != op.hash_before.as_deref() {
                        return Err(FsToolError::VerificationFailed {
                            action: "undo",
                            detail: "restored content does not match the original".into(),
                        });
                    }
                    OperationResult::ok(format!("Restored {}", display_name(&target)))
                }
            };
        record.undone = true;
        Ok((result, record))
    }
}

/// Refuse to undo when the file changed since the operation (hash mismatch).
fn verify_unchanged(
    path: &Path,
    expected_hash: Option<&str>,
    what: &str,
) -> Result<(), FsToolError> {
    let Some(expected) = expected_hash else {
        return Err(FsToolError::UndoUnavailable(format!(
            "no hash was recorded for the {what}"
        )));
    };
    if !path.exists() {
        return Err(FsToolError::UndoUnavailable(format!(
            "the {what} no longer exists"
        )));
    }
    let actual = hash_file(path)?;
    if actual != expected {
        return Err(FsToolError::UndoUnavailable(format!(
            "the {what} has been changed since; undoing would lose those changes"
        )));
    }
    Ok(())
}

fn temp_sibling(target: &Path) -> PathBuf {
    let unique = uuid::Uuid::new_v4().simple().to_string();
    target.with_extension(format!("commonspace-tmp-{unique}"))
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

    struct Fixture {
        _tmp: tempfile::TempDir,
        ws: PathBuf,
        fs: SafeFs,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let fs = SafeFs::new(
            PathGuard::new([&ws]),
            BackupStore::new(tmp.path().join("backups")),
        );
        Fixture { _tmp: tmp, ws, fs }
    }

    #[test]
    fn create_verifies_and_journals() {
        let fx = fixture();
        let target = fx.ws.join("docs").join("naïve 报告.md");
        let (result, op) = fx.fs.create_file(&target, "# report".as_bytes()).unwrap();
        assert!(result.success);
        assert_eq!(result.created.len(), 1);
        assert_eq!(result.validation, ValidationOutcome::Passed);
        assert_eq!(op.kind, FileOpKind::Create);
        assert!(op.hash_after.is_some());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "# report");
    }

    #[test]
    fn create_refuses_existing() {
        let fx = fixture();
        let target = fx.ws.join("a.txt");
        std::fs::write(&target, "old").unwrap();
        let err = fx.fs.create_file(&target, b"new").unwrap_err();
        assert!(matches!(err, FsToolError::AlreadyExists(_)));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    }

    #[test]
    fn create_out_of_scope_refused() {
        let fx = fixture();
        let outside = fx.ws.parent().unwrap().join("outside.txt");
        let err = fx.fs.create_file(&outside, b"x").unwrap_err();
        assert!(matches!(err, FsToolError::OutOfScope(_)));
        assert!(!outside.exists());
    }

    #[test]
    fn overwrite_backs_up_and_undo_restores() {
        let fx = fixture();
        let target = fx.ws.join("contract.md");
        std::fs::write(&target, "original terms").unwrap();
        let (result, op) = fx.fs.overwrite_file(&target, b"revised terms").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "revised terms");
        assert_eq!(result.backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&result.backups[0]).unwrap(),
            "original terms"
        );

        let (undo_result, undone) = fx.fs.undo(&op).unwrap();
        assert!(undo_result.success);
        assert!(undone.undone);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original terms");
    }

    #[test]
    fn undo_refuses_when_file_changed_since() {
        let fx = fixture();
        let target = fx.ws.join("a.txt");
        std::fs::write(&target, "v1").unwrap();
        let (_, op) = fx.fs.overwrite_file(&target, b"v2").unwrap();
        std::fs::write(&target, "user edited afterwards").unwrap();
        let err = fx.fs.undo(&op).unwrap_err();
        assert!(matches!(err, FsToolError::UndoUnavailable(_)));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "user edited afterwards"
        );
    }

    #[test]
    fn rename_move_and_undo() {
        let fx = fixture();
        let src = fx.ws.join("inbox").join("scan_001.pdf");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "pdf-bytes").unwrap();
        let dst = fx.ws.join("archive").join("2026 Contract.pdf");

        let (result, op) = fx.fs.rename_or_move(&src, &dst).unwrap();
        assert!(result.success);
        assert!(!src.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "pdf-bytes");

        let (_, undone) = fx.fs.undo(&op).unwrap();
        assert!(undone.undone);
        assert!(src.exists());
        assert!(!dst.exists());
    }

    #[test]
    fn move_refuses_overwriting_destination() {
        let fx = fixture();
        let src = fx.ws.join("a.txt");
        let dst = fx.ws.join("b.txt");
        std::fs::write(&src, "a").unwrap();
        std::fs::write(&dst, "precious").unwrap();
        let err = fx.fs.rename_or_move(&src, &dst).unwrap_err();
        assert!(matches!(err, FsToolError::AlreadyExists(_)));
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "precious");
    }

    #[test]
    fn undo_create_removes_file() {
        let fx = fixture();
        let target = fx.ws.join("generated.md");
        let (_, op) = fx.fs.create_file(&target, b"gen").unwrap();
        let (_, undone) = fx.fs.undo(&op).unwrap();
        assert!(undone.undone);
        assert!(!target.exists());
    }

    #[test]
    fn double_undo_refused() {
        let fx = fixture();
        let target = fx.ws.join("x.txt");
        std::fs::write(&target, "1").unwrap();
        let (_, op) = fx.fs.overwrite_file(&target, b"2").unwrap();
        let (_, undone) = fx.fs.undo(&op).unwrap();
        let err = fx.fs.undo(&undone).unwrap_err();
        assert!(matches!(err, FsToolError::UndoUnavailable(_)));
    }

    // `trash` on Windows moves to the Recycle Bin, which works in CI-less
    // local runs; the delete tests are best-effort cross-platform.
    #[test]
    fn delete_backs_up_then_trashes_and_undo_restores() {
        let fx = fixture();
        let target = fx.ws.join("old draft.txt");
        std::fs::write(&target, "drafty").unwrap();
        let deleted = match fx.fs.delete_to_trash(&target) {
            Ok(v) => v,
            Err(FsToolError::Trash(_, detail)) => {
                eprintln!("skipping: trash unavailable in this environment: {detail}");
                return;
            }
            Err(other) => panic!("unexpected: {other}"),
        };
        let (result, op) = deleted;
        assert!(!target.exists());
        assert_eq!(result.backups.len(), 1);

        let (_, undone) = fx.fs.undo(&op).unwrap();
        assert!(undone.undone);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "drafty");
    }

    #[cfg(windows)]
    #[test]
    // Clearing the read-only flag is the point of the cleanup below.
    #[allow(clippy::permissions_set_readonly_false)]
    fn read_only_target_fails_gracefully() {
        let fx = fixture();
        let target = fx.ws.join("locked.txt");
        std::fs::write(&target, "ro").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&target, perms).unwrap();

        let err = fx.fs.overwrite_file(&target, b"nope");
        assert!(
            err.is_err(),
            "overwriting a read-only file must error, not panic"
        );

        // Cleanup so the tempdir can be removed.
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
}
