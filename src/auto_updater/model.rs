//! Auto-updater model and state machine
//!
//! This module contains the core AutoUpdater struct that implements a GPUI Global model
//! with a state machine for managing the update lifecycle.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AsyncApp, BorrowAppContext, Global};
use semver::Version;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use super::installer::{InstallError, install_release};
use super::release_source::{GitHubReleaseSource, ReleaseAsset, ReleaseSource};

/// Default polling interval for checking updates (1 hour)
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Minimum polling interval to prevent abuse (5 minutes)
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Represents the current state of the auto-updater
#[derive(Clone, Debug, PartialEq)]
pub enum AutoUpdateStatus {
    /// Waiting for the next poll interval
    Idle,
    /// Currently fetching release metadata
    Checking,
    /// Streaming the binary (holds progress percentage 0.0-1.0)
    Downloading(f32),
    /// Mounting/extracting the update
    Installing,
    /// Update ready, waiting for restart
    Updated {
        /// The version that was downloaded
        version: String,
        /// Path to the downloaded update file
        update_path: PathBuf,
    },
    /// Something went wrong
    Errored(String),
}

impl Default for AutoUpdateStatus {
    fn default() -> Self {
        Self::Idle
    }
}

/// Update channel for release selection
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateChannel {
    /// Stable release channel (default)
    #[default]
    Stable,
    /// Preview/beta releases
    Preview,
    /// Nightly/dev releases
    Nightly,
}

impl UpdateChannel {
    /// Returns the GitHub release tag prefix for this channel
    pub fn tag_prefix(&self) -> &'static str {
        match self {
            Self::Stable => "v",
            Self::Preview => "preview-",
            Self::Nightly => "nightly-",
        }
    }

    /// Returns whether pre-releases should be included
    pub fn include_prerelease(&self) -> bool {
        matches!(self, Self::Preview | Self::Nightly)
    }
}

/// Configuration for the auto-updater
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutoUpdaterConfig {
    /// Whether auto-update is enabled
    pub enabled: bool,
    /// Polling interval for checking updates
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    /// Update channel to track
    pub channel: UpdateChannel,
    /// GitHub repository owner
    pub github_owner: String,
    /// GitHub repository name
    pub github_repo: String,
    /// Whether to automatically download updates
    pub auto_download: bool,
    /// Whether to automatically install updates on quit
    pub auto_install_on_quit: bool,
}

impl Default for AutoUpdaterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: DEFAULT_POLL_INTERVAL,
            channel: UpdateChannel::default(),
            github_owner: "boersmamarcel".to_string(),
            github_repo: "chatty2".to_string(),
            auto_download: true,
            auto_install_on_quit: false,
        }
    }
}

/// The main auto-updater model
///
/// This struct manages the update lifecycle including:
/// - Polling for new releases
/// - Downloading updates
/// - Installing updates based on OS
/// - Managing the update state machine
#[derive(Clone)]
pub struct AutoUpdater {
    /// Current status of the updater
    status: AutoUpdateStatus,
    /// Current application version
    current_version: Version,
    /// Configuration for the updater
    config: AutoUpdaterConfig,
    /// The release source for fetching metadata
    release_source: Arc<dyn ReleaseSource>,
    /// Flag indicating if a restart is required to complete the update
    pub should_restart_on_quit: bool,
    /// Path to pending update (used on Windows for post-quit installation)
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

        let config = AutoUpdaterConfig::default();
        let release_source = Arc::new(GitHubReleaseSource::new(
            &config.github_owner,
            &config.github_repo,
        ));

        Self {
            status: AutoUpdateStatus::Idle,
            current_version: version,
            config,
            release_source,
            should_restart_on_quit: false,
            pending_update_path: None,
        }
    }

    /// Create an AutoUpdater with custom configuration
    pub fn with_config(current_version: &str, config: AutoUpdaterConfig) -> Self {
        let version = Version::parse(current_version).unwrap_or_else(|e| {
            warn!(error = ?e, version = current_version, "Failed to parse current version, using 0.0.0");
            Version::new(0, 0, 0)
        });

        let release_source = Arc::new(GitHubReleaseSource::new(
            &config.github_owner,
            &config.github_repo,
        ));

        Self {
            status: AutoUpdateStatus::Idle,
            current_version: version,
            config,
            release_source,
            should_restart_on_quit: false,
            pending_update_path: None,
        }
    }

    /// Set a custom release source (useful for testing or custom backends)
    pub fn set_release_source(&mut self, source: Arc<dyn ReleaseSource>) {
        self.release_source = source;
    }

    /// Get the current update status
    pub fn status(&self) -> &AutoUpdateStatus {
        &self.status
    }

    /// Get the current version
    pub fn current_version(&self) -> &Version {
        &self.current_version
    }

    /// Get the configuration
    pub fn config(&self) -> &AutoUpdaterConfig {
        &self.config
    }

    /// Update the configuration
    pub fn set_config(&mut self, config: AutoUpdaterConfig) {
        self.config = config;
        // Update release source if repo changed
        self.release_source = Arc::new(GitHubReleaseSource::new(
            &self.config.github_owner,
            &self.config.github_repo,
        ));
    }

    /// Dismiss any error state and return to idle
    pub fn dismiss_error(&mut self) {
        if matches!(self.status, AutoUpdateStatus::Errored(_)) {
            self.status = AutoUpdateStatus::Idle;
        }
    }

    /// Reset the status to idle
    pub fn reset(&mut self) {
        self.status = AutoUpdateStatus::Idle;
    }

    /// Start the polling loop for checking updates
    ///
    /// This spawns a background task that checks for updates at the configured interval.
    pub fn start_polling(&self, cx: &mut App) {
        if !self.config.enabled {
            info!("Auto-updater is disabled, not starting polling loop");
            return;
        }

        let poll_interval = self.config.poll_interval.max(MIN_POLL_INTERVAL);
        info!(
            interval_secs = poll_interval.as_secs(),
            "Starting auto-update polling loop"
        );

        // Perform an initial check immediately
        self.check_for_update(cx);

        // Start the polling loop
        cx.spawn(async move |cx: &mut AsyncApp| {
            loop {
                // Wait for the poll interval
                tokio::time::sleep(poll_interval).await;

                // Check if we should continue polling
                let should_continue = cx
                    .update(|cx| {
                        if let Some(updater) = cx.try_global::<AutoUpdater>() {
                            updater.config.enabled
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);

                if !should_continue {
                    debug!("Auto-updater disabled, stopping polling loop");
                    break;
                }

                // Trigger update check
                cx.update(|cx| {
                    if let Some(updater) = cx.try_global::<AutoUpdater>() {
                        // Only check if we're idle
                        if matches!(updater.status, AutoUpdateStatus::Idle) {
                            // Clone to trigger the check
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
        // Update status to checking
        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
            updater.status = AutoUpdateStatus::Checking;
        });

        let release_source = self.release_source.clone();
        let channel = self.config.channel.clone();
        let current_version = self.current_version.clone();
        let auto_download = self.config.auto_download;

        cx.spawn(async move |cx: &mut AsyncApp| {
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;

            info!(
                channel = ?channel,
                os = os,
                arch = arch,
                "Checking for updates"
            );

            match release_source.get_latest_release(&channel, os, arch).await {
                Ok(Some(asset)) => {
                    // Parse remote version
                    let remote_version = match Version::parse(&asset.version) {
                        Ok(v) => v,
                        Err(e) => {
                            error!(error = ?e, version = &asset.version, "Failed to parse remote version");
                            cx.update(|cx| {
                                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                    updater.status = AutoUpdateStatus::Errored(format!(
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
                        info!(
                            current = %current_version,
                            remote = %remote_version,
                            "New version available"
                        );

                        if auto_download {
                            // Start downloading
                            Self::download_update_async(asset, cx).await;
                        } else {
                            // Just notify about the update
                            cx.update(|cx| {
                                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                    updater.status = AutoUpdateStatus::Idle;
                                });
                            })
                            .ok();
                        }
                    } else {
                        debug!(
                            current = %current_version,
                            remote = %remote_version,
                            "Already up to date"
                        );
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
                            updater.status =
                                AutoUpdateStatus::Errored(format!("Update check failed: {}", e));
                        });
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Download an update asynchronously
    async fn download_update_async(asset: ReleaseAsset, cx: &mut AsyncApp) {
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
                            AutoUpdateStatus::Errored(format!("Failed to create temp dir: {}", e));
                    });
                })
                .ok();
                return;
            }
        };

        let file_name = asset
            .download_url
            .split('/')
            .last()
            .unwrap_or("update_download");
        let download_path = temp_dir.path().join(file_name);

        // Download with progress tracking
        match Self::download_file(&asset.download_url, &download_path, cx).await {
            Ok(()) => {
                info!(path = ?download_path, "Download complete");

                // Verify checksum if available
                if let Some(ref expected_hash) = asset.sha256 {
                    info!(
                        expected_hash = expected_hash,
                        "Verifying download integrity"
                    );

                    match Self::verify_checksum(&download_path, expected_hash).await {
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
                                    updater.status = AutoUpdateStatus::Errored(
                                        "Security check failed: Download integrity verification failed. \
                                         The downloaded file does not match the expected checksum."
                                            .to_string(),
                                    );
                                });
                            })
                            .ok();
                            return;
                        }
                        Err(e) => {
                            error!(error = ?e, "Failed to verify checksum");
                            // Delete the file to be safe
                            let _ = tokio::fs::remove_file(&download_path).await;

                            cx.update(|cx| {
                                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                    updater.status = AutoUpdateStatus::Errored(format!(
                                        "Checksum verification error: {}",
                                        e
                                    ));
                                });
                            })
                            .ok();
                            return;
                        }
                    }
                } else {
                    warn!(
                        "No checksum available for verification - proceeding without integrity check. \
                         This is insecure and should be fixed in the release process."
                    );
                }

                // Keep the temp directory alive by storing the path
                let final_path = download_path.clone();

                // Persist the temp dir so it doesn't get cleaned up
                let _ = temp_dir.keep();

                cx.update(|cx| {
                    cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                        updater.status = AutoUpdateStatus::Updated {
                            version: asset.version.clone(),
                            update_path: final_path,
                        };
                    });
                })
                .ok();
            }
            Err(e) => {
                error!(error = ?e, "Download failed");
                cx.update(|cx| {
                    cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                        updater.status =
                            AutoUpdateStatus::Errored(format!("Download failed: {}", e));
                    });
                })
                .ok();
            }
        }
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
        let mut buffer = vec![0u8; 8192]; // 8KB buffer

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

    /// Download a file with progress tracking
    async fn download_file(
        url: &str,
        path: &PathBuf,
        cx: &mut AsyncApp,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

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

    /// Install the downloaded update and restart the application
    ///
    /// This will install the update using the OS-specific method and then
    /// optionally restart the application.
    pub fn install_and_restart(&mut self, cx: &mut App) {
        let update_path = match &self.status {
            AutoUpdateStatus::Updated { update_path, .. } => update_path.clone(),
            _ => {
                warn!("Cannot install: no update downloaded");
                return;
            }
        };

        self.status = AutoUpdateStatus::Installing;

        let auto_install_on_quit = self.config.auto_install_on_quit;

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

                    // On macOS and Linux, we can restart immediately
                    #[cfg(not(target_os = "windows"))]
                    if !auto_install_on_quit {
                        Self::restart_application();
                    }

                    // On Windows, we'll restart via the helper script
                    #[cfg(target_os = "windows")]
                    {
                        if !auto_install_on_quit {
                            // Request application quit - the finalize_update will handle restart
                            cx.update(|cx| {
                                cx.quit();
                            })
                            .ok();
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Installation failed");
                    cx.update(|cx| {
                        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                            updater.status =
                                AutoUpdateStatus::Errored(format!("Installation failed: {}", e));
                        });
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Restart the application
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

        // Spawn the new process
        if let Err(e) = Command::new(&current_exe).spawn() {
            error!(error = ?e, "Failed to restart application");
        }

        // Exit the current process
        std::process::exit(0);
    }

    /// Finalize pending updates on application quit (Windows only)
    ///
    /// This should be called when the application is about to quit on Windows
    /// to complete the update process.
    #[cfg(target_os = "windows")]
    pub fn finalize_update(&self) -> Result<(), InstallError> {
        use super::installer::finalize_windows_update;

        if self.should_restart_on_quit {
            if let Some(ref update_path) = self.pending_update_path {
                return finalize_windows_update(update_path);
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

/// Module for humantime serialization of Duration
mod humantime_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_version_comparison() {
        let current = Version::parse("0.1.0").unwrap();
        let newer = Version::parse("0.2.0").unwrap();
        let older = Version::parse("0.0.9").unwrap();

        assert!(newer > current);
        assert!(older < current);
    }

    #[test]
    fn test_update_channel_defaults() {
        let channel = UpdateChannel::default();
        assert_eq!(channel, UpdateChannel::Stable);
        assert_eq!(channel.tag_prefix(), "v");
        assert!(!channel.include_prerelease());
    }

    #[test]
    fn test_auto_updater_creation() {
        let updater = AutoUpdater::new("0.1.0");
        assert_eq!(updater.current_version().to_string(), "0.1.0");
        assert!(matches!(updater.status(), AutoUpdateStatus::Idle));
    }

    #[tokio::test]
    async fn test_verify_checksum_valid() {
        // Create a temporary file with known content
        let mut temp_file = tempfile::NamedTempFile::new().unwrap();
        temp_file.write_all(b"Hello, World!").unwrap();
        temp_file.flush().unwrap();

        let path = temp_file.path().to_path_buf();

        // Expected SHA-256 hash of "Hello, World!"
        let expected_hash = "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f";

        let result = AutoUpdater::verify_checksum(&path, expected_hash)
            .await
            .unwrap();
        assert!(result, "Checksum should match");
    }

    #[tokio::test]
    async fn test_verify_checksum_invalid() {
        // Create a temporary file with known content
        let mut temp_file = tempfile::NamedTempFile::new().unwrap();
        temp_file.write_all(b"Hello, World!").unwrap();
        temp_file.flush().unwrap();

        let path = temp_file.path().to_path_buf();

        // Wrong hash
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = AutoUpdater::verify_checksum(&path, wrong_hash)
            .await
            .unwrap();
        assert!(!result, "Checksum should not match");
    }

    #[tokio::test]
    async fn test_verify_checksum_case_insensitive() {
        // Create a temporary file with known content
        let mut temp_file = tempfile::NamedTempFile::new().unwrap();
        temp_file.write_all(b"Test").unwrap();
        temp_file.flush().unwrap();

        let path = temp_file.path().to_path_buf();

        // SHA-256 hash of "Test"
        let hash_lowercase = "532eaabd9574880dbf76b9b8cc00832c20a6ec113d682299550d7a6e0f345e25";
        let hash_uppercase = "532EAABD9574880DBF76B9B8CC00832C20A6EC113D682299550D7A6E0F345E25";

        let result_lower = AutoUpdater::verify_checksum(&path, hash_lowercase)
            .await
            .unwrap();
        let result_upper = AutoUpdater::verify_checksum(&path, hash_uppercase)
            .await
            .unwrap();

        assert!(result_lower, "Lowercase hash should match");
        assert!(result_upper, "Uppercase hash should match");
    }
}
