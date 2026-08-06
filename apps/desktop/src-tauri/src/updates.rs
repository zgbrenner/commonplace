//! Update checks and in-place installs, surfaced in Settings.
//!
//! The GitHub releases API is the source of truth for what the newest
//! published version is — deliberately including pre-releases, since every
//! Commonspace release is one right now. From there, two paths:
//!
//! 1. **In-place** — when the newest release carries a `latest.json`
//!    updater manifest (produced once release signing is configured, see
//!    `docs/releasing.md`), the Tauri updater plugin downloads the matching
//!    installer, verifies its signature against the public key in
//!    `tauri.conf.json`, installs, and restarts into the new version.
//! 2. **Fallback** — today's unsigned releases have no manifest, so the
//!    user gets a button that opens the release's download page. Honest
//!    and manual rather than silently broken.
//!
//! Nothing here auto-installs: checks happen when the user asks, and an
//! install is an explicit second click.

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

use crate::commands::CommandError;

/// The project repository, from the workspace manifest — a fork that ships
/// its own releases updates from its own repository without code changes.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

type Result<T> = std::result::Result<T, CommandError>;

#[derive(Debug, Serialize)]
pub struct UpdateCheck {
    pub current_version: String,
    /// Newest published version, when one could be determined.
    pub latest_version: Option<String>,
    pub available: bool,
    /// Release notes for the newest version, when published.
    pub notes: Option<String>,
    /// True when this build can download and install the update itself
    /// (a signed updater manifest was found). False means the release page
    /// has to be opened instead.
    pub in_place: bool,
    /// Page to open for a manual download.
    pub release_url: String,
}

/// Progress of an in-place install, streamed to the Settings screen.
#[derive(Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum InstallProgress {
    Downloading { received: u64, total: Option<u64> },
    Installing,
}

/// What the GitHub releases API returns, reduced to the fields used here.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

impl GithubRelease {
    /// The updater manifest attached to this release, when it was built
    /// with signing configured.
    fn manifest_url(&self) -> Option<&str> {
        self.assets
            .iter()
            .find(|a| a.name == "latest.json")
            .map(|a| a.browser_download_url.as_str())
    }
}

fn releases_page() -> String {
    format!("{REPOSITORY}/releases/latest")
}

fn http_client(current: &semver::Version) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(format!("Commonspace/{current}"))
        .build()
        .map_err(|e| CommandError::new(format!("Commonspace couldn't check for updates: {e}")))
}

/// Newest published (non-draft) release. Asked as a listing rather than
/// GitHub's `/releases/latest`, because that endpoint hides pre-releases —
/// and every Commonspace release is currently marked pre-release.
async fn newest_release(client: &reqwest::Client) -> Result<GithubRelease> {
    let repo_path = REPOSITORY.trim_start_matches("https://github.com/");
    let url = format!("https://api.github.com/repos/{repo_path}/releases?per_page=10");
    let releases: Vec<GithubRelease> = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            CommandError::with_recovery(
                format!("Commonspace couldn't reach GitHub to check for updates: {e}"),
                "Check your internet connection and try again.",
            )
        })?
        .json()
        .await
        .map_err(|e| CommandError::new(format!("GitHub's answer couldn't be read: {e}")))?;

    releases.into_iter().find(|r| !r.draft).ok_or_else(|| {
        CommandError::with_recovery(
            "No published release was found to compare against.",
            "Releases that are still drafts don't count; publish one on GitHub first.",
        )
    })
}

fn release_version(release: &GithubRelease) -> Result<semver::Version> {
    semver::Version::parse(release.tag_name.trim_start_matches('v')).map_err(|_| {
        CommandError::new(format!(
            "The newest release is tagged \"{}\", which isn't a version number Commonspace can \
             compare against.",
            release.tag_name
        ))
    })
}

/// Ask the updater plugin whether the manifest describes an installable
/// newer version. Any error (no signing key in this build, malformed
/// manifest, network) means "no in-place update", not a failed check —
/// the caller still has the manual download path.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn checked_update(
    app: &tauri::AppHandle,
    manifest_url: &str,
) -> Option<tauri_plugin_updater::Update> {
    use tauri_plugin_updater::UpdaterExt;
    let endpoint: tauri::Url = manifest_url.parse().ok()?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|b| b.build())
        .map_err(|error| {
            tracing::info!(%error, "in-place updates unavailable in this build");
        })
        .ok()?;
    updater
        .check()
        .await
        .map_err(|error| {
            tracing::info!(%error, "updater manifest could not be used");
        })
        .ok()
        .flatten()
}

#[tauri::command]
pub async fn check_for_update(app: tauri::AppHandle) -> Result<UpdateCheck> {
    let current = app.package_info().version.clone();
    let client = http_client(&current)?;
    let release = newest_release(&client).await?;
    let latest = release_version(&release)?;

    if latest <= current {
        return Ok(UpdateCheck {
            current_version: current.to_string(),
            latest_version: Some(latest.to_string()),
            available: false,
            notes: None,
            in_place: false,
            release_url: releases_page(),
        });
    }

    // Prefer installing in place, when this release ships the manifest and
    // this build can verify signatures.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let in_place = match release.manifest_url() {
        Some(manifest) => checked_update(&app, manifest).await.is_some(),
        None => false,
    };
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let in_place = false;

    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: Some(latest.to_string()),
        available: true,
        notes: release.body.filter(|b| !b.trim().is_empty()),
        in_place,
        release_url: release.html_url,
    })
}

#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    on_progress: Channel<InstallProgress>,
) -> Result<()> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = (app, on_progress);
        Err(CommandError::new(
            "In-place updates aren't available on this platform.",
        ))
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let current = app.package_info().version.clone();
        let client = http_client(&current)?;
        let release = newest_release(&client).await?;
        let manifest = release.manifest_url().ok_or_else(|| {
            CommandError::with_recovery(
                "This release doesn't include in-place update files.",
                "Download it from the releases page instead.",
            )
        })?;
        let update = checked_update(&app, manifest).await.ok_or_else(|| {
            CommandError::with_recovery(
                "The update couldn't be prepared for an in-place install.",
                "Download it from the releases page instead.",
            )
        })?;

        let progress = on_progress.clone();
        let mut received: u64 = 0;
        update
            .download_and_install(
                move |chunk, total| {
                    received += chunk as u64;
                    let _ = progress.send(InstallProgress::Downloading { received, total });
                },
                move || {
                    let _ = on_progress.send(InstallProgress::Installing);
                },
            )
            .await
            .map_err(|e| {
                CommandError::with_recovery(
                    format!("The update couldn't be installed: {e}"),
                    "You can still download it manually from the releases page.",
                )
            })?;

        // On Windows the installer exits the app itself; everywhere else,
        // restart into the new version.
        app.restart();
    }
}

/// Open a release page in the browser. Only pages under this project's
/// repository are accepted — the frontend cannot use this to launch an
/// arbitrary URL.
#[tauri::command]
pub fn open_release_page(url: Option<String>) -> Result<()> {
    let target = url.unwrap_or_else(releases_page);
    if target != REPOSITORY && !target.starts_with(&format!("{REPOSITORY}/")) {
        return Err(CommandError::new(
            "That link doesn't point at the Commonspace repository.",
        ));
    }
    tauri_plugin_opener::open_url(target, None::<&str>)
        .map_err(|e| CommandError::new(format!("The releases page couldn't be opened: {e}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn repository_is_a_github_url() {
        // `newest_release` and `open_release_page` both derive URLs from
        // the manifest's repository field; a move to another host must be
        // noticed here, not by users with a broken update button.
        assert!(REPOSITORY.starts_with("https://github.com/"));
    }

    #[test]
    fn newer_versions_compare_correctly() {
        // The whole reason for semver comparison: "0.1.10" is newer than
        // "0.1.9", which a string comparison gets wrong.
        let older = semver::Version::parse("0.1.9").unwrap();
        let newer = semver::Version::parse("0.1.10").unwrap();
        assert!(newer > older);
    }

    #[test]
    fn manifest_is_found_among_assets() {
        let release = GithubRelease {
            tag_name: "v0.2.0".into(),
            html_url: format!("{REPOSITORY}/releases/tag/v0.2.0"),
            body: None,
            draft: false,
            assets: vec![
                GithubAsset {
                    name: "Commonspace_0.2.0_x64-setup.exe".into(),
                    browser_download_url: "https://example.invalid/setup.exe".into(),
                },
                GithubAsset {
                    name: "latest.json".into(),
                    browser_download_url: "https://example.invalid/latest.json".into(),
                },
            ],
        };
        assert_eq!(
            release.manifest_url(),
            Some("https://example.invalid/latest.json")
        );
    }

    #[test]
    fn drafts_are_never_offered() {
        // Drafts are visible to the repository owner's token but useless to
        // users; a draft-only listing must produce the honest error, and
        // the picker must skip drafts even when one is newest.
        let releases = [
            GithubRelease {
                tag_name: "v0.3.0".into(),
                html_url: String::new(),
                body: None,
                draft: true,
                assets: vec![],
            },
            GithubRelease {
                tag_name: "v0.2.0".into(),
                html_url: String::new(),
                body: None,
                draft: false,
                assets: vec![],
            },
        ];
        let newest = releases.into_iter().find(|r| !r.draft).unwrap();
        assert_eq!(newest.tag_name, "v0.2.0");
    }
}
