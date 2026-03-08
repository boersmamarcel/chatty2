//! CLI installer module
//!
//! Installs the chatty-tui binary so users can run it from their terminal.
//!
//! Platform-specific strategies:
//! - macOS: Symlink in /usr/local/bin/ → binary inside .app bundle (like VS Code)
//! - Linux: Copy binary to ~/.local/bin/ (AppImage mount path is ephemeral)
//! - Windows: Add install directory to user PATH

use gpui::App;
use std::path::PathBuf;
use tracing::{error, info};

/// Install the CLI tool. Called from the InstallCli action handler.
///
/// Uses `cx.spawn()` because macOS `osascript` may block waiting for the
/// admin password dialog — we must not freeze the UI thread.
pub fn install_cli(cx: &mut App) {
    cx.spawn(async move |_cx: &mut gpui::AsyncApp| {
        let result = do_install().await;
        match &result {
            Ok(msg) => info!("{}", msg),
            Err(e) => error!("CLI installation failed: {}", e),
        }
    })
    .detach();
}

/// Find the bundled chatty-tui binary next to the running executable.
fn find_bundled_binary() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Cannot determine executable path: {}", e))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "Cannot determine executable directory".to_string())?;

    #[cfg(windows)]
    let name = "chatty-tui.exe";
    #[cfg(not(windows))]
    let name = "chatty-tui";

    let tui_path = dir.join(name);
    if tui_path.exists() {
        Ok(tui_path)
    } else {
        Err(format!(
            "chatty-tui not found at {}. Is this a packaged release build?",
            tui_path.display()
        ))
    }
}

// ── macOS ──────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
async fn do_install() -> Result<String, String> {
    let tui_binary = find_bundled_binary()?;
    let target = PathBuf::from("/usr/local/bin/chatty-tui");

    // Already installed and pointing to current binary?
    if target.exists()
        && let Ok(link_target) = std::fs::read_link(&target)
        && link_target == tui_binary
    {
        return Ok("CLI is already installed. Run 'chatty-tui' in your terminal.".into());
    }

    // Try direct symlink first (works if user has write access to /usr/local/bin)
    if try_symlink(&tui_binary, &target).is_ok() {
        return Ok(format!(
            "CLI installed at {}. Run 'chatty-tui' in your terminal.",
            target.display()
        ));
    }

    // Fall back to osascript for admin privileges (standard macOS password dialog)
    let script = format!(
        r#"do shell script "mkdir -p /usr/local/bin && ln -sf '{}' '{}'" with administrator privileges"#,
        tui_binary.display(),
        target.display()
    );

    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
        .map_err(|e| format!("Failed to run osascript: {}", e))?;

    if output.status.success() {
        Ok(format!(
            "CLI installed at {}. Run 'chatty-tui' in your terminal.",
            target.display()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("User canceled") || stderr.contains("(-128)") {
            Err("Installation cancelled by user.".into())
        } else {
            Err(format!("Installation failed: {}", stderr.trim()))
        }
    }
}

#[cfg(target_os = "macos")]
fn try_symlink(source: &PathBuf, target: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(target);
    std::os::unix::fs::symlink(source, target)
}

// ── Linux ──────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
async fn do_install() -> Result<String, String> {
    let tui_binary = find_bundled_binary()?;
    let bin_dir = dirs::home_dir()
        .ok_or_else(|| "Cannot determine home directory".to_string())?
        .join(".local/bin");

    tokio::fs::create_dir_all(&bin_dir)
        .await
        .map_err(|e| format!("Failed to create {}: {}", bin_dir.display(), e))?;

    let target = bin_dir.join("chatty-tui");

    // Copy binary (not symlink — AppImage mount path is ephemeral)
    tokio::fs::copy(&tui_binary, &target)
        .await
        .map_err(|e| format!("Failed to copy binary: {}", e))?;

    // Set executable permissions
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&target, perms)
            .await
            .map_err(|e| format!("Failed to set permissions: {}", e))?;
    }

    // Check if ~/.local/bin is in PATH
    let path_var = std::env::var("PATH").unwrap_or_default();
    let in_path = path_var.split(':').any(|p| bin_dir == std::path::Path::new(p));

    if in_path {
        Ok(format!(
            "CLI installed at {}. Run 'chatty-tui' in your terminal.",
            target.display()
        ))
    } else {
        Ok(format!(
            "CLI installed at {}. Add ~/.local/bin to your PATH, then run 'chatty-tui'.",
            target.display()
        ))
    }
}

// ── Windows ────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
async fn do_install() -> Result<String, String> {
    let tui_binary = find_bundled_binary()?;
    let install_dir = tui_binary
        .parent()
        .ok_or_else(|| "Cannot determine install directory".to_string())?;
    let dir_str = install_dir.display().to_string();

    // Check if already in user PATH
    let check = tokio::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "[Environment]::GetEnvironmentVariable('Path', 'User') -split ';' | Where-Object {{ $_ -eq '{}' }}",
                dir_str
            ),
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to check PATH: {}", e))?;

    let stdout = String::from_utf8_lossy(&check.stdout);
    if !stdout.trim().is_empty() {
        return Ok("CLI is already in PATH. Run 'chatty-tui' in your terminal.".into());
    }

    // Add to user PATH
    let ps_script = format!(
        "$p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         if ($p) {{ $p += ';{0}' }} else {{ $p = '{0}' }}; \
         [Environment]::SetEnvironmentVariable('Path', $p, 'User')",
        dir_str
    );

    let output = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_script])
        .output()
        .await
        .map_err(|e| format!("Failed to update PATH: {}", e))?;

    if output.status.success() {
        Ok(format!(
            "Added {} to PATH. Restart your terminal, then run 'chatty-tui'.",
            dir_str
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Failed to update PATH: {}", stderr.trim()))
    }
}
