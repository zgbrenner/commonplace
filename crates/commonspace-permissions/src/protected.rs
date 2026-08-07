//! Protected operating-system locations. Denied for every operation class
//! (including read), regardless of workspace configuration. Not user-
//! configurable by design: an agent must never be able to talk a user into
//! whitelisting a credential store.

use crate::path_guard::path_starts_with;
use std::path::{Path, PathBuf};

/// Credential stores under the user profile, as home-relative paths with `/`
/// separators (accepted on Windows too, so one list serves every platform).
///
/// Two groups, one list: the user's own secrets (THREAT_MODEL.md §5), then
/// the credential homes of the provider CLIs Commonspace launches — an agent
/// must never be able to read the credentials of the tool running it.
const CREDENTIAL_STORES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".netrc",
    ".config/gcloud",
    ".config/gh",
    ".password-store",
    ".claude",
    ".codex",
    ".gemini",
    ".config/opencode",
];

/// The credential stores denied here, as home-relative paths.
///
/// Exposed so a provider adapter can mirror the same set into the CLI it
/// launches — a provider's own file-reading tools never reach this engine, so
/// without a mirror they are bounded only by that CLI's working-directory
/// rules. Sharing one list keeps the two from drifting apart.
///
/// Paths only. Whatever pattern syntax a given CLI wants is that adapter's
/// business and stays in `commonspace-agents` (docs/provider-adapters.md).
pub fn credential_store_paths() -> &'static [&'static str] {
    CREDENTIAL_STORES
}

/// True when the resolved path lies inside a protected location.
pub fn is_protected_location(resolved: &Path) -> bool {
    // The per-user temporary directory is scratch space the user owns, not a
    // system directory, and it is never a credential store. It needs an
    // explicit exception because macOS sites it under `/private/var`, which
    // *is* protected: without this, every workspace under the macOS temp
    // directory would be denied outright.
    if let Some(temp) = user_temp_dir() {
        if path_starts_with(resolved, &temp) {
            return false;
        }
    }

    protected_roots()
        .iter()
        .any(|p| path_starts_with(resolved, p))
}

/// The process temp directory, resolved. macOS reports it through the `/var`
/// symlink while resolved paths come back as `/private/var`, so it has to be
/// canonicalized before it can be compared against one.
fn user_temp_dir() -> Option<PathBuf> {
    let temp = std::env::temp_dir();
    let resolved = std::fs::canonicalize(&temp).unwrap_or(temp);
    Some(dunce::simplified(&resolved).to_path_buf())
}

/// Compute the protected roots for this machine.
///
/// Two categories:
/// 1. System directories — OS integrity.
/// 2. Credential stores in the user profile — secrets (THREAT_MODEL.md §5).
fn protected_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    // -- system directories --
    #[cfg(windows)]
    {
        if let Ok(windir) = std::env::var("SystemRoot") {
            roots.push(PathBuf::from(windir));
        } else {
            roots.push(PathBuf::from(r"C:\Windows"));
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
            if let Ok(v) = std::env::var(var) {
                roots.push(PathBuf::from(v));
            }
        }
    }
    #[cfg(not(windows))]
    {
        for p in [
            "/bin",
            "/sbin",
            "/usr",
            "/etc",
            "/boot",
            "/lib",
            "/lib64",
            "/var",
            "/proc",
            "/sys",
            "/dev",
            "/System",
            "/Library",
            "/private/etc",
            "/private/var",
        ] {
            roots.push(PathBuf::from(p));
        }
    }

    // -- credential stores under the user profile --
    if let Some(home) = dirs::home_dir() {
        for rel in CREDENTIAL_STORES {
            roots.push(home.join(rel));
        }
        #[cfg(windows)]
        {
            roots.push(
                home.join("AppData")
                    .join("Roaming")
                    .join("Microsoft")
                    .join("Credentials"),
            );
            roots.push(
                home.join("AppData")
                    .join("Local")
                    .join("Microsoft")
                    .join("Credentials"),
            );
        }
        #[cfg(target_os = "macos")]
        {
            roots.push(home.join("Library").join("Keychains"));
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            roots.push(home.join(".local").join("share").join("keyrings"));
        }
    }

    roots
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn credential_dirs_are_protected() {
        let home = dirs::home_dir().expect("home dir");
        assert!(is_protected_location(&home.join(".ssh").join("id_ed25519")));
        assert!(is_protected_location(
            &home.join(".aws").join("credentials")
        ));
        assert!(is_protected_location(
            &home.join(".claude").join("session.json")
        ));
    }

    /// The exported list is what provider adapters mirror into the CLIs they
    /// launch. If an entry ever stopped being denied here, that mirror would
    /// go on advertising a protection this engine no longer applies.
    #[test]
    fn every_exported_credential_store_is_denied() {
        let home = dirs::home_dir().expect("home dir");
        for rel in credential_store_paths() {
            let inside = home.join(rel).join("secret");
            assert!(
                is_protected_location(&inside),
                "{} is exported but not denied",
                inside.display()
            );
        }
    }

    #[test]
    fn system_dirs_are_protected() {
        #[cfg(windows)]
        assert!(is_protected_location(Path::new(
            r"C:\Windows\System32\cmd.exe"
        )));
        #[cfg(not(windows))]
        assert!(is_protected_location(Path::new("/etc/passwd")));
    }

    /// Regression test for a real failure found by CI on macOS: the per-user
    /// temp directory lives under `/private/var` there, so the blanket `/var`
    /// protection made every workspace inside it unusable.
    #[test]
    fn the_user_temp_directory_is_not_protected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("workspace").join("notes.md");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("create");
        std::fs::write(&file, "x").expect("write");
        let resolved = std::fs::canonicalize(&file).expect("canonicalize");
        assert!(
            !is_protected_location(&resolved),
            "{} should be usable as a workspace",
            resolved.display()
        );
    }

    /// The exception above must stay narrow: system areas of `/var` that are
    /// not the temp directory are still protected.
    #[cfg(target_os = "macos")]
    #[test]
    fn system_var_outside_temp_is_still_protected() {
        assert!(is_protected_location(Path::new("/private/var/db/dslocal")));
    }

    #[test]
    fn ordinary_user_paths_are_not_protected() {
        let home = dirs::home_dir().expect("home dir");
        assert!(!is_protected_location(
            &home.join("Documents").join("report.docx")
        ));
    }
}
