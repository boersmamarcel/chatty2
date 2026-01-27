//! Release source abstraction for fetching release metadata
//!
//! This module provides a trait-based abstraction for fetching release information
//! from various sources (GitHub, custom APIs, etc.).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, error, info};

use super::UpdateChannel;

/// Information about a release asset available for download
#[derive(Clone, Debug)]
pub struct ReleaseAsset {
    /// Version string (semver format, e.g., "0.2.0")
    pub version: String,
    /// URL to download the asset
    pub download_url: String,
    /// File name of the asset
    pub name: String,
    /// File size in bytes (if known)
    pub size: Option<u64>,
    /// Content type of the asset
    pub content_type: Option<String>,
}

/// Error type for release source operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum ReleaseSourceError {
    #[error("HTTP request failed: {0}")]
    HttpError(String),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("No matching release found")]
    NotFound,

    #[error("Rate limited by API")]
    RateLimited,

    #[error("Other error: {0}")]
    Other(String),
}

impl From<reqwest::Error> for ReleaseSourceError {
    fn from(e: reqwest::Error) -> Self {
        ReleaseSourceError::HttpError(e.to_string())
    }
}

/// Type alias for the boxed future returned by ReleaseSource
pub type ReleaseSourceFuture =
    Pin<Box<dyn Future<Output = Result<Option<ReleaseAsset>, ReleaseSourceError>> + Send>>;

/// Trait for abstracting release metadata fetching
///
/// Implement this trait to support different release sources like GitHub Releases,
/// custom update servers, or cloud-based update APIs.
///
/// This trait is object-safe and can be used with `Arc<dyn ReleaseSource>`.
pub trait ReleaseSource: Send + Sync {
    /// Fetch the latest release asset for the given channel, OS, and architecture
    ///
    /// # Arguments
    /// * `channel` - The update channel (stable, preview, nightly)
    /// * `os` - Operating system (linux, macos, windows)
    /// * `arch` - CPU architecture (x86_64, aarch64)
    ///
    /// # Returns
    /// * `Ok(Some(asset))` - A matching release was found
    /// * `Ok(None)` - No matching release exists for this platform
    /// * `Err(e)` - An error occurred while fetching
    fn get_latest_release(
        &self,
        channel: &UpdateChannel,
        os: &str,
        arch: &str,
    ) -> ReleaseSourceFuture;
}

/// GitHub Releases API response structures
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    content_type: String,
}

/// Release source implementation for GitHub Releases API
#[derive(Clone)]
pub struct GitHubReleaseSource {
    /// Repository owner
    owner: String,
    /// Repository name
    repo: String,
    /// HTTP client
    client: reqwest::Client,
}

impl GitHubReleaseSource {
    /// Create a new GitHub release source
    pub fn new(owner: &str, repo: &str) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("chatty-auto-updater/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            client,
        }
    }

    /// Parse version from a GitHub tag name
    fn parse_version(tag: &str, channel: &UpdateChannel) -> Option<String> {
        let prefix = channel.tag_prefix();
        if tag.starts_with(prefix) {
            Some(tag[prefix.len()..].to_string())
        } else if tag.starts_with('v') {
            // Fallback: try stripping 'v' prefix
            Some(tag[1..].to_string())
        } else {
            Some(tag.to_string())
        }
    }

    /// Get the expected asset name pattern for the given OS and architecture
    fn get_asset_pattern(os: &str, arch: &str) -> Vec<String> {
        match (os, arch) {
            ("macos", "aarch64") => vec![
                "chatty-macos-arm64.dmg".to_string(),
                "chatty-darwin-arm64.dmg".to_string(),
                "chatty-macos-aarch64.dmg".to_string(),
                "Chatty-aarch64.dmg".to_string(),
            ],
            ("macos", "x86_64") => vec![
                "chatty-macos-x86_64.dmg".to_string(),
                "chatty-darwin-x86_64.dmg".to_string(),
                "chatty-macos-intel.dmg".to_string(),
                "Chatty-x86_64.dmg".to_string(),
            ],
            ("linux", "x86_64") => vec![
                "chatty-linux-x86_64.tar.gz".to_string(),
                "chatty-linux-amd64.tar.gz".to_string(),
                "Chatty-x86_64.tar.gz".to_string(),
            ],
            ("linux", "aarch64") => vec![
                "chatty-linux-aarch64.tar.gz".to_string(),
                "chatty-linux-arm64.tar.gz".to_string(),
                "Chatty-aarch64.tar.gz".to_string(),
            ],
            ("windows", "x86_64") => vec![
                "chatty-windows-x86_64.exe".to_string(),
                "chatty-windows-setup.exe".to_string(),
                "chatty-installer.exe".to_string(),
                "Chatty-x86_64.exe".to_string(),
            ],
            _ => vec![],
        }
    }

    /// Find a matching asset from the release
    fn find_matching_asset(
        release: &GitHubRelease,
        os: &str,
        arch: &str,
        channel: &UpdateChannel,
    ) -> Option<ReleaseAsset> {
        let patterns = Self::get_asset_pattern(os, arch);
        if patterns.is_empty() {
            debug!(os = os, arch = arch, "No asset pattern for platform");
            return None;
        }

        // Try each pattern in order of preference
        for pattern in &patterns {
            let pattern_lower = pattern.to_lowercase();
            for asset in &release.assets {
                if asset.name.to_lowercase().contains(&pattern_lower)
                    || asset.name.to_lowercase() == pattern_lower
                {
                    let version = Self::parse_version(&release.tag_name, channel)?;
                    return Some(ReleaseAsset {
                        version,
                        download_url: asset.browser_download_url.clone(),
                        name: asset.name.clone(),
                        size: Some(asset.size),
                        content_type: Some(asset.content_type.clone()),
                    });
                }
            }
        }

        // Fallback: try to find any asset that matches the OS
        let os_patterns: Vec<&str> = match os {
            "macos" => vec!["darwin", "macos", "mac", "osx"],
            "linux" => vec!["linux"],
            "windows" => vec!["windows", "win", ".exe"],
            _ => vec![],
        };

        let arch_patterns: Vec<&str> = match arch {
            "x86_64" => vec!["x86_64", "amd64", "x64", "intel"],
            "aarch64" => vec!["aarch64", "arm64"],
            _ => vec![],
        };

        for asset in &release.assets {
            let name_lower = asset.name.to_lowercase();

            let os_match = os_patterns.iter().any(|p| name_lower.contains(p));
            let arch_match =
                arch_patterns.is_empty() || arch_patterns.iter().any(|p| name_lower.contains(p));

            // Check for correct extension
            let ext_match = match os {
                "macos" => name_lower.ends_with(".dmg"),
                "linux" => name_lower.ends_with(".tar.gz") || name_lower.ends_with(".tgz"),
                "windows" => name_lower.ends_with(".exe"),
                _ => false,
            };

            if os_match && arch_match && ext_match {
                let version = Self::parse_version(&release.tag_name, channel)?;
                return Some(ReleaseAsset {
                    version,
                    download_url: asset.browser_download_url.clone(),
                    name: asset.name.clone(),
                    size: Some(asset.size),
                    content_type: Some(asset.content_type.clone()),
                });
            }
        }

        None
    }

    /// Internal async implementation
    async fn fetch_latest_release(
        &self,
        channel: &UpdateChannel,
        os: &str,
        arch: &str,
    ) -> Result<Option<ReleaseAsset>, ReleaseSourceError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases",
            self.owner, self.repo
        );

        debug!(url = &url, "Fetching releases from GitHub API");

        let response = self.client.get(&url).send().await?;

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            // Check for rate limiting
            if response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .map(|s| s == "0")
                .unwrap_or(false)
            {
                return Err(ReleaseSourceError::RateLimited);
            }
        }

        if !response.status().is_success() {
            return Err(ReleaseSourceError::Other(format!(
                "API returned status: {}",
                response.status()
            )));
        }

        let releases: Vec<GitHubRelease> = response.json().await.map_err(|e| {
            error!(error = ?e, "Failed to parse GitHub releases response");
            ReleaseSourceError::ParseError(e.to_string())
        })?;

        debug!(count = releases.len(), "Fetched releases from GitHub");

        // Filter releases by channel
        let include_prerelease = channel.include_prerelease();
        let tag_prefix = channel.tag_prefix();

        for release in releases {
            // Skip drafts
            if release.draft {
                continue;
            }

            // Skip prereleases if we're on stable channel
            if release.prerelease && !include_prerelease {
                continue;
            }

            // Check if tag matches channel prefix
            if !release.tag_name.starts_with(tag_prefix) && !release.tag_name.starts_with('v') {
                continue;
            }

            // Try to find a matching asset for this platform
            if let Some(asset) = Self::find_matching_asset(&release, os, arch, channel) {
                info!(
                    version = &asset.version,
                    name = &asset.name,
                    "Found matching release asset"
                );
                return Ok(Some(asset));
            }
        }

        debug!("No matching release found for platform");
        Ok(None)
    }
}

impl ReleaseSource for GitHubReleaseSource {
    fn get_latest_release(
        &self,
        channel: &UpdateChannel,
        os: &str,
        arch: &str,
    ) -> ReleaseSourceFuture {
        let this = self.clone();
        let channel = channel.clone();
        let os = os.to_string();
        let arch = arch.to_string();

        Box::pin(async move { this.fetch_latest_release(&channel, &os, &arch).await })
    }
}

/// Implementation for Arc<dyn ReleaseSource>
impl ReleaseSource for Arc<dyn ReleaseSource> {
    fn get_latest_release(
        &self,
        channel: &UpdateChannel,
        os: &str,
        arch: &str,
    ) -> ReleaseSourceFuture {
        (**self).get_latest_release(channel, os, arch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock release source for testing
    pub struct MockReleaseSource {
        pub releases: Vec<ReleaseAsset>,
    }

    impl ReleaseSource for MockReleaseSource {
        fn get_latest_release(
            &self,
            _channel: &UpdateChannel,
            _os: &str,
            _arch: &str,
        ) -> ReleaseSourceFuture {
            let releases = self.releases.clone();
            Box::pin(async move { Ok(releases.first().cloned()) })
        }
    }

    #[test]
    fn test_parse_version_with_v_prefix() {
        let version = GitHubReleaseSource::parse_version("v0.1.0", &UpdateChannel::Stable);
        assert_eq!(version, Some("0.1.0".to_string()));
    }

    #[test]
    fn test_parse_version_preview() {
        let version = GitHubReleaseSource::parse_version("preview-0.2.0", &UpdateChannel::Preview);
        assert_eq!(version, Some("0.2.0".to_string()));
    }

    #[test]
    fn test_asset_patterns() {
        let patterns = GitHubReleaseSource::get_asset_pattern("macos", "aarch64");
        assert!(!patterns.is_empty());
        assert!(patterns
            .iter()
            .any(|p| p.contains("arm64") || p.contains("aarch64")));

        let patterns = GitHubReleaseSource::get_asset_pattern("linux", "x86_64");
        assert!(patterns.iter().any(|p| p.ends_with(".tar.gz")));

        let patterns = GitHubReleaseSource::get_asset_pattern("windows", "x86_64");
        assert!(patterns.iter().any(|p| p.ends_with(".exe")));
    }
}
