//! macOS containment via Seatbelt (`sandbox-exec`).
//!
//! Unlike the Linux half, this does not modify the child process — it wraps
//! the command. `sandbox-exec -p '<profile>' -- <program> <args>` starts
//! `<program>` already confined by the kernel to whatever the profile
//! allows; everything else about the spawn (argv, env, cwd, tree-kill via
//! [`crate::process::spawn_cli`]) is unchanged.
//!
//! ## Why `sandbox-exec`, knowing it is deprecated
//!
//! `sandbox-exec` has printed a deprecation warning to stderr since Mac OS X
//! 10.12 (2016), with no successor Apple has ever shipped for this exact
//! job: confining an arbitrary, already-running, non-GUI child process from
//! the outside. App Sandbox — Apple's supported replacement — requires the
//! *target binary* to be code-signed with entitlements and is built for an
//! application confining itself, not a launcher confining someone else's
//! CLI. That is not a gap unique to this project: Codex, Claude Code's own
//! Bash sandbox, VS Code's agent sandbox, and goose all reach for
//! `sandbox-exec` on macOS for the same reason (see `docs/research.md` and
//! `THREAT_MODEL.md`'s survey). Using it is the industry's answer, not a
//! shortcut around one.
//!
//! Apple could remove the binary in a future release; nothing here assumes
//! otherwise. [`probe`] checks that `/usr/bin/sandbox-exec` actually exists
//! before anything is built or spawned, so that day degrades this module to
//! [`Containment::Unavailable`] — loud, in the diagnostics report, exactly
//! as `sandbox/mod.rs` requires — rather than failing every spawn on this
//! platform the moment it happens.

use crate::sandbox::{Containment, SandboxPolicy};
use std::path::{Path, PathBuf};

/// Fixed, not resolved via `PATH`: `sandbox-exec` has lived at this exact
/// path since Mac OS X 10.5, and looking it up on `PATH` would let a
/// hostile `PATH` entry hand back a different binary for the one thing
/// standing between the child and the rest of the disk.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Mechanism name reported through [`Containment`], per `sandbox/mod.rs`'s
/// naming convention (`landlock`, `seatbelt`).
const MECHANISM: &str = "seatbelt";

/// System directories the child needs *read* access to merely to run: the
/// dynamic linker, shared libraries, frameworks, and language runtimes
/// installed system-wide or via Homebrew (the two common install prefixes
/// for `node`/`python`, which is how most provider CLIs actually run).
/// Fixed rather than derived from [`SandboxPolicy`] — every process needs
/// the same OS underneath it, independent of what workspace it was given.
/// None of these are writable; see [`profile`] for the separate,
/// caller-scoped read+write block.
const SYSTEM_READ_PATHS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library",
    "/private/etc",
    "/private/var/db",
    "/var/db",
    "/dev",
    "/opt/homebrew",
    "/usr/local",
];

/// Directory names, relative to `$HOME`, that a provider CLI owns and must
/// be able to read *and write* regardless of what the caller's policy says
/// — session tokens, resumable state, and credentials created by
/// `claude login` / `codex login` live here.
///
/// Named directly rather than reused from
/// `commonspace_permissions::credential_store_paths()`, which lists these
/// same two directories for the opposite reason: that list is what
/// Commonspace's policy engine and the model-facing tool deny rules
/// (`claude.rs::protected_deny_rules`) keep the *model* from reading through
/// a tool call, alongside `.ssh`, `.aws`, and everything else a user's
/// machine holds. This module sits below all of that, constraining the
/// CLI's own process — including the credential-management code inside it
/// that never goes through a model-visible tool. Pulling in the full shared
/// list here would hand every one of those other stores to the child too,
/// which is exactly what this module exists to prevent. The two lists are
/// deliberately different sizes for different reasons, not out of sync.
const PROVIDER_CONFIG_DIRS: &[&str] = &[".claude", ".codex"];

/// The command to run instead, so the child starts confined. Returns the
/// original program and args unchanged when containment is unavailable.
pub fn wrap(
    program: &Path,
    args: &[String],
    policy: &SandboxPolicy,
) -> (PathBuf, Vec<String>, Containment) {
    let containment = probe();
    let Containment::Enforced { mechanism } = containment else {
        return (program.to_path_buf(), args.to_vec(), containment);
    };

    let mut wrapped_args = Vec::with_capacity(args.len() + 3);
    wrapped_args.push("-p".to_string());
    wrapped_args.push(profile(policy));
    wrapped_args.push("--".to_string());
    wrapped_args.push(program.to_string_lossy().into_owned());
    wrapped_args.extend(args.iter().cloned());

    (
        PathBuf::from(SANDBOX_EXEC),
        wrapped_args,
        Containment::Enforced { mechanism },
    )
}

/// What containment this machine can offer, without spawning anything.
///
/// Checks presence on disk rather than assuming every macOS ships it: that
/// is the one fact standing between "Apple removed `sandbox-exec`" being a
/// silent hole and being an honest [`Containment::Unavailable`].
pub fn probe() -> Containment {
    if Path::new(SANDBOX_EXEC).is_file() {
        Containment::Enforced {
            mechanism: MECHANISM,
        }
    } else {
        Containment::Unavailable {
            mechanism: MECHANISM,
            reason: format!("{SANDBOX_EXEC} is not present on this machine"),
        }
    }
}

/// The Seatbelt profile for a policy. Public and separately testable
/// because the profile is the entire security boundary — it must be
/// unit-testable without spawning anything.
///
/// Pure string generation: no filesystem access beyond reading `$HOME` and
/// `$TMPDIR`-equivalent from the environment (not stat-ing or resolving
/// them), so this runs identically, and is fully testable, on any host OS —
/// which is why the tests below are not `#[cfg(target_os = "macos")]`.
///
/// Structure is deny-by-default, then narrow allows:
/// 1. Network — allowed outright (see the comment in the generated profile;
///    it is intentional, not a gap).
/// 2. Baseline process operations every program needs merely to execute.
/// 3. Read-only access to the OS itself ([`SYSTEM_READ_PATHS`]).
/// 4. Read-only access to `policy.readable`, when non-empty.
/// 5. Read+write access to `policy.writable`, the temp directory, and the
///    provider's own config directory.
pub fn profile(policy: &SandboxPolicy) -> String {
    let mut out = String::new();

    out.push_str("(version 1)\n(deny default)\n\n");

    out.push_str(
        "; Network stays open: the CLI has to reach its own provider's API \
(and DNS, and — for some providers — a websocket) to do anything at all. \
This is deliberate, not an oversight: containment here is a filesystem \
boundary, not a network one. Commonspace's own network-touching tools are \
policed separately, at the tool layer (THREAT_MODEL.md).\n",
    );
    out.push_str("(allow network*)\n\n");

    out.push_str(
        "; Baseline operations every process needs merely to run, none of \
which touch the filesystem or network this profile actually bounds: \
forking and exec-ing its own subprocesses (git, npm, language runtimes), \
signalling itself, sysctl reads (libSystem probes these on startup), and \
file *metadata* reads so ordinary path resolution — stat-ing ancestor \
directories, following symlinks — doesn't fail on locations that are \
otherwise denied for content access. mach-lookup is left unrestricted \
rather than enumerated: the well-known service names a process needs \
(opendirectoryd for user lookups, cfprefsd, notifyd, distnoted, ...) \
differ across macOS versions and would be a silent-breakage trap on every \
OS update, and a mach service name is not a file or a socket — allowing \
it does not widen the write/read/network boundary this module exists to \
enforce.\n",
    );
    out.push_str("(allow process-fork)\n");
    out.push_str("(allow process-exec)\n");
    out.push_str("(allow signal (target self))\n");
    out.push_str("(allow sysctl-read)\n");
    out.push_str("(allow file-read-metadata)\n");
    out.push_str("(allow mach-lookup)\n\n");

    out.push_str(
        "; /dev/null and /dev/tty are behaviour sinks, not exfiltration \
routes: any CLI that redirects a subprocess's output, or talks to a \
controlling terminal, needs to write to them. Read access to both is \
already covered by the system read-paths block below.\n",
    );
    out.push_str("(allow file-write-data (literal \"/dev/null\") (literal \"/dev/tty\"))\n\n");

    out.push_str(
        "; Read-only: the OS itself. The dynamic linker, shared libraries, \
frameworks, and language runtimes installed system-wide or via Homebrew. \
None of this is writable.\n",
    );
    out.push_str(&allow_block(
        "file-read*",
        SYSTEM_READ_PATHS.iter().map(Path::new),
    ));
    out.push('\n');

    if !policy.readable.is_empty() {
        out.push_str("; Read-only: caller-specified paths outside the writable set.\n");
        out.push_str(&allow_block(
            "file-read*",
            policy.readable.iter().map(PathBuf::as_path),
        ));
        out.push('\n');
    }

    let mut writable: Vec<PathBuf> = policy.writable.clone();
    writable.push(resolved_temp_dir());
    if let Some(home) = home_dir() {
        for name in PROVIDER_CONFIG_DIRS {
            writable.push(home.join(name));
        }
    }
    out.push_str(
        "; Read+write: the workspace roots this session was given, the \
temp directory (Commonspace's own MCP session-settings file for this run \
lives there — see claude.rs and codex.rs), and the provider CLI's own \
config directory. The CLI owns its credentials and session state under \
the latter and *will* write there mid-run; a sandbox that stopped it \
resuming a session would have broken the product, not secured it.\n",
    );
    out.push_str(&allow_block(
        "file-read* file-write*",
        writable.iter().map(PathBuf::as_path),
    ));

    out
}

/// Renders `(allow <ops>\n    (subpath "...")\n    ...\n)`.
///
/// Never called with an empty `paths` iterator: `(allow file-read*)` with no
/// filter after it is not "allow nothing" in SBPL, it is "allow everything"
/// — the opposite of every intent in this module. [`profile`] guards the one
/// caller that can legitimately be empty (`policy.readable`) before it gets
/// here; [`SYSTEM_READ_PATHS`] is a fixed non-empty constant, and the
/// writable block always carries at least the temp directory.
fn allow_block<'a>(ops: &str, paths: impl Iterator<Item = &'a Path>) -> String {
    let mut out = format!("(allow {ops}\n");
    for path in paths {
        out.push_str("    ");
        out.push_str(&subpath_literal(path));
        out.push('\n');
    }
    out.push_str(")\n");
    out
}

/// `(subpath "<escaped path>")` — the SBPL form for "this path and
/// everything under it", used in preference to `(regex ...)` or
/// `(literal ...)` for every directory in this module: it is the one form
/// meant for exactly this ("a workspace root and its contents"), and it
/// takes a plain string rather than a pattern language with its own
/// injection surface.
fn subpath_literal(path: &Path) -> String {
    format!(
        "(subpath \"{}\")",
        escape_sbpl_string(&path.to_string_lossy())
    )
}

/// Escapes a string for embedding inside an SBPL string literal.
///
/// SBPL strings close on the first unescaped `"`; this function is the one
/// thing standing between a hostile workspace path and a profile that says
/// something Commonspace never wrote. It is a single pass over the
/// *original* characters — not two sequential find-and-replace passes over
/// progressively-mutated output — specifically so a literal backslash that
/// already precedes a quote in the input (`\"` as two real characters,
/// backslash then quote) cannot be read back as this function's own escape
/// sequence for the quote. Every character is classified exactly once,
/// which makes that class of double-escaping bug structurally impossible
/// rather than a matter of getting the pass order right.
///
/// Once backslash and quote are escaped, nothing else in the input —
/// `)`, a bare newline, non-ASCII text — can close the string or start a
/// new profile form, because nothing else *is* the closing delimiter.
/// `\n`, `\r`, and `\t` are escaped too, but for a readable, single-line
/// profile with no embedded control bytes, not because leaving them raw
/// would be unsafe.
fn escape_sbpl_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// The per-user temp directory, canonicalized.
///
/// macOS reports it through the `/var` symlink (`/var/folders/.../T/`)
/// while the kernel resolves filesystem operations against the real path
/// (`/private/var/folders/.../T/`) — the same mismatch already documented
/// and worked around in
/// `commonspace-permissions::protected::user_temp_dir`. A profile built
/// from the symlinked form would match nothing at runtime and silently
/// make the temp directory unwritable, taking the MCP session-settings
/// file every provider adapter writes there down with it. Falls back to
/// the unresolved path if canonicalization fails (e.g. the directory
/// doesn't exist yet) rather than erroring — consistent with "containment
/// never fails a spawn".
fn resolved_temp_dir() -> PathBuf {
    let temp = std::env::temp_dir();
    std::fs::canonicalize(&temp).unwrap_or(temp)
}

/// `$HOME`, the only portable way to find the user profile from inside this
/// module (`dirs::home_dir` is not a dependency of this crate, and this is
/// the one file in it that is macOS-only regardless).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn policy(writable: &[&str], readable: &[&str]) -> SandboxPolicy {
        SandboxPolicy {
            writable: writable.iter().map(PathBuf::from).collect(),
            readable: readable.iter().map(PathBuf::from).collect(),
        }
    }

    /// A minimal, escape-aware paren-balance check — not a full SBPL
    /// reader, just enough to catch the one failure mode this module must
    /// prevent: a quote inside a string literal being read as that
    /// string's closing quote, which would let whatever follows in a
    /// hostile path be parsed as new, unintended profile structure. Parens
    /// are only counted outside string literals, exactly as sandbox-exec's
    /// own reader would.
    fn is_balanced(sbpl: &str) -> bool {
        let mut depth = 0i32;
        let mut in_string = false;
        let mut chars = sbpl.chars();
        while let Some(c) = chars.next() {
            if in_string {
                match c {
                    '\\' => {
                        chars.next(); // escaped char: consumed, not interpreted
                    }
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                _ => {}
            }
        }
        depth == 0 && !in_string
    }

    #[test]
    fn profile_denies_by_default() {
        let text = profile(&SandboxPolicy::default());
        assert!(text.starts_with("(version 1)\n(deny default)\n"), "{text}");
    }

    #[test]
    fn empty_policy_is_still_a_valid_deny_default_profile() {
        let text = profile(&SandboxPolicy::default());
        assert!(is_balanced(&text), "{text}");
        // The one thing that must never happen: an allow with no filter,
        // which SBPL reads as "allow everything" rather than "allow
        // nothing".
        assert!(!text.contains("(allow file-read*)"), "{text}");
        assert!(!text.contains("(allow file-write*)"), "{text}");
        assert!(!text.contains("(allow file-read* file-write*)"), "{text}");
    }

    #[test]
    fn network_is_allowed_and_explained() {
        let text = profile(&SandboxPolicy::default());
        assert!(text.contains("(allow network*)"), "{text}");
        assert!(text.to_lowercase().contains("provider"), "{text}");
    }

    #[test]
    fn writable_paths_land_in_a_read_write_block() {
        let text = profile(&policy(&["/Users/alice/workspace"], &[]));
        let block_start = text
            .find("file-read* file-write*")
            .expect("a combined read+write block");
        assert!(
            text[block_start..].contains("(subpath \"/Users/alice/workspace\")"),
            "{text}"
        );
    }

    #[test]
    fn readable_paths_land_in_a_read_only_block_not_the_write_block() {
        let text = profile(&policy(&[], &["/Users/alice/reference"]));
        let idx = text
            .find("/Users/alice/reference")
            .expect("path present somewhere");
        let preceding_allow = text[..idx]
            .rfind("(allow ")
            .expect("an allow form precedes it");
        let clause = &text[preceding_allow..idx];
        assert!(clause.contains("file-read*"), "{clause}");
        assert!(!clause.contains("file-write*"), "{clause}");
    }

    #[test]
    fn system_read_paths_are_present_and_read_only() {
        let text = profile(&SandboxPolicy::default());
        for system_path in SYSTEM_READ_PATHS {
            assert!(
                text.contains(&format!("(subpath \"{system_path}\")")),
                "missing {system_path} in:\n{text}"
            );
        }
    }

    #[test]
    fn temp_directory_is_always_writable_even_with_an_empty_policy() {
        let text = profile(&SandboxPolicy::default());
        let temp = resolved_temp_dir();
        let expected = subpath_literal(&temp);
        assert!(text.contains(&expected), "missing {expected} in:\n{text}");
    }

    #[test]
    fn provider_config_dirs_are_writable_when_home_is_set() {
        // This module only ever runs where `$HOME` is set (macOS always
        // sets it), but the function degrades instead of panicking if not
        // — assert the behaviour actually observed in *this* process,
        // whatever that is, rather than assuming.
        let text = profile(&SandboxPolicy::default());
        if let Some(home) = home_dir() {
            for dir in PROVIDER_CONFIG_DIRS {
                let expected = subpath_literal(&home.join(dir));
                assert!(text.contains(&expected), "missing {expected} in:\n{text}");
            }
        }
    }

    #[test]
    fn escapes_backslash_and_quote_independently() {
        // Two literal characters: backslash, then quote.
        let escaped = escape_sbpl_string("a\\\"b");
        let mut expected = String::from("a");
        expected.push('\\');
        expected.push('\\'); // the backslash, doubled
        expected.push('\\');
        expected.push('"'); // the quote, escaped
        expected.push('b');
        assert_eq!(escaped, expected);
    }

    #[test]
    fn escapes_control_characters() {
        let escaped = escape_sbpl_string("a\nb\rc\td");
        assert_eq!(escaped, "a\\nb\\rc\\td");
    }

    #[test]
    fn hostile_paths_cannot_unbalance_the_profile() {
        let hostile_inputs = [
            "plain/path",
            "has\"quote/inside",
            "has\\backslash\\inside",
            "has\nnewline\ninside",
            "closes)paren)inside",
            "unicode/héllo/世界/🎯",
            // A realistic injection attempt: close the subpath's string,
            // close the subpath form, then try to append a new directive.
            "x\"); (allow file-read* (subpath \"/\")) ;(\"",
        ];
        for hostile in hostile_inputs {
            let hostile_policy = policy(
                &[&format!("/tmp/{hostile}")],
                &[&format!("/tmp/read/{hostile}")],
            );
            let text = profile(&hostile_policy);
            assert!(is_balanced(&text), "unbalanced for {hostile:?}:\n{text}");
        }
    }

    #[test]
    fn injection_payload_is_neutralized_not_merely_survived() {
        let payload = "x\"); (allow file-read* (subpath \"/\")) ((";
        let text = profile(&policy(&[&format!("/tmp/{payload}")], &[]));
        assert!(is_balanced(&text), "{text}");
        // The raw, unescaped payload must never appear verbatim — only its
        // escaped form, with every backslash and quote doubled/escaped.
        assert!(!text.contains(payload), "{text}");
        assert!(
            text.contains(&escape_sbpl_string(payload)),
            "escaped payload missing from:\n{text}"
        );
    }

    #[test]
    fn wrap_returns_original_command_when_sandbox_exec_is_absent() {
        // This container has no /usr/bin/sandbox-exec — nor would a real
        // Mac on the day Apple finally removes it (see module docs).
        let program = PathBuf::from("/usr/bin/true");
        let args = vec!["--flag".to_string(), "value".to_string()];
        let (resolved_program, resolved_args, containment) =
            wrap(&program, &args, &SandboxPolicy::default());
        assert_eq!(resolved_program, program);
        assert_eq!(resolved_args, args);
        assert!(!containment.is_enforced());
        assert!(matches!(containment, Containment::Unavailable { .. }));
    }

    #[test]
    fn probe_is_unavailable_without_the_binary() {
        assert!(
            !Path::new(SANDBOX_EXEC).exists(),
            "test assumes a container without sandbox-exec"
        );
        let containment = probe();
        assert!(
            matches!(containment, Containment::Unavailable { mechanism, .. } if mechanism == MECHANISM)
        );
    }

    /// Tests below this line need a real macOS kernel and did not run in
    /// this Linux container; see the report accompanying this change.
    #[cfg(target_os = "macos")]
    mod macos_only {
        use super::*;

        #[test]
        fn probe_finds_sandbox_exec_on_a_real_mac() {
            assert!(matches!(
                probe(),
                Containment::Enforced {
                    mechanism: "seatbelt"
                }
            ));
        }

        #[test]
        fn wrap_actually_confines_the_child() {
            let tmp = std::env::temp_dir().join(format!(
                "commonspace-macos-sandbox-test-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&tmp).expect("create workspace");
            let outside = std::env::temp_dir().join(format!(
                "commonspace-macos-sandbox-outside-{}",
                std::process::id()
            ));

            let policy = SandboxPolicy {
                writable: vec![tmp.clone()],
                readable: vec![],
            };
            let (program, args, containment) =
                wrap(Path::new("/bin/sh"), &["-c".into(), String::new()], &policy);
            assert!(containment.is_enforced());

            // Writing inside the writable root succeeds.
            let inside_script = format!("echo hi > {}/ok.txt", tmp.display());
            let mut inside_args = args.clone();
            *inside_args.last_mut().unwrap() = inside_script;
            let status = std::process::Command::new(&program)
                .args(&inside_args)
                .status()
                .expect("run sandboxed write inside");
            assert!(status.success());
            assert!(tmp.join("ok.txt").exists());

            // Writing outside it is denied by the kernel, not by the shell.
            let outside_script = format!("echo hi > {}", outside.display());
            let mut outside_args = args;
            *outside_args.last_mut().unwrap() = outside_script;
            let status = std::process::Command::new(&program)
                .args(&outside_args)
                .status()
                .expect("run sandboxed write outside");
            assert!(!status.success());
            assert!(!outside.exists());

            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
}
