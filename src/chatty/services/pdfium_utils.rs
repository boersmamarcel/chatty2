use pdfium_render::prelude::*;
use std::path::PathBuf;

/// Get the path to the pdfium library directory set by build.rs.
pub fn pdfium_lib_path() -> Option<PathBuf> {
    let lib_dir = option_env!("PDFIUM_LIB_DIR")?;
    Some(PathBuf::from(lib_dir))
}

/// Create a [`Pdfium`] instance bound to the bundled library, falling back to the system library.
///
/// Tries the build-time `PDFIUM_LIB_DIR` path first; if that is unset or the load fails, falls
/// back to whatever pdfium is available on the system library path.
pub fn create_pdfium() -> anyhow::Result<Pdfium> {
    let bindings = if let Some(lib_dir) = pdfium_lib_path() {
        let lib_path = lib_dir.join(Pdfium::pdfium_platform_library_name());
        Pdfium::bind_to_library(&lib_path).or_else(|_| Pdfium::bind_to_system_library())
    } else {
        Pdfium::bind_to_system_library()
    }
    .map_err(|e| anyhow::anyhow!("Failed to bind pdfium: {:?}", e))?;

    Ok(Pdfium::new(bindings))
}
