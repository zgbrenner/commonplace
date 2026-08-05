# Commonspace Architecture

Commonspace is an open-source desktop workspace where users bring their own AI
subscription, choose what files an agent may access, and delegate real work
through a simple chat interface. This document describes the system's structure
and the reasoning behind its core decisions.

## Design principles

1. **Local-first.** Conversations, task history, artifacts, backups, and audit
   records live on the user's machine in SQLite and plain files. Nothing is
   sent anywhere except to the AI provider the user explicitly selected.
2. **Deterministic safety.** Permission checks, path scoping, backups, and
   result verification are enforced in Rust code. An LLM's opinion is never
   the mechanism that decides whether an operation is safe.
3. **Provider-independent core.** All providers are integrated through one
   adapter interface emitting one normalized event protocol. The UI never
   contains provider-specific logic.
4. **Calm surface, honest depth.** Nontechnical users see human-readable
   progress; a collapsible developer view preserves raw commands, provider
   events, and diagnostics without ever being required.
5. **Verify, don't trust.** A file operation is reported successful only after
   Commonspace deterministically confirms the result on disk (existence,
   parseability, validation), never because an agent claimed success.

## Repository layout

```
commonplace/                     # repo name; product name is Commonspace
├── apps/
│   └── desktop/                 # Tauri 2 application
│       ├── src/                 # React + TypeScript + Vite + Tailwind + shadcn/ui
│       └── src-tauri/           # thin Tauri shell: command/event wiring only
├── crates/
│   ├── commonspace-core/        # domain types, normalized event protocol,
│   │                            #   task state machine, ids, errors, fs tools
│   ├── commonspace-agents/      # AgentAdapter trait, Claude Code + Codex
│   │                            #   adapters, process lifecycle management
│   ├── commonspace-permissions/ # deterministic policy engine, path guard,
│   │                            #   audit journal
│   ├── commonspace-documents/   # Markdown/text/PDF/DOCX tooling + validation
│   └── commonspace-storage/     # SQLite (rusqlite) + migrations + repositories
├── packages/
│   └── protocol/                # TypeScript types + Zod schemas mirroring
│                                #   the Rust serde models at the IPC boundary
├── docs/
├── scripts/                     # local quality gates; no hosted CI
└── tests/
    └── fixtures/                # documents, malformed files, tricky filesystems
```

Deliberately omitted: a separate `packages/ui` and `packages/shared`
(components live in the app until a second consumer exists) and a
`commonspace-workflows` crate (skills/workflows are post-MVP; the format is
documented so the crate can be added without redesign).

## Process model

```
┌────────────────────────────────────────────────────────────┐
│ Tauri process (single)                                     │
│                                                            │
│  WebView (React UI)                                        │
│    │  typed commands / streamed Channel events             │
│  Rust host                                                 │
│    ├── task orchestrator (state machine)                   │
│    ├── permission engine  (deterministic)                  │
│    ├── storage (SQLite)                                    │
│    ├── document tools                                      │
│    └── agent adapters ──spawn──► provider CLI subprocess   │
│                                    (claude / codex / ...)  │
│                                        │ stdio JSON stream │
│                                        ▼                   │
│                              Commonspace MCP tool server   │
│                              (same Rust host, stdio)       │
└────────────────────────────────────────────────────────────┘
```

- One Tauri process; no embedded Node server, no duplicate browser processes.
- Provider CLIs run as child processes with piped stdio (no PTY needed for
  headless JSON modes). Process trees are terminated reliably (job objects on
  Windows, process groups on Unix) and cleaned up on crash recovery.
- Commonspace exposes its own safe tools to the agent via an MCP stdio server
  so document operations, artifact generation, and scoped filesystem actions
  run through deterministic Rust code rather than improvised shell commands.

## Normalized agent event protocol

Every adapter translates provider output into one internal event stream
(`commonspace-core::AgentEvent`, a serde-tagged enum):

```
message.started      message.delta        reasoning.summary
plan.created         plan.updated
tool.requested       tool.started         tool.progress       tool.completed
permission.requested
artifact.created     artifact.modified
warning              error
task.completed
```

Rules:

- Events carry stable ids so the UI can correlate deltas, tool lifecycles, and
  permission answers.
- `tool.*` events carry a human-readable `title`/`detail` ("Reading 12
  documents") computed by the adapter, plus machine fields for the developer
  view.
- Provider-specific raw payloads are preserved as `raw` diagnostics attached to
  the session log, never required by the UI.
- The same events are persisted to `task_events` so a conversation can be
  replayed after restart.

## Agent adapter interface

`commonspace-agents::AgentAdapter` (object-safe, async):

| Capability            | Method / type                                    |
|-----------------------|--------------------------------------------------|
| installation status   | `detect() -> InstallStatus` (path, version)      |
| authentication status | `auth_status() -> AuthStatus`                    |
| launch sign-in        | `launch_auth() -> AuthLaunch`                    |
| models                | `capabilities().models` (when discoverable)      |
| session creation      | `start_session(SessionRequest, EventSink)`       |
| session continuation  | `continue_session(session_id, prompt, EventSink)`|
| permission answer     | `respond_permission(session, request_id, decision)` |
| cancellation          | `cancel(session)`                                |
| context limits        | `capabilities().context` (when known)            |
| attachments           | `capabilities().attachments`                     |
| health diagnostics    | `health() -> HealthReport`                       |
| raw logs              | per-session rotating log file                    |

`AuthStatus` is truthful and specific: `NotInstalled`, `SignedOut`,
`Subscription { plan_hint }`, `ApiKey`, `LocalModel`, `Error { detail }`.
The Connections screen renders these states directly — the UI never guesses.

Auth detection is non-destructive: adapters inspect version output, documented
config/credential file *presence* (never contents beyond what's needed for
status), and cheap read-only CLI invocations. Credentials remain owned by the
provider CLI. If a provider requires Commonspace itself to hold an API key, it
is stored only in the OS credential vault (keyring), never in SQLite.

The user picks the agent per task. Commonspace never switches agents
mid-task unless the user has enabled automatic routing (off by default).

## Task state machine

```
Draft ─► Planning ─► AwaitingApproval ─► Running ─► Completed
             │              │              │  ▲          │
             │              └─(rejected)─► Cancelled     └─► RolledBack
             │                             │  │
             └───────(no side effects)────►│  └── Paused ──┘
                                           ├─► Failed
                                           └─► Cancelled
```

States: `Draft`, `Planning`, `AwaitingApproval`, `Running`, `Paused`,
`Completed`, `Failed`, `Cancelled`, `RolledBack`. Transitions are enforced by
an explicit table in `commonspace-core`; illegal transitions are programming
errors, not warnings.

Before any multistep or consequential operation the agent produces a plan
(files/folders touched, files likely created or modified, external services,
consequential actions needing approval, expected deliverables). Tasks with
material side effects hold in `AwaitingApproval` until the user approves,
edits, or rejects the plan.

## Permission engine

`commonspace-permissions` evaluates every operation deterministically:

```
PolicyInput  { workspace, operation_class, canonical_paths, origin }
PolicyOutput { Allow | RequireApproval { reason } | Deny { reason } }
```

Operation classes: `Read`, `Create`, `Modify`, `Rename`, `Move`, `Delete`,
`Execute`, `Install`, `NetworkFetch`, `Upload`, `Send`, `Publish`, `Secret`.

Mechanics:

- All paths are canonicalized before evaluation; symlinks are resolved and the
  *target* must satisfy scope; `..` traversal cannot escape a workspace root.
- Protected OS directories (system roots, other users' homes, credential
  stores) are denied regardless of workspace configuration.
- Batch renames, cross-folder moves, deletions, executable launches, package
  installs, uploads, sends, and publishes always require approval; permanent
  deletion is disabled by default (safe trash instead).
- Every decision — automatic or user-made — is journaled to `permission_decisions`
  with the evaluated input, for the audit history view.

Provider CLIs also have their own permission systems; adapters configure them
to route permission prompts into Commonspace (see docs/provider-adapters.md)
so the user answers one coherent dialog, and Commonspace's engine remains the
final authority for operations executed through its own tools.

## Filesystem safety and undo

- Workspace = explicitly authorized root paths + settings + history.
- Before any modification or deletion, the original is copied into the
  workspace's backup store; the operation and its inverse are journaled in
  `file_operations`.
- Undo replays the inverse when the target hasn't changed since (verified by
  content hash); otherwise the UI explains why undo isn't safe and offers the
  backup file instead.
- Deletion goes to the OS trash/recycle bin via the `trash` mechanism, not
  `rm`.
- After any operation, results are verified on disk; generated Office/PDF
  artifacts are re-parsed by an independent reader before success is reported.

## Document tooling

`commonspace-documents` provides deterministic operations (no LLM-improvised
binary formats): Markdown/plain-text editing, PDF text extraction and page
previews, DOCX structured extraction and creation for the MVP; XLSX/PPTX/OCR
follow. Every operation returns a structured result:

```
{ success, created[], modified[], backups[], warnings[],
  validation, user_summary, diagnostics }
```

Library choices and validation strategy: docs/document-tools.md.

## IPC contract

- Rust models are `serde` types in `commonspace-core`.
- The frontend mirrors them in `packages/protocol` as TypeScript types with
  Zod schemas; a parity test serializes representative Rust values and
  validates them against the Zod schemas so drift fails CI-equivalent local
  gates.
- Commands are typed Tauri commands; streaming uses a per-task Tauri Channel
  carrying `AgentEvent` (ordered, high-frequency-safe).

## Storage

SQLite via `rusqlite` (bundled) with versioned migrations. Core entities:
`workspaces`, `authorized_roots`, `conversations`, `messages`, `tasks`,
`task_events`, `providers`, `provider_sessions`, `permissions`,
`permission_decisions`, `artifacts`, `file_operations`, `backups`, `skills`,
`mcp_servers`, `settings`. Secrets are never stored in SQLite. Provider
session metadata (ids, resume tokens where the provider supports resumption)
is persisted so conversations continue across restarts.

## Reliability

- Crash recovery: on startup, orphaned `Running` tasks are detected, their
  child processes confirmed dead (or terminated), and the tasks moved to
  `Failed` with a recovery explanation; interrupted file operations are
  reconciled against the journal.
- Cancellation kills the full child process tree and finalizes state.
- Transient provider failures retry with backoff; permanent failures surface
  structured errors with recovery actions.
- Logs rotate; backups have retention controls.

## Sandboxing honesty

Isolation strength varies by OS and provider CLI and is documented, not
oversold: Commonspace's own tools are strictly path-scoped in Rust; provider
CLIs are configured with their most restrictive suitable sandbox/approval
flags; full OS-level containment of arbitrary child processes is *partial* on
all three platforms in v1. THREAT_MODEL.md carries the precise breakdown.

## What the MVP intentionally defers

Stronger Office editing, PPTX, XLSX, OCR, additional providers, local models,
reusable skills, MCP server management UI, browser automation, cloud
integrations, remote access, collaboration. The core task experience ships
first.
