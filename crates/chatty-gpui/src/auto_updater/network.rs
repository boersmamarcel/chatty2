//! GitHub release-feed network glue for the auto-updater.
//!
//! `fetch_latest_release` hits the GitHub Releases API and selects a
//! per-OS/arch asset. Pure I/O + parsing — no UI, no install side effects.

use super::*;

pub(super) async fn fetch_latest_release(
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
pub(super) fn find_matching_asset(
    assets: &[GitHubAsset],
    os: &str,
    arch: &str,
) -> Option<GitHubAsset> {
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
