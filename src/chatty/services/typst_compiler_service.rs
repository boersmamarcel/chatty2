use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, Source, VirtualPath};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_pdf::PdfOptions;

/// Full World implementation for compiling complete Typst documents.
///
/// Unlike `MathWorld` (used for inline math rendering), this supports:
/// - Arbitrary typst source content (headings, tables, figures, math, etc.)
/// - Loading additional files from a base directory (`#include`, `#import`, images)
/// - Real system date for `today()`
struct FullWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main_id: FileId,
    source: Source,
    base_dir: Option<PathBuf>,
}

impl FullWorld {
    fn new(content: &str, base_dir: Option<&Path>) -> Self {
        let library = LazyHash::new(Library::builder().build());

        let fonts = typst_assets::fonts()
            .filter_map(|data| Font::new(Bytes::new(data), 0))
            .collect::<Vec<_>>();

        let book = LazyHash::new(FontBook::from_fonts(fonts.iter()));

        let main_id = FileId::new(None, VirtualPath::new("main.typ"));
        let source = Source::new(main_id, content.to_string());

        Self {
            library,
            book,
            fonts,
            main_id,
            source,
            base_dir: base_dir.map(|p| p.to_path_buf()),
        }
    }

    fn resolve_path(&self, id: FileId) -> Option<PathBuf> {
        let base = self.base_dir.as_deref()?;
        let rel = id.vpath().as_rootless_path();
        Some(base.join(rel))
    }
}

impl World for FullWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main_id
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main_id {
            return Ok(self.source.clone());
        }

        let path = self
            .resolve_path(id)
            .ok_or_else(|| FileError::NotFound(id.vpath().as_rootless_path().into()))?;

        let text = std::fs::read_to_string(&path)
            .map_err(|_| FileError::NotFound(id.vpath().as_rootless_path().into()))?;

        Ok(Source::new(id, text))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let path = self
            .resolve_path(id)
            .ok_or_else(|| FileError::NotFound(id.vpath().as_rootless_path().into()))?;

        let bytes = std::fs::read(&path)
            .map_err(|_| FileError::NotFound(id.vpath().as_rootless_path().into()))?;

        Ok(Bytes::new(bytes))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, offset: Option<i64>) -> Option<Datetime> {
        use chrono::{Datelike, Local, Utc};

        let now = if let Some(offset_hours) = offset {
            let offset_secs = (offset_hours * 3600) as i32;
            let fixed = chrono::FixedOffset::east_opt(offset_secs)?;
            Utc::now().with_timezone(&fixed).naive_local()
        } else {
            Local::now().naive_local()
        };

        Datetime::from_ymd(now.year(), now.month() as u8, now.day() as u8)
    }
}

/// Service for compiling full Typst documents to PDF.
pub struct TypstCompilerService;

impl TypstCompilerService {
    /// Compile typst source content to PDF bytes.
    ///
    /// `base_dir` is used to resolve `#include` and image references. If `None`,
    /// only inline content is supported (no file references).
    ///
    /// Returns the PDF bytes and the number of pages produced.
    pub fn compile_to_pdf(content: &str, base_dir: Option<&Path>) -> Result<(Vec<u8>, u32)> {
        let world = FullWorld::new(content, base_dir);

        // Compile the typst source
        let warned_result = typst::compile::<typst::layout::PagedDocument>(&world);

        let document = warned_result.output.map_err(|errors| {
            let messages: Vec<String> = errors.iter().map(|e| e.message.to_string()).collect();
            anyhow::anyhow!("Typst compilation failed:\n{}", messages.join("\n"))
        })?;

        let page_count = document.pages.len() as u32;

        // Export to PDF
        let pdf_bytes = typst_pdf::pdf(&document, &PdfOptions::default())
            .map_err(|errors| {
                let messages: Vec<String> = errors.iter().map(|e| e.message.to_string()).collect();
                anyhow::anyhow!("PDF export failed:\n{}", messages.join("\n"))
            })
            .context("Failed to export Typst document to PDF")?;

        Ok((pdf_bytes, page_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_simple_document() {
        let content = "= Hello World\n\nThis is a simple typst document.";
        let (pdf, pages) = TypstCompilerService::compile_to_pdf(content, None).unwrap();
        assert!(!pdf.is_empty());
        assert_eq!(pages, 1);
        // PDF files start with %PDF-
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn test_compile_document_with_math() {
        let content = "= Math Document\n\n$ E = m c^2 $\n\n$ sum_(i=0)^n i = (n(n+1))/2 $";
        let (pdf, pages) = TypstCompilerService::compile_to_pdf(content, None).unwrap();
        assert!(!pdf.is_empty());
        assert_eq!(pages, 1);
    }

    #[test]
    fn test_compile_invalid_typst_returns_error() {
        // Invalid typst syntax should produce an error
        let content = "#invalid-function-that-does-not-exist()";
        let result = TypstCompilerService::compile_to_pdf(content, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Typst compilation failed") || err.contains("unknown variable"));
    }

    #[test]
    fn test_compile_multipage_document() {
        let content = "#set page(height: 50pt)\n\n= Page 1\n\nSome content here.\n\n= Page 2\n\nMore content.";
        let (pdf, pages) = TypstCompilerService::compile_to_pdf(content, None).unwrap();
        assert!(!pdf.is_empty());
        assert!(pages >= 1);
    }
}
