//! Path resolution and workspace-scope containment.
//!
//! Threat cases handled here (see THREAT_MODEL.md §3):
//! - `..` traversal: evaluation happens on resolved absolute paths.
//! - Symlinks/junctions: the *resolved target* must be inside a root.
//! - Windows quirks: `\\?\` verbatim prefixes normalized via `dunce`;
//!   reserved device names rejected; alternate data streams rejected.

use std::path::{Path, PathBuf};
use thiserror::Error;

/// A path that has been canonicalized and scope-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// Fully resolved absolute path (symlinks followed, `..` eliminated,
    /// Windows verbatim prefix simplified where unambiguous).
    pub resolved: PathBuf,
    /// The authorized root containing it, if any.
    pub within_root: Option<PathBuf>,
}

impl ResolvedPath {
    pub fn in_scope(&self) -> bool {
        self.within_root.is_some()
    }
}

#[derive(Debug, Error)]
pub enum PathGuardError {
    #[error("path could not be resolved: {path}: {source}")]
    Resolve {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reserved device name is not allowed: {0}")]
    ReservedName(String),
    #[error("alternate data streams are not allowed: {0}")]
    AlternateDataStream(String),
    #[error("relative paths are not accepted at the policy boundary: {0}")]
    RelativePath(PathBuf),
}

/// Component-wise containment check. Case-insensitive on Windows, where the
/// filesystem itself is case-insensitive and env-derived paths (`C:\WINDOWS`)
/// may differ in case from on-disk casing. Lowercase folding is a slightly
/// conservative approximation of NTFS case folding; for the protected-location
/// check that errs toward denying, which is the safe direction.
pub(crate) fn path_starts_with(child: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let fold = |p: &Path| -> Vec<String> {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect()
        };
        let b = fold(base);
        let c = fold(child);
        c.len() >= b.len() && b.iter().zip(c.iter()).all(|(x, y)| x == y)
    }
    #[cfg(not(windows))]
    {
        child.starts_with(base)
    }
}

/// Windows reserved device names (case-insensitive, with or without extension).
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Resolves paths and checks containment against a workspace's authorized roots.
#[derive(Debug, Clone)]
pub struct PathGuard {
    /// Canonicalized authorized roots.
    roots: Vec<PathBuf>,
}

impl PathGuard {
    /// Build a guard from authorized roots. Roots that fail to canonicalize
    /// (e.g. removed folders) are skipped — a vanished root must never
    /// silently widen scope, and narrowing is the safe direction.
    pub fn new<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let roots = roots
            .into_iter()
            .filter_map(|r| soft_canonicalize::soft_canonicalize(r.as_ref()).ok())
            .map(|p| dunce::simplified(&p).to_path_buf())
            .collect();
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Resolve a requested path and determine scope membership.
    ///
    /// The target may not exist yet (create operations): resolution follows
    /// every existing ancestor (including symlinks) and appends the
    /// non-existing tail lexically, so a symlinked parent cannot smuggle a
    /// write outside the roots.
    pub fn resolve(&self, requested: &Path) -> Result<ResolvedPath, PathGuardError> {
        if requested.is_relative() {
            return Err(PathGuardError::RelativePath(requested.to_path_buf()));
        }
        reject_windows_hazards(requested)?;

        let resolved = soft_canonicalize::soft_canonicalize(requested).map_err(|source| {
            PathGuardError::Resolve { path: requested.to_path_buf(), source }
        })?;
        let resolved = dunce::simplified(&resolved).to_path_buf();

        // Component-wise containment, not string prefix: "C:\ws-evil" must
        // not match root "C:\ws". Case-insensitive on Windows.
        let within_root = self
            .roots
            .iter()
            .find(|root| path_starts_with(&resolved, root))
            .cloned();

        Ok(ResolvedPath { resolved, within_root })
    }
}

/// Reject reserved device names and alternate data streams anywhere in the
/// requested path. Applied to the *requested* path (before resolution) and
/// cheap enough to run always, on every platform — a workspace may be synced
/// to a Windows machine even if this process runs elsewhere.
fn reject_windows_hazards(path: &Path) -> Result<(), PathGuardError> {
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            continue;
        };
        let Some(part) = part.to_str() else {
            continue; // non-UTF8 components can't be reserved names
        };
        let stem = part.split('.').next().unwrap_or(part);
        if RESERVED_NAMES.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
            return Err(PathGuardError::ReservedName(part.to_string()));
        }
        // "name:stream" is an NTFS alternate data stream. Windows absolute
        // paths carry the drive colon in a Prefix component, not a Normal
        // one, so any colon here is suspect.
        if part.contains(':') {
            return Err(PathGuardError::AlternateDataStream(part.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn contains_paths_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("docs").join("a.txt");
        touch(&file);
        let guard = PathGuard::new([dir.path()]);
        let r = guard.resolve(&file).unwrap();
        assert!(r.in_scope());
    }

    #[test]
    fn traversal_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let guard = PathGuard::new([dir.path().join("ws")]);
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        let sneaky = dir.path().join("ws").join("..").join("outside.txt");
        let r = guard.resolve(&sneaky).unwrap();
        assert!(!r.in_scope(), "resolved {:?} must be out of scope", r.resolved);
    }

    #[test]
    fn sibling_prefix_name_is_not_contained() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        let evil = dir.path().join("ws-evil").join("f.txt");
        std::fs::create_dir_all(&root).unwrap();
        touch(&evil);
        let guard = PathGuard::new([&root]);
        let r = guard.resolve(&evil).unwrap();
        assert!(!r.in_scope());
    }

    #[test]
    fn nonexistent_create_target_stays_in_scope_rules() {
        let dir = tempfile::tempdir().unwrap();
        let guard = PathGuard::new([dir.path()]);
        let new_file = dir.path().join("new").join("report.docx");
        let r = guard.resolve(&new_file).unwrap();
        assert!(r.in_scope());
        let outside = dir.path().join("..").join("nope.txt");
        let r = guard.resolve(&outside).unwrap();
        assert!(!r.in_scope());
    }

    #[test]
    fn reserved_names_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let guard = PathGuard::new([dir.path()]);
        for name in ["CON", "con.txt", "Nul", "COM7.log"] {
            let err = guard.resolve(&dir.path().join(name)).unwrap_err();
            assert!(matches!(err, PathGuardError::ReservedName(_)), "{name}");
        }
    }

    #[test]
    fn alternate_data_streams_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let guard = PathGuard::new([dir.path()]);
        let err = guard.resolve(&dir.path().join("report.txt:hidden")).unwrap_err();
        assert!(matches!(err, PathGuardError::AlternateDataStream(_)));
    }

    #[test]
    fn relative_paths_rejected() {
        let guard = PathGuard::new(Vec::<PathBuf>::new());
        let err = guard.resolve(Path::new("relative/path.txt")).unwrap_err();
        assert!(matches!(err, PathGuardError::RelativePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_outside_root_is_out_of_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        let outside = dir.path().join("secret.txt");
        std::fs::create_dir_all(&root).unwrap();
        touch(&outside);
        let link = root.join("innocent.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let guard = PathGuard::new([&root]);
        let r = guard.resolve(&link).unwrap();
        assert!(!r.in_scope(), "symlink target {:?} escaped", r.resolved);
    }

    #[cfg(windows)]
    #[test]
    fn symlink_target_outside_root_is_out_of_scope() {
        // Symlink creation on Windows requires Developer Mode or elevation;
        // skip (pass vacuously) when unavailable rather than fail the suite.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        let outside = dir.path().join("secret.txt");
        std::fs::create_dir_all(&root).unwrap();
        touch(&outside);
        let link = root.join("innocent.txt");
        if std::os::windows::fs::symlink_file(&outside, &link).is_err() {
            eprintln!("skipping: symlink creation unavailable (needs Developer Mode)");
            return;
        }
        let guard = PathGuard::new([&root]);
        let r = guard.resolve(&link).unwrap();
        assert!(!r.in_scope(), "symlink target {:?} escaped", r.resolved);
    }
}
