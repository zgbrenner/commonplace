# Releasing

Commonspace has two paths to a release, and they use the same gates.

- **CI release** — push a `v*` tag and GitHub Actions builds installers for
  Linux, macOS and Windows, then attaches them to a **draft** release.
- **Local release** — build on each operating system yourself with
  `scripts/build`. Still fully supported; it is what you do when you want to
  inspect a build before anyone else can, or when Actions is unavailable.

Either way the quality gates are `scripts/*`. CI does not maintain its own
list of commands, so a green run locally means the same checks passed.

## Continuous integration

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | push to `main`, every PR, manual | `scripts/lint` and `scripts/test` on Linux, macOS and Windows; `scripts/typecheck`, lint and production build for the frontend; `scripts/security`; coverage and script/workflow lint |
| `pr-builds.yml` | PRs touching code paths, manual | Builds real installers per OS, silently installs the Windows one, and attaches everything as 7-day artifacts |
| `release.yml` | `v*` tag, manual | Verifies, builds installers per OS, drafts a release, then silently installs the drafted Windows installer and checks the binary landed |
| `codeql.yml` | push to `main`, every PR, weekly | CodeQL code scanning |
| `audit.yml` | Mondays 07:00 UTC, manual | `scripts/security` alone, to catch advisories filed against an unchanged dependency tree |

PR builds exist because a dev build is not evidence about a packaged one —
the console-window bug in `docs/status.md` only surfaced in an installed
build. `pr-builds.yml` attaches real installers to every code PR as 7-day
artifacts, so packaged behaviour can be click-tested per PR without a local
release build. Dependabot (`.github/dependabot.yml`) rounds this out with
automated dependency-update PRs.

All three operating systems are in the test matrix deliberately. Commonspace
has genuine per-OS code paths with per-OS tests — path canonicalization and
reserved device names on Windows, symlink resolution on Unix, process
tree-kill via job objects versus process groups, protected system locations,
and trash behaviour. Testing on one platform would leave most of that
unverified.

**CI never runs the real-provider smoke tests.** They spawn the developer's
own authenticated CLIs and spend real subscription usage. They are `#[ignore]`
so `cargo test` skips them; run them deliberately with
`scripts/smoke-providers` when you change an adapter.

## Cutting a release

1. Update the version in `apps/desktop/src-tauri/tauri.conf.json` and the
   workspace `Cargo.toml`, and make sure `docs/status.md` still describes
   reality.
2. Run the full local gate:

   ```sh
   scripts/release-check
   ```

   It refuses to run on a dirty tree and stops at the first failing gate.
3. Run the provider smoke tests once, by hand:

   ```sh
   scripts/smoke-providers
   ```
4. Tag and push:

   ```sh
   git tag -a v0.1.0 -m "Commonspace v0.1.0"
   git push origin v0.1.0
   ```
5. Wait for `release.yml`. Its `verify-windows-install` job downloads the
   drafted Windows installer, installs it silently, and fails the run if the
   packaged binary does not land — so "it installs" is already checked for
   Windows by the time the draft exists. Still open the draft, download one
   installer, and *launch* it before pressing publish: headless CI cannot see
   a window, and a build that installs is not yet evidence that it runs.

## Code signing

Releases are unsigned by default, and the release notes say so:

- **Windows** — SmartScreen warns on first run; the installer still works via
  "More info → Run anyway".
- **macOS** — Gatekeeper blocks by default. Since macOS Sequoia the
  right-click-to-open bypass is gone; users must go to System Settings →
  Privacy & Security → "Open Anyway". Notarizing requires a paid Apple
  Developer account.
- **Linux** — no OS-level signing gate.

## In-app updates

Settings has a "Check for updates" button. It asks the GitHub releases API
for the newest published release (drafts never count; pre-releases do) and
compares versions. What happens next depends on how the release was built:

- **Signed release** — the release carries a `latest.json` updater manifest
  and `.sig` files. The app downloads the matching installer, verifies its
  signature against the public key in `tauri.conf.json`, installs, and
  restarts itself.
- **Unsigned release** (the default today) — there is nothing to verify a
  download against, so the button offers the release's download page
  instead. Honest and manual rather than silently broken.

To enable the signed, in-place path:

1. Generate a keypair once: `npm run tauri signer generate`. Keep the
   private key out of the repository.
2. Set two repository secrets: `TAURI_SIGNING_PRIVATE_KEY` and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. In `apps/desktop/src-tauri/tauri.conf.json`, paste the *public* key into
   `plugins.updater.pubkey` and set `bundle.createUpdaterArtifacts` to
   `true`, then commit both. The flag stays `false` until the secrets
   exist, deliberately: with it on, the Tauri bundler builds updater
   artifacts and *requires* the private key to sign them, so every build
   without the secrets — PR builds included — would fail outright.
4. Cut the next release normally. `release.yml` attaches `latest.json`
   automatically; every install of that release can then update itself in
   place from the release after it.

Until then the installers build unsigned and carry no updater manifest,
and the app keeps using the download-page path.

## Building locally instead

```sh
scripts/build
```

Installers land under `target/release/bundle/` at the repository root (the
Tauri crate is a workspace member, so cargo uses the workspace target
directory): NSIS and MSI on Windows, DMG and `.app` on macOS,
deb/rpm/AppImage on Linux.

There is no cross-compiling. Each installer must be built on the operating
system it targets, and macOS cannot be built anywhere but macOS — which is
precisely the gap `release.yml` fills for a single-machine developer.

## If CI fails but your machine is green

Likely causes, in the order worth checking:

1. **A platform you did not run on.** The matrix covers three; you ran one.
2. **Line endings.** `.gitattributes` normalizes to LF; a file committed with
   CRLF can fail `cargo fmt --check` on Linux only.
3. **A missing Linux system dependency.** The Tauri build needs
   `libwebkit2gtk-4.1-dev` and friends; `ci.yml` installs them.
4. **A new advisory.** `scripts/security` fails on a newly published RustSec
   advisory or a high/critical npm advisory even though nothing in the tree
   changed.
