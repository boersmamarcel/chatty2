//! OS-specific installation logic for auto-updates
//!
//! Platform-specific strategies:
//! - macOS: Mount DMG, rsync .app bundle, unmount
//! - Linux: Extract tarball, rsync binary
//! - Windows: Launch installer with silent flags

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::info;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use tracing::{debug, warn};

/// Error type for installation operations
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Command failed: {0}")]
    CommandFailed(String),

    #[error("Mount failed: {0}")]
    MountFailed(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Invalid update file: {0}")]
    InvalidUpdateFile(String),

    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),
}

/// Install the release from the given path
///
/// Returns `Ok(true)` if a restart is needed to complete installation (Windows),
/// `Ok(false)` if installation is complete (macOS/Linux).
pub async fn install_release(update_path: &Path) -> Result<bool, InstallError> {
    #[cfg(target_os = "macos")]
    return install_release_macos(update_path).await;

    #[cfg(target_os = "linux")]
    return install_release_linux(update_path).await;

    #[cfg(target_os = "windows")]
    return install_release_windows(update_path).await;

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(InstallError::UnsupportedPlatform);
}

// =============================================================================
// macOS Implementation
// =============================================================================

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    /// RAII guard for DMG mounts - ensures unmount on drop
    pub struct DmgMount {
        mount_point: PathBuf,
    }

    impl DmgMount {
        /// Mount a DMG and return the mount point
        pub fn mount(dmg_path: &Path) -> Result<Self, InstallError> {
            if !dmg_path.exists() {
                return Err(InstallError::FileNotFound(dmg_path.display().to_string()));
            }

            info!(path = ?dmg_path, "Mounting DMG");

            let output = Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-plist"])
                .arg(dmg_path)
                .output()
                .map_err(|e| InstallError::CommandFailed(format!("hdiutil attach: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(InstallError::MountFailed(stderr.to_string()));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mount_point = Self::parse_mount_point(&stdout)?;

            info!(mount_point = ?mount_point, "DMG mounted successfully");

            Ok(Self { mount_point })
        }

        /// Parse the mount point from hdiutil plist output
        pub(crate) fn parse_mount_point(output: &str) -> Result<PathBuf, InstallError> {
            // Look for mount-point key in plist output
            let mut found_key = false;
            for line in output.lines() {
                let line = line.trim();
                if line.contains("mount-point") {
                    found_key = true;
                    continue;
                }
                if found_key && line.starts_with("<string>") && line.ends_with("</string>") {
                    let path = line
                        .trim_start_matches("<string>")
                        .trim_end_matches("</string>");
                    return Ok(PathBuf::from(path));
                }
            }

            // Fallback: find /Volumes/ path
            for line in output.lines() {
                if line.contains("/Volumes/") {
                    if let Some(start) = line.find("/Volumes/") {
                        let rest = &line[start..];
                        let end = rest.find('<').unwrap_or(rest.len());
                        let path = rest[..end].trim();
                        if !path.is_empty() {
                            return Ok(PathBuf::from(path));
                        }
                    }
                }
            }

            Err(InstallError::MountFailed(
                "Could not parse mount point".to_string(),
            ))
        }

        /// Find the .app bundle in the mounted DMG
        pub fn find_app_bundle(&self) -> Result<PathBuf, InstallError> {
            let entries = std::fs::read_dir(&self.mount_point)?;

            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "app").unwrap_or(false) {
                    return Ok(path);
                }
            }

            Err(InstallError::FileNotFound(
                "No .app bundle found in DMG".to_string(),
            ))
        }
    }

    impl Drop for DmgMount {
        fn drop(&mut self) {
            info!(mount_point = ?self.mount_point, "Unmounting DMG");

            let result = Command::new("hdiutil")
                .args(["detach", "-force"])
                .arg(&self.mount_point)
                .output();

            match result {
                Ok(output) if output.status.success() => {
                    debug!("DMG unmounted successfully");
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(error = %stderr, "Failed to unmount DMG");
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to run hdiutil detach");
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
async fn install_release_macos(dmg_path: &Path) -> Result<bool, InstallError> {
    use macos::DmgMount;

    if dmg_path.extension().map(|e| e != "dmg").unwrap_or(true) {
        return Err(InstallError::InvalidUpdateFile(
            "Expected .dmg file".to_string(),
        ));
    }

    // Mount the DMG (RAII guard ensures unmount)
    let mount = DmgMount::mount(dmg_path)?;

    // Find the .app bundle
    let app_bundle = mount.find_app_bundle()?;
    info!(app_bundle = ?app_bundle, "Found app bundle in DMG");

    // Determine destination (current .app bundle)
    let current_exe = std::env::current_exe()?;
    let dest_app = current_exe
        .ancestors()
        .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        .ok_or_else(|| {
            InstallError::FileNotFound("Could not find current app bundle".to_string())
        })?;

    info!(source = ?app_bundle, dest = ?dest_app, "Installing update with rsync");

    // Use rsync to copy the app
    let output = Command::new("rsync")
        .args(["-a", "--delete"])
        .arg(format!("{}/", app_bundle.display()))
        .arg(format!("{}/", dest_app.display()))
        .output()
        .map_err(|e| InstallError::CommandFailed(format!("rsync: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InstallError::CommandFailed(format!(
            "rsync failed: {}",
            stderr
        )));
    }

    info!("Update installed successfully");
    Ok(false) // No restart needed on macOS
}

// =============================================================================
// Linux Implementation
// =============================================================================

#[cfg(target_os = "linux")]
async fn install_release_linux(tarball_path: &Path) -> Result<bool, InstallError> {
    use flate2::read::GzDecoder;
    use std::fs::File;
    use tar::Archive;

    // Verify file extension
    let is_tarball = tarball_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".tar.gz") || n.ends_with(".tgz"))
        .unwrap_or(false);

    if !is_tarball {
        return Err(InstallError::InvalidUpdateFile(
            "Expected .tar.gz file".to_string(),
        ));
    }

    // Create temporary extraction directory
    let extract_dir = tempfile::tempdir()?;

    info!(tarball = ?tarball_path, extract_dir = ?extract_dir.path(), "Extracting update");

    // Extract the tarball
    let file = File::open(tarball_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(extract_dir.path())
        .map_err(|e| InstallError::ExtractionFailed(e.to_string()))?;

    // Find the binary
    let binary_name = "chatty";
    let extracted_binary = find_binary_in_dir(extract_dir.path(), binary_name)?;

    info!(binary = ?extracted_binary, "Found extracted binary");

    // Determine destination: try current location first
    let current_exe = std::env::current_exe()?;
    let dest_path = current_exe;

    info!(source = ?extracted_binary, dest = ?dest_path, "Installing binary");

    // Try using rsync if available
    let rsync_available = Command::new("which")
        .arg("rsync")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if rsync_available {
        let output = Command::new("rsync")
            .args(["-a"])
            .arg(&extracted_binary)
            .arg(&dest_path)
            .output()
            .map_err(|e| InstallError::CommandFailed(format!("rsync: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(InstallError::CommandFailed(format!(
                "rsync failed: {}",
                stderr
            )));
        }
    } else {
        // Fallback: use atomic file replacement to avoid race conditions and partial writes
        // Write to a temporary file first, then atomically rename
        let temp_dest = dest_path.with_extension("tmp");

        // Copy to temporary location
        std::fs::copy(&extracted_binary, &temp_dest)?;

        // Atomically rename to final destination (atomic on Unix filesystems)
        std::fs::rename(&temp_dest, &dest_path).map_err(|e| {
            // Clean up temp file on failure
            let _ = std::fs::remove_file(&temp_dest);
            e
        })?;
    }

    // Make the binary executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest_path, perms)?;
    }

    info!("Update installed successfully");
    Ok(false) // No restart needed on Linux
}

#[cfg(target_os = "linux")]
fn find_binary_in_dir(dir: &Path, binary_name: &str) -> Result<PathBuf, InstallError> {
    // Check direct path first
    let direct = dir.join(binary_name);
    if direct.exists() && direct.is_file() {
        return Ok(direct);
    }

    // Search recursively
    fn search_recursive(dir: &Path, name: &str) -> Option<PathBuf> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.file_name().map(|n| n == name).unwrap_or(false) {
                    return Some(path);
                }
                if path.is_dir() {
                    if let Some(found) = search_recursive(&path, name) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    search_recursive(dir, binary_name).ok_or_else(|| {
        InstallError::FileNotFound(format!("Binary '{}' not found in archive", binary_name))
    })
}

// =============================================================================
// Windows Implementation (Installer-only, simplified)
// =============================================================================

#[cfg(target_os = "windows")]
async fn install_release_windows(installer_path: &Path) -> Result<bool, InstallError> {
    if installer_path.extension().and_then(|e| e.to_str()) != Some("exe") {
        return Err(InstallError::InvalidUpdateFile(
            "Expected .exe installer".to_string(),
        ));
    }

    if !installer_path.exists() {
        return Err(InstallError::FileNotFound(
            installer_path.display().to_string(),
        ));
    }

    info!(installer = ?installer_path, "Launching Windows installer");

    // Launch installer with Inno Setup silent flags
    let result = Command::new(installer_path)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
        ])
        .spawn();

    match result {
        Ok(_) => {
            info!("Installer launched successfully");
            Ok(true) // Restart needed - installer will handle file replacement
        }
        Err(e) => {
            // Try NSIS silent flags as fallback
            warn!(error = ?e, "Failed with Inno Setup flags, trying NSIS flags");
            Command::new(installer_path)
                .arg("/S")
                .spawn()
                .map_err(|e| {
                    InstallError::CommandFailed(format!("Failed to launch installer: {}", e))
                })?;
            Ok(true)
        }
    }
}

/// Finalize Windows update - simplified (installer handles everything)
#[cfg(target_os = "windows")]
pub fn finalize_windows_update(_update_path: &Path) -> Result<(), InstallError> {
    // Installer handles file replacement and restart, nothing to do here
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_error_display() {
        let err = InstallError::FileNotFound("test.dmg".to_string());
        assert!(err.to_string().contains("test.dmg"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_parse_mount_point() {
        let output = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN">
<plist version="1.0">
<dict>
    <key>system-entities</key>
    <array>
        <dict>
            <key>mount-point</key>
            <string>/Volumes/Chatty</string>
        </dict>
    </array>
</dict>
</plist>"#;

        let mount_point = macos::DmgMount::parse_mount_point(output);
        assert!(mount_point.is_ok());
        assert_eq!(mount_point.unwrap(), PathBuf::from("/Volumes/Chatty"));
    }
}
