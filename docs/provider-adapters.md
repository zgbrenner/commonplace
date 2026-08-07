# Provider Adapters

How agents plug into Commonspace, and what a contributor needs to know
before adding or maintaining one. Background research on each provider's
CLI surface, license, and event schema is in docs/research.md; this
document is about the adapter interface itself and the rules that keep
provider-specific behavior contained to one crate.

## The AgentAdapter trait

Every provider is integrated through one trait,
`commonspace_agents::adapter::AgentAdapter`
(`crates/commonspace-agents/src/adapter.rs`). It is object-safe and
async; the orchestrator holds `Box<dyn AgentAdapter>` per provider and
never branches on which provider it's talking to:

```rust
#[async_trait::async_trait]
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Is the official CLI installed? Never mutates anything.
    async fn detect(&self) -> InstallStatus;

    /// Truthful auth status via non-destructive probes (read-only
    /// commands, config-file presence). Never triggers a login flow.
    async fn auth_status(&self) -> AuthStatus;

    /// Human instructions + command for the official sign-in flow.
    fn auth_instructions(&self) -> AuthInstructions;

    fn capabilities(&self) -> AdapterCapabilities;

    /// Start a new session (or resume, when request.resume is set).
    async fn start_session(
        &self,
        request: SessionRequest,
        events: EventSink,
    ) -> Result<RunningSession, AdapterError>;

    /// Health diagnostics for the Connections screen.
    async fn health(&self) -> HealthReport;
}
```

`EventSink` is `tokio::sync::mpsc::UnboundedSender<AgentEvent>` — the only
channel through which an adapter speaks to the rest of the system.
`SessionRequest` carries the task id, prompt, working directory, all
authorized workspace roots, an optional model override, an optional
provider-native session id to resume, and an optional `McpEndpoint`
(the loopback URL and per-session bearer token for Commonspace's own MCP
tool server). `RunningSession` returns Commonspace's session id, a
`watch::Receiver<Option<String>>` that fills in with the provider-native
session id once the CLI reports one (for resume), a `KillHandle` that
terminates the full process tree, and a `JoinHandle` that resolves when
the process has fully exited.

`detect`, `auth_status`, and `health` must never mutate anything and must
never trigger an interactive login flow — they exist to answer "what's
the truth right now" for the Connections screen, using version output,
documented config/credential file *presence*, and cheap read-only CLI
invocations (`claude auth status`, `codex login status`). Sign-in itself
is always the user running the provider's own official command;
`auth_instructions()` returns the command and a plain-language
explanation of what it will use for billing, never a flow Commonspace
drives itself.

## The normalized event protocol

Every adapter's job is to translate its CLI's own JSON/JSONL shape into
exactly the events defined once, centrally, in
`crates/commonspace-core/src/events.rs` — `AgentEvent`, a serde-tagged
enum with a dotted `type` field:

```
message.started      message.delta        reasoning.summary
plan.created          plan.updated
tool.requested        tool.started         tool.progress
tool.completed         permission.requested
artifact.created       artifact.modified
warning                error                task.completed
```

The UI, storage layer, and orchestrator only ever see `AgentEvent`. They
never see a provider's raw JSON shape. Provider-specific raw payloads are
preserved separately, as diagnostics attached to the per-session rotating
log file, for the collapsible developer view — never required reading for
the calm surface.

**Rule: provider-specific logic never leaves `commonspace-agents`.** If a
piece of code needs to know whether it's talking to Claude Code or Codex,
it belongs inside that provider's adapter module (`claude.rs`, `codex.rs`,
...), not in the orchestrator, not in storage, not in the UI. Each
adapter owns a private `Normalizer` that walks its CLI's JSONL lines and
emits `AgentEvent`s — see `Normalizer` in both `claude.rs` and `codex.rs`
for the pattern: a `handle(&Value) -> bool` method that returns `true`
only on a terminal event (`result` for Claude Code, `turn.completed` /
`turn.failed` for Codex), plus small helpers that turn a provider tool
call into a human-readable `title`/`detail` pair
(`humanize_tool` in `claude.rs`, the `item_type` match in `codex.rs`'s
`handle_item`).

## Unknown lines, and quota

A normalizer's `handle` matches on the line's `type` and its default arm
returns `false`. That permissiveness is deliberate and must stay: a line
type a provider adds next month has to be ignored, not treated as a
terminal event that kills the session. What it must not also be is
*invisible* — both normalizers log the unrecognized type at `debug`, so a
schema drift shows up in the session log instead of being silently
dropped. An exhaustive match here would be the wrong fix.

That arm was quietly eating something real. Claude Code emits quota state
out of band, on its own line type, whether or not the run is in trouble
(docs/research.md §A). Observed live from v2.1.224:

```json
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning",
 "resetsAt":1786474800,"rateLimitType":"seven_day","utilization":0.8,
 "isUsingOverage":false,"surpassedThreshold":0.75}}
```

`status` is `allowed` / `allowed_warning` / `rejected`; `rateLimitType`
names the window (`five_hour`, `seven_day`); `resetsAt` is Unix seconds.

`AgentEvent` gains no variant for this — a normalized event is a contract
the whole app implements, and quota does not need one. Two existing
channels carry it instead:

- `allowed_warning` becomes an `AgentEvent::Warning` naming the window and
  the reset moment in the reader's own clock terms, emitted once per
  window/reset pair. The CLI repeats the same line while a window stays
  over threshold, and the same sentence four times is noise.
- A run that *stops* on quota now produces an `AgentErrorInfo` that says
  so: `code: "provider_rate_limited"`, `transient: true`, and a `recovery`
  string naming when it resets. Previously a usage-limit failure and a
  bad-input failure were indistinguishable — both arrived as
  `provider_error` / `transient: false` / `recovery: None`, telling the
  user the one thing that is wrong for a quota failure, which is that
  retrying will not help. A failure with no quota evidence behind it is
  still classified exactly as before.

Evidence for "stopped on quota" is the last `rejected` line, or failing
that the CLI's own wording in the terminal message. When Claude Code gives
no reset timestamp, the recovery string says the limit will reset without
naming a time — an invented time would be worse than an absent one.

**Codex has nothing equivalent.** `codex exec --json` reports
`"rate_limits": null`; the field is only populated in `app-server` mode
(docs/research.md §A). Codex quota failures therefore still surface as
plain `turn.failed` errors, with no window, no reset time, and no honest
basis for calling them transient.

## Permission posture (v1)

In v1, **no provider's own mutating tools are enabled.** Every adapter
configures its CLI to allow only read-only exploration (file reads,
search, listing) plus Commonspace's own MCP tool server; every mutation —
create, edit, delete, rename, move — is required to go through
Commonspace's MCP tools, where `commonspace-permissions`'s deterministic
policy engine and the user's approval UI are the only path to disk.

This is a deliberate, provider-by-provider decision, not a default that
happened to be convenient:

- **Claude Code:** v2.1.222 has no `--permission-prompt-tool` flag (see
  docs/research.md §A) — the bridge pattern some third-party GUIs rely on
  to intercept Claude's own permission prompts does not exist on this
  version. Denying Claude's mutating tools outright via
  `--disallowedTools`, and only allowlisting read tools plus
  `mcp__commonspace`, sidesteps the missing bridge entirely rather than
  working around it. What the CLI *reads* is bounded separately — see
  "Configuration comes from Commonspace, not from the workspace" below.
- **Codex CLI:** `codex exec` — the headless mode Commonspace uses —
  never prompts for approval at all, at any sandbox level. There is no
  interactive hook to bridge even if Commonspace wanted one; the sandbox
  is pinned to `read-only` at the CLI level and, as with Claude Code,
  every mutation is routed through Commonspace's MCP tools instead. (The
  separate `codex app-server` subcommand does support interactive
  approval over JSON-RPC — noted in docs/research.md as the path *if*
  Codex's own approval flow is ever wanted instead of Commonspace's, but
  that is not how the shipped adapter talks to Codex today.)

Because of this, `AdapterCapabilities.supports_permission_bridge` is
`true` for Claude Code (it has its own permission-mode surface, even
though v1 doesn't route prompts through it) and `false` for Codex CLI
(`exec` mode has no prompt surface to bridge in the first place). This
field describes what the CLI is capable of, not what v1 currently wires
up — read the adapter source, not just this flag, before assuming a
provider's prompts reach the user.

## Testing an adapter

Two layers, and they are not interchangeable:

1. **Normalization fixture tests**, colocated with each adapter (see the
   `#[cfg(test)] mod tests` blocks in `claude.rs` and `codex.rs`). These
   feed literal JSONL lines — copied from real, observed CLI output, not
   invented — into the adapter's `Normalizer` and assert on the resulting
   `AgentEvent`s. They are fast, deterministic, and run in every local
   `scripts/check`/`scripts/test` pass. They prove the *mapping* is
   correct for the inputs they cover.
2. **Real-CLI smoke tests**, in
   `crates/commonspace-agents/tests/provider_smoke.rs`. These spawn the
   actual installed CLI with the user's own authentication and consume a
   trivial amount of real usage. They are `#[ignore]` by default:

   ```
   cargo test -p commonspace-agents --test provider_smoke -- --ignored --nocapture
   ```

   `scripts/smoke-providers` wraps this command.

**Rule: mock-only tests are never proof an adapter works.** Fixture tests
can pass against a JSONL shape that the real CLI stopped emitting after
an update, or that was transcribed wrong in the first place. Only the
smoke test — a real process, real auth, a real terminal event — is
evidence that an adapter functions end to end. A provider adapter change
is not considered done until the relevant smoke test has been run
against the installed CLI at least once by the person making the change.

## Shipped adapter flags, exactly as sent

**Claude Code** (`crates/commonspace-agents/src/claude.rs`):

```
claude -p --input-format stream-json --output-format stream-json
  --verbose --include-partial-messages
  --permission-mode dontAsk
  --setting-sources user
  --allowedTools "Read,Glob,Grep,LS,TodoWrite,Task,mcp__commonspace"
  --disallowedTools "Bash,PowerShell,Edit,Write,NotebookEdit,WebFetch,WebSearch,KillShell"
  [--add-dir <root>]...      # every workspace root except cwd
  [--model <model>]          # omitted for "default"
  [--resume <id>]            # when continuing a session
  --settings <session file>
  [--mcp-config <the same session file> --strict-mcp-config]
                             # only when an MCP endpoint is configured
```

`--permission-mode dontAsk` makes any tool use outside the allow list fail
fast instead of hanging on a prompt nobody in headless mode can see. The
prompt itself is never passed on the command line — it goes over stdin as
a `stream-json` user message, both because Windows has command-line
length limits and because prompts are user data that shouldn't appear in
a process listing.

The *session file* is one temporary JSON file per run, written to the
system temp directory, tightened to owner-only where the OS supports it,
and removed when the session ends (or immediately, if the spawn fails):

```jsonc
{
  "disableAllHooks": true,
  "permissions": { "deny": ["Read(~/.ssh)", "Read(~/.ssh/**)",
                            "Read(//**/.ssh)", "Read(//**/.ssh/**)", …] },
  // present only when Commonspace has an MCP endpoint for this session
  "mcpServers": { "commonspace": { "type": "http", "url": "…",
                    "headers": { "Authorization": "Bearer <token>" } } }
}
```

The Claude bearer token is **not** on argv. Claude Code reads the keys it
recognizes from whichever flag points at a file, so the same path serves
both `--settings` and `--mcp-config`: one file to restrict, one file to
delete. Verified against v2.1.224 — the MCP server registers from the
combined file and the deny rules apply in the same run. Passing the JSON
inline on `--mcp-config` would put the session token in the process list
and would not survive Windows `.cmd` argument quoting.

**Codex CLI** (`crates/commonspace-agents/src/codex.rs`):

```
codex exec [resume <id>]
  --json --skip-git-repo-check
  -s read-only
  -C <cwd>
  [--add-dir <root>]...      # every workspace root except cwd
  [-m <model>]               # omitted for "default"
  [-c mcp_servers.commonspace.url="<url>"
   -c mcp_servers.commonspace.bearer_token_env_var="COMMONSPACE_MCP_TOKEN"]
  -                          # prompt via stdin
```

The MCP bearer token travels through an environment variable
(`COMMONSPACE_MCP_TOKEN`), not argv — argv is visible to other processes
on the same machine via the process list; environment variables passed
this way are not.

## Configuration comes from Commonspace, not from the workspace

A workspace folder is untrusted content. It can arrive from Dropbox, from
a colleague, from a zip — nobody vets it, and the whole point of the app
is that a user can point it at a folder they were sent.

Claude Code loads `.claude/settings.json` from its working directory, and
that file can define **hooks**, which are shell commands. Anthropic's own
documentation states trust verification is disabled under `-p`. Since
Commonspace spawns `claude -p` with the workspace root as cwd, a folder
could therefore run arbitrary commands — outside the policy engine,
outside the approval UI, before the agent does anything at all.

**This was verified, not assumed.** Against `claude` v2.1.224 on Linux, a
temp workspace containing a `.claude/settings.json` with `SessionStart`
and `PreToolUse` hooks, run under the adapter's exact argv as it stood
before this was fixed: both hooks fired and wrote their canary file.

Two flags close it, and either one is sufficient on its own — both were
confirmed independently:

- `--setting-sources user` restricts discovery to the CLI's own
  `~/.claude/settings.json`, dropping the `project`
  (`<cwd>/.claude/settings.json`) and `local` tiers that are read out of
  the workspace. The `user` tier stays because the person at the keyboard
  owns it and a folder someone sent them cannot reach it; taking it away
  would silently override choices they made in Anthropic's own tool.
- `disableAllHooks: true` in the session file turns hooks off outright,
  covering any route to a hook that the source restriction does not.

### Mirroring the protected locations

`commonspace-permissions`'s `protected.rs` hard-denies `~/.ssh`, `~/.aws`,
`~/.claude` and the rest of the credential stores — but only for calls
that reach the policy engine, which means only Commonspace's own MCP
tools. Claude Code's `Read` goes straight to disk. Without a mirror, "read
my SSH key and summarise it" is bounded by nothing but the CLI's
working-directory rules, which `--add-dir` widens.

So the same list is rendered into the session file's deny rules.
`protected.rs` exports it as `credential_store_paths()` — home-relative
paths, no pattern syntax — and `claude.rs` renders it, because
provider-specific syntax never leaks into the permissions crate. Each
store yields four rules: `~/<path>` and `~/<path>/**` for the user's real
store, `//**/<path>` and `//**/<path>/**` for a copy that turns up inside
a workspace. A bare form and a `/**` form because `.netrc` is a file and
`.ssh` is a directory; `//` because a single leading `/` is read as
relative to the settings file, not to the filesystem root. Fourteen
stores, fifty-six rules. All four pattern forms were confirmed to be
enforced against v2.1.224.

Three limits, stated plainly rather than left to be discovered:

1. **This is enforcement by a cooperating process, not by the kernel.** It
   holds for exactly as long as Claude Code keeps honouring its own
   settings file. Commonspace cannot verify that and gets no help from the
   OS. It is a second lock on a door that is already locked — worth
   fitting, not worth trusting alone. It belongs in the "partial,
   inherited" row of THREAT_MODEL.md's sandboxing table, not above it.
2. **It stops at file contents.** A `Read(...)` rule blocks `Read` and
   `Grep`; `Glob` still returns the *names* of files inside a denied
   directory. Observed directly, not inferred.
3. **Codex has no equivalent, and this is not parity.** `-s read-only`
   constrains writes, not reads — it does nothing to stop Codex reading
   `~/.ssh`. `codex exec` has no deny-by-path configuration Commonspace
   can set. The two adapters are not equally covered here and the UI must
   not imply they are.

Nothing is added to the deny list beyond what `protected.rs` already
protects — not `.env`, not anything else. One list, mirrored; two lists
would drift, and a rule Commonspace's own tools do not honour would be a
promise only one half of the app keeps.

## Per-provider status

| Provider | Status | Notes |
|---|---|---|
| Claude Code | Shipped | `claude.rs`; permission bridge exists at the CLI level but v1 doesn't route through it — see "Permission posture" above |
| Codex CLI | Shipped | `codex.rs`; `exec` mode has no approval surface at all, by design |
| Gemini CLI | Planned | Must integrate via `--acp` (ACP over stdio) for GUI-mediated approval — plain headless `-p --output-format stream-json` has no permission event in its schema at all (docs/research.md §A) |
| OpenCode | Planned | Via `opencode serve` (REST + SSE); its `permission.v2.asked`/`permission.v2.replied` protocol is the best-designed permission surface surveyed (docs/research.md §B) |
| API-compatible providers | Planned | Direct API key usage, billed per token; no CLI subprocess involved |
| Local models | Planned | Nothing leaves the machine; no subprocess/CLI credential concerns apply |

## Process-lifecycle rules

These apply to every adapter, present and future, and are enforced in
`crates/commonspace-agents/src/process.rs` (`spawn_cli`), not left to each
adapter to reimplement:

- **Argv is never shell-interpolated.** Arguments are passed as an argv
  array to the child process, never built into a shell command string.
  The one exception — Windows `.cmd`/`.bat` npm shims, which `CreateProcess`
  can only launch via `cmd.exe` — still quotes each argument individually
  (`quote_for_cmd`) rather than concatenating raw user- or
  agent-controlled text into the command line; callers of `spawn_cli` may
  only pass Commonspace-constructed flags this way, never prompts or
  other untrusted strings (those go over stdin instead).
- **Absolute, pre-resolved binary paths only.** `find_cli` in
  `crates/commonspace-agents/src/detect.rs` resolves a CLI name to an
  absolute path via `PATH` first, then a small set of well-known install
  locations that GUI-launched apps often miss. Adapters call `find_cli`
  and pass the resolved `PathBuf` to `spawn_cli` — never a bare command
  name that would be looked up implicitly by the OS at spawn time.
- **Tree-kill on cancel.** `spawn_cli` wraps every child with a Job Object
  on Windows and a process-group leader on Unix (via the `process-wrap`
  crate), so `KillHandle::kill()` terminates the full process tree —
  including any MCP server or subshell the CLI itself spawned — not just
  the direct child PID. This is why Commonspace does not use
  `tauri-plugin-shell`'s built-in `kill()`, which only signals the direct
  child (docs/research.md §C).
