//! Rasterise `.pptx` slides for the artifact panel (AGE-343).
//!
//! `rpptx` has no public per-slide image API, so a deck is rendered once to a
//! PDF and then paged with the pdfium rasteriser [`super::pdf_thumbnail`]
//! already runs for PDF artifacts. Slide N is page N, so the panel's existing
//! page chrome works unchanged.
//!
//! The intermediate PDF is cached per (path, mtime) in the session temp
//! directory, so turning a slide is a pdfium raster, not a re-render of the
//! whole deck.

use std::path::{Path, PathBuf};

use super::pdf_thumbnail::{PdfThumbnailError, get_thumbnail_dir, path_hash, render_pdf_page};

#[derive(Debug, thiserror::Error)]
pub enum PptxRenderError {
    #[error("Could not read the presentation: {0}")]
    Deck(String),
    /// Not `#[from]`: `PdfThumbnailError` carries a `Display` impl but not
    /// `std::error::Error`, which thiserror's source plumbing would require.
    #[error("{0}")]
    Raster(PdfThumbnailError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<PdfThumbnailError> for PptxRenderError {
    fn from(error: PdfThumbnailError) -> Self {
        Self::Raster(error)
    }
}

/// Number of slides in the deck.
pub fn slide_count(pptx_path: &Path) -> Result<u32, PptxRenderError> {
    Ok(open(pptx_path)?.len() as u32)
}

/// Render one slide to a PNG in the session temp directory.
///
/// `slide_idx` is zero-based. The returned path is stable for the same
/// (deck, mtime, slide, width), so a second call is a cache hit.
pub fn render_slide(
    pptx_path: &Path,
    slide_idx: u32,
    target_width: u32,
) -> Result<PathBuf, PptxRenderError> {
    let pdf = deck_pdf(pptx_path)?;
    Ok(render_pdf_page(&pdf, slide_idx, target_width)?)
}

/// Render the deck to a PDF once and keep it for the life of the session.
///
/// Keyed by modification time as well as path so rewriting a deck in place —
/// which `write_pptx` does on every revision — does not serve stale slides.
fn deck_pdf(pptx_path: &Path) -> Result<PathBuf, PptxRenderError> {
    let pdf = get_thumbnail_dir()?.join(deck_pdf_name(pptx_path));
    if pdf.exists() {
        return Ok(pdf);
    }

    let bytes = open(pptx_path)?
        .to_pdf_deterministic()
        .map_err(|e| PptxRenderError::Deck(e.to_string()))?;
    std::fs::write(&pdf, bytes)?;
    Ok(pdf)
}

/// Cache file name for a deck's rendered PDF: path hash plus modification
/// time, so an in-place rewrite lands on a different file.
fn deck_pdf_name(pptx_path: &Path) -> String {
    let hash = path_hash(pptx_path);
    let stamp = std::fs::metadata(pptx_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |age| age.as_millis());
    format!("deck_{}_{}.pdf", &hash[..12.min(hash.len())], stamp)
}

fn open(pptx_path: &Path) -> Result<rpptx::Presentation, PptxRenderError> {
    rpptx::Presentation::open(pptx_path).map_err(|e| PptxRenderError::Deck(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rig_agent::tool::{Tool, ToolContext};

    use super::*;
    use crate::services::filesystem_service::FileSystemService;
    use crate::tools::pptx_tool::WritePptxTool;
    use crate::tools::pptx_tool::write::{PptxShapeSpec, PptxSlideSpec, WritePptxArgs};

    /// A file that is not a deck must surface as an error, not a panic — the
    /// artifact panel turns this into its muted one-liner.
    #[test]
    fn a_non_deck_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fake = dir.path().join("not-a-deck.pptx");
        std::fs::write(&fake, b"hello slides").expect("write");

        assert!(slide_count(&fake).is_err());
        assert!(render_slide(&fake, 0, 720).is_err());
    }

    /// `write_pptx` rewrites a deck in place, so the cached PDF has to be
    /// keyed by more than the path or the panel keeps showing the old slides.
    #[test]
    fn rewriting_a_deck_changes_the_cache_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let deck = dir.path().join("deck.pptx");
        std::fs::write(&deck, b"v1").expect("write");
        let first = deck_pdf_name(&deck);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&deck, b"v2").expect("rewrite");

        assert_ne!(first, deck_pdf_name(&deck));
    }

    /// The whole path, on a deck we produced ourselves: open, render, page.
    ///
    /// Slide N has to land on page N, because the panel's pager assumes it —
    /// so the deck is written with a different slide count than 1 and the
    /// last slide is the one rastered.
    #[tokio::test]
    async fn a_written_deck_renders_slide_by_slide() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().expect("utf-8 temp dir"))
                .await
                .expect("filesystem service"),
        );
        let deck = tmp.path().join("deck.pptx");
        write_three_slides(&service, &deck).await;

        assert_eq!(slide_count(&deck).expect("slide count"), 3);

        let last = render_slide(&deck, 2, 720).expect("render the third slide");
        let bytes = std::fs::read(&last).expect("read the raster");
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "the panel hands this straight to gpui's img()"
        );

        assert!(
            render_slide(&deck, 3, 720).is_err(),
            "a slide past the end is an error, not a blank page"
        );
    }

    async fn write_three_slides(service: &Arc<FileSystemService>, deck: &Path) {
        let slides = ["One", "Two", "Three"]
            .into_iter()
            .map(|title| PptxSlideSpec {
                title: Some(title.to_string()),
                shapes: vec![PptxShapeSpec::BulletList {
                    x: 0.8,
                    y: 1.7,
                    width: 8.0,
                    height: 2.2,
                    items: vec![format!("Body of {title}")],
                    style: None,
                }],
            })
            .collect();

        WritePptxTool::new(service.clone())
            .call(
                &mut ToolContext::new(),
                WritePptxArgs {
                    path: deck.to_str().expect("utf-8 deck path").to_string(),
                    slides,
                },
            )
            .await
            .expect("write the deck");
    }

    /// Two decks never share a cache file, even at the same mtime.
    #[test]
    fn two_decks_do_not_share_a_cache_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let one = dir.path().join("one.pptx");
        let two = dir.path().join("two.pptx");
        std::fs::write(&one, b"deck").expect("write");
        std::fs::write(&two, b"deck").expect("write");

        assert_ne!(deck_pdf_name(&one), deck_pdf_name(&two));
    }
}
