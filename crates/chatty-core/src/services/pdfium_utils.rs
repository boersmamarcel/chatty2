use pdfium_render::prelude::*;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Get the path to the pdfium library directory set by build.rs (compile-time path).
fn compile_time_lib_path() -> Option<PathBuf> {
    let lib_dir = option_env!("PDFIUM_LIB_DIR")?;
    Some(PathBuf::from(lib_dir))
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
            if frameworks.is_dir() {
                return Some(frameworks);
            }
        }
        // Fallback: raw (non-canonical) path
        if let Some(exe_dir) = exe.parent() {
            let frameworks = exe_dir.join("../Frameworks");
            debug!(path = %frameworks.display(), "pdfium: trying raw Frameworks path");
            if frameworks.is_dir() {
                return Some(frameworks);
            }
        }
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
        if lib_dir.is_dir() {
            return Some(lib_dir);
        }
    }

    // Final fallback: library next to the executable (non-standard installations)
    let exe_for_fallback = canonical_exe.as_ref().unwrap_or(&exe);
    if let Some(exe_dir) = exe_for_fallback.parent() {
        let lib_name = Pdfium::pdfium_platform_library_name();
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
/// 1. Executable-relative path (app bundle / AppImage)
/// 2. Compile-time `PDFIUM_LIB_DIR` (development builds)
/// 3. System library fallback
pub fn create_pdfium() -> anyhow::Result<Pdfium> {
    let lib_name = Pdfium::pdfium_platform_library_name();

    let candidate_dirs = [exe_relative_lib_path(), compile_time_lib_path()];

    let mut last_err = None;
    for dir in candidate_dirs.into_iter().flatten() {
        let lib_path = dir.join(&lib_name);
        debug!(path = %lib_path.display(), "pdfium: trying candidate library");
        match Pdfium::bind_to_library(&lib_path) {
            Ok(bindings) => {
                info!(path = %lib_path.display(), "pdfium: successfully bound library");
                return Ok(Pdfium::new(bindings));
            }
            Err(e) => {
                debug!(path = %lib_path.display(), error = ?e, "pdfium: candidate failed");
                last_err = Some(e);
            }
        }
    }

    // Final fallback: system library
    debug!("pdfium: trying system library fallback");
    Pdfium::bind_to_system_library()
        .map(|bindings| {
            info!("pdfium: successfully bound system library");
            Pdfium::new(bindings)
        })
        .map_err(|e| {
            warn!(
                last_candidate_error = ?last_err,
                system_error = ?e,
                "pdfium: all binding attempts failed"
            );
            anyhow::anyhow!(
                "Failed to bind pdfium (last candidate error: {:?}, system error: {:?})",
                last_err,
                e
            )
        })
}
