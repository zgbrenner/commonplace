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
| `ci.yml` | push to `main`, every PR, manual | `scripts/lint` and `scripts/test` on Linux, macOS and Windows; `scripts/typecheck`, lint and production build for the frontend; `scripts/security` |
| `release.yml` | `v*` tag, manual | Verifies, then builds installers per OS and drafts a release |
| `audit.yml` | Mondays 07:00 UTC, manual | `scripts/security` alone, to catch advisories filed against an unchanged dependency tree |

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
5. Wait for `release.yml`, then open the draft release, download at least one
   installer, install it, and launch it before pressing publish. A build that
   compiles is not evidence that it runs.

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
3. Paste the *public* key into `plugins.updater.pubkey` in
   `apps/desktop/src-tauri/tauri.conf.json` and commit it.
4. Cut the next release normally. `release.yml` attaches `latest.json`
   automatically; every install of that release can then update itself in
   place from the release after it.

Without the secrets the installers still build; the release simply carries
no `.sig` files or manifest, and the app keeps using the download-page path.

## Building locally instead

```sh
scripts/build
```

Installers land under `apps/desktop/src-tauri/target/release/bundle/`: NSIS
and MSI on Windows, DMG and `.app` on macOS, deb/rpm/AppImage on Linux.

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
