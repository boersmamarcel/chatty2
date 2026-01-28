//! Auto-updater module for Chatty
//!
//! Provides automatic update functionality with:
//! - Background polling for new releases from GitHub
//! - Version comparison using semver
//! - Binary downloading with progress tracking
//! - OS-specific installation (macOS, Linux, Windows)
//!
//! Simplified architecture: direct GitHub API integration, no trait abstraction.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use gpui::{App, AsyncApp, BorrowAppContext, Global};
use semver::Version;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

mod installer;
use installer::{InstallError, install_release};

/// Polling interval for checking updates (1 hour)
const POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Minimum polling interval to prevent abuse (5 minutes)
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// GitHub repository owner
const GITHUB_OWNER: &str = "boersmamarcel";

/// GitHub repository name
const GITHUB_REPO: &str = "chatty2";

/// Represents the current state of the auto-updater
#[derive(Clone, Debug, PartialEq)]
pub enum AutoUpdateStatus {
    /// Waiting for the next poll interval
    Idle,
    /// Currently fetching release metadata
    Checking,
    /// Streaming the binary (holds progress percentage 0.0-1.0)
    Downloading(f32),
    /// Update ready, waiting for restart (version, path)
    Ready(String, PathBuf),
    /// Something went wrong
    Error(String),
}

impl Default for AutoUpdateStatus {
    fn default() -> Self {
        Self::Idle
    }
}

/// Information about a release asset available for download
#[derive(Clone, Debug)]
struct ReleaseAsset {
    version: String,
    download_url: String,
    name: String,
}

/// GitHub Releases API response structures
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize, Clone)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// The main auto-updater model
///
/// Manages the update lifecycle:
/// - Polling for new releases
/// - Downloading updates
/// - Installing updates based on OS
/// - State machine management
#[derive(Clone)]
pub struct AutoUpdater {
    /// Current status of the updater
    status: AutoUpdateStatus,
    /// Current application version
    current_version: Version,
    /// HTTP client for GitHub API
    client: reqwest::Client,
    /// Flag indicating if a restart is required (Windows only)
    pub should_restart_on_quit: bool,
    /// Path to pending update (Windows only)
    pending_update_path: Option<PathBuf>,
}

impl Global for AutoUpdater {}

impl AutoUpdater {
    /// Create a new AutoUpdater with the current application version
    pub fn new(current_version: &str) -> Self {
        let version = Version::parse(current_version).unwrap_or_else(|e| {
            warn!(error = ?e, version = current_version, "Failed to parse current version, using 0.0.0");
            Version::new(0, 0, 0)
        });

        let client = reqwest::Client::builder()
            .user_agent("chatty-auto-updater/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            status: AutoUpdateStatus::Idle,
            current_version: version,
            client,
            should_restart_on_quit: false,
            pending_update_path: None,
        }
    }

    /// Get the current update status
    pub fn status(&self) -> &AutoUpdateStatus {
        &self.status
    }

    /// Get the current version
    pub fn current_version(&self) -> &Version {
        &self.current_version
    }

    /// Dismiss any error state and return to idle
    pub fn dismiss_error(&mut self) {
        if matches!(self.status, AutoUpdateStatus::Error(_)) {
            self.status = AutoUpdateStatus::Idle;
        }
    }

    /// Reset the status to idle
    pub fn reset(&mut self) {
        self.status = AutoUpdateStatus::Idle;
    }

    /// Start the polling loop for checking updates
    pub fn start_polling(&self, cx: &mut App) {
        info!(
            interval_secs = POLL_INTERVAL.as_secs(),
            "Starting auto-update polling loop"
        );

        // Perform an initial check immediately
        self.check_for_update(cx);

        // Start the polling loop
        cx.spawn(async move |cx: &mut AsyncApp| {
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;

                // Trigger update check
                cx.update(|cx| {
                    if let Some(updater) = cx.try_global::<AutoUpdater>() {
                        // Only check if we're idle
                        if matches!(updater.status, AutoUpdateStatus::Idle) {
                            let updater_clone = updater.clone();
                            updater_clone.check_for_update(cx);
                        }
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Check for updates now
    pub fn check_for_update(&self, cx: &mut App) {
        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
            updater.status = AutoUpdateStatus::Checking;
        });

        let client = self.client.clone();
        let current_version = self.current_version.clone();

        cx.spawn(async move |cx: &mut AsyncApp| {
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;

            info!(os = os, arch = arch, "Checking for updates");

            match fetch_latest_release(&client, os, arch).await {
                Ok(Some(asset)) => {
                    // Parse remote version
                    let remote_version = match Version::parse(&asset.version) {
                        Ok(v) => v,
                        Err(e) => {
                            error!(error = ?e, version = &asset.version, "Failed to parse remote version");
                            cx.update(|cx| {
                                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                    updater.status = AutoUpdateStatus::Error(format!(
                                        "Invalid version format: {}",
                                        asset.version
                                    ));
                                });
                            })
                            .ok();
                            return;
                        }
                    };

                    if remote_version > current_version {
                        info!(current = %current_version, remote = %remote_version, "New version available");
                        download_update(asset, cx).await;
                    } else {
                        debug!(current = %current_version, remote = %remote_version, "Already up to date");
                        cx.update(|cx| {
                            cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                updater.status = AutoUpdateStatus::Idle;
                            });
                        })
                        .ok();
                    }
                }
                Ok(None) => {
                    debug!("No release found for current platform");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status = AutoUpdateStatus::Idle;
                        });
                    })
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to check for updates");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status = AutoUpdateStatus::Error(format!("Update check failed: {}", e));
                        });
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Install the downloaded update and restart the application
    pub fn install_and_restart(&mut self, cx: &mut App) {
        let update_path = match &self.status {
            AutoUpdateStatus::Ready(_, update_path) => update_path.clone(),
            _ => {
                warn!("Cannot install: no update downloaded");
                return;
            }
        };

        cx.spawn(async move |cx: &mut AsyncApp| {
            match install_release(&update_path).await {
                Ok(needs_restart) => {
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            if needs_restart {
                                updater.should_restart_on_quit = true;
                                updater.pending_update_path = Some(update_path.clone());
                            }
                            updater.status = AutoUpdateStatus::Idle;
                        });
                    })
                    .ok();

                    // On macOS and Linux, restart immediately
                    #[cfg(not(target_os = "windows"))]
                    restart_application();

                    // On Windows, request quit (installer will handle restart)
                    #[cfg(target_os = "windows")]
                    {
                        cx.update(|cx| {
                            cx.quit();
                        })
                        .ok();
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Installation failed");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status =
                                AutoUpdateStatus::Error(format!("Installation failed: {}", e));
                        });
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Finalize pending updates on application quit (Windows only)
    #[cfg(target_os = "windows")]
    pub fn finalize_update(&self) -> Result<(), InstallError> {
        if self.should_restart_on_quit {
            if let Some(ref update_path) = self.pending_update_path {
                return installer::finalize_windows_update(update_path);
            }
        }
        Ok(())
    }

    /// Stub for non-Windows platforms
    #[cfg(not(target_os = "windows"))]
    pub fn finalize_update(&self) -> Result<(), InstallError> {
        Ok(())
    }
}

/// Fetch the latest release from GitHub API
async fn fetch_latest_release(
    client: &reqwest::Client,
    os: &str,
    arch: &str,
) -> Result<Option<ReleaseAsset>, String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases",
        GITHUB_OWNER, GITHUB_REPO
    );

    debug!(url = &url, "Fetching releases from GitHub API");

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if response.status() == reqwest::StatusCode::FORBIDDEN {
        if response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(|s| s == "0")
            .unwrap_or(false)
        {
            return Err("Rate limited by GitHub API".to_string());
        }
    }

    if !response.status().is_success() {
        return Err(format!("API returned status: {}", response.status()));
    }

    let releases: Vec<GitHubRelease> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    debug!(count = releases.len(), "Fetched releases from GitHub");

    // Find the first stable release (not draft, not prerelease) with a matching asset
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }

        // Parse version from tag (strip 'v' prefix)
        let version = release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name);

        // Try to find matching asset using simple convention
        if let Some(asset) = find_matching_asset(&release.assets, os, arch) {
            info!(
                version = version,
                name = &asset.name,
                "Found matching release asset"
            );
            return Ok(Some(ReleaseAsset {
                version: version.to_string(),
                download_url: asset.browser_download_url,
                name: asset.name,
            }));
        }
    }

    debug!("No matching release found for platform");
    Ok(None)
}

/// Find a matching asset for the current platform using simple naming convention
/// Expected format: chatty-{os}-{arch}.{ext}
fn find_matching_asset(assets: &[GitHubAsset], os: &str, arch: &str) -> Option<GitHubAsset> {
    // Build expected asset name based on platform
    let expected_name = match (os, arch) {
        ("macos", "aarch64") => "chatty-macos-aarch64.dmg",
        ("macos", "x86_64") => "chatty-macos-x86_64.dmg",
        ("linux", "x86_64") => "chatty-linux-x86_64.tar.gz",
        ("linux", "aarch64") => "chatty-linux-aarch64.tar.gz",
        ("windows", "x86_64") => "chatty-windows-x86_64.exe",
        _ => {
            warn!(os = os, arch = arch, "Unsupported platform");
            return None;
        }
    };

    // Find exact match
    assets
        .iter()
        .find(|asset| asset.name == expected_name)
        .cloned()
}

/// Download an update asynchronously
async fn download_update(asset: ReleaseAsset, cx: &mut AsyncApp) {
    cx.update(|cx| {
        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
            updater.status = AutoUpdateStatus::Downloading(0.0);
        });
    })
    .ok();

    info!(
        url = &asset.download_url,
        version = &asset.version,
        "Starting update download"
    );

    // Create temp file for download
    let temp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            error!(error = ?e, "Failed to create temp directory");
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status =
                        AutoUpdateStatus::Error(format!("Failed to create temp dir: {}", e));
                });
            })
            .ok();
            return;
        }
    };

    let download_path = temp_dir.path().join(&asset.name);

    // Download with progress tracking
    match download_file(&asset.download_url, &download_path, cx).await {
        Ok(()) => {
            info!(path = ?download_path, "Download complete");

            let final_path = download_path.clone();
            let _ = temp_dir.keep(); // Persist temp dir

            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Ready(asset.version.clone(), final_path);
                });
            })
            .ok();
        }
        Err(e) => {
            error!(error = ?e, "Download failed");
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Error(format!("Download failed: {}", e));
                });
            })
            .ok();
        }
    }
}

/// Download a file with progress tracking
async fn download_file(
    url: &str,
    path: &PathBuf,
    cx: &mut AsyncApp,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }

    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        // Update progress
        if total_size > 0 {
            let progress = downloaded as f32 / total_size as f32;
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Downloading(progress);
                });
            })
            .ok();
        }
    }

    file.flush().await?;
    Ok(())
}

/// Restart the application (macOS and Linux only)
#[cfg(not(target_os = "windows"))]
fn restart_application() {
    use std::process::Command;

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            error!(error = ?e, "Failed to get current executable path");
            return;
        }
    };

    info!(path = ?current_exe, "Restarting application");

    if let Err(e) = Command::new(&current_exe).spawn() {
        error!(error = ?e, "Failed to restart application");
    }

    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison() {
        let current = Version::parse("0.1.0").unwrap();
        let newer = Version::parse("0.2.0").unwrap();
        let older = Version::parse("0.0.9").unwrap();

        assert!(newer > current);
        assert!(older < current);
    }

    #[test]
    fn test_auto_updater_creation() {
        let updater = AutoUpdater::new("0.1.0");
        assert_eq!(updater.current_version().to_string(), "0.1.0");
        assert!(matches!(updater.status(), AutoUpdateStatus::Idle));
    }

    #[test]
    fn test_asset_matching() {
        let assets = vec![
            GitHubAsset {
                name: "chatty-macos-aarch64.dmg".to_string(),
                browser_download_url: "https://example.com/macos-arm".to_string(),
            },
            GitHubAsset {
                name: "chatty-linux-x86_64.tar.gz".to_string(),
                browser_download_url: "https://example.com/linux-x64".to_string(),
            },
        ];

        let result = find_matching_asset(&assets, "macos", "aarch64");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "chatty-macos-aarch64.dmg");

        let result = find_matching_asset(&assets, "linux", "x86_64");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "chatty-linux-x86_64.tar.gz");

        let result = find_matching_asset(&assets, "windows", "x86_64");
        assert!(result.is_none());
    }
}
