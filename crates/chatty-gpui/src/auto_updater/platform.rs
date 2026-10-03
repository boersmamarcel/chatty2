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
/// 4. Clears quarantine attrs and, for an unsigned or ad-hoc bundle only, re-signs it
/// 5. (if relaunch) Relaunches via direct binary execution (bypasses Gatekeeper)
/// 6. Post-install: resets the Launch Services cache, unmounts the DMG
#[cfg(target_os = "macos")]
pub fn launch_macos_install_helper(
    dmg_path: &std::path::Path,
    app_bundle: &std::path::Path,
    relaunch: bool,
) {
    let script = render_macos_install_script(
        &dmg_path.to_string_lossy(),
        &app_bundle.to_string_lossy(),
        relaunch,
        expected_team_id().unwrap_or_default(),
    );
    spawn_macos_install_helper(script, dmg_path);
}

/// The Apple Team ID release builds are signed with, compiled in by
/// `release.yml` from the `NOTARIZE_TEAM_ID` secret (AGE-817). Builds without
/// it (local builds, or releases with Developer ID signing switched off) skip
/// the Team-ID check, since their own releases carry no Team ID either.
#[cfg(target_os = "macos")]
fn expected_team_id() -> Option<&'static str> {
    option_env!("CHATTY_MACOS_TEAM_ID")
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Bash snippet that checks a downloaded update before it may replace the
/// installed app (AGE-817, SEC-10): the DMG must be notarized (its stapled
/// ticket, checked by Gatekeeper), the app inside must pass
/// `codesign --verify --deep --strict`, and its `TeamIdentifier` must equal the
/// Team ID compiled into the running app. Prints `ok`, `skipped: ...` or
/// `refused: ...` and returns non-zero when refused.
///
/// Like the classifier below it is a constant so the unit tests can run the
/// shipped logic against fixture `codesign`/`spctl` output.
#[cfg(any(target_os = "macos", all(test, unix)))]
const MACOS_UPDATE_VERIFIER_SH: &str = r#"# Check the update before the swap: ok | skipped: ... | refused: ...
verify_update_bundle() {
    local dmg="$1" app="$2" expected_team="$3" out team
    if [ -z "$expected_team" ]; then
        echo "skipped: this build has no expected Team ID compiled in"
        return 0
    fi
    if ! out=$(codesign --verify --deep --strict "$app" 2>&1); then
        echo "refused: the new app's code signature is invalid: $(printf '%s\n' "$out" | head -1)"
        return 1
    fi
    team=$(codesign -dv "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p' | head -1)
    if [ "$team" != "$expected_team" ]; then
        echo "refused: Team ID mismatch (expected $expected_team, got ${team:-none})"
        return 1
    fi
    if ! out=$(spctl --assess --type open --context context:primary-signature -vv "$dmg" 2>&1) \
        || ! printf '%s\n' "$out" | grep -qx 'source=Notarized Developer ID'; then
        echo "refused: the update image is not notarized"
        return 1
    fi
    echo "ok"
}"#;

/// Bash snippet that classifies the code signature on an app bundle, printing
/// `adhoc`, `signed` or `unsigned` on stdout.
///
/// It lives in its own constant, interpolated into the install script, so the unit
/// tests can run the shipped logic against fixture `codesign -dv` output.
///
/// The distinction matters: `codesign -dv` prints `Signature=adhoc` for an ad-hoc
/// signature but `Signature size=NNNN` for a real one. Matching on the `Signature=`
/// prefix classifies every Developer ID bundle as unsigned, and the updater then
/// ad-hoc re-signs the notarized bundle it just installed — which strips the Team ID
/// off `Contents/Frameworks/libpdfium.dylib`, so the hardened runtime's library
/// validation refuses to let the app `dlopen` its own pdfium (AGE-337).
#[cfg(any(target_os = "macos", all(test, unix)))]
const MACOS_SIGNATURE_CLASSIFIER_SH: &str = r#"# Print the bundle's signature state: adhoc | signed | unsigned.
classify_signature() {
    local codesign_out
    if ! codesign_out=$(codesign -dv "$1" 2>&1); then
        # codesign exits non-zero when the object carries no signature at all.
        echo "unsigned"
        return 0
    fi
    if printf '%s\n' "$codesign_out" | grep -qx 'Signature=adhoc'; then
        echo "adhoc"
        return 0
    fi
    if printf '%s\n' "$codesign_out" | grep -Eq '^(TeamIdentifier=|Signature size=)'; then
        echo "signed"
        return 0
    fi
    echo "unsigned"
}"#;

/// Render the macOS install helper script. Separated from
/// [`launch_macos_install_helper`] so its step order can be asserted in tests.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn render_macos_install_script(
    dmg: &str,
    bundle: &str,
    relaunch: bool,
    expected_team_id: &str,
) -> String {
    let relaunch_flag = if relaunch { "true" } else { "false" };

    format!(
        r#"#!/bin/bash
set -e

DMG_PATH="{dmg}"
APP_BUNDLE="{bundle}"
RELAUNCH="{relaunch_flag}"
EXPECTED_TEAM_ID="{expected_team_id}"
LOG_FILE="$HOME/Library/Logs/chatty_update.log"

# Logging function
log() {{
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" | tee -a "$LOG_FILE"
}}

{classifier}

{verifier}

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

# Mount the DMG — skip hdiutil verification since we already validated the
# signed SHA-256 checksum; the code signature is checked right after mounting
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

# Authenticate the update BEFORE anything from the DMG is copied out of it
# (the pdfium seed below included): notarized image, strict code signature,
# and the Team ID this app was built to expect (AGE-817).
if ! VERIFY_RESULT=$(verify_update_bundle "$DMG_PATH" "$APP_IN_DMG" "$EXPECTED_TEAM_ID"); then
    log "ERROR: Update refused: $VERIFY_RESULT"
    hdiutil detach -force "$MOUNT_POINT" 2>&1 | tee -a "$LOG_FILE" || true
    if [ "$RELAUNCH" = "true" ] && [ -d "$APP_BUNDLE" ]; then
        log "Relaunching the current, unchanged app"
        open -n "$APP_BUNDLE" >> "$LOG_FILE" 2>&1 || true
    fi
    exit 1
fi
log "Update signature check: $VERIFY_RESULT"

# Seed the user-data-dir backup copy from the DMG mount FIRST, before any rsync.
# This is the most robust seeding point because:
#   - $APP_IN_DMG is the freshly-mounted DMG, guaranteed to contain the dylib.
#   - It runs even if the subsequent rsync writes to a translocated/wrong path.
#   - It runs even if the bundle layout is unusual.
#   - It runs even if codesign re-signing later corrupts the bundle.
# pdfium_utils::create_pdfium() checks this cache path FIRST, so the new release
# will find the library here regardless of what happens to the bundle.
USER_LIB_DIR="$HOME/Library/Application Support/chatty/lib"
USER_LIB_DST="$USER_LIB_DIR/libpdfium.dylib"
PDFIUM_IN_DMG="$APP_IN_DMG/Contents/Frameworks/libpdfium.dylib"
if [ -f "$PDFIUM_IN_DMG" ]; then
    if mkdir -p "$USER_LIB_DIR" 2>>"$LOG_FILE"; then
        # Use temp file + atomic rename so a concurrent reader never sees a partial file.
        USER_LIB_TMP="$USER_LIB_DST.$$.tmp"
        if cp "$PDFIUM_IN_DMG" "$USER_LIB_TMP" 2>>"$LOG_FILE" \
                && mv -f "$USER_LIB_TMP" "$USER_LIB_DST" 2>>"$LOG_FILE"; then
            log "Seeded user-data-dir pdfium cache at $USER_LIB_DST (from DMG)"
        else
            log "WARNING: failed to seed user-data-dir pdfium cache from DMG (non-fatal)"
            rm -f "$USER_LIB_TMP" 2>/dev/null || true
        fi
    else
        log "WARNING: could not create $USER_LIB_DIR (non-fatal)"
    fi
else
    log "WARNING: DMG missing $PDFIUM_IN_DMG; user-data-dir cache not seeded"
fi

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

# Re-sign an unsigned or ad-hoc bundle so future Finder/Spotlight launches work.
# This runs BEFORE the relaunch on purpose: rewriting signatures underneath a live
# process makes that process's later dlopen of Contents/Frameworks/libpdfium.dylib
# fail with "mapping process and mapped file have different Team IDs".
SIGNATURE_STATE=$(classify_signature "$APP_BUNDLE")
log "Bundle signature state: $SIGNATURE_STATE"
if [ "$SIGNATURE_STATE" = "signed" ]; then
    log "Bundle carries a real signature; leaving it untouched"
else
    log "Re-signing $SIGNATURE_STATE bundle for future Gatekeeper compatibility..."
    # Sign nested Mach-O objects one by one, then the bundle. Deep signing is
    # deprecated and can leave the bundle and the libraries it loads on
    # different identities.
    for NESTED in "$APP_BUNDLE"/Contents/Frameworks/*.dylib "$APP_BUNDLE"/Contents/MacOS/*; do
        [ -f "$NESTED" ] || continue
        codesign --force --sign - "$NESTED" >> "$LOG_FILE" 2>&1 \
            || log "WARNING: Re-signing failed for $NESTED"
    done
    codesign --force --sign - "$APP_BUNDLE" >> "$LOG_FILE" 2>&1 || log "WARNING: Re-signing failed"
fi

if [ "$RELAUNCH" = "true" ]; then
    # Relaunch the app IMMEDIATELY — lsregister and the unmount can wait.
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

# Reset Launch Services cache so Finder shows the new version
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP_BUNDLE" >> "$LOG_FILE" 2>&1 || true

# Unmount DMG
log "Unmounting DMG..."
hdiutil detach -force "$MOUNT_POINT" >> "$LOG_FILE" 2>&1 || log "WARNING: Failed to unmount DMG"

log "=== Update Installation Completed Successfully ==="
"#,
        dmg = dmg,
        bundle = bundle,
        classifier = MACOS_SIGNATURE_CLASSIFIER_SH,
        verifier = MACOS_UPDATE_VERIFIER_SH,
    )
}

/// Write the rendered helper to /tmp and spawn it detached, so it outlives the
/// app process it is about to replace.
#[cfg(target_os = "macos")]
fn spawn_macos_install_helper(script: String, dmg_path: &std::path::Path) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

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

/// Tests for the macOS install helper. They run the shipped bash verbatim, so they
/// need a POSIX shell — hence `unix` rather than `macos`: the logic is identical on
/// the Linux CI runner that executes them.
#[cfg(all(test, unix))]
mod macos_install_script_tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    /// Run `classify_signature` from [`MACOS_SIGNATURE_CLASSIFIER_SH`] against a
    /// fixture, with a stub `codesign` on `PATH` that replays `output` on stderr
    /// (where the real one writes it) and exits with `exit_code`.
    fn classify(output: &str, exit_code: i32) -> String {
        let dir = tempfile::tempdir().expect("tempdir");

        let stub = dir.path().join("codesign");
        let mut file = std::fs::File::create(&stub).expect("create stub");
        write!(
            file,
            "#!/bin/bash\ncat <<'CODESIGN_FIXTURE' >&2\n{output}\nCODESIGN_FIXTURE\nexit {exit_code}\n"
        )
        .expect("write stub");
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        drop(file);

        let script = dir.path().join("classify.sh");
        std::fs::write(
            &script,
            format!("{MACOS_SIGNATURE_CLASSIFIER_SH}\nclassify_signature \"$1\"\n"),
        )
        .expect("write script");

        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = std::process::Command::new("bash")
            .arg(&script)
            .arg("/Applications/chatty.app")
            .env("PATH", path)
            .output()
            .expect("run classifier");
        assert!(out.status.success(), "classifier exited non-zero");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    const TEAM: &str = "ABCDE12345";

    fn stub(dir: &std::path::Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/bash\n{body}\n")).expect("write stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }

    /// Run `verify_update_bundle` from [`MACOS_UPDATE_VERIFIER_SH`] with stub
    /// `codesign` and `spctl` on `PATH`: `codesign --verify` exits
    /// `verify_exit`, `codesign -dv` prints `team_line`, and `spctl` prints
    /// `spctl_source`. Returns (succeeded, stdout).
    fn verify(
        expected_team: &str,
        verify_exit: i32,
        team_line: &str,
        spctl_source: &str,
    ) -> (bool, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        stub(
            dir.path(),
            "codesign",
            &format!(
                "if [ \"$1\" = \"--verify\" ]; then echo 'verify output' >&2; exit {verify_exit}; fi\n\
                 printf 'Identifier=com.chatty.app\\nSignature size=9068\\n{team_line}\\n' >&2"
            ),
        );
        stub(
            dir.path(),
            "spctl",
            &format!("printf '/tmp/chatty.dmg: accepted\\n{spctl_source}\\n' >&2"),
        );
        let script = dir.path().join("verify.sh");
        std::fs::write(
            &script,
            format!("{MACOS_UPDATE_VERIFIER_SH}\nverify_update_bundle \"$1\" \"$2\" \"$3\"\n"),
        )
        .expect("write script");
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = std::process::Command::new("bash")
            .arg(&script)
            .args([
                "/tmp/chatty.dmg",
                "/Volumes/Chatty/chatty.app",
                expected_team,
            ])
            .env("PATH", path)
            .output()
            .expect("run verifier");
        (
            out.status.success(),
            String::from_utf8(out.stdout)
                .expect("utf8")
                .trim()
                .to_string(),
        )
    }

    #[test]
    fn macos_team_id_mismatch_refuses_the_swap() {
        let (ok, out) = verify(
            TEAM,
            0,
            "TeamIdentifier=ZZZZZ99999",
            "source=Notarized Developer ID",
        );
        assert!(!ok, "a foreign Team ID must refuse the update: {out}");
        assert_eq!(
            out,
            "refused: Team ID mismatch (expected ABCDE12345, got ZZZZZ99999)"
        );

        // An ad-hoc bundle has no Team ID at all.
        let (ok, out) = verify(
            TEAM,
            0,
            "TeamIdentifier=not set",
            "source=Notarized Developer ID",
        );
        assert!(!ok, "{out}");

        // The refusal stops the script before the swap.
        let script =
            render_macos_install_script("/tmp/chatty.dmg", "/Applications/chatty.app", true, TEAM);
        let check = script
            .find("VERIFY_RESULT=$(verify_update_bundle")
            .expect("script verifies the update");
        let seed = script.find("USER_LIB_DIR=").expect("script seeds pdfium");
        let swap = script
            .find("rsync -a --delete")
            .expect("script swaps the bundle");
        assert!(
            check < seed && check < swap,
            "the check must precede every copy out of the DMG"
        );
    }

    #[test]
    fn a_notarized_bundle_from_the_expected_team_passes() {
        let (ok, out) = verify(
            TEAM,
            0,
            "TeamIdentifier=ABCDE12345",
            "source=Notarized Developer ID",
        );
        assert!(ok, "{out}");
        assert_eq!(out, "ok");
    }

    #[test]
    fn a_broken_or_unnotarized_update_is_refused() {
        let (ok, out) = verify(
            TEAM,
            1,
            "TeamIdentifier=ABCDE12345",
            "source=Notarized Developer ID",
        );
        assert!(
            !ok && out.starts_with("refused: the new app's code signature is invalid"),
            "{out}"
        );

        let (ok, out) = verify(TEAM, 0, "TeamIdentifier=ABCDE12345", "source=Developer ID");
        assert!(!ok, "{out}");
        assert_eq!(out, "refused: the update image is not notarized");
    }

    #[test]
    fn a_build_without_a_team_id_skips_the_check() {
        let (ok, out) = verify("", 1, "TeamIdentifier=not set", "");
        assert!(ok, "{out}");
        assert!(out.starts_with("skipped:"), "{out}");
    }

    /// The regression: a Developer ID bundle prints `Signature size=NNNN`, never
    /// `Signature=`, and must be left alone. Re-signing it ad-hoc strips the Team ID
    /// off the bundled pdfium and breaks every PDF tool (AGE-337).
    #[test]
    fn developer_id_bundle_classifies_as_signed() {
        let fixture = "\
Executable=/Applications/chatty.app/Contents/MacOS/Chatty
Identifier=com.chatty.app
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=30+7 location=embedded
Signature size=9068
Info.plist entries=9
TeamIdentifier=4C4C44FA55
Sealed Resources version=2 rules=13 files=42";
        assert_eq!(classify(fixture, 0), "signed");
    }

    #[test]
    fn adhoc_bundle_classifies_as_adhoc() {
        let fixture = "\
Executable=/Applications/chatty.app/Contents/MacOS/Chatty
Identifier=com.chatty.app
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20400 size=1234 flags=0x2(adhoc) hashes=30+7 location=embedded
Signature=adhoc
Info.plist entries=9
TeamIdentifier=not set
Sealed Resources version=2 rules=13 files=42";
        assert_eq!(classify(fixture, 0), "adhoc");
    }

    #[test]
    fn unsigned_bundle_classifies_as_unsigned() {
        let fixture = "/Applications/chatty.app: code object is not signed at all";
        assert_eq!(classify(fixture, 1), "unsigned");
    }

    /// Signatures must be settled before the app is running again: rewriting them
    /// underneath a live process is what produced the "different Team IDs" dlopen
    /// failure on the first PDF tool call after an update.
    #[test]
    fn resign_happens_before_relaunch() {
        let script =
            render_macos_install_script("/tmp/chatty.dmg", "/Applications/chatty.app", true, TEAM);
        let resign = script
            .find(r#"SIGNATURE_STATE=$(classify_signature "$APP_BUNDLE")"#)
            .expect("script classifies the signature");
        let relaunch = script
            .find(r#"log "Relaunching app...""#)
            .expect("script relaunches the app");
        assert!(
            resign < relaunch,
            "re-sign must precede the relaunch (re-sign at {resign}, relaunch at {relaunch})"
        );
    }

    /// `--deep` signing is deprecated and can leave the bundle and its dylibs on
    /// different identities — the nested objects are signed one by one instead.
    /// (`--verify --deep` in the update check is fine: it only reads.)
    #[test]
    fn script_does_not_deep_sign() {
        let script =
            render_macos_install_script("/tmp/chatty.dmg", "/Applications/chatty.app", true, TEAM);
        for line in script.lines().filter(|l| l.contains("--deep")) {
            assert!(
                !line.contains("--sign"),
                "install helper must not use codesign --sign --deep: {line}"
            );
        }
    }

    /// The helper is generated, never linted — parse it here so a broken edit to
    /// the embedded bash fails the build instead of a user's update.
    #[test]
    fn rendered_script_is_valid_bash() {
        for relaunch in [true, false] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("helper.sh");
            std::fs::write(
                &path,
                render_macos_install_script(
                    "/tmp/chatty.dmg",
                    "/Applications/chatty.app",
                    relaunch,
                    TEAM,
                ),
            )
            .expect("write script");
            let out = std::process::Command::new("bash")
                .arg("-n")
                .arg(&path)
                .output()
                .expect("run bash -n");
            assert!(
                out.status.success(),
                "rendered script (relaunch={relaunch}) is not valid bash: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn script_interpolates_its_arguments() {
        let script =
            render_macos_install_script("/tmp/chatty.dmg", "/Applications/chatty.app", false, TEAM);
        assert!(script.contains(r#"DMG_PATH="/tmp/chatty.dmg""#));
        assert!(script.contains(r#"APP_BUNDLE="/Applications/chatty.app""#));
        assert!(script.contains(r#"RELAUNCH="false""#));
        assert!(script.contains(r#"EXPECTED_TEAM_ID="ABCDE12345""#));
    }
}
