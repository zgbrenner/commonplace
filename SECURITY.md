# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately via GitHub Security
Advisories on this repository ("Report a vulnerability"). If that is not
possible, open a minimal public issue asking for a private contact channel —
do not include exploit details in public issues.

You can expect an acknowledgement within 7 days. Coordinated disclosure is
appreciated; we will credit reporters unless anonymity is requested.

## Scope

Especially interesting reports:

- Workspace scope escapes: path traversal, symlink/junction tricks, Windows
  path canonicalization gaps, TOCTOU races in the permission engine.
- Permission bypasses: operations reaching disk/network without the required
  policy evaluation or approval.
- Secret handling: credentials appearing in SQLite, logs, events, or the
  developer view.
- Prompt-injection paths that turn untrusted file content into unapproved
  consequential actions.
- Malformed-document parsing crashes with security impact.

Out of scope: vulnerabilities in provider CLIs or AI services themselves
(report those upstream), social engineering, and issues requiring an already
compromised OS account.

## Supported versions

Pre-1.0: only the latest release receives fixes.
