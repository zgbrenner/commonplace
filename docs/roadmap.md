# Roadmap

This is direction, not a commitment. For what is actually built and verified
today, see [docs/status.md](status.md) — that document is the honest one;
this one is the plan. Items already underway say so, and point back to
status.md rather than restating its claims.

## Guiding principle

Commonspace's core loop already works end to end (see status.md), but it has
only been exercised by tests, not worn in by daily use. The single best next
move is not a new integration — it's making the loop people will run a
hundred times a day feel excellent:

> Choose files → describe work → review plan → approve → watch progress →
> inspect exact changes → verify output → undo safely → reopen the
> conversation later with everything intact.

Until that loop is excellent, more integrations are mostly decorative
horsepower. Most of what follows serves that loop directly — a better
changes view, real attachments, durable tasks, and search all make those
nine steps easier to trust. Later themes (workflows, additional file
formats, a broader security posture) are real, but they build on top of the
loop rather than compete with it for attention.

The current sequencing, in order: conversation replay ("reopen later with
everything intact"), a persisted attachment model with plain disclosure,
and a task-outcome view; then a real plan-approval stage (approve the
overall plan once, instead of twenty individual writes); then a
knowledge-worker UX pass (projects instead of workspaces in the interface,
a recommended default agent, generated titles, file-aware suggestions,
first-run onboarding, completion notifications, and calm plain progress by
default with technical events under details) — all in progress; then the
document features in the order theme 6 lists.

## 1. A "Changes" center

Right now a task reports success or failure without a single place to
inspect what happened. The plan: a structured changeset per task, listing:

- Files read, created, modified, moved, and deleted.
- Before/after diffs for text and structured formats.
- Rendered previews for PDF and DOCX output (status.md lists artifact
  previews as not started).
- Validation results — what Commonspace's own readers confirmed before
  reporting success.
- Backups taken, so undo's source of truth is visible, not implicit.
- Any data sent off-machine, and to where.
- Provenance per change: which agent, which model, which tool call, which
  task produced it.
- Undo controls scoped to a single file *and* to a whole task.

Permission prompts feed the same idea backward: a modification prompt shows
the proposed diff, and an upload prompt shows the destination and exact
outbound content — before either happens, not after.

## 2. First-class attachments

Today the composer collects file paths and appends them as plain text into
the prompt; status.md already flags this as unfinished ("not yet uploaded,
previewed, or scope-checked separately"). The fix is a real `Attachment`
model instead of string injection:

- Canonical path plus its relationship to the workspace.
- Type, size, hash, and modified date.
- Estimated token cost before the file is sent.
- Explicit disclosure when an attachment will be uploaded to a provider.
- Extraction status and warnings (e.g. a PDF that partially failed to parse).
- Preview and metadata in the UI, not just a filename chip.
- Its own permission status, independent of the surrounding task.
- Persistent association with the conversation or task it belongs to, so it
  survives a restart the way the conversation does.

Drag-and-drop path handling has a separate, narrower fix in progress now;
this item is the broader model it will eventually plug into.

## 3. Durable task execution

The task state machine (`Draft → Planning → AwaitingApproval → Running →
Completed`, with `Paused`, `Failed`, `Cancelled`, `RolledBack`) and crash
recovery already exist and are verified (status.md; ARCHITECTURE.md's "Task
state machine"). Provider sessions are resumable and events persist to
`task_events` — that's the groundwork. What's missing is durability around
it:

- A real queue with concurrency limits, instead of one task at a time.
- Pause and resume as user-facing actions, not just internal states.
- Checkpoint restart, so a long task doesn't start over after an interruption.
- Automatic retry of provider calls on transient failures.
- Running in the system tray, so a task can proceed while the window is
  closed.
- Native OS notifications when a task needs approval or finishes.
- Per-task visibility into elapsed time, usage, and current step.
- Resource limits, so one task can't monopolize the machine.
- Graceful handling of rate limits and an exhausted subscription — surfaced
  as a clear state, not a generic error.
- A user-approved option to continue a task with a different provider when
  the original one is unavailable, rather than failing outright.

## 4. Workflows and skills

Reusable skills and workflows are explicitly out of scope for the MVP
(status.md, "Not started"; ARCHITECTURE.md's deferral list); this is the
design for what comes after.

The container format is decided: the [Agent Skills
specification](https://agentskills.io/specification), rather than a bespoke
Markdown/YAML package of our own. A workflow is a directory whose `SKILL.md`
carries YAML frontmatter — `name` and `description`, plus the optional
`license`, `compatibility`, `metadata` and `allowed-tools` — followed by
Markdown instructions, alongside `references/` and `assets/`
subdirectories. Loading is progressive: name and description at startup, the
body once the workflow is chosen, referenced files only when a step needs
them.

Two consequences matter more than the file format itself:

1. **A Commonspace workflow is also a working Claude Code skill.** For a
   product whose premise is "use the CLI you already pay for", a workflow
   that also runs in the CLI the user already has is worth more than a
   better-designed private format.
2. **`allowed-tools` is documented but not enforced.** The spec marks the
   field experimental and says support varies between implementations, and
   it has been reported repeatedly against Claude Code that tools stay
   accessible regardless (anthropics/claude-code issues
   [37683](https://github.com/anthropics/claude-code/issues/37683),
   [27099](https://github.com/anthropics/claude-code/issues/27099) and
   [14956](https://github.com/anthropics/claude-code/issues/14956)). So it
   is a *declaration* — something the UI shows the user before they run a
   workflow — and never an enforcement point. The permission envelope is
   enforced by Commonspace's own policy engine, the same one that gates
   every other operation, never by that field. This is the ecosystem
   rediscovering the principle this project already holds: a manifest that
   declares what something may do is documentation, not a control.

Commonspace's own additions ride inside `metadata` rather than as new
top-level frontmatter keys, so a workflow stays a valid skill:

- A permission envelope — what it's allowed to touch, declared up front and
  enforced by `commonspace-permissions`.
- Deterministic pre- and post-processing steps around the model call.
- Test fixtures, so a workflow can be verified the same way the app is.
- Validation rules for its own results, and expected outputs.
- Input variables, required folders, and provider requirements (which
  agents/models it needs).
- Version and author metadata.

Descriptions and worked examples go in the `SKILL.md` body and
`references/`. The spec's optional `scripts/` directory is deliberately
**not** supported in v1: it is the one part of the format that introduces
arbitrary code execution, and a package that can only describe work — never
carry an executable — keeps that surface at zero.

Candidate first workflows, chosen for being common and boring rather than
impressive: compare two contracts, PDFs into a spreadsheet, rename a folder
of scans, a research report with sources, dedupe and organize a folder,
documents into Markdown, redact sensitive information, and assemble meeting
materials from a folder.

Workflow actions in the UI: dry-run, duplicate-and-edit, export, inspect
source. No marketplace until there's a mature trust model for third-party
packages — publishing one earlier would invite exactly the supply-chain and
prompt-injection risk THREAT_MODEL.md takes seriously.

## 5. Workspace search before vector RAG

Conversation and message full-text search is being built right now; this
item is the fuller shape it's headed toward. Search should cover
conversations, messages, artifact metadata, and extracted document text via
SQLite FTS5, with:

- Incremental, hash-based indexing (only re-index what changed).
- Permission-aware results — search only surfaces authorized roots, never
  files the user hasn't granted access to.
- Filters by workspace, file type, date, task, and provider.
- Citations that open the exact file and page a result came from, not just a
  filename.

Local embeddings and vector search are a later addition, not a prerequisite —
FTS5 covers the large majority of "find that thing I asked about" without the
cost and complexity of an embedding pipeline.

## 6. Deterministic document engine

`commonspace-documents` already does Markdown/text editing and DOCX/PDF work
for the MVP (ARCHITECTURE.md, "Document tooling";
[docs/document-tools.md](document-tools.md)); status.md lists
formatting-preserving DOCX edits, PPTX, OCR, and format conversion as not
started, and spreadsheet reading and creation as built and tested but not
yet exercised by a human. The order below is deliberate — it serves
knowledge workers before it serves integrations, and it comes before
adding new providers, local-model configuration, browser automation,
arbitrary MCP servers, vector RAG, or multi-agent controls:

1. XLSX reading and polished spreadsheet creation, including formulas and
   validation (in progress — see status.md for what is built and what is
   still unverified).
2. OCR for scans and image-only PDFs, with confidence scores and
   page-level provenance — not a black-box transcription.
3. DOCX edits that preserve existing formatting instead of regenerating
   the file.
4. PDF page rendering, previews, and source citations that open the exact
   page.
5. PPTX creation and editing.
6. OneDrive and SharePoint folder awareness.
7. Email draft creation.
8. Saved routines and skills (see theme 4).

Cross-cutting, in support of the above: format conversion; tables,
comments, tracked changes, headers, footers, and footnotes as first-class
structures that survive round-trips; streaming for large documents so a
300-page file doesn't require holding the whole thing in memory.

All of it keeps the existing rule: success is reported only after
Commonspace's own reader confirms the result on disk, never because the
agent said so.

## 7. Engineering

A few files carry more than their share and should split along existing
seams before they get harder to touch: `apps/desktop/src/App.tsx` (~600
lines) into feature modules (task view, composer, permissions, connections)
under `apps/desktop/src/components` and `apps/desktop/src/lib`;
`crates/commonspace-runtime/src/tools.rs` (~1,100 lines) split by tool
category (filesystem, document, artifact) behind the existing MCP server
boundary; `crates/commonspace-storage/src/store.rs` (~900 lines) split by
entity (tasks, conversations, permissions, artifacts) behind the existing
repository pattern; and provider adapters
(`crates/commonspace-agents/src/claude.rs`, `codex.rs`) factored so shared
process/event-parsing logic isn't copied per provider as more are added.

Other engineering work:

- Task state managed through a reducer/store on the frontend instead of ad
  hoc component state, matching the state machine already enforced in
  `commonspace-core`.
- Generating the TypeScript side of the IPC contract from the Rust types
  (Specta or ts-rs) instead of hand-mirroring `packages/protocol`, while
  keeping runtime Zod validation — generation removes drift, it doesn't
  replace the safety net.
- Storage: write-ahead logging (in progress), integrity checks, FTS5 (in
  progress, theme 5), artifact version history, attachment records (theme
  2), task-queue fields (theme 3), backup retention policy, usage reporting,
  export/import, a pre-migration backup, and pagination with bounded event
  retention so history doesn't grow unbounded.
- A provider compatibility layer: declared version ranges per adapter,
  recorded event-format fixtures, capability probing at startup,
  supported/untested/incompatible states shown honestly in the UI, feature
  flags per provider, a redacted diagnostics bundle for bug reports, and a
  warning when a detected provider version is newer than anything verified.

## 8. Testing and release readiness

- Component tests for permissions, plans, artifacts, error states, and event
  replay.
- Tauri end-to-end tests covering onboarding and a full task loop, not just
  unit-level coverage.
- Smoke tests against the packaged build specifically — status.md already
  shows one bug (`CREATE_NO_WINDOW` on Windows) that only appeared packaged,
  invisible in development.
- Accessibility checks.
- Property-based tests for the permission policy engine.
- Fuzzing for parsers, path handling, archive extraction, and provider event
  streams — extending THREAT_MODEL.md's existing fuzz-style document
  fixtures.
- Golden document fixtures for the deterministic document engine.
- Performance tests at tens of thousands of events and files.
- Branch protection and PR-based changes as the norm.
- Code signing, notarization, and signed updater manifests before a broad
  launch — status.md notes today's releases are unsigned; this is the
  precondition for in-place update installs.

## 9. Security

- Optional at-rest encryption, keyed via the OS credential vault — status.md
  and THREAT_MODEL.md both currently state local data is unencrypted; this
  changes that without changing the disclosure until it ships.
- OS-level containment beyond today's partial, inherited sandboxing
  (THREAT_MODEL.md, "Sandboxing"): AppContainer and restricted tokens on
  Windows, sandbox profiles on macOS, Landlock/Bubblewrap on Linux.
- Per-task network destination allowlists, sharpening today's coarser
  network policy gate.
- File-handle-based filesystem operations, closing the residual
  symlink/TOCTOU race between a path being checked and being used.
- Automatic secret detection before content reaches a provider.
- A "what the provider will see" preview, extending theme 1's
  upload-disclosure idea to every provider-bound send.
- A redacted diagnostics bundle a user can safely attach to a bug report.
- Signed workflow packages, once workflows (theme 4) exist to sign.
- External security review before allowing third-party MCP servers —
  THREAT_MODEL.md already names them a post-MVP surface with mitigations
  designed but not shipped; this is the gate before that surface opens.

## 10. Small UX with outsized value

- Markdown rendering in the chat view (in progress).
- A command palette and keyboard shortcuts.
- Searchable conversation history (in progress, see theme 5).
- Editable task titles (in progress).
- File-aware prompt suggestions based on what's in the authorized folders
  (in progress).
- Empty states tailored to context instead of one generic placeholder
  (in progress).
- Compact and expanded progress views, so a task can be glanced at or
  inspected in depth (in progress).
- Clear visibility into which provider, model, and billing mode (subscription
  vs. API) a task is using — matching the truthful-status principle already
  in README.md and ARCHITECTURE.md.
- A diagnostics dashboard for provider health and recent errors.
- Reduced-motion and high-contrast display options.
- An onboarding demo that uses disposable sample files, so a new user can see
  the approve/watch/undo loop before pointing the app at anything real.
- An "open backup" action for the cases where undo isn't safe, rather than
  leaving the user to find the backup file manually.
- One-click copy of a report, a changeset, or provenance information, for
  pasting elsewhere.

## How this document is used

Honest, current status lives in [docs/status.md](status.md) — what's built,
partial, or not started. This file is direction, not a commitment: items
move, get reordered, or get dropped as the guiding-principle loop above
teaches us more. An item here is checked off by linking the pull request
that shipped it, not by editing this file to say "done" — that belongs in
status.md.
