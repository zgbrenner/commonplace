//! Process lifecycle management for provider CLIs.
//!
//! - Spawns with **Job Objects** on Windows and **process groups** on Unix
//!   (via `process-wrap`) so cancellation kills the *whole* tree, including
//!   MCP servers and subshells the CLI spawns.
//! - Streams stdout line-by-line (the CLIs speak JSONL).
//! - Keeps a bounded stderr tail for diagnostics.
//! - `.cmd`/`.bat` shims (npm on Windows) are launched through `cmd.exe`;
//!   the Job Object still captures every descendant.

use crate::sandbox::{Containment, SandboxPolicy};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};

use process_wrap::tokio::*;

/// Maximum stderr lines retained for the developer view.
const STDERR_TAIL_LINES: usize = 400;

/// A handle that can terminate the full process tree.
#[derive(Clone)]
pub struct KillHandle {
    child: Arc<Mutex<Box<dyn ChildWrapper>>>,
}

impl KillHandle {
    /// Terminate the process tree. Idempotent; errors after exit are ignored.
    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// A spawned CLI with streaming IO.
pub struct SpawnedCli {
    pub kill: KillHandle,
    /// One JSONL line per message.
    pub stdout_lines: mpsc::UnboundedReceiver<String>,
    /// Rolling stderr tail (diagnostics; secrets are the CLI's own concern
    /// but Commonspace never logs this at info level).
    pub stderr_tail: Arc<std::sync::Mutex<VecDeque<String>>>,
    /// Write end of stdin, for stream-json input protocols. `None` after
    /// `take_stdin`.
    stdin: Option<tokio::process::ChildStdin>,
    child: Arc<Mutex<Box<dyn ChildWrapper>>>,
    /// What confinement this process actually got. Carried rather than
    /// assumed: the caller reports it, and a boundary that silently was not
    /// applied is the failure this whole layer exists to make visible.
    pub containment: Containment,
}

impl SpawnedCli {
    /// Take the stdin writer (once).
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.stdin.take()
    }

    /// Write one line to stdin (JSONL input protocols).
    pub async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "stdin already taken or closed",
            ));
        };
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    }

    /// Close stdin (EOF) without killing the process.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Wait for exit and return the code (when available).
    pub async fn wait(&self) -> std::io::Result<Option<i32>> {
        let mut child = self.child.lock().await;
        let status = child.wait().await?;
        Ok(status.code())
    }
}

/// Spawn a short-lived probe (`--version`, `auth status`) and collect its
/// stdout.
///
/// Deliberately does *not* use a job object. Probes spawn no process tree, so
/// there is nothing to tree-kill, and staying off that path means nothing can
/// overwrite `CREATE_NO_WINDOW` — these four startup spawns are exactly the
/// ones a user sees as console windows if suppression fails.
pub async fn probe_output(
    program: &Path,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
    timeout: std::time::Duration,
) -> std::io::Result<(Option<i32>, String)> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in envs {
        command.env(k, v);
    }
    #[cfg(windows)]
    {
        // 0x08000000 = CREATE_NO_WINDOW. Applied directly, with no wrapper
        // in the way that could replace it.
        command.creation_flags(0x0800_0000);
    }

    let child = command.spawn()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok((output.status.code(), text))
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("{} did not respond in time", program.display()),
        )),
    }
}

/// Spawn a CLI with tree-kill support and line-streamed stdout.
///
/// `program` must be an absolute, pre-resolved path (see `detect`); callers
/// never pass user- or agent-controlled strings here. Arguments are passed
/// as argv, never through shell interpolation.
pub fn spawn_cli(
    program: &Path,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
    policy: Option<&SandboxPolicy>,
) -> std::io::Result<SpawnedCli> {
    // Confinement, where this platform offers it. Deliberately infallible:
    // an old kernel or a missing profile means the CLI runs unconfined, not
    // that it refuses to run. What was actually achieved rides along on the
    // handle so the caller can report it rather than assume it.
    let (program, args, containment) = contain(program, args, policy);
    let (program, args) = (program.as_path(), &args[..]);
    // npm shims on Windows are .cmd batch files; CreateProcess can only run
    // them via cmd.exe. The Job Object wraps cmd.exe and every descendant.
    let is_batch = matches!(
        program.extension().and_then(|e| e.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat")
    );

    let mut wrap = if is_batch {
        let w = CommandWrap::with_new("cmd.exe", |c| {
            c.arg("/d").arg("/s").arg("/c");
            let mut full = std::ffi::OsString::from("\"");
            full.push(program.as_os_str());
            for a in args {
                full.push(" ");
                // Arguments are quoted individually; cmd /s strips the outer
                // quotes of the whole string.
                full.push(quote_for_cmd(a));
            }
            full.push("\"");
            #[cfg(windows)]
            {
                c.raw_arg(full);
            }
            #[cfg(not(windows))]
            {
                let _ = full;
            }
            configure(c, cwd, envs, policy);
        });
        // Only Windows has `.cmd` shims, so only Windows wraps here. The
        // rebinding keeps `w` immutable elsewhere, which would otherwise be
        // an unused-`mut` warning on Linux and macOS.
        #[cfg(windows)]
        let mut w = w;
        #[cfg(windows)]
        no_console(&mut w);
        #[cfg(windows)]
        w.wrap(JobObject);
        w
    } else {
        let mut w = CommandWrap::with_new(program, |c| {
            c.args(args);
            configure(c, cwd, envs, policy);
        });
        #[cfg(windows)]
        no_console(&mut w);
        #[cfg(windows)]
        w.wrap(JobObject);
        #[cfg(unix)]
        w.wrap(ProcessGroup::leader());
        w
    };

    let mut child = wrap.spawn()?;

    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout not captured"))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| std::io::Error::other("child stderr not captured"))?;
    let stdin = child.stdin().take();

    let (line_tx, line_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let stderr_tail = Arc::new(std::sync::Mutex::new(VecDeque::new()));
    let tail = Arc::clone(&stderr_tail);
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
            if tail.len() >= STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    });

    let child = Arc::new(Mutex::new(child));
    Ok(SpawnedCli {
        kill: KillHandle {
            child: Arc::clone(&child),
        },
        stdout_lines: line_rx,
        stderr_tail,
        stdin,
        child,
        containment,
    })
}

/// Suppress the console window Windows would otherwise create for each child.
///
/// This *must* go through process-wrap's `CreationFlags` wrapper rather than
/// `Command::creation_flags`. `JobObject::pre_spawn` calls `creation_flags`
/// itself, which replaces rather than merges, so a flag set directly on the
/// command is silently discarded — and only the wrapper's value is OR-ed back
/// in. Applied before `JobObject` so the wrappers run in the documented order.
///
/// Getting this wrong is invisible in development, where the app already owns
/// a console that children inherit. In a packaged build there is no console,
/// so every child allocates a visible one: exactly the terminal windows
/// Commonspace exists to keep out of sight.
#[cfg(windows)]
fn no_console(w: &mut CommandWrap) {
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    w.wrap(CreationFlags(CREATE_NO_WINDOW));
}

/// Rewrite the command, or arrange for the child to restrict itself, so it
/// starts confined. Returns what was actually achieved.
///
/// The platforms genuinely differ in shape: macOS wraps the command in
/// `sandbox-exec`, Linux has the child restrict itself between fork and exec,
/// and Windows has no mechanism this product can ship. One function hides
/// that from the spawn path without pretending the difference is not there.
fn contain(
    program: &Path,
    args: &[String],
    policy: Option<&SandboxPolicy>,
) -> (PathBuf, Vec<String>, Containment) {
    let Some(policy) = policy else {
        return (
            program.to_path_buf(),
            args.to_vec(),
            Containment::NotRequested,
        );
    };
    let _ = policy;

    #[cfg(target_os = "macos")]
    {
        crate::sandbox::macos::wrap(program, args, policy)
    }
    #[cfg(target_os = "linux")]
    {
        // Nothing is rewritten on Linux. The ruleset is built here, in the
        // parent, and applied by `install_linux_restriction` between fork and
        // exec — see there for why the halves are split.
        (
            program.to_path_buf(),
            args.to_vec(),
            crate::sandbox::linux::prepare(policy).containment().clone(),
        )
    }
    #[cfg(windows)]
    {
        (
            program.to_path_buf(),
            args.to_vec(),
            crate::sandbox::windows::probe(),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        (
            program.to_path_buf(),
            args.to_vec(),
            Containment::NotImplemented {
                platform: "this platform",
            },
        )
    }
}

/// Have the child confine itself to `policy` immediately before `exec`.
///
/// Landlock restricts a thread, and the only thread that survives into the
/// new program is the one between `fork` and `exec` — so this has to happen
/// there, not in the parent, which would confine Commonspace itself.
///
/// The allocating half (opening paths, building the ruleset) runs here in the
/// parent. The closure does three syscalls and no allocation, which is what
/// makes it safe in a context where taking a lock could deadlock the child.
#[cfg(target_os = "linux")]
fn install_linux_restriction(c: &mut tokio::process::Command, policy: &SandboxPolicy) {
    let prepared = crate::sandbox::linux::prepare(policy);
    // SAFETY: the closure runs between `fork` and `exec`, where
    // async-signal-unsafe work can deadlock the child. `Prepared::apply` is
    // an `fcntl`, a `prctl` and a `landlock_restrict_self`, with no
    // allocation, no locking and no logging — which is the entire reason
    // `prepare` and `apply` are separate functions.
    #[allow(unsafe_code)]
    unsafe {
        c.pre_exec(move || {
            // Deliberately ignored: containment never fails a spawn, and what
            // was actually achieved was already reported from `contain`.
            let _ = prepared.apply();
            Ok(())
        });
    }
}

fn configure(
    c: &mut tokio::process::Command,
    cwd: &Path,
    envs: &[(String, String)],
    policy: Option<&SandboxPolicy>,
) {
    #[cfg(target_os = "linux")]
    if let Some(policy) = policy {
        install_linux_restriction(c, policy);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = policy;

    c.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in envs {
        c.env(k, v);
    }
}

/// Quote one argument for cmd.exe. Conservative: wraps in double quotes and
/// doubles embedded quotes. Callers must never pass untrusted strings —
/// arguments here are Commonspace-constructed flags, prompts go via stdin.
fn quote_for_cmd(arg: &str) -> std::ffi::OsString {
    let escaped = arg.replace('"', "\"\"");
    std::ffi::OsString::from(format!("\"{escaped}\""))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn shell() -> (PathBuf, Vec<String>) {
        #[cfg(windows)]
        {
            (
                PathBuf::from(std::env::var("ComSpec").unwrap_or("cmd.exe".into())),
                vec!["/c".into()],
            )
        }
        #[cfg(not(windows))]
        {
            (PathBuf::from("/bin/sh"), vec!["-c".into()])
        }
    }

    #[tokio::test]
    async fn streams_stdout_lines_and_exits() {
        let (sh, mut args) = shell();
        #[cfg(windows)]
        args.push("echo one& echo two".into());
        #[cfg(not(windows))]
        args.push("echo one; echo two".into());

        let cwd = std::env::temp_dir();
        let mut cli = spawn_cli(&sh, &args, &cwd, &[], None).unwrap();
        let mut got = Vec::new();
        while let Some(line) = cli.stdout_lines.recv().await {
            got.push(line.trim().to_string());
        }
        assert_eq!(got, vec!["one", "two"]);
        let code = cli.wait().await.unwrap();
        assert_eq!(code, Some(0));
    }

    #[tokio::test]
    async fn kill_terminates_process() {
        let (sh, mut args) = shell();
        #[cfg(windows)]
        args.push("ping -n 30 127.0.0.1 > nul".into());
        #[cfg(not(windows))]
        args.push("sleep 30".into());

        let cwd = std::env::temp_dir();
        let cli = spawn_cli(&sh, &args, &cwd, &[], None).unwrap();
        let started = std::time::Instant::now();
        cli.kill.kill().await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "kill took too long"
        );
    }

    #[tokio::test]
    async fn stdin_round_trip() {
        let (sh, mut args) = shell();
        // `more`/`cat` echo stdin back.
        #[cfg(windows)]
        args.push("more".into());
        #[cfg(not(windows))]
        args.push("cat".into());

        let cwd = std::env::temp_dir();
        let mut cli = spawn_cli(&sh, &args, &cwd, &[], None).unwrap();
        cli.write_line("hello-stdin").await.unwrap();
        cli.close_stdin();
        let mut got = Vec::new();
        while let Some(line) = cli.stdout_lines.recv().await {
            if !line.trim().is_empty() {
                got.push(line.trim().to_string());
            }
        }
        assert_eq!(got, vec!["hello-stdin"]);
    }
}
