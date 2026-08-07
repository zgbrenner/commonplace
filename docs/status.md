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
- Conversation history replay in the UI. Opening a conversation replays its
  newest task's event stream through the same reducer the live path uses, so
  the reopened view matches what was on screen; artifacts, undo, and the
  provider session all survive. Covered by unit tests over completed, failed,
  cancelled, permission-denied and interrupted tasks; the click-through is
  unverified.
- Update checks in Settings. "Check for updates" asks the GitHub releases
  API for the newest published release and either installs it in place
  (signed releases, once signing is configured — see `docs/releasing.md`)
  or opens the download page (today's unsigned releases). The Rust side is
  unit-tested; nobody has clicked the button against a real newer release
  yet.
- Attachments. Paths are canonicalized, recorded with size, modified time,
  content hash and whether they sit inside the project, and disclosed to the
  user before anything is sent. They are not yet previewed, and extraction
  status is not tracked per attachment.
- The knowledge-worker pass: the interface says "projects", the composer
  hides the agent picker when there is only one agent and marks the
  recommended one otherwise, conversation titles are derived from the prompt
  and improved from a completed task's own summary (never overwriting a name
  someone typed), the empty state offers jobs drawn from a bounded survey of
  the project's folders, first run is a three-step screen, and progress is
  one plain sentence with the step-by-step record under Details. Each piece
  is unit-tested; none has been clicked through in a packaged build.
- Desktop notifications when a task finishes. Off until turned on in
  Settings, sent only when the window is not in front. The wording is
  unit-tested; no one has yet seen one arrive from the operating system.
- Plan approval as a distinct step. A task now plans in a read-only
  provider session (no mutation tools are offered at all), parks in
  `AwaitingApproval`, and executes only when the person presses Start.
  Execution resumes that same provider session under an approval envelope
  covering create, modify, rename and move inside the authorized folders;
  deletes, anything outside those folders, uploads, and protected
  locations still interrupt for a separate answer. "Change plan" sends
  feedback back into planning; Cancel ends the task. Covered by unit and
  orchestrator tests; nobody has yet approved a plan in a packaged build.
- OS-level containment of the provider CLI subprocess. Beyond the flags and
  MCP-only mutation path described elsewhere in this file, a spawn now asks
  the OS for a kernel-enforced floor and reports exactly what it got rather
  than assuming: `Containment::Enforced` names the mechanism,
  `Unavailable` gives the plain-language reason, `NotImplemented` says so
  outright. Linux attempts Landlock, scoped to the authorized workspace
  roots; a kernel without the `landlock` LSM enabled, or too old for the
  ABI Commonspace targets, degrades to `Unavailable` rather than failing
  the spawn. macOS attempts a `sandbox-exec` profile scoped the same way.
  Windows returns `NotImplemented`: AppContainer does not fit a
  filesystem-heavy CLI, and the mechanism that would actually hold — a
  restricted token bound to a dedicated lower-privilege local account —
  needs administrator-approved account provisioning this product does not
  perform; the full evaluation is in
  `crates/commonspace-agents/src/sandbox/windows.rs`, unit-tested and
  verified to compile for real against the `x86_64-pc-windows-gnu` target.
  None of the three platform modules has been exercised in a packaged
  build on its own operating system, so whether Landlock and
  `sandbox-exec` actually apply on a real machine — as opposed to
  compiling and passing under test — is unverified. See THREAT_MODEL.md's
  sandboxing table for the rating this does and does not change.
- Spreadsheets. Reading `.xlsx`, `.xlsm`, `.xls`, `.xlsb` and `.ods` through
  `calamine`, plus `.csv` and `.tsv` parsed directly, with row and sheet
  limits so a large workbook cannot swamp a model's context; and creating a
  new `.xlsx` with a styled header row, frozen panes, autofilter, column
  widths, per-column number formats, and real numbers and dates rather than
  text. A created file is re-parsed with `calamine` before success is
  reported — a different library from the one that wrote it, which makes
  this the strongest validation in the document layer. Covered by unit
  tests; nobody has yet asked an agent for a spreadsheet in a packaged
  build and opened the result in Excel. Creation only: there is no
  formatting-preserving edit of an existing workbook, no charts or images,
  and formulas are written for the recipient's application to evaluate,
  not computed here. See `docs/document-tools.md`.

## Not started

Named here so the roadmap is not mistaken for the product:

- Formatting-preserving DOCX edits, PPTX, OCR, and format conversion.
  Editing an existing spreadsheet in place belongs here too — the
  spreadsheet work listed above reads and creates, it does not modify.
- Artifact previews. Artifacts are listed, opened in their default
  application, and revealed in the file manager; there is no in-app preview
  of PDF pages, documents, or diffs yet.
- Reusable skills and workflows, MCP server management UI, browser automation,
  cloud integrations, additional providers, local models.
- Installer verification on macOS and Linux. The code is cross-platform and
  the Windows build works; the other two have not been built or run, and each
  must be built on its own operating system.

## Known limitations

- **Sandboxing is partial, inherited, and per-platform.** Commonspace's own
  tools are strictly path-scoped in Rust, and the provider CLI is configured
  with its most restrictive suitable flags everywhere. On top of that,
  Linux and macOS spawns now attempt a kernel-enforced boundary (Landlock,
  a `sandbox-exec` profile) that visibly degrades — never silently — to
  running uncontained when the platform cannot provide it; Windows has no
  containment mechanism at all — see
  `crates/commonspace-agents/src/sandbox/windows.rs` for why. None of this
  moves the rating in THREAT_MODEL.md's sandboxing table off *Partial*.
- **Local data is not encrypted at rest.** Conversations, history, and backups
  rely on ordinary operating-system file permissions.
- **Provider behaviour can change without notice.** Every CLI flag and event
  shape recorded in docs/research.md was verified in August 2026 against the
  versions above; re-verify before assuming any of it still holds.
