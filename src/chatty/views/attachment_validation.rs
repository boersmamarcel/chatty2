//! Attachment validation logic
//!
//! This module provides validation functions for file attachments,
//! checking file size, extension, and existence.

use std::path::Path;

pub const MAX_FILE_SIZE: u64 = 5_242_880; // 5MB
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];
pub const PDF_EXTENSION: &str = "pdf";

#[derive(Debug, PartialEq, Eq)]
pub enum ValidationError {
    FileTooLarge { size: u64, max: u64 },
    UnsupportedExtension(String),
    NoExtension,
    FileNotFound,
}

/// Validate a file for attachment
pub fn validate_attachment(path: &Path) -> Result<(), ValidationError> {
    // Check if file exists
    let metadata = std::fs::metadata(path).map_err(|_| ValidationError::FileNotFound)?;

    // Check file size
    let size = metadata.len();
    if size > MAX_FILE_SIZE {
        return Err(ValidationError::FileTooLarge {
            size,
            max: MAX_FILE_SIZE,
        });
    }

    // Check extension
    let ext = path
        .extension()
        .ok_or(ValidationError::NoExtension)?
        .to_string_lossy()
        .to_lowercase();

    if !is_supported_extension(&ext) {
        return Err(ValidationError::UnsupportedExtension(ext.to_string()));
    }

    Ok(())
}

/// Check if an extension is supported
pub fn is_supported_extension(ext: &str) -> bool {
    let ext_lower = ext.to_lowercase();
    IMAGE_EXTENSIONS.contains(&ext_lower.as_str()) || ext_lower == PDF_EXTENSION
}

/// Check if a file is an image based on extension
pub fn is_image_extension(ext: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// Check if a file is a PDF based on extension
#[allow(dead_code)]
pub fn is_pdf_extension(ext: &str) -> bool {
    ext.to_lowercase() == PDF_EXTENSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn create_test_file(path: &Path, size: u64) -> std::io::Result<()> {
        let mut file = fs::File::create(path)?;
        let data = vec![0u8; size as usize];
        file.write_all(&data)?;
        Ok(())
    }

    #[test]
    fn test_validate_valid_image() {
        let temp_dir = std::env::temp_dir();
        let image_path = temp_dir.join("test_valid_image.png");

        create_test_file(&image_path, 1024).expect("Failed to create test file");

        let result = validate_attachment(&image_path);
        assert!(result.is_ok(), "Valid image should pass validation");

        let _ = fs::remove_file(&image_path);
    }

    #[test]
    fn test_validate_valid_pdf() {
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("test_valid_document.pdf");

        create_test_file(&pdf_path, 2048).expect("Failed to create test file");

        let result = validate_attachment(&pdf_path);
        assert!(result.is_ok(), "Valid PDF should pass validation");

        let _ = fs::remove_file(&pdf_path);
    }

    #[test]
    fn test_validate_file_too_large() {
        let temp_dir = std::env::temp_dir();
        let large_path = temp_dir.join("test_large.jpg");

        create_test_file(&large_path, MAX_FILE_SIZE + 1).expect("Failed to create test file");

        let result = validate_attachment(&large_path);
        assert!(matches!(result, Err(ValidationError::FileTooLarge { .. })));

        let _ = fs::remove_file(&large_path);
    }

    #[test]
    fn test_validate_file_at_size_limit() {
        let temp_dir = std::env::temp_dir();
        let max_path = temp_dir.join("test_max_size.png");

        create_test_file(&max_path, MAX_FILE_SIZE).expect("Failed to create test file");

        let result = validate_attachment(&max_path);
        assert!(result.is_ok(), "File at exactly 5MB should pass");

        let _ = fs::remove_file(&max_path);
    }

    #[test]
    fn test_validate_unsupported_extension() {
        let temp_dir = std::env::temp_dir();
        let txt_path = temp_dir.join("test_unsupported.txt");

        create_test_file(&txt_path, 1024).expect("Failed to create test file");

        let result = validate_attachment(&txt_path);
        assert!(matches!(
            result,
            Err(ValidationError::UnsupportedExtension(_))
        ));

        let _ = fs::remove_file(&txt_path);
    }

    #[test]
    fn test_validate_no_extension() {
        let temp_dir = std::env::temp_dir();
        let no_ext_path = temp_dir.join("test_no_extension");

        create_test_file(&no_ext_path, 1024).expect("Failed to create test file");

        let result = validate_attachment(&no_ext_path);
        assert_eq!(result, Err(ValidationError::NoExtension));

        let _ = fs::remove_file(&no_ext_path);
    }

    #[test]
    fn test_validate_nonexistent_file() {
        let temp_dir = std::env::temp_dir();
        let nonexistent = temp_dir.join("nonexistent_file.png");

        let _ = fs::remove_file(&nonexistent);

        let result = validate_attachment(&nonexistent);
        assert_eq!(result, Err(ValidationError::FileNotFound));
    }

    #[test]
    fn test_is_supported_extension_images() {
        for ext in IMAGE_EXTENSIONS {
            assert!(
                is_supported_extension(ext),
                "Image extension {} should be supported",
                ext
            );
        }
    }

    #[test]
    fn test_is_supported_extension_pdf() {
        assert!(is_supported_extension("pdf"));
        assert!(is_supported_extension("PDF"));
        assert!(is_supported_extension("Pdf"));
    }

    #[test]
    fn test_is_supported_extension_unsupported() {
        assert!(!is_supported_extension("txt"));
        assert!(!is_supported_extension("doc"));
        assert!(!is_supported_extension("exe"));
    }

    #[test]
    fn test_is_supported_extension_case_insensitive() {
        assert!(is_supported_extension("PNG"));
        assert!(is_supported_extension("JpG"));
        assert!(is_supported_extension("JPEG"));
    }

    #[test]
    fn test_is_image_extension() {
        assert!(is_image_extension("png"));
        assert!(is_image_extension("jpg"));
        assert!(is_image_extension("PNG"));
        assert!(!is_image_extension("pdf"));
        assert!(!is_image_extension("txt"));
    }

    #[test]
    fn test_is_pdf_extension() {
        assert!(is_pdf_extension("pdf"));
        assert!(is_pdf_extension("PDF"));
        assert!(is_pdf_extension("Pdf"));
        assert!(!is_pdf_extension("png"));
        assert!(!is_pdf_extension("txt"));
    }

    #[test]
    fn test_all_image_extensions_valid() {
        let temp_dir = std::env::temp_dir();

        for ext in IMAGE_EXTENSIONS {
            let path = temp_dir.join(format!("test.{}", ext));
            create_test_file(&path, 512).expect("Failed to create test file");

            let result = validate_attachment(&path);
            assert!(
                result.is_ok(),
                "Extension {} should be valid, got {:?}",
                ext,
                result
            );

            let _ = fs::remove_file(&path);
        }
    }

    #[test]
    fn test_validation_error_display() {
        let err = ValidationError::FileTooLarge {
            size: 10_000_000,
            max: MAX_FILE_SIZE,
        };
        assert!(matches!(err, ValidationError::FileTooLarge { .. }));

        let err = ValidationError::UnsupportedExtension("txt".to_string());
        assert!(matches!(err, ValidationError::UnsupportedExtension(_)));

        let err = ValidationError::NoExtension;
        assert_eq!(err, ValidationError::NoExtension);

        let err = ValidationError::FileNotFound;
        assert_eq!(err, ValidationError::FileNotFound);
    }
}
