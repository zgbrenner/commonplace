//! CLI discovery and version probing. Read-only, never triggers logins.

use commonspace_core::InstallStatus;
use std::path::PathBuf;

/// Locate a CLI binary: PATH first (with PATHEXT handling on Windows), then
/// well-known install locations that GUI-launched apps often miss.
pub fn find_cli(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    fallback_locations(name)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn fallback_locations(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = dirs_home();
    #[cfg(windows)]
    {
        if let Some(home) = &home {
            for ext in ["cmd", "exe"] {
                out.push(
                    home.join("AppData/Roaming/npm")
                        .join(format!("{name}.{ext}")),
                );
                out.push(home.join(".local/bin").join(format!("{name}.{ext}")));
            }
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            out.push(PathBuf::from(pf).join(name).join(format!("{name}.exe")));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = &home {
            out.push(home.join(".local/bin").join(name));
            out.push(home.join(".npm-global/bin").join(name));
            out.push(home.join("bin").join(name));
        }
        out.push(PathBuf::from("/usr/local/bin").join(name));
        out.push(PathBuf::from("/opt/homebrew/bin").join(name));
    }
    out
}

fn dirs_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// Probe `<cli> --version` and classify the install.
pub async fn probe_version(name: &str) -> InstallStatus {
    let Some(path) = find_cli(name) else {
        return InstallStatus::NotInstalled;
    };
    let cwd = std::env::temp_dir();
    let args = vec!["--version".to_string()];
    match crate::process::spawn_cli(&path, &args, &cwd, &[]) {
        Ok(mut cli) => {
            let collect = async {
                let mut first = String::new();
                while let Some(line) = cli.stdout_lines.recv().await {
                    if !line.trim().is_empty() {
                        first = line.trim().to_string();
                        break;
                    }
                }
                first
            };
            match tokio::time::timeout(std::time::Duration::from_secs(20), collect).await {
                Ok(version) if !version.is_empty() => InstallStatus::Installed { version, path },
                Ok(_) => InstallStatus::Broken {
                    detail: format!("{} produced no version output", path.display()),
                },
                Err(_) => {
                    cli.kill.kill().await;
                    InstallStatus::Broken {
                        detail: format!("{} timed out responding to --version", path.display()),
                    }
                }
            }
        }
        Err(e) => InstallStatus::Broken {
            detail: format!("failed to run {}: {e}", path.display()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_cli_missing_returns_none() {
        assert!(find_cli("definitely-not-a-real-cli-xyz").is_none());
    }
}
