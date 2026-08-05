# Commonspace

**Your files. Your AI subscription. Real work done on your desk.**

Commonspace is an open-source desktop workspace that lets you use the AI
subscriptions you already have — through their official tools — to do real
work with your own files: summarize and compare documents, organize folders,
extract information from piles of PDFs, draft reports and spreadsheets, and
(when you point it at a code repository) build software.

No terminal. No raw shell output. You chat, the agent plans, you approve what
matters, and you watch understandable progress — with previews, backups, and
undo.

> **Status: pre-release.** Commonspace is under active initial development.
> Interfaces and formats may change without notice.

## How it works

1. **Connect** an agent you already pay for — Claude Code or OpenAI Codex CLI
   today; Gemini CLI, OpenCode, API providers, and local models on the
   roadmap. Commonspace detects the official CLI, walks you through its
   official sign-in, and never touches its credentials.
2. **Authorize** the folders the agent may work in. Everything else on your
   computer stays off-limits.
3. **Delegate**: describe the task in chat. The agent proposes a plan; you
   approve consequential steps; files are backed up before they're changed;
   results are verified before they're reported.

## Principles

- **Bring your own subscription** — no new account, no markup, no middleman
  billing. Commonspace tells you truthfully whether a provider is using your
  subscription, API billing, or a local model.
- **Local-first** — conversations, history, backups, and audit logs stay on
  your machine (see [PRIVACY.md](PRIVACY.md)).
- **Deterministic safety** — permissions, path scoping, backups, and
  verification are enforced in Rust, not by trusting the model (see
  [THREAT_MODEL.md](THREAT_MODEL.md)).
- **Calm by default, transparent on demand** — human-readable progress for
  everyone; a collapsible developer view with raw commands and provider
  events for those who want it.

## Tech

Tauri 2 · Rust · React · TypeScript · Vite · Tailwind · SQLite.
Lightweight by design: one process, no embedded Node server, no bundled
browser.

## Building from source

```
scripts/setup       # install workspace dependencies, verify toolchain
scripts/dev         # run the app in development mode
scripts/check       # format + lint + typecheck + tests
scripts/build       # production build + installers for this OS
scripts/release-check  # every quality gate, required before tagging
```

Requires Rust (stable), Node 20+, and the platform's Tauri prerequisites.
There is no hosted CI; all gates run locally (see
[docs/releasing.md](docs/releasing.md)).

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — system design
- [THREAT_MODEL.md](THREAT_MODEL.md) — security model, honestly stated
- [PRIVACY.md](PRIVACY.md) — what stays local, what reaches your provider
- [docs/permissions.md](docs/permissions.md) — the permission system
- [docs/provider-adapters.md](docs/provider-adapters.md) — how agents plug in
- [docs/subscription-authentication.md](docs/subscription-authentication.md) —
  what each provider officially supports
- [docs/document-tools.md](docs/document-tools.md) — deterministic document layer
- [CONTRIBUTING.md](CONTRIBUTING.md) · [SECURITY.md](SECURITY.md)

## Trademarks

Claude, Anthropic, OpenAI, ChatGPT, Codex, Google, and Gemini are trademarks
of their respective owners. Commonspace is an independent open-source project,
not affiliated with or endorsed by any AI provider; provider names are used
only to identify the official tools Commonspace integrates with.

## License

MIT — see [LICENSE](LICENSE).
