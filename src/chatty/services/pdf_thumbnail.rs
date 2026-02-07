use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

const THUMBNAIL_SIZE: u32 = 64;

#[derive(Debug)]
pub enum PdfThumbnailError {
    Pdfium(String),
    Io(std::io::Error),
    Image(String),
}

impl std::fmt::Display for PdfThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfThumbnailError::Pdfium(e) => write!(f, "Pdfium error: {}", e),
            PdfThumbnailError::Io(e) => write!(f, "IO error: {}", e),
            PdfThumbnailError::Image(e) => write!(f, "Image error: {}", e),
        }
    }
}

impl From<PdfiumError> for PdfThumbnailError {
    fn from(err: PdfiumError) -> Self {
        PdfThumbnailError::Pdfium(format!("{:?}", err))
    }
}

impl From<std::io::Error> for PdfThumbnailError {
    fn from(err: std::io::Error) -> Self {
        PdfThumbnailError::Io(err)
    }
}

impl From<image::ImageError> for PdfThumbnailError {
    fn from(err: image::ImageError) -> Self {
        PdfThumbnailError::Image(format!("{:?}", err))
    }
}

/// Get the path to the pdfium library set by build.rs
fn pdfium_lib_path() -> Option<PathBuf> {
    let lib_dir = option_env!("PDFIUM_LIB_DIR")?;
    Some(PathBuf::from(lib_dir))
}

/// Render PDF first page to a temporary thumbnail PNG file
pub fn render_pdf_thumbnail(pdf_path: &Path) -> Result<PathBuf, PdfThumbnailError> {
    let lib_dir = pdfium_lib_path().ok_or_else(|| {
        PdfThumbnailError::Pdfium("PDFIUM_LIB_DIR not set by build.rs".to_string())
    })?;

    // Construct full library path directly (avoids issues with special chars in path)
    let lib_path = lib_dir.join(Pdfium::pdfium_platform_library_name());
    let bindings = Pdfium::bind_to_library(&lib_path)
        .or_else(|_| Pdfium::bind_to_system_library())
        .map_err(|e| PdfThumbnailError::Pdfium(format!("Failed to bind pdfium: {:?}", e)))?;

    let pdfium = Pdfium::new(bindings);

    let document = pdfium.load_pdf_from_file(pdf_path, None)?;
    let page = document.pages().get(0)?;

    let render_config = PdfRenderConfig::new()
        .set_target_width(THUMBNAIL_SIZE as i32)
        .set_maximum_height(THUMBNAIL_SIZE as i32);

    let bitmap = page.render_with_config(&render_config)?;
    let image = bitmap.as_image();

    // Use a unique temp file per PDF path to support concurrent thumbnails
    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        pdf_path.hash(&mut hasher);
        hasher.finish()
    };

    let mut temp_path = std::env::temp_dir();
    temp_path.push(format!("chatty_pdf_thumb_{:x}.png", hash));

    image.save_with_format(&temp_path, image::ImageFormat::Png)?;

    Ok(temp_path)
}
