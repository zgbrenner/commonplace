//! The backup store: copies of files taken before any modification or
//! deletion. Backups live outside the workspace (in app data) so agent
//! operations on the workspace can never touch them.

use std::io;
use std::path::{Path, PathBuf};

/// Stores timestamped backup copies under a dedicated root directory.
#[derive(Debug, Clone)]
pub struct BackupStore {
    root: PathBuf,
}

impl BackupStore {
    /// Create a store rooted at `root` (created on demand).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Copy `file` into the store and return the backup path.
    ///
    /// Layout: `<root>/<UTC yyyymmdd-hhmmss>-<8 hex chars>/<file name>` —
    /// unique per call, original file name preserved for human browsing.
    pub fn backup(&self, file: &Path) -> io::Result<PathBuf> {
        let name = file
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let unique = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let dir = self.root.join(format!("{stamp}-{unique}"));
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(name);
        std::fs::copy(file, &dest)?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_preserves_content_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BackupStore::new(tmp.path().join("backups"));
        let f = tmp.path().join("données café.txt");
        std::fs::write(&f, "important").unwrap();
        let b = store.backup(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "important");
        assert_eq!(b.file_name().unwrap(), f.file_name().unwrap());
        assert!(b.starts_with(tmp.path().join("backups")));
    }

    #[test]
    fn repeated_backups_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BackupStore::new(tmp.path().join("b"));
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "1").unwrap();
        let b1 = store.backup(&f).unwrap();
        std::fs::write(&f, "2").unwrap();
        let b2 = store.backup(&f).unwrap();
        assert_ne!(b1, b2);
        assert_eq!(std::fs::read_to_string(&b1).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(&b2).unwrap(), "2");
    }
}
