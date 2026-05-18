//! Per-OS install/relaunch helpers for the auto-updater.
//!
//! Linux relaunches the installed binary in place; macOS spawns a helper
//! app that elevates if needed. Windows install logic is inline in
//! `download::download_update` (NSIS installer invocation).

use super::*;

#[cfg(target_os = "linux")]
pub(super) fn relaunch_linux_process() -> std::io::Result<()> {
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
/// When `relaunch` is true, the script prioritises fast restart by launching
/// the app immediately after copying. When false (install-on-quit), the app
/// is not relaunched — the new version will be active on the next manual launch.
///
/// Steps:
/// 1. Polls for app exit with 0.2 s intervals (up to 10 s total)
/// 2. Mounts the downloaded .dmg with `hdiutil` (`-noverify` — checksum already validated)
/// 3. Rsyncs the new .app bundle over the current installation
/// 4. (if relaunch) Relaunches via direct binary execution (bypasses Gatekeeper)
/// 5. Post-install: clears quarantine attrs, re-signs adhoc bundles, resets LS cache, unmounts DMG
#[cfg(target_os = "macos")]
pub fn launch_macos_install_helper(
    dmg_path: &std::path::Path,
    app_bundle: &std::path::Path,
    relaunch: bool,
) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dmg = dmg_path.to_string_lossy();
    let bundle = app_bundle.to_string_lossy();
    let relaunch_flag = if relaunch { "true" } else { "false" };

    let script = format!(
        r#"#!/bin/bash
set -e

DMG_PATH="{dmg}"
APP_BUNDLE="{bundle}"
RELAUNCH="{relaunch_flag}"
LOG_FILE="$HOME/Library/Logs/chatty_update.log"

# Logging function
log() {{
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" | tee -a "$LOG_FILE"
}}

log "=== Chatty Update Installation Started ==="
log "DMG: $DMG_PATH"
log "Target: $APP_BUNDLE"

# Wait for the app to fully exit — poll quickly (0.2s) to minimize delay
log "Waiting for app to exit..."
APP_NAME="Chatty"
MAX_WAIT=50
WAIT_COUNT=0

while pgrep -x "$APP_NAME" > /dev/null 2>&1; do
    if [ $WAIT_COUNT -ge $MAX_WAIT ]; then
        log "WARNING: App still running after 10 seconds, proceeding anyway"
        break
    fi
    sleep 0.2
    WAIT_COUNT=$((WAIT_COUNT + 1))
done

log "App has exited, proceeding with installation"

# Mount the DMG — skip verification since we already validated the SHA-256 checksum
log "Mounting DMG..."
MOUNT_OUTPUT=$(hdiutil attach -nobrowse -noverify -plist "$DMG_PATH" 2>&1)
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

# Ensure pdfium dylib is present in the updated bundle before relaunching.
# This prevents launching a partially-copied app where PDF tools fail at runtime.
PDFIUM_SRC="$APP_IN_DMG/Contents/Frameworks/libpdfium.dylib"
PDFIUM_DST="$APP_BUNDLE/Contents/Frameworks/libpdfium.dylib"

if [ -f "$PDFIUM_SRC" ]; then
    if [ ! -f "$PDFIUM_DST" ]; then
        log "WARNING: libpdfium.dylib missing after rsync; attempting direct copy repair"
        mkdir -p "$APP_BUNDLE/Contents/Frameworks"
        if ! cp "$PDFIUM_SRC" "$PDFIUM_DST"; then
            log "ERROR: Failed to repair missing libpdfium.dylib"
            hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
            exit 1
        fi
        chmod 755 "$PDFIUM_DST" || true
    fi
else
    log "ERROR: Source DMG is missing libpdfium.dylib at $PDFIUM_SRC"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    exit 1
fi

if [ ! -f "$PDFIUM_DST" ]; then
    log "ERROR: libpdfium.dylib still missing after install (expected at $PDFIUM_DST)"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    exit 1
fi

# Clear quarantine attributes so Gatekeeper won't block future launches.
xattr -cr "$APP_BUNDLE" >> "$LOG_FILE" 2>&1 || log "No quarantine attributes to clear"

if [ "$RELAUNCH" = "true" ]; then
    # Relaunch the app IMMEDIATELY — don't wait for codesign/lsregister.
    # Direct binary execution bypasses Gatekeeper, so those steps are only
    # needed for future Finder/Spotlight launches and can run after relaunch.
    log "Relaunching app..."
    APP_BINARY="$APP_BUNDLE/Contents/MacOS/$APP_NAME"

    if [ -x "$APP_BINARY" ]; then
        log "Launching via direct binary: $APP_BINARY"
        nohup "$APP_BINARY" > /dev/null 2>&1 &
        LAUNCH_METHOD="direct"
    else
        log "Binary not found at $APP_BINARY, falling back to 'open' command"
        OPEN_OUTPUT=$(open -n "$APP_BUNDLE" 2>&1)
        OPEN_EXIT=$?

        if [ $OPEN_EXIT -ne 0 ]; then
            log "ERROR: open command failed with exit code $OPEN_EXIT"
            log "Output: $OPEN_OUTPUT"
            exit 1
        fi
        LAUNCH_METHOD="open"
    fi

    log "App launched via $LAUNCH_METHOD, running post-install tasks in background..."
else
    log "Install-on-quit mode: skipping relaunch, new version will be active on next launch"
fi

# --- Post-install housekeeping (non-blocking) ---
# These tasks prepare the bundle for future Finder/Spotlight launches.

# Re-sign adhoc/unsigned bundles for future Gatekeeper compatibility
SIGNATURE=$(codesign -dv "$APP_BUNDLE" 2>&1 | grep "Signature=" | cut -d= -f2)
if [ "$SIGNATURE" = "adhoc" ] || [ -z "$SIGNATURE" ]; then
    log "Re-signing adhoc bundle for future Gatekeeper compatibility..."
    codesign --force --deep --sign - "$APP_BUNDLE" >> "$LOG_FILE" 2>&1 || log "WARNING: Re-signing failed"
fi

# Reset Launch Services cache so Finder shows the new version
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP_BUNDLE" >> "$LOG_FILE" 2>&1 || true

# Unmount DMG
log "Unmounting DMG..."
hdiutil detach -force "$MOUNT_POINT" >> "$LOG_FILE" 2>&1 || log "WARNING: Failed to unmount DMG"

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
