mod read;

pub use read::ReadPptxTool;

#[derive(Debug, thiserror::Error)]
pub enum PptxToolError {
    #[error("PPTX error: {0}")]
    OperationError(#[from] anyhow::Error),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rig::tool::Tool;

    use crate::services::filesystem_service::FileSystemService;

    use super::read::ReadPptxArgs;
    use super::*;

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadPptxTool::new(service);
        let result = tool
            .call(ReadPptxArgs {
                path: tmp.path().join("nope.pptx").to_str().unwrap().to_string(),
                include_notes: None,
                max_chars: None,
            })
            .await;
        assert!(result.is_err());
    }
}
