# Developer / release scripts

Everything here runs **locally**. Commonspace has no hosted CI, no GitHub
Actions, no `.github/workflows` -- these scripts *are* the CI, run on your
own machine before you commit, push, or tag a release. That is a deliberate
project policy, not a temporary gap: see the root `README.md` and
[`docs/releasing.md`](../docs/releasing.md) if present.

Each script is POSIX-ish `bash` with no file extension, works from any
directory (they `cd` to the repo root themselves), and is safe to re-run
(idempotent). They work on Windows (Git Bash), macOS, and Linux.

All of them tolerate `apps/desktop` not existing yet -- the frontend and the
Tauri shell are scaffolded later. Once `apps/desktop/package.json` and
`apps/desktop/src-tauri/tauri.conf.json` show up, the scripts pick them up
automatically; nothing here needs to change.

## Scripts

| Script | What it does | When to run it |
| --- | --- | --- |
| `scripts/setup` | Verifies the Rust/Node toolchain, installs missing rustfmt/clippy components, runs `npm ci`/`npm install` if a root `package.json` exists, and warns (without failing) about missing platform Tauri prerequisites. | Once after cloning; again after pulling toolchain/dependency changes. |
| `scripts/fmt` | `cargo fmt --all`, plus the frontend's `format` script once it exists. Mutates files. | Before committing, or any time you want the workspace auto-formatted. |
| `scripts/lint` | `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings`, plus the frontend's `lint` script once it exists. Never mutates files. | To check style/lint without changing anything. |
| `scripts/typecheck` | Runs the frontend's `typecheck` script (`tsc --noEmit`). A no-op that exits 0 until `apps/desktop` exists. | Part of `scripts/check`; standalone when iterating on types only. |
| `scripts/test` | `cargo test --workspace` (excludes `#[ignore]`d real-provider tests automatically), plus the frontend's `test` script once it's defined. | Before committing; part of `scripts/check`. |
| `scripts/smoke-providers` | Runs the **real** provider CLI smoke tests (`cargo test -p commonspace-agents --test provider_smoke -- --ignored --nocapture`). Spawns your actual, authenticated Claude Code / Codex CLIs and uses a small amount of real subscription usage. Never run automatically. | Manually, deliberately, before tagging a release -- or whenever you want to prove the provider adapters work end to end against a live CLI. |
| `scripts/security` | `cargo audit` (if installed) for known vulnerabilities; `cargo deny check licenses` (if installed) against `../deny.toml`; an informational LGPL scan that warns but never fails; `npm audit --omit=dev --audit-level=high` once the frontend exists (low/moderate findings are reported, not fatal). | Before a release; periodically to catch new advisories. |
| `scripts/check` | The fast pre-commit-grade gate: fmt check, clippy, typecheck, `cargo test`. No release builds, no provider smoke tests. **This is the bar every change must clear.** | Before every commit/push; this is what the `pre-push` git hook runs. |
| `scripts/dev` | Runs the app in development mode (`npm run tauri dev`) once `apps/desktop/src-tauri` exists. Prints a clear "not scaffolded yet, run scripts/check instead" message and exits 0 otherwise. | Day-to-day development, once the Tauri shell exists. |
| `scripts/build` | Production build: `cargo build --workspace --release`, then (once the Tauri shell exists) the frontend production build and `npm run tauri build`. Prints where installers land for the current OS. Cross-compiling is not supported -- build each OS's installers on that OS. | Before a release, or to sanity-check a release build locally. |
| `scripts/release-check` | **The** single pre-release gate: clean working tree -> toolchain verification -> fmt check -> clippy -> typecheck -> tests -> security -> release build -> summary. Stops at the first failure. Prints `ALL GATES PASSED -- safe to tag` on success, plus reminders to run `scripts/smoke-providers` manually and that other OSes' installers must be built on those OSes. | Immediately before tagging a release. |
| `scripts/install-hooks` | Installs a `pre-commit` hook (`scripts/fmt` + `cargo fmt --all --check`) and a `pre-push` hook (`scripts/check`) into `.git/hooks`. Prints how to bypass either (`--no-verify`) if you ever need to. | Once, after cloning. |
| `scripts/lib.sh` | Shared helpers (`run_step`, `have`, `has_frontend`, `has_tauri`, `os_name`, npm-script detection, repo-root `cd`). Sourced by every other script; not meant to be run directly. | Never run directly. |

## Suggested first-time setup

```sh
scripts/setup
scripts/install-hooks
```

## Suggested day-to-day loop

```sh
scripts/dev     # while working
scripts/check   # before every commit/push (also runs automatically via hooks)
```

## Before tagging a release

```sh
scripts/release-check      # every required gate; stops on first failure
scripts/smoke-providers    # manual -- real subscription usage, run deliberately
```

Then build installers on each OS you ship for -- there is no cross-compiling
and no hosted CI to do it for you.
