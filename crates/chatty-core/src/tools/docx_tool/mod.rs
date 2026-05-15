mod read;
mod write;

pub use read::ReadDocxTool;
pub use write::WriteDocxTool;

#[derive(Debug, thiserror::Error)]
pub enum DocxToolError {
    #[error("DOCX error: {0}")]
    OperationError(#[from] anyhow::Error),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rig::tool::Tool;

    use crate::services::filesystem_service::FileSystemService;

    use super::read::ReadDocxArgs;
    use super::write::WriteDocxArgs;
    use super::*;

    #[tokio::test]
    async fn test_write_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );

        let path = tmp.path().join("test.docx").to_str().unwrap().to_string();

        // Write a document with heading + paragraph + table
        let write_tool = WriteDocxTool::new(service.clone());
        let write_output = write_tool
            .call(WriteDocxArgs {
                path: path.clone(),
                content: "# Introduction\n\nHello, world!\n\n## Section 2\n\nSome text here.\n\n| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob | 25 |".to_string(),
            })
            .await
            .unwrap();

        assert!(write_output.bytes_written > 0);
        assert!(std::path::Path::new(&path).exists());

        // Read it back
        let read_tool = ReadDocxTool::new(service.clone());
        let read_output = read_tool
            .call(ReadDocxArgs {
                path: path.clone(),
                include_tables: None,
                max_chars: None,
            })
            .await
            .unwrap();

        assert!(read_output.char_count > 0);
        assert!(read_output.text.contains("Introduction"));
        assert!(read_output.text.contains("Hello, world!"));
        assert!(read_output.text.contains("Section 2"));
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadDocxTool::new(service);
        let result = tool
            .call(ReadDocxArgs {
                path: tmp.path().join("nope.docx").to_str().unwrap().to_string(),
                include_tables: None,
                max_chars: None,
            })
            .await;
        assert!(result.is_err());
    }
}
