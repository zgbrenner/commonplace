# Contributing to Commonspace

Thanks for helping. Commonspace aims to be a calm, trustworthy desktop agent
for nontechnical users — contributions are judged against that goal.

## Ground rules

- **Safety is deterministic.** Anything that touches files, processes, or the
  network goes through `commonspace-permissions` and produces journal entries.
  PRs that bypass the policy engine, log secrets, or report unverified success
  will not merge.
- **No provider special-casing outside adapters.** UI and orchestrator speak
  the normalized event protocol only.
- **No new heavyweight dependencies without discussion.** Commonspace stays
  lighter than an Electron app; every dependency is a cost.
- **Honest UX.** Don't claim sandboxing, encryption, or support a provider
  doesn't actually offer.
- **Licensing.** Linked code must be permissively licensed (MIT/Apache/BSD;
  MPL-2.0 file-level acceptable). GPL/AGPL tools may only be optional,
  user-installed subprocesses. Preserve all attribution notices.

## Development

```
scripts/setup      # one-time: install deps, check toolchain
scripts/dev        # run the desktop app with hot reload
scripts/check      # fmt + clippy + eslint + tsc + unit tests
scripts/test       # full test suite including fixtures
scripts/release-check  # everything; must pass before any tag
```

`scripts/check` is the bar for every PR. GitHub Actions runs those same
scripts on Linux, macOS and Windows, so a green local run should mean a green
CI run — please still run it before pushing rather than using CI as your
first check. Optional git hooks (`scripts/install-hooks`) run the fast subset
on commit.

CI never runs `scripts/smoke-providers`: those tests drive real, authenticated
provider CLIs and consume subscription usage. Run them yourself when you touch
an adapter, and say so in the PR.

## Standards

- Rust: rustfmt, clippy clean (`-D warnings`), minimal `unsafe`
  (each instance justified in a comment), errors via `thiserror` types.
- TypeScript: strict mode, no `any` at boundaries, Zod validation for all
  data crossing the IPC boundary.
- Tests: unit tests beside code; integration tests in `tests/`; new file
  operations need fixture coverage (unicode names, long paths, symlinks,
  read-only, locked files as applicable); permission changes need policy
  tests; adapter changes need protocol-fixture tests plus, where possible, a
  local smoke test against the real CLI.
- Commits: imperative subject, body explains why.

## Reporting issues

Use the issue tracker for bugs and proposals. For security issues, see
[SECURITY.md](SECURITY.md) — do not open public issues for vulnerabilities.
