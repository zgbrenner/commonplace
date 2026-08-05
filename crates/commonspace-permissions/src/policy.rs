//! The policy table. Implements the default policy from docs/permissions.md
//! as explicit, testable Rust.

use crate::path_guard::{PathGuard, PathGuardError};
use crate::protected::is_protected_location;
use commonspace_core::{OperationClass, PolicyVerdict};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User setting for how modifications to existing files are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifyPolicy {
    /// Show a preview/approval before applying (default).
    #[default]
    RequireApproval,
    /// Apply immediately; backup + undo still always happen.
    AllowWithBackup,
}

/// Workspace-level policy settings. The derived defaults are the safe
/// defaults from docs/permissions.md: modifications need approval, permanent
/// deletion off, network approval per request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PolicySettings {
    pub modify: ModifyPolicy,
    /// Permanent deletion (bypassing safe trash). Off by default.
    pub permanent_delete_enabled: bool,
    /// Network fetches allowed without per-request approval. Off by default.
    pub network_pre_approved: bool,
}

/// One evaluated request. `targets` are the paths the operation touches;
/// `destination` is set for move/rename targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRequest {
    pub class: OperationClass,
    pub targets: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    /// True when the caller wants a permanent (non-trash) delete.
    #[serde(default)]
    pub permanent: bool,
}

/// The deterministic policy engine for one workspace.
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    guard: PathGuard,
    settings: PolicySettings,
}

impl PolicyEngine {
    pub fn new(guard: PathGuard, settings: PolicySettings) -> Self {
        Self { guard, settings }
    }

    pub fn guard(&self) -> &PathGuard {
        &self.guard
    }

    /// Evaluate a request. Deny > RequireApproval > Allow across all targets.
    pub fn evaluate(&self, request: &PolicyRequest) -> Result<PolicyVerdict, PathGuardError> {
        use OperationClass::*;

        // Class-level absolutes that need no path inspection.
        match request.class {
            Secret => {
                return Ok(deny("Access to credentials and secrets is not permitted."));
            }
            Send | Publish => {
                return Ok(approval(
                    "Sending messages or making external changes always needs your approval.",
                ));
            }
            Upload => {
                return Ok(approval("Uploading files always needs your approval."));
            }
            NetworkFetch => {
                return Ok(if self.settings.network_pre_approved {
                    PolicyVerdict::Allow
                } else {
                    approval("Fetching from the internet needs your approval.")
                });
            }
            _ => {}
        }

        if request.class == Delete && request.permanent && !self.settings.permanent_delete_enabled {
            return Ok(deny(
                "Permanent deletion is disabled. Files can be moved to the trash instead.",
            ));
        }

        let mut verdict = PolicyVerdict::Allow;

        let mut all_paths: Vec<&PathBuf> = request.targets.iter().collect();
        if let Some(dest) = &request.destination {
            all_paths.push(dest);
        }

        let mut resolved_targets = Vec::with_capacity(request.targets.len());
        for path in all_paths {
            let resolved = self.guard.resolve(path)?;
            if is_protected_location(&resolved.resolved) {
                return Ok(deny(format!(
                    "\"{}\" is a protected system or credential location.",
                    resolved.resolved.display()
                )));
            }
            if !resolved.in_scope() {
                // Out-of-scope access is an approval (native-picker grant),
                // never silent.
                verdict = strongest(
                    verdict,
                    approval(format!(
                        "\"{}\" is outside this workspace's authorized folders.",
                        resolved.resolved.display()
                    )),
                );
            }
            resolved_targets.push(resolved);
        }

        let class_verdict = match request.class {
            OperationClass::Read | OperationClass::Create => PolicyVerdict::Allow,
            OperationClass::Modify => match self.settings.modify {
                ModifyPolicy::RequireApproval => {
                    approval("Changing an existing file needs your review.")
                }
                ModifyPolicy::AllowWithBackup => PolicyVerdict::Allow,
            },
            OperationClass::Rename => {
                if request.targets.len() > 1 {
                    approval(format!(
                        "Renaming {} files at once needs your approval.",
                        request.targets.len()
                    ))
                } else {
                    PolicyVerdict::Allow
                }
            }
            OperationClass::Move => {
                let cross_folder = match (&request.destination, resolved_targets.first()) {
                    (Some(dest), Some(src)) => {
                        let dest_parent = self
                            .guard
                            .resolve(dest)?
                            .resolved
                            .parent()
                            .map(PathBuf::from);
                        let src_parent = src.resolved.parent().map(PathBuf::from);
                        dest_parent != src_parent
                    }
                    _ => true,
                };
                if cross_folder || request.targets.len() > 1 {
                    approval("Moving files between folders needs your approval.")
                } else {
                    PolicyVerdict::Allow
                }
            }
            OperationClass::Delete => approval("Deleting files always needs your approval."),
            OperationClass::Execute => {
                approval("Running a program or installer always needs your approval.")
            }
            OperationClass::Install => approval("Installing software needs your approval."),
            // Handled in the early match; unreachable here.
            OperationClass::NetworkFetch
            | OperationClass::Upload
            | OperationClass::Send
            | OperationClass::Publish
            | OperationClass::Secret => PolicyVerdict::Allow,
        };

        Ok(strongest(verdict, class_verdict))
    }
}

fn deny(reason: impl Into<String>) -> PolicyVerdict {
    PolicyVerdict::Deny {
        reason: reason.into(),
    }
}

fn approval(reason: impl Into<String>) -> PolicyVerdict {
    PolicyVerdict::RequireApproval {
        reason: reason.into(),
    }
}

fn rank(v: &PolicyVerdict) -> u8 {
    match v {
        PolicyVerdict::Allow => 0,
        PolicyVerdict::RequireApproval { .. } => 1,
        PolicyVerdict::Deny { .. } => 2,
    }
}

fn strongest(a: PolicyVerdict, b: PolicyVerdict) -> PolicyVerdict {
    if rank(&b) > rank(&a) {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonspace_core::OperationClass::*;
    use std::path::Path;

    fn engine_for(root: &Path) -> PolicyEngine {
        PolicyEngine::new(PathGuard::new([root]), PolicySettings::default())
    }

    fn req(class: OperationClass, targets: Vec<PathBuf>) -> PolicyRequest {
        PolicyRequest {
            class,
            targets,
            destination: None,
            permanent: false,
        }
    }

    #[test]
    fn read_in_scope_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let v = engine_for(dir.path())
            .evaluate(&req(Read, vec![f]))
            .unwrap();
        assert_eq!(v, PolicyVerdict::Allow);
    }

    #[test]
    fn create_in_scope_allowed_even_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let v = engine_for(dir.path())
            .evaluate(&req(Create, vec![dir.path().join("new/report.md")]))
            .unwrap();
        assert_eq!(v, PolicyVerdict::Allow);
    }

    #[test]
    fn read_out_of_scope_requires_approval() {
        let ws = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let f = other.path().join("outside.txt");
        std::fs::write(&f, "x").unwrap();
        let v = engine_for(ws.path()).evaluate(&req(Read, vec![f])).unwrap();
        assert!(matches!(v, PolicyVerdict::RequireApproval { .. }));
    }

    #[test]
    fn modify_defaults_to_approval() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let v = engine_for(dir.path())
            .evaluate(&req(Modify, vec![f]))
            .unwrap();
        assert!(matches!(v, PolicyVerdict::RequireApproval { .. }));
    }

    #[test]
    fn single_rename_allowed_batch_needs_approval() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "x").unwrap();
        let e = engine_for(dir.path());
        assert_eq!(
            e.evaluate(&req(Rename, vec![a.clone()])).unwrap(),
            PolicyVerdict::Allow
        );
        assert!(matches!(
            e.evaluate(&req(Rename, vec![a, b])).unwrap(),
            PolicyVerdict::RequireApproval { .. }
        ));
    }

    #[test]
    fn cross_folder_move_needs_approval() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in/a.txt");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "x").unwrap();
        let e = engine_for(dir.path());
        let mut r = req(Move, vec![src.clone()]);
        r.destination = Some(dir.path().join("out/a.txt"));
        assert!(matches!(
            e.evaluate(&r).unwrap(),
            PolicyVerdict::RequireApproval { .. }
        ));
        // Same-folder single move (rename-like) is allowed.
        let mut r2 = req(Move, vec![src]);
        r2.destination = Some(dir.path().join("in/b.txt"));
        assert_eq!(e.evaluate(&r2).unwrap(), PolicyVerdict::Allow);
    }

    #[test]
    fn delete_always_needs_approval_permanent_denied() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let e = engine_for(dir.path());
        assert!(matches!(
            e.evaluate(&req(Delete, vec![f.clone()])).unwrap(),
            PolicyVerdict::RequireApproval { .. }
        ));
        let mut r = req(Delete, vec![f]);
        r.permanent = true;
        assert!(matches!(
            e.evaluate(&r).unwrap(),
            PolicyVerdict::Deny { .. }
        ));
    }

    #[test]
    fn execute_install_need_approval() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("setup.exe");
        std::fs::write(&f, "x").unwrap();
        let e = engine_for(dir.path());
        for class in [Execute, Install] {
            assert!(matches!(
                e.evaluate(&req(class, vec![f.clone()])).unwrap(),
                PolicyVerdict::RequireApproval { .. }
            ));
        }
    }

    #[test]
    fn secrets_denied_send_publish_upload_gated() {
        let e = engine_for(tempfile::tempdir().unwrap().path());
        assert!(matches!(
            e.evaluate(&req(Secret, vec![])).unwrap(),
            PolicyVerdict::Deny { .. }
        ));
        for class in [Send, Publish, Upload] {
            assert!(matches!(
                e.evaluate(&req(class, vec![])).unwrap(),
                PolicyVerdict::RequireApproval { .. }
            ));
        }
    }

    #[test]
    fn protected_location_denied_for_read() {
        let home = dirs::home_dir().unwrap();
        let e = engine_for(&home); // even with home itself authorized
        let v = e
            .evaluate(&req(Read, vec![home.join(".ssh/id_ed25519")]))
            .unwrap();
        assert!(matches!(v, PolicyVerdict::Deny { .. }));
    }

    #[test]
    fn deny_outranks_approval_in_batches() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("a.txt");
        std::fs::write(&ok, "x").unwrap();
        let secret = dirs::home_dir().unwrap().join(".ssh/id_ed25519");
        let v = engine_for(dir.path())
            .evaluate(&req(Delete, vec![ok, secret]))
            .unwrap();
        assert!(matches!(v, PolicyVerdict::Deny { .. }));
    }
}
