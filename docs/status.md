# Status

What is built and verified, what is partial, and what has not been started.
Kept honest deliberately: the project's whole premise is not overstating what
it does.

Last verified: August 2026, on Windows 11 with Claude Code 2.1.222 and Codex
CLI 0.146.0 installed and signed in.

## Verified working end to end

These were exercised against the real, installed, authenticated CLIs — not
mocks. The proof lives in `crates/commonspace-runtime/tests/vertical_slice.rs`
and `crates/commonspace-agents/tests/provider_smoke.rs`, both `#[ignore]` by
default and run with `scripts/smoke-providers`.

- **Connect → authorize → delegate → verify → undo.** Authorize a folder,
  start a task with Claude Code, watch the agent call Commonspace's own tools
  over MCP, see the file appear as an artifact, and undo it. The test asserts
  the file's *contents*, the readable timeline entry, the artifact record, a
  successful undo, and the persisted task history.
- **Approval actually gates the write.** A modification raises a permission
  request and the file is untouched until it is answered; declining leaves the
  original bytes in place. Verified both deterministically
  (`tools::tests::denied_modify_leaves_the_file_alone`) and against a live
  agent (`declining_an_approval_changes_nothing`).
- **Scope enforcement.** Writes outside the authorized roots are never silent:
  they raise an approval naming the resolved path, and a decline creates
  nothing. Protected system and credential locations are denied outright, even
  if the user authorized a parent folder.
- **Provider detection and truthful auth status.** `claude auth status` and
  `codex login status` are parsed for real; the Connections screen reports the
  actual plan (for example "Connected · Claude Max") rather than a guess.
- **Document creation with validation.** A generated `.docx` is re-read by
  Commonspace's own reader before success is reported; a missing line fails the
  operation instead of producing a file the user later finds is empty.
- **Crash recovery.** Tasks left running by a previous process are found on
  startup and failed with an explanation, rather than spinning forever.
- **The application launches and talks to its own backend.** The window opens,
  the interface renders in both themes, and the first-run state appears. The
  Connections check runs through real Tauri IPC and Zod validation on startup
  and correctly detects the signed-in provider.

## Built, with tests, not yet exercised by a human in the running app

The Rust side of each of these is covered by unit tests and compiles into the
shipped binary, but no one has yet clicked through them:

- Interactive use of the assembled application: sending a prompt, answering
  an approval dialog, and undoing from the artifact card. Every one of those
  paths is verified in tests against the real CLI; what is unverified is the
  click-through. **This is the first thing to do next.**

  The first real install did surface a packaged-build-only bug: every
  provider CLI probe opened a visible console window, because the
  `CREATE_NO_WINDOW` flag was being overwritten by process-wrap's job-object
  wrapper. It is invisible in development, where the app already owns a
  console that children inherit. Fixed and verified against a packaged
  build; `crates/commonspace-agents/examples/console_probe.rs` reproduces
  both paths. The lesson generalises: a dev build is not evidence about a
  packaged one.

  Console windows during a task were reported again even with that fix in
  place. Everything Commonspace spawns is created hidden, but Claude Code
  spawns processes of its own — most notably its background auto-updater,
  which runs on every fresh `claude -p` invocation, i.e. once per message —
  and those spawns flashing console windows on Windows is a long-standing
  upstream Claude Code issue with no parent-side flag that reaches
  grandchildren. Two mitigations shipped, neither yet verified by a human
  on a packaged Windows build: the CLI's auto-updater and telemetry are
  disabled for Commonspace's invocations via the CLI's documented
  environment variables (`DISABLE_AUTOUPDATER`,
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`), and the per-message
  `claude auth status` probe is gone — a recent "signed in" answer is now
  cached instead of re-spawning the CLI before every task. If a console
  still appears after this, it is coming from inside Claude Code and needs
  an upstream fix.
- Codex CLI task execution. The adapter's detection, auth probe, event
  normalization, and error path are verified live; a full task run was not,
  because the test account hit its usage limit during the verification pass.
- Conversation history replay in the UI (`list_task_events` is implemented and
  tested at the storage layer; the UI does not yet call it on open).
- Update checks in Settings. "Check for updates" asks the GitHub releases
  API for the newest published release and either installs it in place
  (signed releases, once signing is configured — see `docs/releasing.md`)
  or opens the download page (today's unsigned releases). The Rust side is
  unit-tested; nobody has clicked the button against a real newer release
  yet.
- Attachments. The composer collects paths and appends them to the prompt;
  they are not yet uploaded, previewed, or scope-checked separately.

## Not started

Named here so the roadmap is not mistaken for the product:

- Plan approval as a distinct step. The task state machine has
  `AwaitingApproval` and the orchestrator can resolve a plan, but no adapter
  emits `plan.created` yet, so tasks currently run without a plan gate. Per-
  operation approval still applies.
- Formatting-preserving DOCX edits, XLSX, PPTX, OCR, and format conversion.
- Artifact previews. Artifacts are listed, opened in their default
  application, and revealed in the file manager; there is no in-app preview
  of PDF pages, documents, or diffs yet.
- Reusable skills and workflows, MCP server management UI, browser automation,
  cloud integrations, additional providers, local models.
- Installer verification on macOS and Linux. The code is cross-platform and
  the Windows build works; the other two have not been built or run, and each
  must be built on its own operating system.

## Known limitations

- **Sandboxing is partial and inherited.** Commonspace's own tools are
  strictly path-scoped in Rust. The provider CLI is configured with its most
  restrictive suitable flags, but Commonspace does not add OS-level
  containment of the child process in v1. See THREAT_MODEL.md for the
  per-layer breakdown.
- **Local data is not encrypted at rest.** Conversations, history, and backups
  rely on ordinary operating-system file permissions.
- **Provider behaviour can change without notice.** Every CLI flag and event
  shape recorded in docs/research.md was verified in August 2026 against the
  versions above; re-verify before assuming any of it still holds.
