//! Landlock containment for a spawned provider CLI.
//!
//! [Landlock](https://landlock.io) is the Linux security module that lets an
//! ordinary process drop its own filesystem rights: no privilege, no daemon,
//! no setup. That is why it is the mechanism here. The person running
//! Commonspace is not an administrator and is never going to be asked to
//! become one, which rules out the dedicated sandbox accounts and elevated
//! installers that `THREAT_MODEL.md` records other projects requiring.
//!
//! The work splits across the spawn. [`prepare`] runs in the parent, where
//! opening paths and building a ruleset is ordinary code. [`Prepared::apply`]
//! runs in the child between `fork` and `exec`, where it is not — see
//! *Async-signal-safety* below. [`restrict`] does both in one call, for
//! callers that are not in the middle of a spawn.
//!
//! # What the child keeps
//!
//! Read and execute on the system hierarchies any Node- or Rust-based CLI
//! needs to start at all; read and write on the workspace roots, on the temp
//! directory, and on the provider's own configuration directory. Everything
//! else — the rest of the home directory, other users, every credential store
//! `commonspace-permissions` knows about — is denied by not being named.
//!
//! The provider's configuration is granted deliberately, not by oversight.
//! The CLI owns its credentials and its session state and writes to them
//! while it runs; a boundary that stopped it resuming a session would not
//! have secured anything, it would have broken the product, and the fix a
//! person reaches for then is turning the boundary off.
//!
//! # What this deliberately does not do
//!
//! It does not touch the network. Landlock ABI v4 can restrict TCP bind and
//! connect, and handling that access would deny every port not explicitly
//! named — including the one the CLI talks to its own provider on, and the
//! loopback port Commonspace serves its MCP tools from. Whether an agent may
//! reach the network is a decision Commonspace makes at the tool layer, in
//! terms the user can see and revoke. It is not made here, and this is not an
//! omission to be tidied up later.
//!
//! # Async-signal-safety
//!
//! After `fork` the child holds copies of locks other threads owned at the
//! moment of the fork, including the allocator's, so anything that allocates
//! can deadlock there. Everything allocatable is therefore hoisted into
//! [`prepare`]: the path strings, the `open` calls behind each rule, the
//! ruleset descriptor, and the [`Containment`] that describes the result.
//! What is left for the child is one `fcntl`, one `prctl` and one
//! `landlock_restrict_self` — three syscalls, no allocation, no locks, no
//! logging.
//!
//! This is why the split exists rather than a single child-side call. Codex
//! reached the same constraint and answered it by re-executing a helper
//! binary; hoisting into the parent gets the same guarantee without a second
//! process, because a `SandboxPolicy` is fully known before the spawn.
//!
//! # The one way this can break a spawn
//!
//! `execve` needs execute rights on the program and on its interpreter, and
//! Landlock is applied just before it. A `policy.readable` that does not name
//! the directory the provider CLI lives in makes the `execve` fail with
//! `EACCES`, and the spawn fails with it. Provider CLIs installed through npm
//! or a user-level installer live under the home directory, not under `/usr`,
//! so this is the normal case rather than the exotic one. The caller resolves
//! that path already and must include it.

use super::{Containment, SandboxPolicy};
use landlock::{
    path_beneath_rules, Access, AccessFs, BitFlags, CompatLevel, Compatible, CreateRulesetError,
    Ruleset, RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetError, ABI,
};
use std::path::PathBuf;

/// Named in [`Containment`] and in the diagnostics report.
const MECHANISM: &str = "landlock";

/// The ABI this ruleset is written against.
///
/// v5 (Linux 6.10) is the newest ABI whose additions are all plain filesystem
/// rights — truncation, cross-directory rename, ioctls on device files — which
/// this policy can describe accurately. v9 adds `ResolveUnix`, and handling it
/// would deny connecting to UNIX sockets by path: a different boundary from
/// the one described here, and one that would silently cut the CLI off from
/// things like the session bus. Raising this constant means testing on a
/// kernel that supports the new rights, not just compiling against them.
const TARGET_ABI: ABI = ABI::V5;

/// The ABI below which there is no Landlock to negotiate with.
const MINIMUM_ABI: ABI = ABI::V1;

/// Why a kernel offers nothing, said the way a person could act on it.
///
/// Both causes read the same to the user and neither is their fault: a kernel
/// older than 5.13 has no Landlock, and several distributions ship it built
/// but not in `CONFIG_LSM`, where it stays off until `lsm=landlock` is on the
/// kernel command line. Somebody running a desktop app will have set neither.
const NO_LANDLOCK: &str = "the kernel does not provide it — Landlock needs Linux 5.13 or newer \
                           with the module enabled, which some distributions only do when \
                           `lsm=landlock` is on the kernel command line";

/// System hierarchies granted read and execute.
///
/// This is what it takes for a CLI to reach its own `main`: the interpreter,
/// the loader, the shared libraries, and the configuration those read on the
/// way up — `/etc/ld.so.cache`, `/etc/ssl`, `/etc/resolv.conf`. None of it is
/// writable.
const SYSTEM_READ_EXEC: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt",
    // Node's runtime asks the kernel about itself through these — processor
    // count, memory, its own open descriptors. A CLI that cannot read them
    // fails at startup, nowhere near the boundary this policy is about.
    "/proc", "/sys",
    // On systemd distributions `/etc/resolv.conf` is a symlink into `/run`,
    // and Landlock evaluates the resolved target rather than the link. Name
    // resolution stops working without this.
    "/run",
];

/// The one system directory the child also writes to: `/dev/null`,
/// `/dev/urandom`, `/dev/tty`.
///
/// Files there can be read, written, truncated and ioctl'd; entries cannot be
/// created or removed, because nothing legitimate here does that and `/dev` is
/// a poor place to be surprised.
fn device_access(abi: ABI) -> BitFlags<AccessFs> {
    AccessFs::from_read(abi) | AccessFs::WriteFile | AccessFs::Truncate | AccessFs::IoctlDev
}

/// What containment this kernel can offer, without applying anything.
///
/// The connections screen and the diagnostics report call this before a task
/// starts, so a person can be told what the boundary will be while it is still
/// their decision.
///
/// This creates a ruleset descriptor and drops it again. Creating one is not
/// applying one — only `landlock_restrict_self` confines a thread, and nothing
/// on this path calls it — so the calling process is exactly as it was.
pub fn probe() -> Containment {
    // `HardRequirement` is what turns "this kernel supports nothing" into an
    // error instead of a ruleset that quietly enforces nothing, which is the
    // only question being asked here. It is asked at v1, the floor: anything
    // below that is the absence of Landlock, not an older dialect of it.
    let attempt = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(MINIMUM_ABI))
        .and_then(Ruleset::create);
    match attempt {
        Ok(_) => Containment::Enforced {
            mechanism: MECHANISM,
        },
        // The kernel has Landlock and refused anyway — out of descriptors, a
        // seccomp filter in the way. Rare, and worth quoting verbatim rather
        // than folding into the general answer, because the general answer
        // would send the reader to fix a kernel that is already fine.
        Err(RulesetError::CreateRuleset(CreateRulesetError::CreateRulesetCall {
            source, ..
        })) => Containment::Unavailable {
            mechanism: MECHANISM,
            reason: format!("the kernel refused to create a ruleset ({source})"),
        },
        Err(_) => Containment::Unavailable {
            mechanism: MECHANISM,
            reason: NO_LANDLOCK.to_string(),
        },
    }
}

/// A ruleset built and held open by the parent, ready for a child to apply.
///
/// Built by [`prepare`]. Holds an open descriptor for the ruleset and one for
/// each granted hierarchy until it is dropped, which is the point: those are
/// the allocations and `open` calls that must not happen after `fork`.
#[derive(Debug)]
pub struct Prepared {
    /// `None` when there is nothing to apply, either because the kernel
    /// offers no Landlock or because the ruleset could not be built. Both
    /// cases are already described by `containment`.
    ruleset: Option<RulesetCreated>,
    containment: Containment,
}

impl Prepared {
    /// What [`apply`](Self::apply) will achieve, known before the spawn.
    ///
    /// [`Containment::Enforced`] here means the kernel accepted the ruleset
    /// and will apply it, not that it has been applied yet. The remaining gap
    /// is `landlock_restrict_self` itself failing in the child, which the
    /// parent cannot observe; [`restrict`] reports that case because it is on
    /// both sides of the call.
    pub fn containment(&self) -> &Containment {
        &self.containment
    }

    /// Confine the calling thread to the prepared ruleset.
    ///
    /// Safe to call between `fork` and `exec`: one `fcntl` to duplicate the
    /// ruleset descriptor, one `prctl` to set `no_new_privs`, one
    /// `landlock_restrict_self`. Nothing here allocates, takes a lock, or
    /// logs.
    ///
    /// Returns whether the kernel applied the ruleset. `false` also covers
    /// there being nothing to apply, which [`containment`](Self::containment)
    /// already said. Callers in `pre_exec` should ignore it and return
    /// `Ok(())` regardless: containment never fails a spawn.
    ///
    /// The descriptor is duplicated rather than consumed so this can be called
    /// from the `FnMut` closure `pre_exec` requires.
    ///
    /// Landlock restricts the calling *thread*. Between `fork` and `exec` the
    /// child has exactly one, and it is the one that becomes the new program.
    pub fn apply(&self) -> bool {
        let Some(ruleset) = self.ruleset.as_ref() else {
            return false;
        };
        let Ok(ruleset) = ruleset.try_clone() else {
            return false;
        };
        // `no_new_privs` is left on: Landlock requires it of an unprivileged
        // process, and it also stops the child regaining through a setuid
        // binary what this ruleset just took away.
        ruleset.restrict_self().is_ok()
    }
}

/// Build the ruleset for `policy` in the parent, ready to apply in the child.
///
/// Does not restrict the caller. Never fails: a kernel without Landlock, or a
/// ruleset the kernel rejects, both come back as a [`Prepared`] that applies
/// nothing and a [`Containment::Unavailable`] saying why.
///
/// `policy.readable` must name the directory the provider CLI itself lives in,
/// or the `execve` that follows will be denied — see the module documentation.
pub fn prepare(policy: &SandboxPolicy) -> Prepared {
    let containment = probe();
    if !containment.is_enforced() {
        return Prepared {
            ruleset: None,
            containment,
        };
    }
    match build(policy) {
        Ok(ruleset) => Prepared {
            ruleset: Some(ruleset),
            containment,
        },
        Err(error) => Prepared {
            ruleset: None,
            containment: Containment::Unavailable {
                mechanism: MECHANISM,
                reason: format!("the ruleset could not be built ({error})"),
            },
        },
    }
}

/// Restrict the calling thread to `policy`, in one call.
///
/// Opens the paths, builds the ruleset and applies it. Opening a path
/// allocates, which makes this the wrong function to call between `fork` and
/// `exec` — use [`prepare`] there and [`Prepared::apply`] in the child, so
/// the allocating half stays in the parent.
pub fn restrict(policy: &SandboxPolicy) -> Containment {
    let prepared = prepare(policy);
    if prepared.apply() || !prepared.containment.is_enforced() {
        return prepared.containment;
    }
    Containment::Unavailable {
        mechanism: MECHANISM,
        reason: "the kernel accepted the ruleset but would not apply it".to_string(),
    }
}

/// Assemble the ruleset. Every path that cannot be opened is dropped rather
/// than refused, because a policy may name a workspace that has since been
/// unmounted or a provider that is not installed, and neither is a reason to
/// hand back no boundary at all.
fn build(policy: &SandboxPolicy) -> Result<RulesetCreated, RulesetError> {
    let abi = TARGET_ABI;
    Ruleset::default()
        // The negotiation. Best-effort is what lets this ruleset be written
        // against v5 and still mean something on a 5.13 kernel: rights the
        // running kernel does not know are dropped from the request instead of
        // failing it. It is the crate's default; it is set explicitly because
        // it is a decision, not an accident.
        .set_compatibility(CompatLevel::BestEffort)
        // Filesystem rights only. Adding `handle_access(AccessNet::…)` here
        // would cut the CLI off from its own provider — see the module
        // documentation before reaching for it.
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules(
            SYSTEM_READ_EXEC,
            AccessFs::from_read(abi),
        ))?
        .add_rules(path_beneath_rules(["/dev"], device_access(abi)))?
        .add_rules(path_beneath_rules(
            readable_roots(policy),
            AccessFs::from_read(abi),
        ))?
        .add_rules(path_beneath_rules(
            writable_roots(policy),
            AccessFs::from_all(abi),
        ))
}

/// Everything the child may read but not write.
fn readable_roots(policy: &SandboxPolicy) -> Vec<PathBuf> {
    policy.readable.clone()
}

/// Everything the child may write, which is also everything it may read.
fn writable_roots(policy: &SandboxPolicy) -> Vec<PathBuf> {
    let mut roots = policy.writable.clone();
    // The session settings file — the one carrying the MCP bearer token — is
    // written to the temp directory and handed to the CLI by path. A child
    // that cannot read it starts with no tools and no configuration.
    roots.push(std::env::temp_dir());
    if let Some(home) = home_dir() {
        // Credentials, session transcripts, per-project state. Writable
        // because the CLI writes there throughout a run, not only at login.
        roots.push(home.join(".claude"));
        roots.push(home.join(".codex"));
        // A file rather than a directory, and the one Claude Code keeps the
        // signed-in account and its project history in. `path_beneath_rules`
        // narrows the rights to those a non-directory can carry.
        roots.push(home.join(".claude.json"));
    }
    roots
}

/// The user's home directory, from the environment Commonspace itself
/// inherited. Absent or empty means the provider directories are simply not
/// granted, which is a weaker sandbox for that CLI, never a broken one.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
#[cfg(target_os = "linux")]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The crate's own manifest: a real file, outside every hierarchy this
    /// module grants, so being able to read it proves nothing has confined
    /// the test process.
    const OUTSIDE_EVERY_POLICY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

    /// A policy shaped like a real one, over paths that exist.
    fn policy() -> SandboxPolicy {
        SandboxPolicy {
            writable: vec![std::env::temp_dir()],
            readable: vec![PathBuf::from("/usr")],
        }
    }

    /// `apply()` is never called on a test thread. Landlock confines the
    /// caller for the rest of its life, and libtest reuses the harness thread
    /// for reporting; a test that confined itself would take the run down
    /// with it. Enforcement is proved in a child process instead, below.
    #[test]
    fn probe_reports_without_confining_the_caller() {
        let containment = probe();
        assert!(
            !matches!(containment, Containment::NotImplemented { .. }),
            "Linux has an implementation; NotImplemented is for platforms that do not: \
             {containment:?}"
        );
        assert!(
            containment.summary().ends_with('.'),
            "the summary is shown to a person: {}",
            containment.summary()
        );

        // Twice, because a probe that restricted the caller would still look
        // fine the first time.
        let _ = probe();
        std::fs::read(OUTSIDE_EVERY_POLICY)
            .expect("probe() must not confine the process that called it");
    }

    /// [`build`] is exercised directly, not through [`prepare`], because
    /// `prepare` stops at [`probe`] on a kernel without Landlock and the
    /// builder would then never be reached on the machine running these tests.
    /// Building is safe anywhere: it opens descriptors and closes them again
    /// when the returned ruleset is dropped, and confines nothing.
    #[test]
    fn a_policy_naming_paths_that_do_not_exist_still_builds() {
        let missing = SandboxPolicy {
            // A workspace on a drive that has since been unplugged, and a
            // provider that turned out not to be installed.
            writable: vec![PathBuf::from("/commonspace-no-such-workspace")],
            readable: vec![PathBuf::from("/commonspace-no-such-runtime")],
        };
        for policy in [policy(), missing, SandboxPolicy::default()] {
            let built = build(&policy);
            assert!(
                built.is_ok(),
                "a path that has gone missing must not cost the boundary: {built:?}"
            );
            let prepared = prepare(&policy);
            assert_eq!(
                prepared.containment().is_enforced(),
                probe().is_enforced(),
                "{:?}",
                prepared.containment()
            );
        }
        std::fs::read(OUTSIDE_EVERY_POLICY).expect("building must not confine the builder");
    }

    #[test]
    fn preparing_reports_what_the_kernel_will_do() {
        let prepared = prepare(&policy());
        match prepared.containment() {
            Containment::Enforced { mechanism } => assert_eq!(*mechanism, MECHANISM),
            Containment::Unavailable { mechanism, reason } => {
                assert_eq!(*mechanism, MECHANISM);
                assert!(!reason.is_empty(), "a degrade must say why");
            }
            other => panic!("Linux never reports {other:?}"),
        }
    }

    /// The negotiation itself, in the terms the crate states it: the same
    /// request that a hard requirement would refuse on an older kernel must
    /// still produce a ruleset under best-effort. On a kernel new enough for
    /// [`TARGET_ABI`] both succeed, which is equally the point — the
    /// negotiation costs nothing where there is nothing to negotiate.
    #[test]
    fn best_effort_degrades_where_a_hard_requirement_refuses() {
        let strict = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(TARGET_ABI))
            .and_then(Ruleset::create);
        let negotiated = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(TARGET_ABI))
            .and_then(Ruleset::create);

        assert!(
            negotiated.is_ok(),
            "best-effort must build on any kernel: {negotiated:?}"
        );
        if let Err(error) = strict {
            eprintln!("this kernel is below Landlock ABI v{TARGET_ABI}: {error}");
        }
        // Rights only ever accumulate, so a v1 kernel is offered a subset of
        // what v5 asks for rather than something different.
        assert!(AccessFs::from_all(TARGET_ABI).contains(AccessFs::from_all(MINIMUM_ABI)));
    }

    #[test]
    fn the_shipped_ruleset_covers_the_provider_and_the_session_file() {
        let writable = writable_roots(&policy());
        assert!(
            writable.contains(&std::env::temp_dir()),
            "the MCP session settings file lives there"
        );
        if let Some(home) = home_dir() {
            for provider in [".claude", ".codex", ".claude.json"] {
                assert!(
                    writable.contains(&home.join(provider)),
                    "the CLI writes its own session state to {provider}"
                );
            }
        }
        assert!(SYSTEM_READ_EXEC.contains(&"/usr"));
        assert!(
            !writable.iter().any(|root| root == Path::new("/usr")),
            "system hierarchies are read and execute only"
        );
    }

    /// End-to-end: a child confined to a workspace cannot read a file outside
    /// it, and can still read one inside it.
    ///
    /// The control half matters as much as the denial. A ruleset that denied
    /// everything, including the `execve` of the program itself, would pass a
    /// test that only checked for failure.
    // SAFETY: `pre_exec` is unsafe because its closure runs between `fork` and
    // `exec`, where async-signal-unsafe work can deadlock the child. The
    // closure here is `Prepared::apply`, which is three syscalls and no
    // allocation — precisely the constraint the split between `prepare` and
    // `apply` exists to satisfy. Scoped to this function; nothing else in this
    // module is unsafe.
    #[allow(unsafe_code)]
    #[test]
    fn a_confined_child_cannot_read_outside_its_workspace() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        if !probe().is_enforced() {
            eprintln!(
                "SKIPPED a_confined_child_cannot_read_outside_its_workspace: {}",
                probe().summary()
            );
            return;
        }
        let Some(home) = home_dir() else {
            eprintln!(
                "SKIPPED a_confined_child_cannot_read_outside_its_workspace: no HOME to put \
                 a file the policy does not cover"
            );
            return;
        };
        // The temp directory is granted unconditionally, so a secret placed
        // there would prove nothing.
        if home.starts_with(std::env::temp_dir()) {
            eprintln!(
                "SKIPPED a_confined_child_cannot_read_outside_its_workspace: HOME is inside \
                 the temp directory this policy grants"
            );
            return;
        }

        let workspace =
            std::env::temp_dir().join(format!("commonspace-landlock-{}", std::process::id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let allowed = workspace.join("inside.txt");
        std::fs::write(&allowed, b"workspace\n").unwrap();
        let secret = home.join(format!(".commonspace-landlock-{}", std::process::id()));
        std::fs::write(&secret, b"secret\n").unwrap();

        let policy = SandboxPolicy {
            writable: vec![workspace.clone()],
            readable: vec![],
        };
        let read = |target: &Path| {
            let prepared = prepare(&policy);
            assert!(prepared.containment().is_enforced());
            let mut command = Command::new("/bin/cat");
            command
                .arg(target)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            unsafe {
                command.pre_exec(move || {
                    prepared.apply();
                    // Containment never fails a spawn.
                    Ok(())
                });
            }
            command.status().unwrap()
        };

        let denied = read(&secret);
        let permitted = read(&allowed);
        let _ = std::fs::remove_file(&secret);
        let _ = std::fs::remove_dir_all(&workspace);

        assert!(
            !denied.success(),
            "a confined child read {} — the boundary is not holding",
            secret.display()
        );
        assert!(
            permitted.success(),
            "a confined child could not read its own workspace — the boundary is breaking \
             legitimate work"
        );
    }
}
