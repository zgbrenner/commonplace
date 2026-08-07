//! OS-level containment for a spawned provider CLI.
//!
//! Commonspace's own tools are path-scoped in Rust and every mutation goes
//! through the policy engine. The provider CLI is a different matter: it is
//! someone else's binary, running as the user, and the only things standing
//! between it and the rest of the disk are the flags it was launched with and
//! its own good behaviour. This module adds a kernel-enforced floor under
//! that, where the platform offers one.
//!
//! Three rules shape everything here.
//!
//! **Containment never fails a spawn.** A kernel too old, a missing security
//! module, a profile the OS rejects — all of these degrade to running
//! uncontained. An agent that will not start is worse for the person than an
//! agent that starts with a weaker boundary, and the alternative to weaker is
//! not stronger, it is nothing.
//!
//! **Degrading is never silent.** [`Containment`] is returned from every
//! attempt and surfaced, because a security property the user believes they
//! have and does not is worse than one they know they lack. This is the same
//! rule the rest of the project holds itself to.
//!
//! **The boundary must not break legitimate work.** A sandbox that stops the
//! CLI reading its own credentials, or writing the session state it resumes
//! from, has not made anything safer — it has made the product not work, and
//! the fix people reach for is turning it off.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

/// What the child may touch. Everything not named here is denied by whatever
/// mechanism is in force; where no mechanism is in force this is advisory and
/// [`Containment`] says so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// The workspace roots: the only places the child may write.
    pub writable: Vec<PathBuf>,
    /// Places the child may read but not write — system libraries, the
    /// interpreter, and the provider's own configuration.
    pub readable: Vec<PathBuf>,
}

/// What containment a spawn actually achieved.
///
/// Deliberately not a bool. "Off because this kernel lacks it" and "off
/// because nobody has written it for this platform" are different facts, and
/// a person deciding whether to trust an untrusted folder needs the
/// difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Containment {
    /// The kernel is enforcing the policy.
    Enforced {
        /// The mechanism, named for the diagnostics report: `landlock`,
        /// `seatbelt`.
        mechanism: &'static str,
    },
    /// The mechanism exists here but could not be applied, with the reason in
    /// plain language.
    Unavailable {
        mechanism: &'static str,
        reason: String,
    },
    /// No containment is implemented for this platform. Honest rather than
    /// aspirational: see `THREAT_MODEL.md` for why, and who else has and has
    /// not solved it.
    NotImplemented { platform: &'static str },
    /// The caller asked for no confinement. Distinct from the three above so
    /// a spawn that was never meant to be confined — an internal probe, a
    /// test — cannot be mistaken for one whose confinement failed.
    NotRequested,
}

impl Containment {
    /// Whether the kernel is actually enforcing anything.
    pub fn is_enforced(&self) -> bool {
        matches!(self, Containment::Enforced { .. })
    }

    /// One sentence for the diagnostics report and the connections screen.
    pub fn summary(&self) -> String {
        match self {
            Containment::Enforced { mechanism } => {
                format!("Confined by {mechanism}.")
            }
            Containment::Unavailable { mechanism, reason } => {
                format!("Not confined: {mechanism} is unavailable here ({reason}).")
            }
            Containment::NotImplemented { platform } => {
                format!("Not confined: Commonspace has no containment for {platform} yet.")
            }
            Containment::NotRequested => "Not confined: none was asked for.".to_string(),
        }
    }
}
