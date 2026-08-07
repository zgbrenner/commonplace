//! Windows containment: not implemented, on purpose documented rather than
//! silently absent.
//!
//! This is the one platform in the module where [`probe`] always returns
//! [`Containment::NotImplemented`]. That is not an oversight; it is the
//! honest answer after evaluating the mechanisms Windows actually offers.
//! Anyone tempted to "just wire up AppContainer" should read the next three
//! sections first — each one was tried by a team with more resources than
//! this project has, and each has a reason it does not fit a filesystem-
//! heavy CLI launched with no administrator interaction.
//!
//! ## AppContainer: rejected
//!
//! AppContainer is Windows's default-deny process isolation primitive —
//! the mechanism behind UWP app sandboxing and Chromium's own renderer
//! sandbox on Windows. Chromium's sandbox design documentation describes
//! the practical cost directly: an AppContainer process starts with almost
//! no filesystem access, and every path it legitimately needs — DLL search
//! paths, fonts, temp directories, the file it was actually asked to open —
//! has to be granted back individually
//! (<https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md>).
//! Chromium can afford that because its renderer's job is narrow and known
//! in advance. A provider CLI is the opposite case: it reads its own
//! config, credential cache, node_modules or site-packages tree, and
//! arbitrary files inside whatever workspace the user authorized, none of
//! which is known at packaging time. OpenAI's Codex team evaluated the same
//! primitive for the same kind of process and reached the same conclusion:
//! their own survey of Windows sandboxing options states that a
//! default-deny AppContainer profile for a general-purpose coding agent
//! needs so many read exceptions punched through it that the exceptions
//! swallow the boundary
//! (<https://learn.chatgpt.com/docs/windows/windows-sandbox>). Two
//! independent teams sandboxing two different filesystem-heavy agent
//! processes threw this out for the same reason; that is not a coincidence
//! this project needs to re-discover by shipping it and finding out.
//!
//! ## Restricted tokens: real, but the working version needs accounts
//!
//! A restricted token — `CreateRestrictedToken`, deny-only SIDs, a lowered
//! integrity level — is the mechanism that Codex actually ships on Windows,
//! and it is proof the approach works in principle, including from Rust.
//! But the form that provides a real filesystem boundary is not "launch
//! the CLI with a token restricted in-process"; NTFS permissions are
//! evaluated against the *account* on the ACL, and a token restricted
//! in-place while still carrying the interactive user's SID still carries
//! that user's file access. The boundary that holds is a **separate,
//! dedicated, lower-privileged local Windows user account** the CLI runs
//! as, with its own NTFS permissions — which is exactly what Codex's
//! documented Windows sandbox provisions: dedicated sandbox user accounts,
//! filesystem permission boundaries between them and the real user, and a
//! firewall rule for the network-off case, all set up through a one-time
//! **administrator-approved** installer step
//! (<https://learn.chatgpt.com/docs/windows/windows-sandbox>). The
//! `sandbox-runtime` project behind Claude Code's own sandbox marks its
//! Windows support alpha for the identical reason: it requires a one-time
//! elevated install that provisions a dedicated local user account, a
//! local group, and machine-wide filtering rules
//! (<https://github.com/anthropic-experimental/sandbox-runtime>). Both
//! projects converged on the same shape of fix and the same cost.
//!
//! Commonspace is a first-run, no-administrator desktop install. "Would
//! you like Commonspace to create a Windows user account on this PC" is
//! not a dialog this product gets to show on first launch, and quietly
//! provisioning one without asking is worse. So the mechanism that
//! actually holds is off the table for the same reason it was expensive
//! for Codex and alpha-quality for Claude Code: it costs an account, and
//! an account costs an administrator.
//!
//! ## Job Objects: already in use, not a security boundary
//!
//! `process.rs` wraps every spawn in a Job Object already — but for tree-
//! kill on cancel, not confinement. A Job Object controls resource limits
//! and lets a process tree be terminated as a unit; it does not restrict
//! what a process inside it can read, write, or execute. `docs/research.md`
//! records this plainly ("Windows Job Objects are resource controls, not a
//! security boundary") and this module does not contradict it. Nothing
//! here should be read as Job Objects providing containment — they do not,
//! and are not being asked to.
//!
//! ## A candidate worth naming, not worth building yet
//!
//! There is a narrower use of restricted tokens that does not require a
//! second account: apply `CreateRestrictedToken` with deny-only SIDs and a
//! lowered mandatory integrity level to the *same* user's token, then
//! launch the CLI with it. This does not gain NTFS-enforced confinement —
//! the underlying account is unchanged, so any path already readable or
//! writable by that account's SID stays readable or writable — but a
//! lowered integrity level does block writes to higher-integrity objects
//! under Windows Mandatory Integrity Control, which is a real, if narrow,
//! property: it stops the child from writing into locations the OS treats
//! as more trusted than an ordinary process, without provisioning
//! anything or asking for elevation. It would not have stopped the child
//! reading or writing anywhere the interactive user can already read or
//! write, which in practice is most of what this threat model cares
//! about — so on its own it is a thin floor, not the boundary the Linux
//! and macOS modules provide, and it has not been evaluated closely enough
//! here to claim it works end-to-end with the provider CLIs Commonspace
//! spawns (does the CLI's own install layout, credential cache, or
//! auto-updater write anywhere that a lowered integrity level would break?
//! unverified). If a future contributor wants to close part of this gap
//! without an account-provisioning flow, this is the place to start
//! investigating — not a mechanism to wire in speculatively from this
//! comment alone.
//!
//! ## What is actually in force on Windows today
//!
//! None of the above changes what already constrains the provider CLI on
//! this platform: it is launched with its most restrictive suitable flags
//! (Codex `--sandbox read-only`, Claude Code `--permission-mode dontAsk`),
//! its own write and shell tools are disabled at the flag level, workspace-
//! discovered settings and hooks are pinned off so a file in the folder
//! cannot silently change how the CLI runs, and every mutation is routed
//! through Commonspace's own staged, policy-gated tool server rather than
//! the CLI's native filesystem access. That is defence in depth by a
//! cooperating process, not a kernel boundary — see THREAT_MODEL.md — and
//! it is unrelated to, and unaffected by, this module returning
//! `NotImplemented`.

use super::Containment;

/// Always [`Containment::NotImplemented`]. See the module docs for why:
/// AppContainer does not fit a filesystem-heavy CLI, and the mechanism that
/// does work — restricted tokens with a dedicated lower-privilege account —
/// needs administrator-approved setup this product does not perform.
pub fn probe() -> Containment {
    Containment::NotImplemented {
        platform: "Windows",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_honest_about_not_implemented() {
        assert_eq!(
            probe(),
            Containment::NotImplemented {
                platform: "Windows"
            }
        );
    }
}
