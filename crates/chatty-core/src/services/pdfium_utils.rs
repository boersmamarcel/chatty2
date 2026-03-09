use pdfium_render::prelude::*;
use std::path::PathBuf;

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
fn exe_relative_lib_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    if cfg!(target_os = "macos") {
        // Contents/MacOS/<exe> → Contents/Frameworks/
        let frameworks = exe_dir.join("../Frameworks");
        if frameworks.is_dir() {
            return Some(frameworks);
        }
    }

    // Linux AppImage: usr/bin/<exe> → usr/lib/
    // Also works as a generic fallback for other layouts
    let lib_dir = exe_dir.join("../lib");
    if lib_dir.is_dir() {
        return Some(lib_dir);
    }

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
        match Pdfium::bind_to_library(&lib_path) {
            Ok(bindings) => return Ok(Pdfium::new(bindings)),
            Err(e) => last_err = Some(e),
        }
    }

    // Final fallback: system library
    Pdfium::bind_to_system_library()
        .map(Pdfium::new)
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to bind pdfium (last candidate error: {:?}, system error: {:?})",
                last_err,
                e
            )
        })
}
