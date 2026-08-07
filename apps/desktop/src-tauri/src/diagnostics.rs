//! Logging to a file, and the diagnostics report the user writes themselves.
//!
//! A packaged Windows GUI build has no console attached, so anything printed
//! to stdout is discarded before it reaches anyone. Every bug that only
//! reproduces in a packaged build is therefore invisible to the person who
//! hits it — which is most of the people who will ever hit one. The rolling
//! file here is the record they can look at.
//!
//! The report is deliberately not telemetry. PRIVACY.md promises that nothing
//! is sent anywhere, and that stays true: this writes one Markdown file next
//! to the logs, reveals it in the file manager, and stops. Whether it is ever
//! shared, and with whom, is the user's decision and no one else's — which is
//! why every value that goes into it passes through [`Redactor`] first, and
//! why the file says out loud what it could not redact.

use std::fmt::Write as _;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::{Captures, Regex};
use serde::Deserialize;
use tauri::{Manager, State};
use tracing_appender::rolling::{RollingFileAppender, RollingWriter, Rotation};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::commands::CommandError;
use crate::state::AppState;

type Result<T> = std::result::Result<T, CommandError>;

/// Log files are named `commonspace.<date>.log`, one per day.
const LOG_PREFIX: &str = "commonspace";
const LOG_SUFFIX: &str = "log";

/// A week of daily files describes a bug someone noticed yesterday and is
/// still small enough that an install nobody looks at cannot fill a disk.
const MAX_LOG_FILES: usize = 7;

/// The report always overwrites this one name, so repeated presses leave one
/// file to read rather than a pile of stale copies to leak.
const REPORT_FILE_NAME: &str = "commonspace-diagnostics.md";

/// How much of the log the report carries.
const REPORT_LOG_LINES: usize = 200;

/// The report reads the end of the log, never the whole of it: a chatty day
/// runs to megabytes and only the last part describes what just happened.
const MAX_TAIL_BYTES: u64 = 512 * 1024;

/// Frontend messages are bounded before they reach the log; a runaway loop in
/// the webview must not be able to fill the disk through this door.
const MAX_WEBVIEW_CHARS: usize = 2_000;

/// Where the storage layer records the journal mode an open actually got.
/// Read through the ordinary settings API rather than a storage-side
/// accessor, so a build where nothing writes it still compiles and simply
/// reports that nothing recorded it.
const JOURNAL_MODE_SETTING: &str = "storage.journal_mode";

/* --------------------------------------------------------------- logging */

/// The file every log line and every panic ends up in.
///
/// Empty until Tauri can resolve the log directory, which happens later than
/// the point where the subscriber has to exist. Lines emitted in between are
/// dropped rather than buffered: they still reach stdout in development, and
/// an unbounded buffer for a destination that may never open is the worse of
/// the two failures.
static LOG_FILE: OnceLock<RollingFileAppender> = OnceLock::new();

/// Install the subscriber (stdout for development, the rolling file for
/// everyone else) and the panic hook.
pub fn install() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "commonspace=info,warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                // Escape codes in a file people open in a text editor are
                // noise, and the report copies these lines verbatim.
                .with_ansi(false)
                .with_writer(LogFileWriter),
        )
        .init();
    install_panic_hook();
}

/// Point the file layer at `directory` and report where it landed.
///
/// The error is returned rather than logged, because a logger that cannot
/// write is precisely the failure that logging cannot report.
pub fn open_log_file(directory: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(directory)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let _ = LOG_FILE.set(appender);
    Ok(directory.to_path_buf())
}

struct LogFileWriter;

/// Where a formatted line goes: the rolling file, or nowhere at all while the
/// log directory is still unknown.
enum LogSink<'a> {
    File(RollingWriter<'a>),
    Discard,
}

impl io::Write for LogSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            LogSink::File(file) => file.write(buf),
            LogSink::Discard => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            LogSink::File(file) => file.flush(),
            LogSink::Discard => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for LogFileWriter {
    type Writer = LogSink<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        match LOG_FILE.get() {
            Some(appender) => LogSink::File(appender.make_writer()),
            None => LogSink::Discard,
        }
    }
}

/// Record panics in the log before the default hook runs.
///
/// The workspace denies `unsafe_code` and warns on `unwrap`/`expect`, so a
/// panic is a case nobody anticipated — the one kind of failure where the
/// message and the backtrace are the entire investigation, and the one that
/// is gone forever if it only ever reached a console that does not exist.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
            .unwrap_or_else(|| "an unknown location".to_owned());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(the panic carried no message)");
        // Forced rather than conditional on `RUST_BACKTRACE`: nobody sets an
        // environment variable before the crash they did not expect.
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("panic at {location}: {message}\n{backtrace}");
        previous(info);
    }));
}

/* ------------------------------------------------------ webview messages */

/// What the frontend is allowed to record. Deliberately narrow: this is for
/// things that went wrong, not a second copy of the event stream.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebviewLevel {
    Warn,
    Error,
}

/// Record a frontend message in the same file as everything else.
///
/// Without this, a warning in the webview reaches `console.warn` — and a
/// packaged build has no devtools open and, on Windows, no console at all.
#[tauri::command]
pub fn log_from_webview(level: WebviewLevel, message: String, detail: Option<String>) {
    let message = clamp(&message, MAX_WEBVIEW_CHARS);
    let detail = detail.map(|detail| clamp(&detail, MAX_WEBVIEW_CHARS));
    let detail = detail.as_deref().unwrap_or("");
    match level {
        WebviewLevel::Warn => tracing::warn!(target: "commonspace::webview", %detail, "{message}"),
        WebviewLevel::Error => {
            tracing::error!(target: "commonspace::webview", %detail, "{message}")
        }
    }
}

/// Cut to `limit` characters (not bytes, so a multi-byte character is never
/// split in half) and say so when something was cut.
fn clamp(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}… (truncated)")
}

/* ------------------------------------------------------------ redaction */

/// Stands in for the user's home directory, wherever it appears.
const HOME_PLACEHOLDER: &str = "<home>";
/// Stands in for the user's name on this computer.
const USER_PLACEHOLDER: &str = "<user>";
/// Stands in for anything shaped like a credential.
const SECRET_PLACEHOLDER: &str = "<redacted-secret>";

/// Replaces the three things a diagnostics file must never carry out of the
/// machine: where the user's home directory is, what they are called on this
/// computer, and anything shaped like a credential.
///
/// Every rule here over-redacts on purpose. A report with a mangled directory
/// name in it is a small annoyance; a report with a working API key in it is
/// the thing this project promises will not happen.
pub struct Redactor {
    /// Applied in order. Ordering matters: credentials first, so a key is
    /// never split by a path substitution, and the entropy rule last, so it
    /// only sees what the named rules left behind.
    rules: Vec<(Regex, String)>,
    /// Long mixed-case runs that no rule recognized by name. Applied through
    /// a closure because "contains a letter and a digit" is not something the
    /// `regex` crate can express without lookaround.
    entropy: Regex,
}

impl Redactor {
    /// Build a redactor for an explicit home directory and username. Tests
    /// use this; production uses [`Redactor::from_environment`].
    pub fn new(
        home: Option<&str>,
        username: Option<&str>,
    ) -> std::result::Result<Self, regex::Error> {
        let mut rules: Vec<(Regex, String)> = Vec::new();

        // `api_key = "…"`, `Authorization: Bearer …`, `token=…`. Only the
        // value is replaced, so the surrounding line still reads as itself.
        rules.push((
            Regex::new(
                r#"(?i)(?P<lead>\b(?:api[_-]?key|secret|token|password|passwd|authorization|bearer)\b["']?\s*[:=]?\s*["']?)(?P<secret>[A-Za-z0-9_\-.]{8,})"#,
            )?,
            format!("${{lead}}{SECRET_PLACEHOLDER}"),
        ));
        // Prefixes several providers publish. Named individually because they
        // are short enough to slip under the entropy rule below.
        rules.push((
            Regex::new(r"\bsk-[A-Za-z0-9_-]{8,}")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));
        rules.push((
            Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{16,}")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));
        rules.push((
            Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{16,}")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));
        rules.push((
            Regex::new(r"\bxox[abprs]-[A-Za-z0-9-]{10,}")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));
        rules.push((
            Regex::new(r"\bAKIA[0-9A-Z]{16}\b")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));
        rules.push((
            Regex::new(r"\bAIza[A-Za-z0-9_-]{20,}")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));
        // A JWT: three dot-separated base64url segments starting with the
        // encoding of `{"`.
        rules.push((
            Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}")?,
            SECRET_PLACEHOLDER.to_owned(),
        ));

        // This machine's actual home directory, in both separator forms —
        // Windows paths reach the log written either way depending on who
        // produced them.
        if let Some(home) = home.filter(|home| home.len() >= 3) {
            let forward = home.replace('\\', "/");
            let backward = home.replace('/', "\\");
            for spelling in [home.to_owned(), forward, backward] {
                rules.push((
                    Regex::new(&format!("(?i){}", regex::escape(&spelling)))?,
                    HOME_PLACEHOLDER.to_owned(),
                ));
            }
        }
        // Home directories belonging to anyone else, or to this user on the
        // machine a copied log came from.
        rules.push((
            Regex::new(r#"(?i)(?P<lead>[/\\](?:Users|home)[/\\])[^/\\\s"':;,)\]}]+"#)?,
            format!("${{lead}}{USER_PLACEHOLDER}"),
        ));

        // The bare name, wherever it appears on its own. Short names are
        // skipped: replacing a two-letter login turns prose into confetti.
        if let Some(username) = username.filter(|name| name.chars().count() >= 3) {
            rules.push((
                Regex::new(&format!(r"(?i)\b{}\b", regex::escape(username)))?,
                USER_PLACEHOLDER.to_owned(),
            ));
        }

        Ok(Self {
            rules,
            entropy: Regex::new(r"\b[A-Za-z0-9_-]{32,}\b")?,
        })
    }

    /// Build a redactor from this process's environment.
    pub fn from_environment() -> std::result::Result<Self, regex::Error> {
        let home = std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok())
            .filter(|home| !home.is_empty());
        let username = std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("USERNAME").ok())
            .or_else(|| std::env::var("LOGNAME").ok())
            // The last component of the home directory is the spelling that
            // actually appears inside paths on macOS and Windows, which is
            // the one that has to disappear.
            .or_else(|| {
                Path::new(home.as_deref()?)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .filter(|name| !name.is_empty());
        Self::new(home.as_deref(), username.as_deref())
    }

    /// Apply every rule. Safe to call on text that has already been redacted:
    /// the placeholders match nothing.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (pattern, replacement) in &self.rules {
            out = pattern.replace_all(&out, replacement.as_str()).into_owned();
        }
        self.entropy
            .replace_all(&out, |caps: &Captures<'_>| {
                let candidate = &caps[0];
                let mixed = candidate.chars().any(|c| c.is_ascii_digit())
                    && candidate.chars().any(|c| c.is_ascii_alphabetic());
                if mixed {
                    SECRET_PLACEHOLDER.to_owned()
                } else {
                    candidate.to_owned()
                }
            })
            .into_owned()
    }
}

/* ---------------------------------------------------------------- report */

/// Write the diagnostics report and show it in the file manager.
///
/// Returns where it was written so the Settings screen can name the file even
/// if the file manager refused to open.
#[tauri::command]
pub async fn write_diagnostics_report(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String> {
    // A redactor that failed to build means every rule below is unenforced,
    // so there is no report to write. Refusing is the only safe answer.
    let redactor = Redactor::from_environment().map_err(|error| {
        CommandError::new(format!(
            "Commonspace couldn't prepare the redaction rules, so it didn't write a diagnostics \
             file: {error}"
        ))
    })?;

    let log_dir = app.path().app_log_dir().map_err(|error| {
        CommandError::new(format!(
            "Commonspace couldn't work out where its logs live: {error}"
        ))
    })?;
    std::fs::create_dir_all(&log_dir).map_err(|error| {
        CommandError::new(format!(
            "Commonspace couldn't create its log folder: {error}"
        ))
    })?;

    let report = build_report(&app, &state, &log_dir, &redactor).await;
    let path = log_dir.join(REPORT_FILE_NAME);
    std::fs::write(&path, report).map_err(|error| {
        CommandError::with_recovery(
            format!("Commonspace couldn't write the diagnostics file: {error}"),
            "Check that there is free space and that the log folder is writable.",
        )
    })?;

    // The file exists either way; failing to reveal it is worth a log line,
    // not an error, because the returned path still tells the user where it is.
    if let Err(error) = tauri_plugin_opener::reveal_item_in_dir(&path) {
        tracing::warn!(%error, "the diagnostics file could not be revealed");
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Assemble the report. Every string that came from outside this function
/// goes through `redactor` on the way in.
async fn build_report(
    app: &tauri::AppHandle,
    state: &AppState,
    log_dir: &Path,
    redactor: &Redactor,
) -> String {
    let mut out = String::new();
    let redact = |value: &str| redactor.redact(value);

    let _ = writeln!(out, "# Commonspace diagnostics\n");
    let _ = writeln!(
        out,
        "Written {} by Commonspace, on this computer. Nothing was sent anywhere: this file exists \
         so you can read it and then decide whether to share it.\n",
        chrono::Utc::now().to_rfc3339()
    );
    let _ = writeln!(
        out,
        "Home directory paths, your username on this computer, and anything shaped like an API key \
         have been replaced with `{HOME_PLACEHOLDER}`, `{USER_PLACEHOLDER}` and \
         `{SECRET_PLACEHOLDER}`. Names of your own files and folders can still appear in the log \
         excerpt at the end — read it before you send this to anyone.\n"
    );

    /* application */
    let _ = writeln!(out, "## Application\n");
    let _ = writeln!(out, "- Version: {}", app.package_info().version);
    let _ = writeln!(
        out,
        "- Build target: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    match tauri::webview_version() {
        Ok(version) => {
            let _ = writeln!(out, "- System webview: {version}");
        }
        Err(error) => {
            // On Windows this is the answer to a whole class of "it doesn't
            // start" reports, so the failure itself is the useful part.
            let _ = writeln!(
                out,
                "- System webview: not reported ({})",
                redact(&error.to_string())
            );
        }
    }

    /* operating system */
    let info = os_info::get();
    let _ = writeln!(out, "\n## Operating system\n");
    let _ = writeln!(out, "- Type: {}", info.os_type());
    let _ = writeln!(out, "- Version: {}", info.version());
    let _ = writeln!(
        out,
        "- Edition: {}",
        info.edition().unwrap_or("not reported")
    );
    let _ = writeln!(out, "- Bitness: {}", info.bitness());

    /* providers */
    let _ = writeln!(out, "\n## Providers\n");
    for adapter in state.adapters() {
        let _ = writeln!(out, "### {}\n", adapter.id().display_name());
        match adapter.detect().await {
            commonspace_core::InstallStatus::Installed { version, path } => {
                let _ = writeln!(out, "- Installed: yes, version {}", redact(&version));
                let _ = writeln!(out, "- Location: `{}`", redact(&path.to_string_lossy()));
            }
            commonspace_core::InstallStatus::NotInstalled => {
                let _ = writeln!(out, "- Installed: no");
            }
            commonspace_core::InstallStatus::Broken { detail } => {
                let _ = writeln!(
                    out,
                    "- Installed: present but unusable — {}",
                    redact(&detail)
                );
            }
        }
        let health = adapter.health().await;
        let _ = writeln!(
            out,
            "- Health: {}",
            if health.healthy {
                "healthy"
            } else {
                "reporting a problem"
            }
        );
        for check in health.checks {
            let _ = writeln!(
                out,
                "  - {}: {}{}",
                redact(&check.name),
                if check.passed { "passed" } else { "failed" },
                check
                    .detail
                    .map(|detail| format!(" — {}", redact(&detail)))
                    .unwrap_or_default()
            );
        }
        // Auth *status* is deliberately absent: it says which plan a person
        // pays for, which describes the user rather than the bug.
        let _ = writeln!(out);
    }

    /* storage */
    let _ = writeln!(out, "## Storage\n");
    let _ = writeln!(out, "- Journal mode: {}", journal_mode(app, state));

    /* configuration */
    let _ = writeln!(out, "\n## Configuration\n");
    let _ = writeln!(
        out,
        "- Theme: {}",
        state
            .storage()
            .get_setting::<String>("theme")
            .ok()
            .flatten()
            .unwrap_or_else(|| "match system".to_owned())
    );
    let _ = writeln!(
        out,
        "- Finished-task notifications: {}",
        match state.storage().get_setting::<bool>("notifications.enabled") {
            Ok(Some(true)) => "on",
            _ => "off",
        }
    );
    // Counts only. Workspace names and authorized folder paths are the user's
    // own project vocabulary and say nothing about a bug in Commonspace.
    match state.storage().list_workspaces() {
        Ok(workspaces) => {
            let roots: usize = workspaces.iter().map(|w| w.roots.len()).sum();
            let _ = writeln!(
                out,
                "- Projects: {} ({roots} authorized folders in total)",
                workspaces.len()
            );
        }
        Err(error) => {
            let _ = writeln!(
                out,
                "- Projects: could not be read ({})",
                redact(&error.to_string())
            );
        }
    }
    let _ = writeln!(
        out,
        "- RUST_LOG: {}",
        std::env::var("RUST_LOG")
            .map(|value| redact(&value))
            .unwrap_or_else(|_| "not set".to_owned())
    );

    /* log */
    let _ = writeln!(out, "\n## Log\n");
    match newest_log_file(log_dir) {
        Some(file) => {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "the log file".to_owned());
            match tail_lines(&file, REPORT_LOG_LINES) {
                Ok(lines) => {
                    let _ = writeln!(out, "Last {} lines of `{name}`:\n", lines.len());
                    // Four backticks, because a log line can perfectly well
                    // contain three and end the block early.
                    let _ = writeln!(out, "````");
                    for line in lines {
                        let _ = writeln!(out, "{}", redact(&line));
                    }
                    let _ = writeln!(out, "````");
                }
                Err(error) => {
                    let _ = writeln!(
                        out,
                        "`{name}` could not be read: {}",
                        redact(&error.to_string())
                    );
                }
            }
        }
        None => {
            let _ = writeln!(
                out,
                "No log file has been written yet in this installation's log folder."
            );
        }
    }

    out
}

/// What the database is actually journaling with.
///
/// Preferred from the recorded setting, so the answer is the one the storage
/// layer itself believes. When nothing has recorded it, the write-ahead log's
/// sidecar file next to the database is an observation rather than a guess,
/// and is reported as exactly that.
fn journal_mode(app: &tauri::AppHandle, state: &AppState) -> String {
    if let Ok(Some(recorded)) = state.storage().get_setting::<String>(JOURNAL_MODE_SETTING) {
        return format!("{recorded} (recorded by the storage layer)");
    }
    match app.path().app_data_dir() {
        Ok(data_dir) if data_dir.join("commonspace.db-wal").exists() => {
            "not recorded; a write-ahead-log sidecar file is present next to the database"
                .to_owned()
        }
        _ => "not recorded".to_owned(),
    }
}

/// The newest rolled log file in `directory`.
///
/// Daily filenames carry an ISO date, so they sort in the order they were
/// written and the last name is the file currently being appended to.
fn newest_log_file(directory: &Path) -> Option<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(directory)
        .ok()?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy())
                .is_some_and(|name| name.starts_with(LOG_PREFIX) && name.ends_with(LOG_SUFFIX))
        })
        .collect();
    names.sort();
    names.pop()
}

/// The last `limit` lines of `path`.
fn tail_lines(path: &Path, limit: usize) -> io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // A seek can land inside a multi-byte character; decoding lossily keeps
    // the rest of the tail readable instead of losing the report to one byte.
    let text = String::from_utf8_lossy(&bytes);

    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // Whatever the seek landed in the middle of is a fragment, not a line.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    Ok(lines)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(Some("/home/ada"), Some("ada")).unwrap()
    }

    #[test]
    fn the_home_directory_is_replaced_wherever_it_appears() {
        let out = redactor()
            .redact("opened /home/ada/.config/commonspace and /home/ada/Documents/notes.md");
        assert!(!out.contains("/home/ada"), "{out}");
        assert!(out.contains("<home>/.config/commonspace"), "{out}");
        assert!(out.contains("<home>/Documents/notes.md"), "{out}");
    }

    #[test]
    fn windows_home_directories_are_replaced_in_both_separator_forms() {
        let redactor = Redactor::new(Some(r"C:\Users\Ada"), Some("Ada")).unwrap();
        let out = redactor.redact(r"C:\Users\Ada\AppData\Local and C:/Users/Ada/AppData/Local");
        assert!(!out.to_lowercase().contains("ada"), "{out}");
        assert_eq!(out.matches("<home>").count(), 2, "{out}");
    }

    #[test]
    fn other_peoples_home_directories_are_replaced_too() {
        // A log copied from another machine, or a path under a different
        // account, still names a person.
        let out = redactor().redact("failed to read /Users/grace/Library/Logs/x.log");
        assert!(!out.contains("grace"), "{out}");
        assert!(out.contains("/Users/<user>/Library/Logs/x.log"), "{out}");
    }

    #[test]
    fn the_bare_username_is_replaced() {
        let out = redactor().redact("running as ada (uid 1000)");
        assert!(!out.contains("ada"), "{out}");
        assert!(out.contains("<user>"), "{out}");
    }

    #[test]
    fn a_username_that_is_a_substring_of_a_word_is_left_alone() {
        // Word boundaries, so a short login does not shred ordinary prose.
        let out = redactor().redact("adapter loaded");
        assert_eq!(out, "adapter loaded");
    }

    #[test]
    fn api_key_shapes_are_replaced() {
        // Every fixture is assembled from pieces rather than written out
        // whole. A credential-shaped literal in a source file is a literal
        // a secret scanner has to judge, and it will sometimes judge it a
        // leak and block the push — which is the scanner working correctly.
        // Splitting the prefix keeps the string the redactor sees identical
        // while leaving nothing in the file that looks like a key.
        let fixtures = [
            format!("ANTHROPIC_API_KEY={}-ant-api03-{}", "sk", "A".repeat(36)),
            format!("openai key {}-proj-{}", "sk", "B".repeat(30)),
            format!("token {}_{}", "ghp", "C".repeat(36)),
            format!("{}_11{}", "github_pat", "D".repeat(40)),
            format!("slack {}-1111111111-2222222222-{}", "xoxb", "E".repeat(16)),
            format!("aws {}IOSFODNN7EXAMPLE", "AKIA"),
            format!("google {}SyA{}", "AIza", "F".repeat(32)),
            format!(
                "Authorization: Bearer {}.{}.{}",
                "eyJhbGciOiJIUzI1NiJ9",
                "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
                "G".repeat(43)
            ),
        ];
        for fixture in &fixtures {
            let out = redactor().redact(fixture);
            assert!(
                out.contains(SECRET_PLACEHOLDER),
                "nothing was redacted in {fixture:?} -> {out:?}"
            );
            // Assembled for the same reason as the fixtures above.
            for secret in [
                format!("{}-ant", "sk"),
                format!("{}_CCC", "ghp"),
                format!("{}_11DDD", "github_pat"),
                format!("{}-1111111111", "xoxb"),
                format!("{}IOSFODNN7", "AKIA"),
                format!("{}SyA", "AIza"),
                "eyJhbGciOiJIUzI1NiJ9".to_string(),
            ] {
                let secret = secret.as_str();
                assert!(
                    !out.contains(secret),
                    "{secret:?} survived redaction of {fixture:?} -> {out:?}"
                );
            }
        }
    }

    #[test]
    fn a_key_shaped_string_with_no_recognized_prefix_is_still_replaced() {
        // The rule that has to hold for a provider nobody here has heard of.
        let out = redactor().redact("value: 8f3Ab21ceD94f0778aB3c5D6e7F8901234a5b6C7");
        assert!(out.contains(SECRET_PLACEHOLDER), "{out}");
        assert!(!out.contains("8f3Ab21ceD94"), "{out}");
    }

    #[test]
    fn ordinary_prose_and_versions_survive() {
        let text = "Claude Code 2.1.222 started in 41ms; 12 documents read";
        assert_eq!(redactor().redact(text), text);
    }

    #[test]
    fn redacting_twice_changes_nothing_the_second_time() {
        let once = redactor().redact("/home/ada ran with sk-ant-api03-AbCdEfGhIjKlMnOpQrSt");
        assert_eq!(redactor().redact(&once), once);
    }

    #[test]
    fn a_redactor_without_a_home_or_username_still_removes_credentials() {
        // The environment can be missing both on a stripped-down system; the
        // credential rules must not depend on them.
        let redactor = Redactor::new(None, None).unwrap();
        let out = redactor.redact("sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx");
        assert_eq!(out, SECRET_PLACEHOLDER);
    }

    #[test]
    fn the_newest_log_file_is_the_one_reported() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "commonspace.2026-08-05.log",
            "commonspace.2026-08-07.log",
            "commonspace.2026-08-06.log",
            "notes.txt",
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let newest = newest_log_file(dir.path()).unwrap();
        assert_eq!(
            newest.file_name().unwrap().to_string_lossy(),
            "commonspace.2026-08-07.log"
        );
    }

    #[test]
    fn no_log_files_means_no_answer_rather_than_a_wrong_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "x").unwrap();
        assert!(newest_log_file(dir.path()).is_none());
    }

    #[test]
    fn the_tail_is_the_end_of_the_file_and_no_longer_than_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commonspace.2026-08-07.log");
        let body: String = (0..500).map(|n| format!("line {n}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let lines = tail_lines(&path, 10).unwrap();
        assert_eq!(lines.len(), 10);
        assert_eq!(lines.first().map(String::as_str), Some("line 490"));
        assert_eq!(lines.last().map(String::as_str), Some("line 499"));
    }

    #[test]
    fn a_short_file_is_returned_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commonspace.2026-08-07.log");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(tail_lines(&path, 100).unwrap(), vec!["one", "two"]);
    }

    #[test]
    fn a_long_webview_message_is_cut_and_says_so() {
        let long: String = std::iter::repeat_n('x', MAX_WEBVIEW_CHARS + 50).collect();
        let clamped = clamp(&long, MAX_WEBVIEW_CHARS);
        assert!(clamped.ends_with("… (truncated)"));
        assert_eq!(
            clamped.chars().filter(|c| *c == 'x').count(),
            MAX_WEBVIEW_CHARS
        );
        assert_eq!(clamp("short", MAX_WEBVIEW_CHARS), "short");
    }
}
