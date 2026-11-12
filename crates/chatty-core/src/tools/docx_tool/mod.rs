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

    use rig_agent::tool::{Tool, ToolContext};

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
            .call(&mut ToolContext::new(), WriteDocxArgs {
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
            .call(
                &mut ToolContext::new(),
                ReadDocxArgs {
                    path: path.clone(),
                    include_tables: None,
                    max_chars: None,
                },
            )
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
            .call(
                &mut ToolContext::new(),
                ReadDocxArgs {
                    path: tmp.path().join("nope.docx").to_str().unwrap().to_string(),
                    include_tables: None,
                    max_chars: None,
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_docx_refuses_a_non_docx_path() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let main_rs = tmp.path().join("main.rs");
        std::fs::write(&main_rs, "fn main() {}").unwrap();

        let tool = WriteDocxTool::new(service.clone());
        let err = tool
            .call(
                &mut ToolContext::new(),
                WriteDocxArgs {
                    path: main_rs.to_str().unwrap().to_string(),
                    content: "# Hi".to_string(),
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains(".docx"));
        assert_eq!(std::fs::read_to_string(&main_rs).unwrap(), "fn main() {}");

        let upper = tmp.path().join("report.DOCX");
        tool.call(
            &mut ToolContext::new(),
            WriteDocxArgs {
                path: upper.to_str().unwrap().to_string(),
                content: "# Hi".to_string(),
            },
        )
        .await
        .unwrap();
        assert!(upper.exists());
    }

    #[tokio::test]
    async fn write_docx_asks_before_writing_under_always_ask() {
        use std::time::Duration;

        use crate::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
        use crate::settings::models::execution_settings::ApprovalMode;

        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let target = tmp.path().join("report.docx");
        let path = target.to_str().unwrap().to_string();

        let mut store = WriteApprovalStore::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (resolution_tx, _resolution_rx) = tokio::sync::mpsc::unbounded_channel();
        store.set_notifiers(tx, resolution_tx);
        let tool = WriteDocxTool::new(service)
            .with_write_approval(ApprovalMode::AlwaysAsk, store.get_pending_approvals());

        // Denied: the call fails and nothing is written.
        let denied = {
            let tool = tool.clone();
            let path = path.clone();
            tokio::spawn(async move {
                tool.call(
                    &mut ToolContext::new(),
                    WriteDocxArgs {
                        path,
                        content: "# Report\n\nBody".to_string(),
                    },
                )
                .await
                .map(|_| ())
            })
        };
        let asked = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the write is asked about")
            .expect("notifier open");
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Denied));
        assert!(denied.await.unwrap().is_err(), "a denied write fails");
        assert!(!target.exists(), "a denied write writes nothing");

        // Approved: the call succeeds and the file exists.
        let approved = {
            let tool = tool.clone();
            let path = path.clone();
            tokio::spawn(async move {
                tool.call(
                    &mut ToolContext::new(),
                    WriteDocxArgs {
                        path,
                        content: "# Report\n\nBody".to_string(),
                    },
                )
                .await
                .map(|_| ())
            })
        };
        let asked = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the second write is asked about")
            .expect("notifier open");
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Approved));
        approved.await.unwrap().unwrap();
        assert!(target.exists(), "an approved write is written");
    }

    /// AGE-1113: a symlink at the target (out of the workspace, or
    /// dangling) is refused and nothing is written where it points; the
    /// approval card shows the link as an overwrite, not as its target.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_docx_refuses_a_symlink_at_the_target() {
        use std::time::Duration;

        use crate::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
        use crate::settings::models::execution_settings::ApprovalMode;

        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.docx");
        std::fs::write(&victim, "keep").unwrap();
        let link = tmp.path().join("link.docx");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let would_be = outside.path().join("created.docx");
        let dangling = tmp.path().join("dangling.docx");
        std::os::unix::fs::symlink(&would_be, &dangling).unwrap();

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = WriteDocxTool::new(service.clone());
        for target in [&link, &dangling] {
            let err = tool
                .call(
                    &mut ToolContext::new(),
                    WriteDocxArgs {
                        path: target.to_str().unwrap().to_string(),
                        content: "# Pwned".to_string(),
                    },
                )
                .await
                .unwrap_err();
            assert!(err.to_string().contains("not a regular file"), "{err}");
        }
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!would_be.exists());

        // The approval card: the dangling link is shown as an overwrite.
        let mut store = WriteApprovalStore::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (resolution_tx, _resolution_rx) = tokio::sync::mpsc::unbounded_channel();
        store.set_notifiers(tx, resolution_tx);
        let tool = WriteDocxTool::new(service)
            .with_write_approval(ApprovalMode::AlwaysAsk, store.get_pending_approvals());
        let path = dangling.to_str().unwrap().to_string();
        let call = tokio::spawn(async move {
            tool.call(
                &mut ToolContext::new(),
                WriteDocxArgs {
                    path,
                    content: "# Pwned".to_string(),
                },
            )
            .await
            .map(|_| ())
        });
        let asked = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the write is asked about")
            .expect("notifier open");
        assert!(
            asked.command.starts_with("Overwrite file:"),
            "{}",
            asked.command
        );
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Approved));
        assert!(call.await.unwrap().is_err(), "approved, but still refused");
        assert!(!would_be.exists());
    }

    #[tokio::test]
    async fn write_docx_overwrites_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let target = tmp.path().join("report.docx");
        std::fs::write(&target, "old").unwrap();
        WriteDocxTool::new(service)
            .call(
                &mut ToolContext::new(),
                WriteDocxArgs {
                    path: target.to_str().unwrap().to_string(),
                    content: "# New".to_string(),
                },
            )
            .await
            .unwrap();
        assert_ne!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
}
