//! Auto-updater module for Chatty
//!
//! Provides automatic update functionality with:
//! - Background polling for new releases from GitHub
//! - Version comparison using semver
//! - Binary downloading with progress tracking
//! - OS-specific installation (macOS, Linux, Windows)
//!
//! Simplified architecture: direct GitHub API integration, no trait abstraction.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use gpui::{App, AsyncApp, BorrowAppContext, Global};
use semver::Version;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

mod installer;
#[cfg(not(target_os = "macos"))]
use installer::install_release;

/// Polling interval for checking updates (1 hour)
const POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// GitHub repository owner
const GITHUB_OWNER: &str = "boersmamarcel";

/// GitHub repository name
const GITHUB_REPO: &str = "chatty2";

/// Represents the current state of the auto-updater
#[derive(Clone, Debug, PartialEq, Default)]
pub enum AutoUpdateStatus {
    /// Waiting for the next poll interval
    #[default]
    Idle,
    /// Currently fetching release metadata
    Checking,
    /// Streaming the binary (holds progress percentage 0.0-1.0)
    Downloading(f32),
    /// Update ready, waiting for restart (version, path)
    Ready(String, PathBuf),
    /// Installing the update — on macOS this means the app is quitting gracefully
    /// so a helper script can replace the bundle and relaunch. On other platforms
    /// this is a brief transient state before the process exits.
    Installing,
    /// Something went wrong
    Error(String),
}

/// Information about a release asset available for download
#[derive(Clone, Debug)]
struct ReleaseAsset {
    version: String,
    download_url: String,
    name: String,
    sha256: Option<String>,
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
        use reqwest::header::{AUTHORIZATION, HeaderMap};

        let version = Version::parse(current_version).unwrap_or_else(|e| {
            warn!(error = ?e, version = current_version, "Failed to parse current version, using 0.0.0");
            Version::new(0, 0, 0)
        });

        let mut headers = HeaderMap::new();
        if let Ok(token) = env::var("GITHUB_TOKEN") {
            info!("Using GITHUB_TOKEN for authenticated updater requests.");
            match format!("Bearer {}", token).parse() {
                Ok(header_value) => {
                    headers.insert(AUTHORIZATION, header_value);
                }
                Err(e) => {
                    error!(error = ?e, "Failed to create authorization header from GITHUB_TOKEN");
                }
            }
        }

        let client = reqwest::Client::builder()
            .user_agent("chatty-auto-updater/1.0")
            .default_headers(headers)
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

    /// Fetch and parse checksums from the release
    async fn fetch_checksums(
        client: &reqwest::Client,
        release: &GitHubRelease,
    ) -> std::collections::HashMap<String, String> {
        let checksum_patterns = [
            "checksums.txt",
            "checksums.sha256",
            "SHA256SUMS",
            "CHECKSUMS",
        ];

        for pattern in &checksum_patterns {
            if let Some(checksum_asset) = release
                .assets
                .iter()
                .find(|a| a.name.to_lowercase() == pattern.to_lowercase())
            {
                debug!(
                    url = &checksum_asset.browser_download_url,
                    "Found checksums file"
                );

                match client
                    .get(&checksum_asset.browser_download_url)
                    .send()
                    .await
                {
                    Ok(response) => {
                        if let Ok(text) = response.text().await {
                            return Self::parse_checksums(&text);
                        }
                    }
                    Err(e) => {
                        debug!(error = ?e, "Failed to fetch checksums file");
                    }
                }
            }
        }

        std::collections::HashMap::new()
    }

    /// Parse checksums from text format
    fn parse_checksums(text: &str) -> std::collections::HashMap<String, String> {
        let mut checksums = std::collections::HashMap::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Try "hash  filename" or "hash filename" format
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let hash = parts[0];
                let filename = parts[1..].join(" ");

                if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    checksums.insert(filename, hash.to_lowercase());
                    continue;
                }
            }

            // Try "filename: hash" format
            if let Some((filename, hash)) = line.split_once(':') {
                let hash = hash.trim();
                let filename = filename.trim();

                if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    checksums.insert(filename.to_string(), hash.to_lowercase());
                }
            }
        }

        debug!(count = checksums.len(), "Parsed checksums");
        checksums
    }

    /// Verify SHA-256 checksum of a file
    async fn verify_checksum(
        path: &PathBuf,
        expected_hash: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 8192];

        loop {
            let bytes_read = file.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let hash = format!("{:x}", hasher.finalize());
        let expected_hash_lower = expected_hash.to_lowercase();

        Ok(hash == expected_hash_lower)
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
                .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
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
                            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI")).ok();
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
                        .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI")).ok();
                    }
                }
                Ok(None) => {
                    debug!("No release found for current platform");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status = AutoUpdateStatus::Idle;
                        });
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI")).ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to check for updates");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status = AutoUpdateStatus::Error(format!("Update check failed: {}", e));
                        });
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI")).ok();
                }
            }
        })
        .detach();
    }

    /// Install the downloaded update and restart the application.
    ///
    /// On macOS this uses a Zed-style deferred approach for smooth UX:
    /// 1. Set status to Installing for visual feedback
    /// 2. Write a helper shell script to /tmp that will mount the DMG,
    ///    rsync the .app bundle, and relaunch the app
    /// 3. Spawn the helper as a detached process (it sleeps 2s first)
    /// 4. Call cx.quit() for a graceful GPUI shutdown with window animations
    ///
    /// On Linux/Windows the existing in-process approach is used, now with
    /// async I/O to keep the Tokio runtime unblocked.
    pub fn install_and_restart(&mut self, cx: &mut App) {
        let update_path = match &self.status {
            AutoUpdateStatus::Ready(_, update_path) => update_path.clone(),
            _ => {
                warn!("Cannot install: no update downloaded");
                return;
            }
        };

        // Show Installing status immediately for visual feedback
        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
            updater.status = AutoUpdateStatus::Installing;
        });

        // PHASE 1 (macOS): Deferred installation via helper script + graceful quit
        #[cfg(target_os = "macos")]
        {
            let app_bundle = std::env::current_exe().ok().and_then(|exe| {
                exe.ancestors()
                    .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
                    .map(|p| p.to_path_buf())
            });

            match app_bundle {
                Some(bundle) => {
                    launch_macos_install_helper(&update_path, &bundle);
                    cx.quit();
                }
                None => {
                    error!("Could not find current app bundle for macOS update");
                    cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                        updater.status = AutoUpdateStatus::Error(
                            "Could not find app bundle path for update installation".to_string(),
                        );
                    });
                }
            }
        }

        // PHASE 2 (Linux / Windows): In-process installation then restart
        #[cfg(not(target_os = "macos"))]
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
                    .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                    .ok();

                    // Linux: relaunch the process (AppImage replacement is done)
                    #[cfg(target_os = "linux")]
                    restart_application();

                    // Windows: quit and let the installer handle the relaunch
                    #[cfg(target_os = "windows")]
                    cx.update(|cx| {
                        cx.quit();
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to quit for Windows update"))
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Installation failed");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status =
                                AutoUpdateStatus::Error(format!("Installation failed: {}", e));
                        });
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                    .ok();
                }
            }
        })
        .detach();
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

    if response.status() == reqwest::StatusCode::FORBIDDEN
        && response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(|s| s == "0")
            .unwrap_or(false)
    {
        return Err("Rate limited by GitHub API".to_string());
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
            // Fetch checksums for this release
            let checksums = AutoUpdater::fetch_checksums(client, &release).await;
            let sha256 = checksums.get(&asset.name).cloned();

            if sha256.is_none() {
                debug!(asset = &asset.name, "Warning: No checksum found for asset");
            }

            info!(
                version = version,
                name = &asset.name,
                has_checksum = sha256.is_some(),
                "Found matching release asset"
            );
            return Ok(Some(ReleaseAsset {
                version: version.to_string(),
                download_url: asset.browser_download_url,
                name: asset.name,
                sha256,
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
        ("linux", "x86_64") => "chatty-linux-x86_64.AppImage",
        ("linux", "aarch64") => "chatty-linux-aarch64.AppImage",
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
    .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
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
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
            return;
        }
    };

    let download_path = temp_dir.path().join(&asset.name);

    // Download with progress tracking
    match download_file(&asset.download_url, &download_path, cx).await {
        Ok(()) => {
            info!(path = ?download_path, "Download complete");

            // Verify checksum if available
            if let Some(ref expected_hash) = asset.sha256 {
                info!(
                    expected_hash = expected_hash,
                    "Verifying download integrity"
                );

                match AutoUpdater::verify_checksum(&download_path, expected_hash).await {
                    Ok(true) => {
                        info!("Checksum verification passed");
                    }
                    Ok(false) => {
                        error!(
                            expected = expected_hash,
                            "Checksum verification failed - download may be corrupted or tampered"
                        );
                        // Delete the corrupted file
                        let _ = tokio::fs::remove_file(&download_path).await;

                        cx.update(|cx| {
                            cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                updater.status = AutoUpdateStatus::Error(
                                    "Security check failed: Download integrity verification failed. \
                                     The downloaded file does not match the expected checksum."
                                        .to_string(),
                                );
                            });
                        })
                        .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI")).ok();
                        return;
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to verify checksum");
                        // Delete the file to be safe
                        let _ = tokio::fs::remove_file(&download_path).await;

                        cx.update(|cx| {
                            cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                updater.status = AutoUpdateStatus::Error(format!(
                                    "Checksum verification error: {}",
                                    e
                                ));
                            });
                        })
                        .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                        .ok();
                        return;
                    }
                }
            } else {
                error!(
                    "Security check failed: No checksum available for this release. \
                     Checksums are mandatory for security. This must be fixed in the release process."
                );
                // Delete the downloaded file since we cannot verify its integrity
                let _ = tokio::fs::remove_file(&download_path).await;

                cx.update(|cx| {
                    cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                        updater.status = AutoUpdateStatus::Error(
                            "Security check failed: No checksum available for this release. \
                             Updates require integrity verification."
                                .to_string(),
                        );
                    });
                })
                .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                .ok();
                return;
            }

            let final_path = download_path.clone();
            let _ = temp_dir.keep(); // Persist temp dir

            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Ready(asset.version.clone(), final_path);
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
        }
        Err(e) => {
            error!(error = ?e, "Download failed");
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Error(format!("Download failed: {}", e));
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
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
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
        }
    }

    file.flush().await?;
    Ok(())
}

/// Restart the application after a Linux AppImage update.
///
/// The old executable was atomically replaced by the installer, so we just
/// re-exec the path (stripping any " (deleted)" suffix the kernel may have
/// appended) and then exit the current process.
#[cfg(target_os = "linux")]
fn restart_application() {
    use std::process::Command;

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            error!(error = ?e, "Failed to get current executable path");
            return;
        }
    };

    // On Linux, when the executable is replaced while running, the path may have
    // " (deleted)" appended. Strip this suffix to get the actual path.
    let current_exe = {
        let path_str = current_exe.to_string_lossy();
        if let Some(stripped) = path_str.strip_suffix(" (deleted)") {
            std::path::PathBuf::from(stripped)
        } else {
            current_exe
        }
    };

    info!(path = ?current_exe, "Restarting application");

    if let Err(e) = Command::new(&current_exe).spawn() {
        error!(error = ?e, "Failed to restart application");
    }

    std::process::exit(0);
}

/// Write and spawn a detached shell script that installs the macOS update
/// after the app has fully exited.
///
/// The script:
/// 1. Sleeps 2 s to let the app finish its GPUI shutdown
/// 2. Mounts the downloaded .dmg with `hdiutil`
/// 3. Rsyncs the new .app bundle over the current installation
/// 4. Unmounts the .dmg
/// 5. Relaunches the updated app with `open`
#[cfg(target_os = "macos")]
pub fn launch_macos_install_helper(dmg_path: &std::path::Path, app_bundle: &std::path::Path) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dmg = dmg_path.to_string_lossy();
    let bundle = app_bundle.to_string_lossy();

    let script = format!(
        r#"#!/bin/bash
set -e

DMG_PATH="{dmg}"
APP_BUNDLE="{bundle}"

# Wait for the app to fully exit
sleep 2

# Mount the DMG and capture plist output
MOUNT_OUTPUT=$(hdiutil attach -nobrowse -plist "$DMG_PATH" 2>/dev/null)

# Extract mount point from plist output
MOUNT_POINT=$(echo "$MOUNT_OUTPUT" \
    | grep -A1 "mount-point" \
    | grep "<string>" \
    | sed 's/.*<string>\(.*\)<\/string>.*/\1/' \
    | head -1)

# Fallback: scan for /Volumes/ path if plist parse failed
if [ -z "$MOUNT_POINT" ]; then
    MOUNT_POINT=$(echo "$MOUNT_OUTPUT" | grep -o '/Volumes/[^<"]*' | head -1 | tr -d '[:space:]')
fi

if [ -z "$MOUNT_POINT" ]; then
    exit 1
fi

# Find the .app bundle inside the mounted volume
APP_IN_DMG=$(find "$MOUNT_POINT" -maxdepth 1 -name "*.app" | head -1)

if [ -z "$APP_IN_DMG" ]; then
    hdiutil detach -force "$MOUNT_POINT" 2>/dev/null || true
    exit 1
fi

# Replace the current installation with the new bundle
rsync -a --delete "$APP_IN_DMG/" "$APP_BUNDLE/"

# Unmount
hdiutil detach -force "$MOUNT_POINT" 2>/dev/null || true

# Relaunch the updated app
open "$APP_BUNDLE"
"#,
        dmg = dmg,
        bundle = bundle,
    );

    let script_path = std::path::PathBuf::from("/tmp/chatty_update_helper.sh");

    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&script_path)?;
        file.write_all(script.as_bytes())?;
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms)?;
        Ok(())
    })();

    if let Err(e) = result {
        error!(error = ?e, "Failed to write macOS install helper script");
        return;
    }

    // Spawn as a detached process — it must outlive the current process
    match std::process::Command::new("bash")
        .arg(&script_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            // Drop the handle immediately so we don't wait for the child
            drop(child);
            info!(
                script = ?script_path,
                dmg = ?dmg_path,
                "macOS install helper launched; quitting app for graceful restart"
            );
        }
        Err(e) => {
            error!(error = ?e, "Failed to launch macOS install helper");
        }
    }
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
                name: "chatty-linux-x86_64.AppImage".to_string(),
                browser_download_url: "https://example.com/linux-x64".to_string(),
            },
        ];

        let result = find_matching_asset(&assets, "macos", "aarch64");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "chatty-macos-aarch64.dmg");

        let result = find_matching_asset(&assets, "linux", "x86_64");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "chatty-linux-x86_64.AppImage");

        let result = find_matching_asset(&assets, "windows", "x86_64");
        assert!(result.is_none());
    }
}
