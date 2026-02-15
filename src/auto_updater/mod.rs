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

        info!("Looking for checksums file in release");

        for pattern in &checksum_patterns {
            if let Some(checksum_asset) = release
                .assets
                .iter()
                .find(|a| a.name.to_lowercase() == pattern.to_lowercase())
            {
                info!(
                    pattern = pattern,
                    url = &checksum_asset.browser_download_url,
                    "Found checksums file, downloading..."
                );

                match client
                    .get(&checksum_asset.browser_download_url)
                    .send()
                    .await
                {
                    Ok(response) => {
                        let status = response.status();
                        debug!(status = ?status, "Received response for checksums file");

                        match response.text().await {
                            Ok(text) => {
                                debug!(
                                    length = text.len(),
                                    preview = &text.chars().take(100).collect::<String>(),
                                    "Successfully downloaded checksums file"
                                );
                                return Self::parse_checksums(&text);
                            }
                            Err(e) => {
                                warn!(error = ?e, "Failed to read checksums response body");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "Failed to fetch checksums file");
                    }
                }
            }
        }

        let available_assets: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        warn!(
            available_assets = ?available_assets,
            "No checksums file found in release assets"
        );

        std::collections::HashMap::new()
    }

    /// Parse checksums from text format
    fn parse_checksums(text: &str) -> std::collections::HashMap<String, String> {
        let mut checksums = std::collections::HashMap::new();

        for (line_num, line) in text.lines().enumerate() {
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
                    debug!(
                        line = line_num + 1,
                        filename = &filename,
                        hash = &hash[..16],
                        "Parsed checksum entry"
                    );
                    checksums.insert(filename, hash.to_lowercase());
                    continue;
                }
            }

            // Try "filename: hash" format
            if let Some((filename, hash)) = line.split_once(':') {
                let hash = hash.trim();
                let filename = filename.trim();

                if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    debug!(
                        line = line_num + 1,
                        filename = filename,
                        hash = &hash[..16],
                        "Parsed checksum entry"
                    );
                    checksums.insert(filename.to_string(), hash.to_lowercase());
                }
            }
        }

        let filenames: Vec<&str> = checksums.keys().map(|s| s.as_str()).collect();
        info!(
            checksum_count = checksums.len(),
            filenames = ?filenames,
            "Successfully parsed checksums"
        );

        if checksums.is_empty() {
            warn!("No checksums were successfully parsed from checksums file");
        }

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

    /// Check if a previous update installation succeeded or failed
    ///
    /// On macOS, this reads the update log file to detect installation failures
    /// that occurred after the app quit. If errors are found, sets the status
    /// to Error so the user can see diagnostic information.
    ///
    /// Should be called during app initialization on macOS.
    pub fn check_previous_update_status(&self, _cx: &mut App) {
        #[cfg(target_os = "macos")]
        {
            use std::path::PathBuf;

            let log_path = PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Library/Logs/chatty_update.log");

            if log_path.exists() {
                // Read last few lines to check for success/failure
                if let Ok(content) = std::fs::read_to_string(&log_path) {
                    let lines: Vec<&str> = content.lines().collect();
                    let last_10_lines = lines.iter().rev().take(10).rev().collect::<Vec<_>>();

                    for line in last_10_lines {
                        if line.contains("ERROR:") {
                            warn!(
                                log_file = ?log_path,
                                "Previous update installation failed - check log file"
                            );
                            cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                updater.status = AutoUpdateStatus::Error(format!(
                                    "Previous update installation failed. Check {} for details.",
                                    log_path.display()
                                ));
                            });
                            return;
                        }
                    }
                }
            }
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
            let current_exe_result = std::env::current_exe();
            debug!(current_exe = ?current_exe_result, "Looking for app bundle");

            let app_bundle = current_exe_result.ok().and_then(|exe| {
                debug!(exe_path = ?exe, "Current executable path");

                // Log all ancestors to help debug
                let ancestors: Vec<_> = exe.ancestors().collect();
                debug!(
                    ancestor_count = ancestors.len(),
                    "Searching ancestors for .app bundle"
                );

                for (i, ancestor) in ancestors.iter().enumerate() {
                    debug!(
                        level = i,
                        path = ?ancestor,
                        extension = ?ancestor.extension(),
                        "Checking ancestor"
                    );
                }

                exe.ancestors()
                    .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
                    .map(|p| p.to_path_buf())
            });

            match app_bundle {
                Some(bundle) => {
                    info!(bundle = ?bundle, "Found app bundle, launching install helper");
                    launch_macos_install_helper(&update_path, &bundle);
                    cx.quit();
                }
                None => {
                    // Check if we're running in development mode (not from a .app bundle)
                    let is_dev_mode = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.to_str().map(|s| s.contains("/target/")))
                        .unwrap_or(false);

                    if is_dev_mode {
                        warn!(
                            "Skipping auto-update installation in development mode (not running from .app bundle)"
                        );
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status = AutoUpdateStatus::Error(
                                "Auto-updates are only available when running from a packaged .app bundle. \
                                 Build with ./scripts/package-macos.sh to test updates. \
                                 Check ~/Library/Logs/chatty_update.log for details."
                                    .to_string(),
                            );
                        });
                    } else {
                        error!("Could not find current app bundle for macOS update");
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status = AutoUpdateStatus::Error(
                                "Could not find app bundle path for update installation. \
                                 This may occur when running outside of a packaged .app bundle. \
                                 Check ~/Library/Logs/chatty_update.log for details."
                                    .to_string(),
                            );
                        });
                    }
                }
            }
        }

        // PHASE 2 (Linux / Windows): In-process installation then graceful quit
        //
        // Both platforms keep the Installing status visible right up to quit —
        // there is no intermediate Idle flash.  The new process is spawned
        // (Linux) or already running (Windows installer) before cx.quit() is
        // called so GPUI can close windows with its normal animations.
        #[cfg(not(target_os = "macos"))]
        cx.spawn(async move |cx: &mut AsyncApp| {
            match install_release(&update_path).await {
                Ok(_) => {
                    // Linux: the AppImage on disk has already been atomically
                    // replaced.  Spawn the new binary, then quit gracefully.
                    #[cfg(target_os = "linux")]
                    {
                        if let Err(e) = relaunch_linux_process() {
                            error!(error = ?e, "Failed to spawn updated AppImage");
                            cx.update(|cx| {
                                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                    updater.status = AutoUpdateStatus::Error(format!(
                                        "Failed to relaunch: {}",
                                        e
                                    ));
                                });
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                            .ok();
                            return;
                        }
                        cx.update(|cx| cx.quit())
                            .map_err(|e| warn!(error = ?e, "Failed to quit after Linux update"))
                            .ok();
                    }

                    // Windows: the silent installer is already running and will
                    // handle file replacement and relaunch on its own.
                    #[cfg(target_os = "windows")]
                    cx.update(|cx| cx.quit())
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
                let available_checksums: Vec<&str> = checksums.keys().map(|s| s.as_str()).collect();
                warn!(
                    asset = &asset.name,
                    available_checksums = ?available_checksums,
                    "No checksum found for asset"
                );
            } else {
                debug!(
                    asset = &asset.name,
                    hash = sha256.as_ref().map(|h| &h[..16]),
                    "Found checksum for asset"
                );
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

/// Spawn the updated AppImage binary on Linux.
///
/// The on-disk executable was atomically replaced by the installer before this
/// is called.  We only spawn — the caller is responsible for quitting the
/// current process gracefully via `cx.quit()`.
///
/// Uses the APPIMAGE environment variable to get the correct path to the
/// AppImage file (same approach as the installer uses).
#[cfg(target_os = "linux")]
fn relaunch_linux_process() -> std::io::Result<()> {
    use std::process::Command;

    // Use APPIMAGE env var when available (running as AppImage)
    // Otherwise fall back to current_exe for non-AppImage installs
    let appimage_path = if let Ok(appimage_env) = std::env::var("APPIMAGE") {
        std::path::PathBuf::from(appimage_env)
    } else {
        std::env::current_exe()?
    };

    info!(path = ?appimage_path, "Spawning updated AppImage");
    Command::new(&appimage_path).spawn()?;
    Ok(())
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
LOG_FILE="$HOME/Library/Logs/chatty_update.log"

# Logging function
log() {{
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" | tee -a "$LOG_FILE"
}}

log "=== Chatty Update Installation Started ==="
log "DMG: $DMG_PATH"
log "Target: $APP_BUNDLE"

# Wait for the app to fully exit
log "Waiting for app to exit..."
sleep 2

# Mount the DMG and capture plist output
log "Mounting DMG..."
MOUNT_OUTPUT=$(hdiutil attach -nobrowse -plist "$DMG_PATH" 2>&1)
HDIUTIL_EXIT=$?

if [ $HDIUTIL_EXIT -ne 0 ]; then
    log "ERROR: hdiutil failed with exit code $HDIUTIL_EXIT"
    log "Output: $MOUNT_OUTPUT"
    exit 1
fi

log "DMG mounted successfully"

# Extract mount point from plist output
MOUNT_POINT=$(echo "$MOUNT_OUTPUT" \
    | grep -A1 "mount-point" \
    | grep "<string>" \
    | sed 's/.*<string>\(.*\)<\/string>.*/\1/' \
    | head -1)

# Fallback: scan for /Volumes/ path if plist parse failed
if [ -z "$MOUNT_POINT" ]; then
    log "Primary mount point extraction failed, trying fallback..."
    MOUNT_POINT=$(echo "$MOUNT_OUTPUT" | grep -o '/Volumes/[^<"]*' | head -1 | tr -d '[:space:]')
fi

if [ -z "$MOUNT_POINT" ]; then
    log "ERROR: Could not extract mount point from hdiutil output"
    log "hdiutil output: $MOUNT_OUTPUT"
    exit 1
fi

log "Mount point: $MOUNT_POINT"

# Verify mount point exists
if [ ! -d "$MOUNT_POINT" ]; then
    log "ERROR: Mount point does not exist: $MOUNT_POINT"
    exit 1
fi

# Find the .app bundle inside the mounted volume
log "Searching for .app bundle in $MOUNT_POINT..."
APP_IN_DMG=$(find "$MOUNT_POINT" -maxdepth 1 -name "*.app" | head -1)

if [ -z "$APP_IN_DMG" ]; then
    log "ERROR: No .app bundle found in DMG"
    log "DMG contents:"
    ls -la "$MOUNT_POINT" | tee -a "$LOG_FILE"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    exit 1
fi

log "Found app bundle: $APP_IN_DMG"

# Verify target bundle exists and is writable
if [ ! -d "$APP_BUNDLE" ]; then
    log "ERROR: Target app bundle does not exist: $APP_BUNDLE"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    exit 1
fi

if [ ! -w "$APP_BUNDLE" ]; then
    log "ERROR: Target app bundle is not writable: $APP_BUNDLE"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    exit 1
fi

# Replace the current installation with the new bundle
log "Replacing app bundle with rsync..."
if ! rsync -a --delete "$APP_IN_DMG/" "$APP_BUNDLE/" 2>&1 | tee -a "$LOG_FILE"; then
    log "ERROR: rsync failed"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    exit 1
fi

log "App bundle replaced successfully"

# Unmount
log "Unmounting DMG..."
if ! hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE"; then
    log "WARNING: Failed to unmount DMG (continuing anyway)"
fi

# Relaunch the updated app
log "Relaunching app..."
if ! open "$APP_BUNDLE" 2>&1 | tee -a "$LOG_FILE"; then
    log "ERROR: Failed to relaunch app"
    exit 1
fi

log "=== Update Installation Completed Successfully ==="
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

    // --- parse_checksums tests ---

    #[test]
    fn test_parse_checksums_standard_format() {
        // Each SHA-256 hash must be exactly 64 hex characters
        let hash1 = "a".repeat(64);
        let hash2 = "b".repeat(64);
        let text = format!(
            "{}  chatty-linux-x86_64.AppImage\n{}  chatty-macos-aarch64.dmg",
            hash1, hash2
        );
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 2);
        assert!(checksums.contains_key("chatty-linux-x86_64.AppImage"));
        assert!(checksums.contains_key("chatty-macos-aarch64.dmg"));
    }

    #[test]
    fn test_parse_checksums_single_space_separator() {
        let hash = "a".repeat(64);
        let text = format!("{} chatty-linux-x86_64.AppImage", hash);
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 1);
        assert!(checksums.contains_key("chatty-linux-x86_64.AppImage"));
    }

    #[test]
    fn test_parse_checksums_colon_format() {
        let hash = "a".repeat(64);
        let text = format!("chatty-linux-x86_64.AppImage: {}", hash);
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 1);
        assert!(checksums.contains_key("chatty-linux-x86_64.AppImage"));
    }

    #[test]
    fn test_parse_checksums_skips_empty_lines_and_comments() {
        let hash = "a".repeat(64);
        let text = format!(
            "# This is a comment\n\n{}  file.tar.gz\n\n# Another comment",
            hash
        );
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 1);
        assert!(checksums.contains_key("file.tar.gz"));
    }

    #[test]
    fn test_parse_checksums_empty_input() {
        let checksums = AutoUpdater::parse_checksums("");
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_invalid_hash_length() {
        // Hash too short (not 64 hex chars)
        let text = "abc123  file.tar.gz";
        let checksums = AutoUpdater::parse_checksums(text);
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_non_hex_chars() {
        // 64 chars but contains non-hex characters
        let text = "zzzz567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef  file.tar.gz";
        let checksums = AutoUpdater::parse_checksums(text);
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_normalizes_to_lowercase() {
        let text = "ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12  file.bin";
        let checksums = AutoUpdater::parse_checksums(text);

        assert_eq!(checksums.len(), 1);
        let hash = checksums.get("file.bin").unwrap();
        assert_eq!(
            hash, "abcdef12abcdef12abcdef12abcdef12abcdef12abcdef12abcdef12abcdef12",
            "hash should be lowercased"
        );
    }

    #[test]
    fn test_parse_checksums_only_comments() {
        let text = "# comment 1\n# comment 2\n# comment 3";
        let checksums = AutoUpdater::parse_checksums(text);
        assert!(checksums.is_empty());
    }

    // --- verify_checksum tests ---

    #[tokio::test]
    async fn test_verify_checksum_matching() {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.bin");
        let content = b"hello world";
        tokio::fs::write(&file_path, content).await.unwrap();

        // Compute the expected SHA-256
        let expected = format!("{:x}", Sha256::digest(content));

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), &expected).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "checksum should match");
    }

    #[tokio::test]
    async fn test_verify_checksum_mismatch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.bin");
        tokio::fs::write(&file_path, b"hello world").await.unwrap();

        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), wrong_hash).await;
        assert!(result.is_ok());
        assert!(!result.unwrap(), "checksum should not match");
    }

    #[tokio::test]
    async fn test_verify_checksum_case_insensitive() {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.bin");
        let content = b"test data";
        tokio::fs::write(&file_path, content).await.unwrap();

        let expected = format!("{:x}", Sha256::digest(content)).to_uppercase();

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), &expected).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "checksum should match case-insensitively");
    }

    #[tokio::test]
    async fn test_verify_checksum_nonexistent_file() {
        let path = PathBuf::from("/tmp/does_not_exist_at_all_12345.bin");
        let hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = AutoUpdater::verify_checksum(&path, hash).await;
        assert!(result.is_err(), "should error for nonexistent file");
    }

    #[tokio::test]
    async fn test_verify_checksum_empty_file() {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("empty.bin");
        tokio::fs::write(&file_path, b"").await.unwrap();

        let expected = format!("{:x}", Sha256::digest(b""));

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), &expected).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "checksum of empty file should match");
    }

    // --- asset matching edge cases ---

    #[test]
    fn test_asset_matching_all_platforms() {
        let assets = vec![
            GitHubAsset {
                name: "chatty-macos-aarch64.dmg".to_string(),
                browser_download_url: "https://example.com/1".to_string(),
            },
            GitHubAsset {
                name: "chatty-macos-x86_64.dmg".to_string(),
                browser_download_url: "https://example.com/2".to_string(),
            },
            GitHubAsset {
                name: "chatty-linux-x86_64.AppImage".to_string(),
                browser_download_url: "https://example.com/3".to_string(),
            },
            GitHubAsset {
                name: "chatty-linux-aarch64.AppImage".to_string(),
                browser_download_url: "https://example.com/4".to_string(),
            },
            GitHubAsset {
                name: "chatty-windows-x86_64.exe".to_string(),
                browser_download_url: "https://example.com/5".to_string(),
            },
        ];

        assert_eq!(
            find_matching_asset(&assets, "macos", "aarch64")
                .unwrap()
                .name,
            "chatty-macos-aarch64.dmg"
        );
        assert_eq!(
            find_matching_asset(&assets, "macos", "x86_64")
                .unwrap()
                .name,
            "chatty-macos-x86_64.dmg"
        );
        assert_eq!(
            find_matching_asset(&assets, "linux", "x86_64")
                .unwrap()
                .name,
            "chatty-linux-x86_64.AppImage"
        );
        assert_eq!(
            find_matching_asset(&assets, "linux", "aarch64")
                .unwrap()
                .name,
            "chatty-linux-aarch64.AppImage"
        );
        assert_eq!(
            find_matching_asset(&assets, "windows", "x86_64")
                .unwrap()
                .name,
            "chatty-windows-x86_64.exe"
        );
    }

    #[test]
    fn test_asset_matching_unsupported_platform() {
        let assets = vec![GitHubAsset {
            name: "chatty-linux-x86_64.AppImage".to_string(),
            browser_download_url: "https://example.com/1".to_string(),
        }];

        assert!(find_matching_asset(&assets, "freebsd", "x86_64").is_none());
    }

    #[test]
    fn test_asset_matching_empty_assets() {
        assert!(find_matching_asset(&[], "linux", "x86_64").is_none());
    }

    // --- dismiss_error test ---

    #[test]
    fn test_dismiss_error() {
        let mut updater = AutoUpdater::new("1.0.0");
        updater.status = AutoUpdateStatus::Error("something went wrong".to_string());

        updater.dismiss_error();
        assert_eq!(*updater.status(), AutoUpdateStatus::Idle);
    }

    #[test]
    fn test_dismiss_error_noop_when_not_error() {
        let mut updater = AutoUpdater::new("1.0.0");
        updater.status = AutoUpdateStatus::Checking;

        updater.dismiss_error();
        assert_eq!(*updater.status(), AutoUpdateStatus::Checking);
    }

    #[test]
    fn test_auto_updater_invalid_version_fallback() {
        let updater = AutoUpdater::new("not-a-version");
        assert_eq!(updater.current_version().to_string(), "0.0.0");
        assert!(matches!(updater.status(), AutoUpdateStatus::Idle));
    }
}
