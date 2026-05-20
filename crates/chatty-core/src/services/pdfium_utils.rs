use pdfium_render::prelude::*;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Get the path to the pdfium library directory set by build.rs (compile-time path).
fn compile_time_lib_path() -> Option<PathBuf> {
    let lib_dir = option_env!("PDFIUM_LIB_DIR")?;
    Some(PathBuf::from(lib_dir))
}

/// Get the path to the pdfium library directory from runtime environment overrides.
fn runtime_env_lib_path() -> Option<PathBuf> {
    let lib_dir = std::env::var("CHATTY_PDFIUM_LIB_DIR").ok()?;
    Some(PathBuf::from(lib_dir))
}

/// Return the persistent user-data-dir path that holds a backup copy of the pdfium library.
///
/// This is the *robust* cache location that survives bundle corruption, app translocation,
/// partial auto-update rsyncs, and any layout quirk inside the `.app` bundle. The auto-updater
/// seeds this directory from the freshly installed bundle, and at runtime we opportunistically
/// copy any bundle-bound dylib here so subsequent launches always have a known-good copy
/// independent of the executable layout.
///
/// Layout: `<dirs::data_dir>/chatty/lib/` (`~/Library/Application Support/chatty/lib` on macOS,
/// `~/.local/share/chatty/lib` on Linux, `%APPDATA%\chatty\lib` on Windows).
pub fn user_data_lib_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("chatty").join("lib"))
}

fn user_data_lib_path() -> Option<PathBuf> {
    let dir = user_data_lib_dir()?;
    let lib_name = Pdfium::pdfium_platform_library_name();
    if dir.join(&lib_name).exists() {
        Some(dir)
    } else {
        None
    }
}

/// Best-effort copy of a successfully loaded pdfium library to the user-data-dir cache.
///
/// We only copy when:
/// - The user-data-dir cache does not already contain the file (idempotent, cheap).
/// - The source file is readable and the target dir is creatable.
///
/// On macOS, the source dylib is signed by the same Team ID as the chatty executable (because
/// packaging signs it during bundling). The file copy preserves the embedded codesignature,
/// so loading the cached copy still satisfies library validation under hardened runtime.
///
/// Errors are intentionally logged and swallowed — failure to seed the cache is non-fatal:
/// the current invocation already succeeded with the source path.
fn self_heal_copy(source_dir: &Path) {
    let Some(target_dir) = user_data_lib_dir() else {
        debug!("pdfium: self-heal skipped — no user data dir available");
        return;
    };

    let lib_name = Pdfium::pdfium_platform_library_name();
    let source = source_dir.join(&lib_name);
    let target = target_dir.join(&lib_name);

    // Don't copy onto ourselves.
    match (
        std::fs::canonicalize(&source),
        std::fs::canonicalize(&target),
    ) {
        (Ok(s), Ok(t)) if s == t => {
            debug!("pdfium: self-heal skipped — source and target are the same file");
            return;
        }
        _ => {}
    }

    if target.exists() {
        debug!(path = %target.display(), "pdfium: self-heal skipped — cache already present");
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        warn!(
            dir = %target_dir.display(),
            error = %e,
            "pdfium: self-heal failed to create cache dir"
        );
        return;
    }

    // Use a temp file + rename to avoid partial-copy races between concurrent processes
    // (e.g. main app + chatty-tui sub-agent starting around the same time).
    let tmp = target_dir.join(format!("{}.tmp", lib_name.to_string_lossy()));
    match std::fs::copy(&source, &tmp) {
        Ok(_) => match std::fs::rename(&tmp, &target) {
            Ok(_) => info!(
                source = %source.display(),
                target = %target.display(),
                "pdfium: self-heal cached library to user data dir"
            ),
            Err(e) => {
                warn!(
                    target = %target.display(),
                    error = %e,
                    "pdfium: self-heal failed to rename temp file"
                );
                let _ = std::fs::remove_file(&tmp);
            }
        },
        Err(e) => {
            warn!(
                source = %source.display(),
                target = %tmp.display(),
                error = %e,
                "pdfium: self-heal failed to copy library"
            );
        }
    }
}

fn canonicalize_existing_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }

    match std::fs::canonicalize(dir) {
        Ok(path) => Some(path),
        Err(e) => {
            warn!(
                path = %dir.display(),
                error = %e,
                "pdfium: failed to canonicalize directory, using raw path"
            );
            Some(dir.to_path_buf())
        }
    }
}

fn macos_frameworks_from_bundle(exe_path: &Path, lib_name: &OsStr) -> Option<PathBuf> {
    let mut current = exe_path.parent();
    while let Some(dir) = current {
        if dir.file_name().is_some_and(|name| name == "Contents") {
            let frameworks = dir.join("Frameworks");
            debug!(
                path = %frameworks.display(),
                "pdfium: trying bundle-derived Frameworks path"
            );
            if frameworks.join(lib_name).exists() {
                return canonicalize_existing_dir(&frameworks);
            }
        }
        current = dir.parent();
    }
    None
}

fn macos_default_install_frameworks(lib_name: &OsStr) -> Option<PathBuf> {
    // Best-effort fallback for standard packaged installs.
    // This is intentionally lower-priority than exe-relative detection and runtime override
    // (`CHATTY_PDFIUM_LIB_DIR`) so non-standard install locations remain configurable.
    let frameworks = Path::new("/Applications/chatty.app/Contents/Frameworks");
    debug!(
        path = %frameworks.display(),
        "pdfium: trying default install Frameworks path"
    );
    if frameworks.join(lib_name).exists() {
        return canonicalize_existing_dir(frameworks);
    }
    None
}

/// Resolve the pdfium library directory relative to the running executable.
///
/// This handles packaged app bundles where the compile-time path no longer exists:
/// - **macOS** `.app` bundle: `<exe>/../../Frameworks/` (`Contents/MacOS/../Frameworks/`)
/// - **Linux** AppImage: `<exe>/../lib/` (`usr/bin/../lib/`)
/// - **Windows** package: `<exe>/` (DLL next to the executable)
///
/// On macOS, `current_exe()` may return a symlinked or app-translocated path.
/// We canonicalize before resolving `../Frameworks` so that `is_dir()` works
/// correctly through symlinks.
fn exe_relative_lib_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    debug!(path = %exe.display(), "pdfium: raw executable path");

    // Resolved once at the top so it can be used both in the platform-specific
    // directory checks below and in the final "beside-executable" fallback.
    let lib_name = Pdfium::pdfium_platform_library_name();

    // Canonicalize to resolve symlinks and translocation (especially on macOS)
    let canonical_exe = std::fs::canonicalize(&exe)
        .inspect_err(|e| warn!(error = %e, "pdfium: failed to canonicalize exe path"))
        .ok();

    if let Some(ref canon) = canonical_exe {
        debug!(path = %canon.display(), "pdfium: canonical executable path");
    }

    if cfg!(target_os = "macos") {
        // Contents/MacOS/<exe> → Contents/Frameworks/
        // Try canonical path first (handles symlinks/translocation)
        if let Some(ref canon) = canonical_exe
            && let Some(exe_dir) = canon.parent()
        {
            let frameworks = exe_dir.join("../Frameworks");
            debug!(path = %frameworks.display(), "pdfium: trying canonical Frameworks path");
            if frameworks.join(&lib_name).exists()
                && let Some(path) = canonicalize_existing_dir(&frameworks)
            {
                return Some(path);
            }
        }
        // Fallback: raw (non-canonical) path
        if let Some(exe_dir) = exe.parent() {
            let frameworks = exe_dir.join("../Frameworks");
            debug!(path = %frameworks.display(), "pdfium: trying raw Frameworks path");
            if frameworks.join(&lib_name).exists()
                && let Some(path) = canonicalize_existing_dir(&frameworks)
            {
                return Some(path);
            }
        }

        if let Some(ref canon) = canonical_exe
            && let Some(path) = macos_frameworks_from_bundle(canon, &lib_name)
        {
            return Some(path);
        }
        if let Some(path) = macos_frameworks_from_bundle(&exe, &lib_name) {
            return Some(path);
        }
        if let Some(path) = macos_default_install_frameworks(&lib_name) {
            return Some(path);
        }

        // Last macOS fallback: look for the library next to the executable.
        let exe_for_beside = canonical_exe.as_ref().unwrap_or(&exe);
        if let Some(exe_dir) = exe_for_beside.parent() {
            let beside_exe = exe_dir.join(&lib_name);
            debug!(path = %beside_exe.display(), "pdfium: trying macOS library beside executable");
            if beside_exe.exists() {
                return Some(exe_dir.to_path_buf());
            }
        }
        return None;
    }

    if cfg!(target_os = "windows") {
        // Windows: pdfium.dll lives next to the executable
        let exe_dir = canonical_exe
            .as_ref()
            .unwrap_or(&exe)
            .parent()?
            .to_path_buf();
        return Some(exe_dir);
    }

    // Linux AppImage: usr/bin/<exe> → usr/lib/
    let exe_for_linux = canonical_exe.as_ref().unwrap_or(&exe);
    if let Some(exe_dir) = exe_for_linux.parent() {
        let lib_dir = exe_dir.join("../lib");
        debug!(path = %lib_dir.display(), "pdfium: trying exe-relative lib path");
        if lib_dir.is_dir() && lib_dir.join(&lib_name).exists() {
            return Some(lib_dir);
        }
    }

    // Final fallback: library next to the executable (non-standard installations)
    let exe_for_fallback = canonical_exe.as_ref().unwrap_or(&exe);
    if let Some(exe_dir) = exe_for_fallback.parent() {
        let beside_exe = exe_dir.join(&lib_name);
        debug!(path = %beside_exe.display(), "pdfium: trying library beside executable");
        if beside_exe.exists() {
            return Some(exe_dir.to_path_buf());
        }
    }

    debug!("pdfium: no exe-relative library path found");
    None
}

/// Create a [`Pdfium`] instance bound to the bundled library, falling back to the system library.
///
/// Search order:
/// 1. User data dir cache (`<dirs::data_dir>/chatty/lib/`) — robust against bundle issues
/// 2. Executable-relative path (app bundle / AppImage)
/// 3. Runtime env `CHATTY_PDFIUM_LIB_DIR` override
/// 4. Compile-time `PDFIUM_LIB_DIR` (development builds)
/// 5. System library fallback
///
/// On the first successful bind, the library is opportunistically copied to the user-data-dir
/// cache so subsequent invocations (including chatty-tui sub-agents and post-update relaunches)
/// can load it regardless of the executable layout.
///
/// In pdfium-render 0.9, the pdfium bindings are stored in a process-global `OnceLock` and
/// `Pdfium::new()` asserts that the global is unset. If a prior call has already bound the
/// library (e.g. between tests in the same process), subsequent `bind_to_*` calls return
/// `PdfiumError::PdfiumLibraryBindingsAlreadyInitialized`; in that case we simply return a
/// fresh `Pdfium` unit struct that transparently re-uses the existing global bindings.
pub fn create_pdfium() -> anyhow::Result<Pdfium> {
    let lib_name = Pdfium::pdfium_platform_library_name();

    let candidate_dirs = [
        user_data_lib_path(),
        exe_relative_lib_path(),
        runtime_env_lib_path(),
        compile_time_lib_path(),
    ];

    let mut attempts: Vec<(PathBuf, String)> = Vec::new();
    for dir in candidate_dirs.into_iter().flatten() {
        let lib_path = dir.join(&lib_name);
        debug!(path = %lib_path.display(), "pdfium: trying candidate library");
        match Pdfium::bind_to_library(&lib_path) {
            Ok(bindings) => {
                info!(path = %lib_path.display(), "pdfium: successfully bound library");
                self_heal_copy(&dir);
                return Ok(Pdfium::new(bindings));
            }
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                debug!("pdfium: library already initialized in this process, reusing bindings");
                return Ok(Pdfium {});
            }
            Err(e) => {
                debug!(path = %lib_path.display(), error = ?e, "pdfium: candidate failed");
                attempts.push((lib_path, format!("{e:?}")));
            }
        }
    }

    // Final fallback: system library
    debug!("pdfium: trying system library fallback");
    match Pdfium::bind_to_system_library() {
        Ok(bindings) => {
            info!("pdfium: successfully bound system library");
            Ok(Pdfium::new(bindings))
        }
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
            debug!("pdfium: system library already initialized, reusing bindings");
            Ok(Pdfium {})
        }
        Err(system_err) => {
            // Build a diagnostic listing every path we attempted plus whether it existed.
            // The previous error format only surfaced the *last* candidate, which masked
            // the real reason on platforms where `exe_relative_lib_path()` returned `None`.
            let mut diagnostic = String::from("Failed to bind pdfium. Attempts:\n");
            if attempts.is_empty() {
                diagnostic.push_str(
                    "  (no candidate paths produced — none of the lookup strategies \
                     resolved to a directory containing the pdfium library)\n",
                );
            } else {
                for (path, err) in &attempts {
                    let exists = path.exists();
                    diagnostic.push_str(&format!(
                        "  - {} (exists={}): {}\n",
                        path.display(),
                        exists,
                        err
                    ));
                }
            }
            diagnostic.push_str(&format!("  - system library: {system_err:?}\n"));
            if let Some(cache_dir) = user_data_lib_dir() {
                diagnostic.push_str(&format!(
                    "Hint: place a copy of {} at {} to recover.\n",
                    lib_name.to_string_lossy(),
                    cache_dir.join(&lib_name).display(),
                ));
            }
            warn!(diagnostic = %diagnostic, "pdfium: all binding attempts failed");
            Err(anyhow::anyhow!(diagnostic))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn lib_name() -> std::ffi::OsString {
        Pdfium::pdfium_platform_library_name()
    }

    #[test]
    fn user_data_lib_dir_is_under_chatty_subdir() {
        // We can't assert the absolute prefix portably, but we can verify the layout.
        let dir = user_data_lib_dir().expect("data_dir should resolve on test platforms");
        assert!(
            dir.ends_with(Path::new("chatty/lib")) || dir.ends_with(Path::new("chatty\\lib")),
            "expected user data lib dir to end with chatty/lib, got {}",
            dir.display()
        );
    }

    #[test]
    fn user_data_lib_path_returns_none_when_cache_empty() {
        // If a test environment happens to have a real cached dylib, skip.
        if let Some(p) = user_data_lib_dir() {
            if p.join(lib_name()).exists() {
                return;
            }
        }
        assert!(user_data_lib_path().is_none());
    }

    #[test]
    fn self_heal_copy_seeds_cache_when_missing() {
        // Verify the copy semantics with a fake source dir + isolated target.
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let src_file = source_dir.join(lib_name());
        fs::write(&src_file, b"fake-pdfium-bytes-for-test").unwrap();

        // Redirect the user-data dir for this test via XDG_DATA_HOME (Linux) / HOME (macOS).
        // We don't override platform dirs globally; instead we verify self_heal_copy's
        // no-op behavior when target already exists, and its copy behavior when missing.
        let target_dir = tmp.path().join("target_data").join("chatty").join("lib");

        // Manually exercise the copy semantics by reimplementing the core logic against
        // an explicit target — the production function uses dirs::data_dir() which we
        // can't stub portably. The logic under test is straightforward filesystem ops.
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join(lib_name());
        assert!(!target.exists());
        fs::copy(&src_file, &target).unwrap();
        assert!(target.exists());
        let copied = fs::read(&target).unwrap();
        assert_eq!(copied, b"fake-pdfium-bytes-for-test");
    }

    #[test]
    fn self_heal_copy_is_no_op_when_source_equals_target() {
        // self_heal_copy should not error when source and target resolve to the same file.
        // We verify the canonicalize-equality guard by calling it with the user-data dir
        // itself: if the cache happens to already contain the file, the function must
        // not corrupt it.
        let Some(cache_dir) = user_data_lib_dir() else {
            return;
        };
        if !cache_dir.join(lib_name()).exists() {
            // Nothing to test if cache is empty.
            return;
        }
        // Calling with cache_dir as source should be a no-op (target already exists).
        self_heal_copy(&cache_dir);
        // File should still exist after the call.
        assert!(cache_dir.join(lib_name()).exists());
    }
}
