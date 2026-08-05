# Subscription and Authentication

This is the most important honesty document in the repository. It exists
because "your AI subscription" is easy to say and easy to get wrong: a
user needs to know, without guessing, whether a given connection is
running on a subscription they already pay for, on metered API billing,
or on their own machine — and Commonspace must never blur that line to
look more capable than it is.

Four categories, kept clearly separate below: officially supported
subscription authentication, API-key authentication, local models, and
what is explicitly out of bounds.

## 1. Officially supported subscription authentication

In every case, **the official provider CLI owns the credentials — not
Commonspace.** Commonspace detects the CLI, tells the user to run its
official sign-in, and spawns it as a subprocess. It never sees, stores,
or transmits the resulting token itself.

- **Claude Code**, with a Claude Pro or Claude Max subscription, via
  Anthropic's own sign-in (running `claude` for the first time opens it).
- **Codex CLI**, with a ChatGPT Plus, Pro, or Team plan, via OpenAI's own
  sign-in (`codex login`).
- **Gemini CLI**, with a Google account, including AI Pro/Ultra tiers,
  via Google's own sign-in.

Commonspace's role in all three is identical and limited: detect that the
official CLI is installed, run its documented read-only status check
(`claude auth status`, `codex login status`, or the equivalent
non-destructive probe for Gemini), and report the truthful result. It
never runs a login flow on the user's behalf and never reads the
resulting credential file's contents beyond the minimal presence/shape
check needed for status.

## 2. API-key authentication

When a connection is billed per token by the provider rather than through
a consumer subscription, the key is:

- Entered by the user, for that provider, explicitly.
- Stored **only** in the OS credential vault (Windows Credential Manager,
  macOS Keychain, or the Linux secret service) via the `keyring` crate —
  never in SQLite, never in a config file Commonspace writes, never in
  logs or the developer view (a redaction filter applies at the logging
  boundary; see THREAT_MODEL.md §5).
- Billed directly by the provider to the account the key belongs to.
  Commonspace adds no markup and is not a party to that billing
  relationship.

## 3. Local models

Nothing leaves the machine. No credential, no network request to a
provider, no billing relationship at all. When local model support ships,
this category exists specifically so the UI can say, truthfully, "this
conversation never left your computer" — a claim that is only true for
this category and must never be implied for the other three.

## 4. Unsupported and explicitly out of bounds

These are not omissions Commonspace intends to fill in later; they are
things Commonspace will not do, on principle, regardless of how
technically convenient they might be:

- **Scraping browser sessions** to harvest a provider's web login instead
  of using the official CLI's own sign-in.
- **Extracting or reusing a provider CLI's stored OAuth token** to call
  that provider's API directly, bypassing the official CLI. This is the
  exact behavior Gemini CLI's own terms name as a violation (quoted in
  full below) — Commonspace never reads a provider's credential store for
  any purpose beyond the minimal non-destructive status check described
  in section 1, and never uses what it reads to make an API call itself.
- **Impersonating an official client** — presenting Commonspace's own
  requests as if they came from the provider's official CLI or app.
- **Using one provider's subscription credentials through a different
  vendor's tool.** OpenCode's own documentation states that Anthropic
  explicitly prohibits third-party tools from using Claude Pro/Max
  subscription credentials, and that OpenCode removed the bundled plugins
  that did this as of version 1.3.0 — while confirming that ChatGPT Plus,
  GitHub Copilot, and GitLab Duo subscriptions are permitted for
  third-party use (docs/research.md §A). Commonspace always spawns each
  provider's own official CLI for that provider's work; it never routes
  one provider's subscription through another provider's or another
  vendor's tooling.
- **Any other unsupported authentication hack** — anything that achieves
  a connection state the provider's own official tooling doesn't support,
  by construction, is out of bounds even if it would technically work.

### The Gemini ToS finding, and the resulting hard constraint

Gemini CLI's documentation states:

> "Directly accessing the services powering Gemini CLI ... using
> third-party software, tools, or services ... is a violation of
> applicable terms and policies."

Read closely, this targets extracting and reusing the OAuth token to call
Google's API directly — not spawning the official, signed `gemini` binary
through its own documented CLI or ACP surface, which is exactly what
Commonspace does. The hard constraint this produces, applied to every
provider Commonspace integrates, not only Gemini: **never read a
provider's credential store for anything beyond a non-destructive status
check, and never call a provider's API directly** — only ever drive the
official binary through its documented CLI or ACP surface.

## Per-provider truth table

What each connection state means, and who is billed for usage under it.
These map directly to `commonspace_core::AuthStatus`
(`crates/commonspace-core/src/provider.rs`), which the Connections screen
renders without embellishment:

| State | Meaning | Billed to |
|---|---|---|
| `NotInstalled` | The official CLI was not found on this machine | — |
| `SignedOut` | The CLI is installed but no session is active | — |
| `Subscription { plan_hint }` | Connected through a consumer subscription the provider officially supports for this tooling (e.g. Claude Max, ChatGPT Plus) | The user's existing subscription; no additional charge from Commonspace |
| `ApiKey` | Connected with an API key | The provider, per token, to the account the key belongs to |
| `LocalModel` | Running locally | Nobody; no network usage |
| `Error { detail }` | The status probe itself failed (timeout, unreadable output, etc.) | Unknown — shown as an error, never guessed as one of the above |

`plan_hint`, when present, comes directly from the CLI's own status
output (e.g. Claude Code's `subscriptionType` field) — Commonspace
displays what the CLI reported, it does not infer a plan from indirect
signals.

## Credential storage locations, per provider, per OS

Documented here specifically as a record of what Commonspace deliberately
does **not** touch beyond the minimal status checks in section 1. All of
these paths are also enforced as protected, unreadable locations by
`commonspace-permissions::is_protected_location`
(`crates/commonspace-permissions/src/protected.rs`), independent of any
workspace configuration — an agent cannot be talked into gaining access
to them.

| Provider | macOS | Windows / Linux |
|---|---|---|
| Claude Code | System Keychain | `~/.claude/.credentials.json` (mode 0600); `~/.claude.json` holds non-secret account metadata |
| Codex CLI | `~/.codex/auth.json` (plaintext by default; OS keyring optional via `cli_auth_credentials_store`) | Same path, same defaults |
| Gemini CLI | OS keychain via `keytar` (service `gemini-cli-oauth`, account `main-account`) | AES-256-GCM encrypted file fallback at `~/.gemini/gemini-credentials.json` |
| OpenCode | Not verified in this survey | Not verified in this survey |

## What the UI must say

- **Connections must state whether a provider is on a subscription, API
  billing, or local inference** — plainly, using the truth table above,
  never a vaguer phrase that could be read as any of the three.
- **The UI must never imply a subscription works where the official
  tooling doesn't support it.** If a provider's official CLI does not
  offer subscription auth for a given plan or platform, Commonspace does
  not offer it either, and does not phrase the limitation as if it might
  be a Commonspace restriction rather than the provider's own.
- **Each provider's own terms are linked** from the Connections screen,
  next to that provider's connection card — the user can always go read
  what they actually agreed to, rather than trusting Commonspace's
  summary of it.
