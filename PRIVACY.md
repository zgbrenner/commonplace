# Commonspace Privacy

## The short version

- Commonspace is local-first. Your conversations, task history, workspace
  configuration, backups, audit history, skills, and preferences are stored on
  your computer and nowhere else.
- Commonspace itself sends nothing to Commonspace's developers. There is no
  telemetry, no analytics, no crash reporting, and no account.
- When you run a task with a **cloud AI provider** (for example a Claude or
  OpenAI subscription), your prompts — and any file contents or excerpts the
  agent reads for that task — are sent to **that provider**, under that
  provider's terms. Commonspace shows you which provider, what destination,
  and which files are involved before it happens.
- Local models (when supported) keep everything on your machine.

## What is stored locally, and where

| Data | Location |
|---|---|
| Conversations, messages, tasks, events | SQLite database in the app data directory |
| Artifacts and generated files | Your workspace folders |
| Backups of modified/deleted files | App data backup store, per workspace, with retention controls |
| Audit history (permissions, file operations) | SQLite database |
| Preferences and workspace configuration | SQLite database |
| Diagnostic logs (rotated) | App data log directory. Nothing in Commonspace logs a credential, but the log is written as the code emits it — there is no filter over it. The diagnostics report you can produce from Settings *is* redacted before it is written. |

Secrets are never stored in the database or logs. If a provider requires
Commonspace to hold an API key, it is kept in your operating system's
credential vault (Windows Credential Manager, macOS Keychain, or the Linux
secret service).

## What leaves your computer

Only traffic between the provider tooling you connected and that provider's
service. Before file contents are sent to a cloud provider, the task view
shows: the selected provider, the destination, the files or excerpts
involved, and the permission decision that authorized it.

Provider CLIs (Claude Code, Codex CLI, etc.) may have their own telemetry
governed by their own settings and policies; Commonspace does not add to it
and links each provider's documentation from the Connections screen.

## Honest limits

- Local data is protected by your operating system's normal file permissions.
  Commonspace does not encrypt data at rest in v1 and does not claim to; we
  recommend full-disk encryption (BitLocker, FileVault, LUKS).
- Once data is sent to a cloud provider with your approval, its handling is
  governed by that provider's privacy policy, not by Commonspace.

## Future changes

Optional local redaction and sensitive-data detection are planned as a
preprocessing step before content leaves the machine; the pipeline is designed
so this can be inserted cleanly. If any diagnostics or analytics are ever
added, they will be opt-in, documented here, and off by default.
