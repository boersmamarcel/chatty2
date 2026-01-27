//! OS-specific installation logic for auto-updates
//!
//! This module provides installation strategies for different operating systems:
//! - macOS: Mount DMG, rsync .app bundle, unmount
//! - Linux: Extract tarball, rsync binary
//! - Windows: Launch installer with silent flags, defer final swap to quit

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

/// Error type for installation operations
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Command failed: {0}")]
    CommandFailed(String),

    #[error("Mount failed: {0}")]
    MountFailed(String),

    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Invalid update file: {0}")]
    InvalidUpdateFile(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("rsync not available")]
    RsyncNotAvailable,

    #[error("Unsupported platform")]
    UnsupportedPlatform,
}

/// Install the release from the given path
///
/// Returns `Ok(true)` if a restart is needed to complete the installation,
/// `Ok(false)` if the installation is complete.
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
                return Err(InstallError::FileNotFound(
                    dmg_path.display().to_string(),
                ));
            }

            info!(path = ?dmg_path, "Mounting DMG");

            // Run hdiutil attach and capture output
            let output = Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-plist"])
                .arg(dmg_path)
                .output()
                .map_err(|e| InstallError::CommandFailed(format!("hdiutil attach: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(InstallError::MountFailed(stderr.to_string()));
            }

            // Parse the plist output to find mount point
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mount_point = Self::parse_mount_point(&stdout)?;

            info!(mount_point = ?mount_point, "DMG mounted successfully");

            Ok(Self { mount_point })
        }

        /// Parse the mount point from hdiutil plist output
        fn parse_mount_point(output: &str) -> Result<PathBuf, InstallError> {
            // Look for mount-point in the output
            // The plist format has <key>mount-point</key> followed by <string>/path</string>

            // Simple parsing without full plist library
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

            // Fallback: try to find /Volumes/Chatty or similar
            for line in output.lines() {
                if line.contains("/Volumes/") {
                    if let Some(start) = line.find("/Volumes/") {
                        let rest = &line[start..];
                        // Extract path until next < or end
                        let end = rest.find('<').unwrap_or(rest.len());
                        let path = rest[..end].trim();
                        if !path.is_empty() {
                            return Ok(PathBuf::from(path));
                        }
                    }
                }
            }

            Err(InstallError::MountFailed("Could not parse mount point".to_string()))
        }

        /// Get the mount point path
        pub fn path(&self) -> &Path {
            &self.mount_point
        }

        /// Find the .app bundle in the mounted DMG
        pub fn find_app_bundle(&self) -> Result<PathBuf, InstallError> {
            let entries = std::fs::read_dir(&self.mount_point)
                .map_err(|e| InstallError::IoError(e))?;

            for entry in entries {
                let entry = entry.map_err(|e| InstallError::IoError(e))?;
                let path = entry.path();
                if path.extension().map(|e| e == "app").unwrap_or(false) {
                    return Ok(path);
                }
            }

            Err(InstallError::FileNotFound("No .app bundle found in DMG".to_string()))
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

    // Verify file extension
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

    // Determine the destination
    let current_exe = std::env::current_exe()
        .map_err(|e| InstallError::IoError(e))?;

    // Navigate up to find the .app bundle
    // /path/to/Chatty.app/Contents/MacOS/chatty -> /path/to/Chatty.app
    let dest_app = current_exe
        .ancestors()
        .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        .ok_or_else(|| InstallError::FileNotFound("Could not find current app bundle".to_string()))?;

    info!(
        source = ?app_bundle,
        dest = ?dest_app,
        "Installing update with rsync"
    );

    // Use rsync to copy the app
    let output = Command::new("rsync")
        .args(["-a", "--delete"])
        .arg(format!("{}/", app_bundle.display()))
        .arg(format!("{}/", dest_app.display()))
        .output()
        .map_err(|e| InstallError::CommandFailed(format!("rsync: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InstallError::CommandFailed(format!("rsync failed: {}", stderr)));
    }

    info!("Update installed successfully");

    // On macOS, we need to restart to use the new version
    Ok(false)
}

// =============================================================================
// Linux Implementation
// =============================================================================

#[cfg(target_os = "linux")]
async fn install_release_linux(tarball_path: &Path) -> Result<bool, InstallError> {
    use flate2::read::GzDecoder;
    use tar::Archive;
    use std::fs::File;

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

    // Check if rsync is available
    let rsync_check = Command::new("which")
        .arg("rsync")
        .output()
        .map_err(|e| InstallError::CommandFailed(format!("which: {}", e)))?;

    if !rsync_check.status.success() {
        warn!("rsync not found, falling back to direct copy");
    }

    // Create a temporary extraction directory
    let extract_dir = tempfile::tempdir()
        .map_err(|e| InstallError::IoError(e))?;

    info!(
        tarball = ?tarball_path,
        extract_dir = ?extract_dir.path(),
        "Extracting update"
    );

    // Extract the tarball
    let file = File::open(tarball_path)
        .map_err(|e| InstallError::IoError(e))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    archive.unpack(extract_dir.path())
        .map_err(|e| InstallError::ExtractionFailed(e.to_string()))?;

    // Find the binary in the extracted contents
    let binary_name = "chatty";
    let extracted_binary = find_binary_in_dir(extract_dir.path(), binary_name)?;

    info!(binary = ?extracted_binary, "Found extracted binary");

    // Determine destination
    let current_exe = std::env::current_exe()
        .map_err(|e| InstallError::IoError(e))?;

    // Try to determine the best installation location
    let dest_path = if current_exe.starts_with("/usr/local/bin") {
        PathBuf::from("/usr/local/bin").join(binary_name)
    } else if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local/bin");
        if local_bin.exists() || std::fs::create_dir_all(&local_bin).is_ok() {
            local_bin.join(binary_name)
        } else {
            current_exe.clone()
        }
    } else {
        current_exe.clone()
    };

    info!(
        source = ?extracted_binary,
        dest = ?dest_path,
        "Installing binary"
    );

    // Copy the binary
    if rsync_check.status.success() {
        let output = Command::new("rsync")
            .args(["-a"])
            .arg(&extracted_binary)
            .arg(&dest_path)
            .output()
            .map_err(|e| InstallError::CommandFailed(format!("rsync: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(InstallError::CommandFailed(format!("rsync failed: {}", stderr)));
        }
    } else {
        // Fallback to std::fs::copy
        std::fs::copy(&extracted_binary, &dest_path)
            .map_err(|e| InstallError::IoError(e))?;
    }

    // Make the binary executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest_path)
            .map_err(|e| InstallError::IoError(e))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest_path, perms)
            .map_err(|e| InstallError::IoError(e))?;
    }

    info!("Update installed successfully");

    // On Linux, we can restart immediately
    Ok(false)
}

#[cfg(target_os = "linux")]
fn find_binary_in_dir(dir: &Path, binary_name: &str) -> Result<PathBuf, InstallError> {
    // First, check for direct binary
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

    search_recursive(dir, binary_name)
        .ok_or_else(|| InstallError::FileNotFound(format!("Binary '{}' not found in archive", binary_name)))
}

// =============================================================================
// Windows Implementation
// =============================================================================

#[cfg(target_os = "windows")]
async fn install_release_windows(update_path: &Path) -> Result<bool, InstallError> {
    let extension = update_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match extension {
        "exe" => install_windows_installer(update_path).await,
        "zip" => install_windows_zip(update_path).await,
        _ => Err(InstallError::InvalidUpdateFile(
            "Expected .exe installer or .zip archive".to_string(),
        )),
    }
}

/// Install using a Windows installer (.exe)
#[cfg(target_os = "windows")]
async fn install_windows_installer(installer_path: &Path) -> Result<bool, InstallError> {
    if !installer_path.exists() {
        return Err(InstallError::FileNotFound(
            installer_path.display().to_string(),
        ));
    }

    info!(
        installer = ?installer_path,
        "Launching Windows installer"
    );

    // Launch the installer with silent flags
    // Common silent flags for various installers:
    // NSIS: /S
    // Inno Setup: /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
    // WiX: /quiet /norestart

    // We'll try Inno Setup flags first as they're most common for Rust apps
    let output = Command::new(installer_path)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
        ])
        .spawn();

    match output {
        Ok(_child) => {
            info!("Installer launched successfully");
            // On Windows, the installer runs separately
            // We need to quit and let the installer do its work
            Ok(true)
        }
        Err(e) => {
            // Try alternative silent flags
            warn!(error = ?e, "Failed with Inno Setup flags, trying NSIS flags");

            Command::new(installer_path)
                .args(["/S"])
                .spawn()
                .map_err(|e| {
                    InstallError::CommandFailed(format!("Failed to launch installer: {}", e))
                })?;

            Ok(true)
        }
    }
}

/// Install from a ZIP archive (portable distribution)
///
/// This extracts the ZIP to a staging area and prepares for the helper script
/// to swap the files after the main process exits.
#[cfg(target_os = "windows")]
async fn install_windows_zip(zip_path: &Path) -> Result<bool, InstallError> {
    use std::fs::File;
    use std::io::{Read, Write};

    if !zip_path.exists() {
        return Err(InstallError::FileNotFound(zip_path.display().to_string()));
    }

    info!(zip = ?zip_path, "Extracting Windows ZIP update");

    // Create staging directory in temp
    let staging_dir = std::env::temp_dir().join("chatty_update_staging");
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir).map_err(InstallError::IoError)?;
    }
    std::fs::create_dir_all(&staging_dir).map_err(InstallError::IoError)?;

    // Extract ZIP using PowerShell (built-in on Windows)
    let output = Command::new("powershell")
        .args([
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!(
                "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                zip_path.display(),
                staging_dir.display()
            ),
        ])
        .output()
        .map_err(|e| InstallError::CommandFailed(format!("PowerShell extract: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InstallError::ExtractionFailed(stderr.to_string()));
    }

    // Find the new binary in the staging directory
    let binary_name = "chatty.exe";
    let new_binary = find_windows_binary(&staging_dir, binary_name)?;

    info!(new_binary = ?new_binary, "Found new binary in staging");

    // Store the staging path for the helper script
    let staging_info_path = std::env::temp_dir().join("chatty_update_info.txt");
    let mut info_file = File::create(&staging_info_path).map_err(InstallError::IoError)?;
    writeln!(info_file, "{}", new_binary.display()).map_err(InstallError::IoError)?;

    info!("ZIP extracted, ready for finalization on quit");

    // Signal that we need the helper script to complete the update
    Ok(true)
}

/// Find the binary in the extracted directory (handles nested folders)
#[cfg(target_os = "windows")]
fn find_windows_binary(dir: &Path, binary_name: &str) -> Result<PathBuf, InstallError> {
    // Check direct path
    let direct = dir.join(binary_name);
    if direct.exists() {
        return Ok(direct);
    }

    // Search one level deep (ZIP might have a folder)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let nested = path.join(binary_name);
                if nested.exists() {
                    return Ok(nested);
                }
            }
        }
    }

    // Recursive search as fallback
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

/// Finalize Windows update after the main application quits
///
/// This creates a helper script that:
/// 1. Waits for the main process to exit
/// 2. Backs up the current executable
/// 3. Copies the new executable in place
/// 4. Restarts the application
/// 5. Cleans up staging files and itself
#[cfg(target_os = "windows")]
pub fn finalize_windows_update(update_path: &Path) -> Result<(), InstallError> {
    use std::io::Write;

    let current_exe =
        std::env::current_exe().map_err(InstallError::IoError)?;
    let current_pid = std::process::id();
    let current_dir = current_exe
        .parent()
        .ok_or_else(|| InstallError::FileNotFound("Could not get exe directory".to_string()))?;

    // Check if we have staging info (ZIP update) or just need restart (installer update)
    let staging_info_path = std::env::temp_dir().join("chatty_update_info.txt");
    let new_binary_path = if staging_info_path.exists() {
        std::fs::read_to_string(&staging_info_path)
            .map_err(InstallError::IoError)?
            .trim()
            .to_string()
    } else {
        // No staging info = installer-based update, just restart
        return finalize_windows_restart_only();
    };

    // Create the PowerShell helper script for file swapping
    let script_content = generate_windows_helper_script(
        current_pid,
        &current_exe,
        current_dir,
        &new_binary_path,
    );

    // Write the script to a temp file
    let script_path = std::env::temp_dir().join("chatty_update_helper.ps1");

    let mut file =
        std::fs::File::create(&script_path).map_err(InstallError::IoError)?;
    file.write_all(script_content.as_bytes())
        .map_err(InstallError::IoError)?;

    info!(script = ?script_path, "Created Windows update helper script");

    // Launch the PowerShell script hidden
    Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script_path)
        .spawn()
        .map_err(|e| {
            InstallError::CommandFailed(format!("Failed to launch helper script: {}", e))
        })?;

    info!("Windows update helper script launched");

    Ok(())
}

/// Generate the PowerShell helper script content
#[cfg(target_os = "windows")]
fn generate_windows_helper_script(
    pid: u32,
    current_exe: &Path,
    install_dir: &Path,
    new_binary: &str,
) -> String {
    let exe_escaped = current_exe.display().to_string().replace("'", "''");
    let install_dir_escaped = install_dir.display().to_string().replace("'", "''");
    let new_binary_escaped = new_binary.replace("'", "''");

    format!(
        r#"# Chatty Auto-Update Helper Script
# This script handles the file swap after the main application exits

param()

$ErrorActionPreference = "Continue"
$processId = {pid}
$currentExe = '{exe}'
$installDir = '{install_dir}'
$newBinary = '{new_binary}'
$backupExe = '{exe}.backup'
$maxWaitSeconds = 60
$logFile = Join-Path $env:TEMP "chatty_update.log"

function Write-Log {{
    param([string]$Message)
    $timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    "$timestamp - $Message" | Out-File -FilePath $logFile -Append
    Write-Host $Message
}}

Write-Log "Starting Chatty update process..."
Write-Log "Waiting for process $processId to exit..."

# Wait for the main process to exit
$waited = 0
while ($waited -lt $maxWaitSeconds) {{
    $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
    if ($null -eq $process) {{
        Write-Log "Process exited after $waited seconds"
        break
    }}
    Start-Sleep -Seconds 1
    $waited++
}}

if ($waited -ge $maxWaitSeconds) {{
    Write-Log "WARNING: Timed out waiting for process to exit"
}}

# Extra wait for file handles to release
Start-Sleep -Seconds 2

# Backup the current executable
Write-Log "Creating backup of current executable..."
try {{
    if (Test-Path $currentExe) {{
        Copy-Item -Path $currentExe -Destination $backupExe -Force
        Write-Log "Backup created: $backupExe"
    }}
}} catch {{
    Write-Log "WARNING: Failed to create backup: $_"
}}

# Copy the new binary
Write-Log "Installing new version..."
try {{
    Copy-Item -Path $newBinary -Destination $currentExe -Force
    Write-Log "New binary installed successfully"
}} catch {{
    Write-Log "ERROR: Failed to install new binary: $_"
    # Try to restore backup
    if (Test-Path $backupExe) {{
        Write-Log "Attempting to restore backup..."
        Copy-Item -Path $backupExe -Destination $currentExe -Force
    }}
    exit 1
}}

# Clean up staging directory
Write-Log "Cleaning up staging files..."
$stagingDir = Join-Path $env:TEMP "chatty_update_staging"
if (Test-Path $stagingDir) {{
    Remove-Item -Path $stagingDir -Recurse -Force -ErrorAction SilentlyContinue
}}
$stagingInfo = Join-Path $env:TEMP "chatty_update_info.txt"
if (Test-Path $stagingInfo) {{
    Remove-Item -Path $stagingInfo -Force -ErrorAction SilentlyContinue
}}

# Remove backup after successful update
if (Test-Path $backupExe) {{
    Remove-Item -Path $backupExe -Force -ErrorAction SilentlyContinue
}}

# Restart the application
Write-Log "Restarting application..."
Start-Process -FilePath $currentExe

Write-Log "Update completed successfully!"

# Clean up this script (delayed)
Start-Sleep -Seconds 2
Remove-Item -Path $MyInvocation.MyCommand.Path -Force -ErrorAction SilentlyContinue
"#,
        pid = pid,
        exe = exe_escaped,
        install_dir = install_dir_escaped,
        new_binary = new_binary_escaped,
    )
}

/// Simple restart-only finalization (for installer-based updates)
#[cfg(target_os = "windows")]
fn finalize_windows_restart_only() -> Result<(), InstallError> {
    use std::io::Write;

    let current_exe =
        std::env::current_exe().map_err(InstallError::IoError)?;
    let current_pid = std::process::id();

    let script_content = format!(
        r#"# Chatty Restart Helper
$processId = {pid}
$maxWait = 30

# Wait for process to exit
$waited = 0
while ($waited -lt $maxWait) {{
    $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
    if ($null -eq $process) {{ break }}
    Start-Sleep -Seconds 1
    $waited++
}}

Start-Sleep -Seconds 2

# Restart
Start-Process -FilePath '{exe}'

# Self-delete
Start-Sleep -Seconds 1
Remove-Item -Path $MyInvocation.MyCommand.Path -Force
"#,
        pid = current_pid,
        exe = current_exe.display().to_string().replace("'", "''"),
    );

    let script_path = std::env::temp_dir().join("chatty_restart_helper.ps1");
    let mut file =
        std::fs::File::create(&script_path).map_err(InstallError::IoError)?;
    file.write_all(script_content.as_bytes())
        .map_err(InstallError::IoError)?;

    Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script_path)
        .spawn()
        .map_err(|e| {
            InstallError::CommandFailed(format!("Failed to launch restart helper: {}", e))
        })?;

    Ok(())
}

// Stub for non-Windows platforms
#[cfg(not(target_os = "windows"))]
pub fn finalize_windows_update(_update_path: &Path) -> Result<(), InstallError> {
    Ok(())
}

// =============================================================================
// Fallback for other platforms
// =============================================================================

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
async fn install_release(_update_path: &Path) -> Result<bool, InstallError> {
    Err(InstallError::UnsupportedPlatform)
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
