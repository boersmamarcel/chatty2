//! OS-specific installation logic for auto-updates
//!
//! Platform-specific strategies:
//! - macOS: Deferred installation via a helper shell script (see `launch_macos_install_helper`
//!   in `mod.rs`). The app quits gracefully and the script replaces the .app bundle.
//! - Linux: Atomically replace the running AppImage with the downloaded one using
//!   async tokio I/O so the Tokio runtime stays responsive.
//! - Windows: Launch the silent installer; it handles file replacement and relaunch.

use std::path::Path;
#[cfg(target_os = "windows")]
use std::process::Command;

use tracing::info;
#[cfg(target_os = "windows")]
use tracing::warn;

/// Error type for installation operations
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Command failed: {0}")]
    #[cfg(target_os = "windows")]
    CommandFailed(String),

    #[error("File not found: {0}")]
    #[cfg(target_os = "windows")]
    FileNotFound(String),

    #[error("Invalid update file: {0}")]
    InvalidUpdateFile(String),

    #[error("Extraction failed: {0}")]
    #[allow(dead_code)]
    ExtractionFailed(String),
}

/// Install the release from the given path.
///
/// Returns `Ok(true)` if a restart is needed to complete installation (Windows),
/// `Ok(false)` if installation is complete (Linux).
///
/// Note: macOS is handled separately via `launch_macos_install_helper` in `mod.rs`
/// and does not go through this function.
#[cfg(not(target_os = "macos"))]
pub async fn install_release(update_path: &Path) -> Result<bool, InstallError> {
    #[cfg(target_os = "linux")]
    return install_release_linux(update_path).await;

    #[cfg(target_os = "windows")]
    return install_release_windows(update_path).await;

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    Err(InstallError::InvalidUpdateFile(
        "Unsupported platform".to_string(),
    ))
}

// =============================================================================
// Linux Implementation
// =============================================================================

#[cfg(target_os = "linux")]
async fn install_release_linux(appimage_path: &Path) -> Result<bool, InstallError> {
    // Verify file extension
    let is_appimage = appimage_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".AppImage"))
        .unwrap_or(false);

    if !is_appimage {
        return Err(InstallError::InvalidUpdateFile(
            "Expected .AppImage file".to_string(),
        ));
    }

    // Determine destination: current executable location
    let current_exe = std::env::current_exe()?;
    let dest_path = current_exe;

    info!(source = ?appimage_path, dest = ?dest_path, "Installing AppImage");

    let temp_dest = dest_path.with_extension("tmp");

    // Copy to temporary location using async I/O to avoid blocking the runtime
    tokio::fs::copy(appimage_path, &temp_dest).await?;

    // Atomically rename to final destination (atomic on Unix filesystems)
    if let Err(e) = tokio::fs::rename(&temp_dest, &dest_path).await {
        // Clean up temp file on failure
        let _ = tokio::fs::remove_file(&temp_dest).await;
        return Err(InstallError::IoError(e));
    }

    // Make the AppImage executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(&dest_path).await?;
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&dest_path, perms).await?;
    }

    info!("AppImage update installed successfully");
    Ok(false) // No restart needed — caller handles relaunch
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
    // Note: We don't use /NORESTART because the [Run] section in the .iss file
    // handles launching the new app after installation
    let result = Command::new(installer_path)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/CLOSEAPPLICATIONS"])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_error_display() {
        let err = InstallError::InvalidUpdateFile("test.dmg".to_string());
        assert!(err.to_string().contains("test.dmg"));
    }
}
