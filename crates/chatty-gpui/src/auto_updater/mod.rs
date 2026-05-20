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

        // Auto-updater needs custom UA + optional GitHub auth headers, so it
        // builds its own client rather than using the centralised factory.
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

        let hash = hex::encode(hasher.finalize());
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
                            _cx.update_global::<AutoUpdater, _>(|updater, _cx| {
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

    /// Install the downloaded update without relaunching the application.
    ///
    /// Used for mandatory updates when the user quits the app — the update
    /// is applied silently so the next manual launch runs the new version.
    ///
    /// On macOS: launches the install helper script with relaunch disabled.
    /// On Linux: atomically replaces the AppImage without spawning a new process.
    /// On Windows: launches the silent installer with /NORESTART flag.
    pub fn install_on_quit(&mut self, cx: &mut App) {
        let update_path = match &self.status {
            AutoUpdateStatus::Ready(_, update_path) => update_path.clone(),
            _ => {
                warn!("Cannot install on quit: no update downloaded");
                return;
            }
        };

        info!("Installing pending update before quit (no relaunch)");

        self.status = AutoUpdateStatus::Installing;

        #[cfg(target_os = "macos")]
        {
            let app_bundle = std::env::current_exe().ok().and_then(|exe| {
                exe.ancestors()
                    .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
                    .map(|p| p.to_path_buf())
            });

            if let Some(bundle) = app_bundle {
                launch_macos_install_helper(&update_path, &bundle, false);
            } else {
                warn!(
                    "Could not find app bundle for install-on-quit; update will apply on next manual install"
                );
            }
            cx.quit();
        }

        #[cfg(not(target_os = "macos"))]
        cx.spawn(async move |cx: &mut AsyncApp| {
            match install_release(&update_path, false).await {
                Ok(_) => {
                    info!("Update installed on quit — new version will be active on next launch");
                    // On Windows, the installer was launched with /NORESTART so it
                    // won't relaunch. On Linux, the AppImage is already replaced.
                    cx.update(|cx| cx.quit())
                        .map_err(|e| warn!(error = ?e, "Failed to quit after install-on-quit"))
                        .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to install update on quit");
                    cx.update(|cx| cx.quit())
                        .map_err(
                            |e| warn!(error = ?e, "Failed to quit after failed install-on-quit"),
                        )
                        .ok();
                }
            }
        })
        .detach();
    }

    /// Install the downloaded update and restart the application.
    ///
    /// On macOS this uses a deferred approach for fast restart:
    /// 1. Set status to Installing for visual feedback
    /// 2. Write a helper shell script to /tmp that will mount the DMG,
    ///    rsync the .app bundle, clear quarantine, relaunch immediately,
    ///    then run housekeeping (codesign, lsregister) post-launch
    /// 3. Spawn the helper as a detached process (polls for app exit)
    /// 4. Call cx.quit() for a graceful GPUI shutdown
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
                    launch_macos_install_helper(&update_path, &bundle, true);
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
            match install_release(&update_path, true).await {
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
mod download;
mod network;
mod platform;

use download::download_update;
use network::{fetch_latest_release, find_matching_asset};
#[cfg(target_os = "macos")]
pub use platform::launch_macos_install_helper;
#[cfg(target_os = "linux")]
use platform::relaunch_linux_process;

#[cfg(test)]
mod tests;
