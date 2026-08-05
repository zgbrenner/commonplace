# Research and Licensing Notes

This document records findings from a survey conducted in August 2026 of
provider CLI integration surfaces, reference desktop applications, Tauri 2
platform behavior, and document-tooling libraries considered for
Commonspace. These are point-in-time findings — CLI flags, event schemas,
licenses, and project activity in this space all change quickly — and
should be re-verified before being relied on for any release decision.
Where a finding was checked against a live install or a live command run,
that is noted; where it rests on documentation or a project's own README,
that is noted too. Nothing here is invented; anything not independently
verified is marked uncertain.

## A. Provider CLI integration surfaces

### Claude Code CLI

Package `@anthropic-ai/claude-code`, v2.1.222 tested locally. **License is
proprietary/commercial** — Claude Code is not OSI open source; it is
governed by Anthropic's Commercial Terms of Service, not a permissive or
copyleft license. This matters for Commonspace only as an integration
target (spawned as a subprocess), never as linked code.

- **Headless invocation:** `claude -p --input-format stream-json
  --output-format stream-json --verbose --include-partial-messages`.
- **Flags verified against the local `--help` output:** `--permission-mode`
  (choices: `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`,
  `plan`), `--allowedTools`, `--disallowedTools`, `--add-dir`, `--model`,
  `--resume`, `--session-id`, `--mcp-config <json or file>`,
  `--strict-mcp-config`, `--json-schema`.
- **Verified absence:** there is no `--permission-prompt-tool` flag in
  v2.1.222 — a grep of the local `--help` output found nothing. The
  "MCP permission-prompt-tool bridge" pattern used by some older
  third-party GUIs is therefore not available on this version. Commonspace
  works around this by denying Claude Code's own mutating tools outright
  (`--disallowedTools`) and routing every mutation through Commonspace's
  own MCP tool server instead of trying to intercept Claude's native
  permission prompts.
- **Auth:** `claude auth status` exits 0 and prints JSON. Verified live:
  `{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
  "subscriptionType":"max",...}`.
- **Credentials:** macOS Keychain; on Linux/Windows,
  `~/.claude/.credentials.json` (mode 0600).
- **Sessions:** `~/.claude/projects/<encoded-path>/<session-id>.jsonl`.
- **Event schema**, verified live from a real run — line `type` values
  observed: `system` (subtypes `init`, `hook_started`, `hook_completed`),
  `stream_event` (wrapping Anthropic API events: `message_start`,
  `content_block_start`, `content_block_delta` with `delta.text` /
  `delta.thinking` / `signature_delta`, `content_block_stop`,
  `message_delta`, `message_stop`), `assistant` (`message.content` blocks:
  `text`, `thinking`, `tool_use`), `user` (`tool_result` blocks),
  `rate_limit_event`, and a terminal `result` line carrying result text,
  `is_error`, `session_id`, `usage` (`input_tokens`, `output_tokens`,
  `cache_*`), and `total_cost_usd`.
- **MCP config:** `--mcp-config` accepts inline JSON or a file path;
  `--strict-mcp-config` disables config-file auto-discovery so only the
  servers Commonspace passes are loaded.
- **Cancellation:** SIGINT/SIGTERM against the process. No documented
  protocol-level stop message in headless stream-json mode.

### OpenAI Codex CLI

Package `@openai/codex`, codex-cli 0.146.0 tested locally. License
Apache-2.0; implementation is Rust.

- **Headless invocation:** `codex exec --json` with the prompt delivered
  via stdin (`-`).
- **Flags verified locally:** `-s`/`--sandbox` (`read-only` |
  `workspace-write` | `danger-full-access`), `--skip-git-repo-check`,
  `-C`/`--cd`, `--add-dir`, `-m`/`--model`, `-c key=value` (TOML config
  override), `-o`/`--output-last-message`, `--ephemeral`, and
  `codex exec resume [SESSION_ID|--last]`.
- **`codex app-server`:** a separate subcommand exposing JSON-RPC 2.0 over
  stdio with a Thread → Turn → Item model. It *does* support GUI-mediated
  approval, via methods such as `item/commandExecution/requestApproval`,
  plus `turn/interrupt` for protocol-level cancellation. `codex exec`
  itself never prompts for approval at all — if Codex's own interactive
  approval is ever needed (rather than routing everything through
  Commonspace's MCP tools), `app-server` is the integration path, not
  `exec`.
- **Auth:** `codex login status` exits 0 and prints, e.g., "Logged in
  using ChatGPT" — verified live.
- **Credentials:** `~/.codex/auth.json`, plaintext by default; OS keyring
  storage is optional via `cli_auth_credentials_store`.
- **Sessions:** `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`.
- **Event schema**, verified live: `{"type":"thread.started",
  "thread_id":...}`, `{"type":"turn.started"}`,
  `{"type":"item.started"/"item.updated"/"item.completed","item":
  {id,type/item_type,...}}` with item types `agent_message`, `reasoning`,
  `command_execution`, `file_change`, `mcp_tool_call`, `web_search`,
  `todo_list`, `error`; `{"type":"turn.completed","usage":{...}}`,
  `{"type":"turn.failed","error":{message}}`, and a top-level
  `{"type":"error","message":...}`.
- **MCP config:** `~/.codex/config.toml`, under
  `[mcp_servers.NAME]`, with either `command`/`args`/`env_vars` for a
  stdio server or `url`/`bearer_token_env_var` for an HTTP server.
- **Cancellation:** process-level SIGINT/SIGTERM for `exec`;
  protocol-level `turn/interrupt` is available only under `app-server`.

### Gemini CLI

Apache-2.0, `@google/gemini-cli`.

- **Critical finding:** headless `-p --output-format stream-json` has no
  permission/tool-confirmation event type at all — the documented schema
  is only `init`/`message`/`tool_use`/`tool_result`/`error`/`result`.
  GUI-mediated approval is only possible in ACP mode (`gemini --acp`,
  JSON-RPC over stdio), which exposes a `session/request_permission`
  method and a `cancel` notification for protocol-level cancellation.
  Plain headless mode has no approval hook to bridge to Commonspace's UI
  at all.
- **Credentials:** OS keychain via `keytar` (service
  `gemini-cli-oauth`, account `main-account`), with an AES-256-GCM
  encrypted file fallback at `~/.gemini/gemini-credentials.json`.
- **Non-destructive auth probe:** read `~/.gemini/settings.json`'s
  `security.auth.selectedType` and `~/.gemini/google_accounts.json`,
  rather than invoking any command that could trigger a login flow.
- **ToS finding.** Gemini CLI's documentation states:

  > "Directly accessing the services powering Gemini CLI ... using
  > third-party software, tools, or services ... is a violation of
  > applicable terms and policies."

  Read closely, this targets extracting and reusing the OAuth token to
  call the Gemini API directly — not spawning the official signed
  `gemini` binary through its own CLI surface. Commonspace's hard
  constraint follows directly from this: **never read a provider's
  credential store, and never call a provider API directly** — only ever
  drive the official binary through its documented CLI or ACP surface.
- **License:** Apache-2.0. Status in Commonspace: planned, not shipped
  (see docs/provider-adapters.md).

### OpenCode

MIT, npm package `opencode-ai`; the repository moved from `sst/opencode`
to `anomalyco/opencode`. OpenCode is its own agent, not a thin CLI wrapper
around another vendor's model.

- **`opencode serve`** exposes REST + Server-Sent Events, with what
  appears to be the best-designed permission protocol surveyed: SSE events
  `permission.v2.asked` / `permission.v2.replied`, plus
  `POST /session/:id/permissions/:permissionID` to answer one, and
  `POST /session/:id/abort` for cancellation. Permission configuration is
  allow/ask/deny per tool category with glob patterns. Runtime MCP server
  injection is available via `POST /mcp`.
- **Compliance finding.** OpenCode's own documentation states that
  Anthropic explicitly prohibits third-party tools from using Claude
  Pro/Max subscription credentials, and that OpenCode removed the bundled
  plugins that did this as of version 1.3.0 — while noting that ChatGPT
  Plus, GitHub Copilot, and GitLab Duo subscriptions are permitted for
  third-party use. This is recorded here as a compliance consideration
  and reflected in Commonspace's hard rule in section E and in
  docs/subscription-authentication.md: never use one provider's
  subscription credentials through a different vendor's tool.
- **Status in Commonspace:** planned, via `opencode serve` (see
  docs/provider-adapters.md).

## B. Reference desktop applications surveyed

Projects below were surveyed for architectural patterns, not for code to
copy. License matters a great deal here: Commonspace is MIT-licensed, and
code from AGPL-3.0 projects must never be copied into it — those entries
are architecture reference only.

| Project | License | Stack | Notable pattern |
|---|---|---|---|
| opcode (formerly Claudia, winfunc/opcode) | AGPL-3.0 | Tauri 2 + React + shadcn | Closest structural analog surveyed, but dormant roughly 10 months; invokes `claude` with `--dangerously-skip-permissions` — exactly the posture Commonspace rejects |
| OpenLoaf | AGPL-3.0 | not independently verified | Reference only; stack details beyond the license were not confirmed in this survey |
| Claude Code UI / CloudCLI (siteboon/claudecodeui) | AGPL-3.0 | not independently verified | Very active; a sequence-numbered WebSocket replay buffer worth learning from; a 7-facet provider abstraction |
| different-ai/openwork (`/ee` directory) | Fair Source | not independently verified | Fair Source carve-out pattern, scoped to one directory of an otherwise differently-licensed project |
| T3 Code | MIT | Electron | Event-sourced orchestration; per-thread permission modes |
| AionUi | Apache-2.0 | Electron + Vue | 20+ CLIs integrated via ACP — closest to Commonspace's pitch at scale |
| OpenCoworkAI/open-cowork | MIT | not independently verified | Routes shell execution through WSL2 on Windows and Lima on macOS for VM-level isolation — the strongest concrete Windows sandboxing answer found in this survey |
| andrewyng/openworker | MIT | Tauri 2 + local Python sidecar | Sidecar over `127.0.0.1` with a per-launch token; "ask before acting"; an approval inbox for unattended runs |
| humanlayer/humanlayer CodeLayer | Apache-2.0 | Tauri 2 + Go daemon | Blocking human-in-the-loop approvals, but now deprecated in favor of a paid product; process handling is Unix-only via the `nix` crate |
| Crystal (stravu) | MIT | not independently verified | Deprecated February 2026; worth copying its scripted license-compatibility gate that blocks GPL/AGPL/SSPL dependencies; defaults to `--dangerously-skip-permissions` |
| Conductor (Melty Labs) | Closed source | Electron, rewritten to Tauri | Publicly rewrote from Electron to Tauri for bundle size and cold start — validates the Tauri choice; git-worktree-per-session; Mac-only, Windows still unshipped |
| vibe-kanban | Apache-2.0 | not independently verified | 27k GitHub stars; sponsor company shut down April 2026 |
| Dimillian/CodexMonitor | MIT | Tauri 2 | Talks to `codex app-server` over stdio JSON-RPC — the most rigorous structured-protocol example surveyed |
| TOKENICODE | Apache-2.0 | Tauri 2 | Uses Claude CLI's native control protocol; four permission modes |
| desktop-cc-gui | MIT | Tauri 2 | Multi-CLI runtime adapters — the closest precedent found for Commonspace's premise of one app driving several official CLIs |

A sustainability pattern is worth stating plainly rather than glossing
over: vibe-kanban's sponsor shut down, HumanLayer pivoted from open source
to a paid product, and Crystal was deprecated — all within roughly
February to April 2026, in this exact product category (desktop
wrappers around coding-agent CLIs). This is not a reason to avoid the
category, but it is a reason not to assume any single reference project
will still be maintained by the time Commonspace ships, and a reason
Commonspace's own core (permissions, path safety, document tooling) does
not depend on any of them.

Anthropic's own **Claude Cowork** (launched January 2026, macOS desktop
first, web/mobile added July 2026) is noted here as a UX reference only —
folder-scoped permission grants with one-time vs. always-allow choices,
three named modes (Manual/Auto/Skip), deletion always requiring explicit
permission with a preview list, a right-side step/progress panel, and an
artifacts pane with edit-by-highlight. Commonspace must not copy
Anthropic's naming, icons, panel layout, or other trade dress; only the
underlying UX ideas (explicit deletion confirmation, a progress panel,
editable artifacts) are fair game, reimplemented in Commonspace's own
visual language.

## C. Platform findings (Tauri 2)

**Subprocess management.** `tauri-plugin-shell`'s `kill()` only signals
the direct child PID — documented upstream as not-planned to fix. A
provider CLI that spawns its own children (an MCP server, a shell) would
leak them on cancel. Commonspace instead spawns via `tokio::process`
wrapped by the `process-wrap` crate (v9.1.0), using a Job Object on
Windows and `ProcessGroup::leader()` on Unix for true tree-kill. Job
Objects are kernel-tracked, so they reliably catch grandchildren spawned
after the kill decision is made — more reliable than shelling out to
`taskkill /T` after the fact. This matches
`crates/commonspace-agents/src/process.rs`.

**Streaming IPC.** Tauri's documentation states the event system is not
designed for high throughput and recommends Channels instead. The
JavaScript `Channel` class does monotonic-index reordering on delivery, so
the correct pattern is one Channel per task/session rather than a single
shared event stream.

**SQLite.** `rusqlite` (bundled) with backend-owned typed commands was
chosen over `tauri-plugin-sql`, because that plugin's design lets the
webview send raw SQL across the IPC boundary — a shape Commonspace's
"deterministic Rust owns every mutation" principle rules out.

**Secrets.** The `keyring` crate, v4 (v3 is now a legacy branch). Note:
Linux `keyutils` has roughly a 3-day TTL and does not survive a reboot, so
it is unsuitable as the store of record on Linux; any degradation to a
weaker backend must be surfaced in the UI, never silent.

**PATH for GUI-launched apps.** The macOS `launchd` PATH problem (GUI
apps not inheriting a login shell's PATH, so a CLI installed via a shell
profile is invisible) is addressed by the Tauri-org `fix-path-env-rs`
crate — a git dependency, not published on crates.io as of this survey.

**Plugins used or considered:** `dialog`, `opener` (replaces v1's
`shell.open`), `single-instance` (must be registered first),
`window-state`, `notification`, `process`, `os`, `log`,
`persisted-scope` (register after `fs`; needs the `protocol-asset`
feature for local file previews).

**Updater.** Works without any hosted CI: generate a signing keypair, set
`createUpdaterArtifacts`, export the signing environment variables, build
locally per OS, hand-write `latest.json`, and upload it anywhere over
HTTPS. This matches Commonspace's "no hosted CI" posture in
README.md/CONTRIBUTING.md.

**Sandboxing reality, per OS** (see also THREAT_MODEL.md's sandboxing
table):

- Windows Job Objects are resource controls, not a security boundary.
- Windows restricted tokens are a real security boundary, but a large
  engineering project — this is what OpenAI built for Codex's own
  sandboxing.
- Windows AppContainer is impractical for filesystem-heavy CLIs.
- macOS `sandbox-exec` has been deprecated since 2016 but is still what
  Codex itself uses.
- Linux's `landlock` crate is the cheapest real kernel-enforced boundary
  available, filesystem-only in practice.
- `bubblewrap` is stronger than landlock but is not guaranteed to be
  installed on an arbitrary Linux desktop.

The resulting v1 decision: Commonspace does not claim to sandbox the
provider CLI beyond what the CLI does for itself. It enforces a Rust-side
binary allowlist (only launches the specific, resolved provider binaries
it knows about) and passes through each CLI's own most restrictive
suitable sandbox/approval flags. This is stated in THREAT_MODEL.md and
must not be oversold in UI copy.

**Packaging.** Must be built on each target OS — macOS is a hard wall,
there is no cross-compilation shortcut. Signing optionality differs per
OS; notably, macOS Sequoia removed the right-click-to-open Gatekeeper
bypass that older macOS versions offered for unsigned apps. Realistic
bundle sizes observed: a Windows installer around 16 MB and a `.deb`
around 17 MB, versus roughly 200 MB for equivalent Electron apps — but an
AppImage comes in around 94 MB because it embeds WebKitGTK.

**Local file previews.** Via `convertFileSrc` plus the asset protocol's
scope configuration, with a Linux-specific gotcha: dot-directories can be
excluded or included by the scope glob in ways that are easy to get
wrong.

**Typed IPC.** `tauri-specta` (must be pinned to an exact version; it is
still on a `2.0.0-rc` release despite long-standing use) versus `ts-rs`.
Commonspace's IPC parity approach is described in ARCHITECTURE.md's IPC
contract section.

## D. Document tooling libraries and licensing

**DOCX.** `docx-rust` (MIT, a fork of the stalled PoiScript `docx-rs`)
and `ooxmlsdk` (MIT OR Apache-2.0, young/pre-1.0, but the only library
found that explicitly claims and tests preservation of unknown/extension
XML; covers DOCX, XLSX, and PPTX). Typed round-trip libraries re-serialize
a fresh XML tree and so risk silently dropping unmodeled content;
formatting-preserving edits should therefore use surgical zip and
part-level `quick-xml` editing that leaves untouched parts byte-identical
rather than a full parse/rebuild round trip.

**XLSX.** `calamine` 0.36 (MIT) for reading — the de facto standard in
the Rust ecosystem; `rust_xlsxwriter` 0.97 (MIT/Apache) for creating new
files, including native chart support; `umya-spreadsheet` 3.0 (MIT) for
round-trip editing, with the maintainer's own stated caveat that
drawings/OLE object editing is rough.

**PPTX.** No mature pure-Rust crate as of August 2026 — `ppt-rs` is
young and the `pptx` crate is at 0.1.0. The realistic approaches are
template-injection over minimal OOXML skeletons, or optional LibreOffice
conversion (see below), not a general-purpose PPTX editing library.

**PDF.** `pdf-extract` 0.12 (MIT) for text extraction; `lopdf` 0.44 (MIT)
for merge/split/rotate/create operations; `pdfium-render` 0.9 (MIT/Apache
crate binding the BSD-licensed PDFium engine, with binaries sourced from
bblanchon/pdfium-binaries) for page-preview rendering; `printpdf` 0.12
(MIT) or `typst` (Apache-2.0) for generation. **Flag:** `mupdf-rs` is
AGPL-3.0 and must not be linked into Commonspace; `pdfium-render` covers
the same rendering need cleanly under a permissive license.

**Optional external tools** (never bundled, only detected at runtime as
subprocesses): LibreOffice's `soffice --headless --convert-to` (MPL-2.0;
use `-env:UserInstallation` to point at an isolated profile so it never
clobbers the user's live LibreOffice profile) and `pandoc` (GPL-2.0+;
subprocess-only invocation does not affect Commonspace's own MIT license,
since nothing is linked).

**OCR.** `ocrs` (MIT/Apache, pure Rust) is preferred over the thinner
Tesseract bindings available.

**General-purpose crates considered:** `infer`, `mime_guess`, `blake3`,
`trash` 5, `encoding_rs` + `chardetng`, `zip`, `image`, `walkdir`,
`notify`, `dunce`, and `soft-canonicalize` (purpose-built for validating
that a not-yet-existing path still resolves inside a root — this is what
Commonspace's `PathGuard` in `commonspace-permissions` uses).

**Markdown:** `comrak` (BSD-2) for GFM fidelity, or `pulldown-cmark`
(MIT) for a lighter footprint.

**Validation strategy.** Every generated file should be verified by
re-parsing it with a *different* library than the one that wrote it, plus
a required-parts/well-formedness check on the underlying zip container,
plus an optional LibreOffice conversion as a smoke test, plus a golden
corpus of real-world files from varied producers (Word, Google Docs
exports, LibreOffice, older Office versions) rather than only
self-generated fixtures. This maps to the validation rule described in
docs/document-tools.md and to `OperationResult.validation` in
`crates/commonspace-core/src/op_result.rs`.

**A caution on newer crates.** Several new crates surveyed in this space
— `ppt-rs`, `pdf_oxide`, `office_oxide` — carry unusually promotional
crates.io descriptions ("the library that actually works", "100x faster
than industry leaders"), a pattern consistent with packages written to
rank well with AI coding assistants rather than to describe themselves
accurately to a human reader. Any such crate needs an actual source
read-through — not just a README skim — before it enters Commonspace's
dependency tree.

## E. Licensing constraints summary

| Category | Examples | Rule |
|---|---|---|
| May be linked | MIT, Apache-2.0, BSD, MPL-2.0 (file-level) | Preferred; matches CONTRIBUTING.md's licensing policy |
| Architecture reference only, never copied | opcode, OpenLoaf, Claude Code UI/CloudCLI (all AGPL-3.0); `mupdf-rs` (AGPL-3.0) | Study the pattern, write Commonspace's own implementation from scratch |
| Optional detected subprocess only, never bundled or linked | LibreOffice (MPL-2.0), pandoc (GPL-2.0+) | Invoked as an external process the user already has installed; never shipped inside Commonspace's installer |

Two provider-compliance rules stand apart from open-source licensing but
carry the same weight for Commonspace:

1. **Never read a provider's credential store, and never call a
   provider's API directly.** Commonspace only ever drives the official,
   signed CLI binary through its documented CLI or ACP surface. This
   follows directly from the Gemini CLI ToS finding in section A and is
   restated as a hard constraint in docs/subscription-authentication.md.
2. **Never use one provider's subscription credentials through a
   different vendor's tool.** This follows from the OpenCode/Anthropic
   finding in section A (OpenCode removed bundled plugins that did this,
   while confirming ChatGPT Plus, GitHub Copilot, and GitLab Duo
   permit third-party use of their subscriptions). Commonspace always
   spawns each provider's own official CLI for that provider's work.
