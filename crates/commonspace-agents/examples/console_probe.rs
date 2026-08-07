//! A/B probe for the Windows "no console window" behaviour.
//!
//! Built as a GUI-subsystem binary — the same condition as the packaged
//! Commonspace app, and the only condition where the bug is visible. In a
//! console build children inherit the parent's console and nothing appears.
//!
//! Usage (the caller measures the conhost delta around each run):
//!
//! ```text
//! cargo run -p commonspace-agents --example console_probe --release -- fixed
//! cargo run -p commonspace-agents --example console_probe --release -- buggy
//! ```
//!
//! `fixed` spawns through `spawn_cli`, which routes CREATE_NO_WINDOW through
//! process-wrap's `CreationFlags` wrapper. `buggy` reproduces the original
//! mistake — setting the flag directly on the `Command`, where
//! `JobObject::pre_spawn` overwrites it.

#![cfg_attr(windows, windows_subsystem = "windows")]
// A diagnostic, not shipped code: panicking on failure is the whole point.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use std::process::Stdio;

    let mode = std::env::args().nth(1).unwrap_or_else(|| "fixed".into());
    // Something that lives long enough to be sampled by the caller.
    let program = std::path::PathBuf::from(std::env::var("ComSpec").unwrap_or("cmd.exe".into()));
    let args: Vec<String> = vec!["/c".into(), "ping -n 4 127.0.0.1 > nul".into()];
    let cwd = std::env::temp_dir();

    if mode == "fixed" {
        let cli = commonspace_agents::process::spawn_cli(&program, &args, &cwd, &[], None)
            .expect("spawn via spawn_cli");
        let _ = cli.wait().await;
    } else {
        // The original bug, kept here so the fix can be demonstrated rather
        // than merely asserted.
        use process_wrap::tokio::*;
        let mut w = CommandWrap::with_new(&program, |c| {
            c.args(&args)
                .current_dir(&cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, set directly
        });
        w.wrap(JobObject);
        let mut child = w.spawn().expect("spawn buggy");
        let _ = child.wait().await;
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("console_probe is a Windows-only diagnostic");
}
