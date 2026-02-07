#[cfg(test)]
mod tests {
    use super::super::pdf_thumbnail::{
        cleanup_thumbnails, get_thumbnail_dir, render_pdf_thumbnail, PdfThumbnailError,
    };
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    /// Helper to create a minimal valid PDF file for testing
    fn create_test_pdf(path: &PathBuf) -> std::io::Result<()> {
        // Minimal PDF structure with one empty page
        let pdf_content = b"%PDF-1.4
1 0 obj
<<
/Type /Catalog
/Pages 2 0 R
>>
endobj
2 0 obj
<<
/Type /Pages
/Kids [3 0 R]
/Count 1
>>
endobj
3 0 obj
<<
/Type /Page
/Parent 2 0 R
/MediaBox [0 0 612 792]
/Contents 4 0 R
/Resources <<
/ProcSet [/PDF /Text]
>>
>>
endobj
4 0 obj
<<
/Length 44
>>
stream
BT
/F1 12 Tf
100 700 Td
(Test) Tj
ET
endstream
endobj
xref
0 5
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
0000000115 00000 n
0000000261 00000 n
trailer
<<
/Size 5
/Root 1 0 R
>>
startxref
354
%%EOF";

        let mut file = fs::File::create(path)?;
        file.write_all(pdf_content)?;
        Ok(())
    }

    /// Helper to create an invalid PDF file
    fn create_invalid_pdf(path: &PathBuf) -> std::io::Result<()> {
        let mut file = fs::File::create(path)?;
        file.write_all(b"This is not a valid PDF file")?;
        Ok(())
    }

    #[test]
    fn test_render_valid_pdf() {
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("test_valid.pdf");

        // Create a valid PDF
        create_test_pdf(&pdf_path).expect("Failed to create test PDF");

        // Render thumbnail
        let result = render_pdf_thumbnail(&pdf_path);

        // Clean up test PDF
        let _ = fs::remove_file(&pdf_path);

        // Assertions
        assert!(
            result.is_ok(),
            "Expected valid PDF to render successfully"
        );

        let thumbnail_path = result.unwrap();
        assert!(
            thumbnail_path.exists(),
            "Thumbnail file should exist at {:?}",
            thumbnail_path
        );
        assert_eq!(
            thumbnail_path.extension().and_then(|s| s.to_str()),
            Some("png"),
            "Thumbnail should be a PNG file"
        );

        // Verify it's in the session temp directory
        let session_dir = get_thumbnail_dir().expect("Should get thumbnail dir");
        assert!(
            thumbnail_path.starts_with(&session_dir),
            "Thumbnail should be in session directory"
        );

        // Clean up
        cleanup_thumbnails();
    }

    #[test]
    fn test_render_invalid_pdf() {
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("test_invalid.pdf");

        // Create an invalid PDF
        create_invalid_pdf(&pdf_path).expect("Failed to create invalid PDF");

        // Attempt to render thumbnail
        let result = render_pdf_thumbnail(&pdf_path);

        // Clean up test file
        let _ = fs::remove_file(&pdf_path);

        // Assertions
        assert!(result.is_err(), "Expected invalid PDF to return an error");

        match result.unwrap_err() {
            PdfThumbnailError::Pdfium(_) => {
                // Expected error type
            }
            other => panic!("Expected PdfiumError, got {:?}", other),
        }

        cleanup_thumbnails();
    }

    #[test]
    fn test_render_missing_file() {
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("nonexistent_file.pdf");

        // Ensure file doesn't exist
        let _ = fs::remove_file(&pdf_path);

        // Attempt to render thumbnail
        let result = render_pdf_thumbnail(&pdf_path);

        // Assertions
        assert!(result.is_err(), "Expected missing file to return an error");

        // Should be either IO error or Pdfium error depending on implementation
        assert!(
            matches!(
                result.unwrap_err(),
                PdfThumbnailError::Io(_) | PdfThumbnailError::Pdfium(_)
            ),
            "Expected IO or Pdfium error for missing file"
        );

        cleanup_thumbnails();
    }

    #[test]
    fn test_thumbnail_dir_creation() {
        // Clear any existing thumbnail dir
        cleanup_thumbnails();

        // Get thumbnail directory (should create it)
        let dir_result = get_thumbnail_dir();
        assert!(dir_result.is_ok(), "Should successfully create temp dir");

        let dir = dir_result.unwrap();
        assert!(dir.exists(), "Thumbnail directory should exist");
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "Should be in system temp directory"
        );

        // Verify directory name contains session ID
        let dir_name = dir.file_name().unwrap().to_string_lossy();
        assert!(
            dir_name.starts_with("chatty_thumbnails_"),
            "Directory should have correct prefix"
        );

        cleanup_thumbnails();
    }

    #[test]
    fn test_cleanup_thumbnails() {
        // Create a thumbnail to verify cleanup
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("test_cleanup.pdf");
        create_test_pdf(&pdf_path).expect("Failed to create test PDF");

        let thumbnail_result = render_pdf_thumbnail(&pdf_path);
        assert!(thumbnail_result.is_ok());

        let thumbnail_path = thumbnail_result.unwrap();
        let session_dir = thumbnail_path.parent().unwrap().to_path_buf();

        assert!(thumbnail_path.exists(), "Thumbnail should exist before cleanup");
        assert!(session_dir.exists(), "Session dir should exist before cleanup");

        // Cleanup
        cleanup_thumbnails();

        // Verify cleanup
        assert!(
            !session_dir.exists(),
            "Session directory should be removed after cleanup"
        );
        assert!(
            !thumbnail_path.exists(),
            "Thumbnail should be removed after cleanup"
        );

        // Clean up test PDF
        let _ = fs::remove_file(&pdf_path);
    }

    #[test]
    fn test_multiple_thumbnails_unique_names() {
        let temp_dir = std::env::temp_dir();
        let pdf_path1 = temp_dir.join("test_unique1.pdf");
        let pdf_path2 = temp_dir.join("test_unique2.pdf");

        create_test_pdf(&pdf_path1).expect("Failed to create PDF 1");
        create_test_pdf(&pdf_path2).expect("Failed to create PDF 2");

        let thumb1 = render_pdf_thumbnail(&pdf_path1).expect("Failed to render PDF 1");
        let thumb2 = render_pdf_thumbnail(&pdf_path2).expect("Failed to render PDF 2");

        // Verify both thumbnails exist and have different names
        assert!(thumb1.exists());
        assert!(thumb2.exists());
        assert_ne!(
            thumb1, thumb2,
            "Thumbnails for different PDFs should have different names"
        );

        // Clean up
        let _ = fs::remove_file(&pdf_path1);
        let _ = fs::remove_file(&pdf_path2);
        cleanup_thumbnails();
    }

    #[test]
    fn test_thumbnail_idempotency() {
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("test_idempotent.pdf");

        create_test_pdf(&pdf_path).expect("Failed to create test PDF");

        // Render thumbnail twice with the same PDF
        let thumb1 = render_pdf_thumbnail(&pdf_path).expect("Failed first render");
        let thumb2 = render_pdf_thumbnail(&pdf_path).expect("Failed second render");

        // Should produce the same path (hash-based naming)
        assert_eq!(
            thumb1, thumb2,
            "Same PDF should produce same thumbnail path"
        );

        // Clean up
        let _ = fs::remove_file(&pdf_path);
        cleanup_thumbnails();
    }
}
