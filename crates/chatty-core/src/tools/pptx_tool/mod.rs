mod edit;
mod read;
mod write;

pub use edit::EditPptxTool;
pub use read::ReadPptxTool;
pub use write::WritePptxTool;

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

    use rig_core::tool::Tool;

    use crate::services::filesystem_service::FileSystemService;

    use super::edit::{EditPptxArgs, EditPptxOperation};
    use super::read::ReadPptxArgs;
    use super::write::{PptxShapeSpec, PptxSlideSpec, TextStyleSpec, WritePptxArgs};
    use super::*;

    #[tokio::test]
    async fn test_write_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );

        let path = tmp.path().join("deck.pptx").to_str().unwrap().to_string();

        let write_tool = WritePptxTool::new(service.clone());
        let write_output = write_tool
            .call(WritePptxArgs {
                path: path.clone(),
                slides: vec![PptxSlideSpec {
                    title: Some("Quarterly Review".to_string()),
                    shapes: vec![
                        PptxShapeSpec::BulletList {
                            x: 0.8,
                            y: 1.7,
                            width: 8.0,
                            height: 2.2,
                            items: vec![
                                "Revenue grew 18%".into(),
                                "Enterprise led expansion".into(),
                            ],
                            style: Some(TextStyleSpec {
                                font_size: Some(20.0),
                                bold: None,
                                italic: None,
                                color: None,
                            }),
                        },
                        PptxShapeSpec::Table {
                            x: 0.8,
                            y: 4.3,
                            width: 8.0,
                            height: 1.2,
                            rows: vec![
                                vec!["Metric".into(), "Value".into()],
                                vec!["ARR".into(), "$2.1M".into()],
                            ],
                        },
                    ],
                }],
            })
            .await
            .unwrap();

        assert_eq!(write_output.slide_count, 1);
        assert_eq!(write_output.shapes_written, 3);
        assert!(write_output.bytes_written > 0);
        assert!(std::path::Path::new(&path).exists());

        let read_tool = ReadPptxTool::new(service);
        let read_output = read_tool
            .call(ReadPptxArgs {
                path,
                include_notes: None,
                max_chars: None,
            })
            .await
            .unwrap();

        assert_eq!(read_output.slide_count, 1);
        assert!(read_output.text.contains("## Slide 1: Quarterly Review"));
        assert!(read_output.text.contains("Revenue grew 18%"));
        assert!(read_output.text.contains("Enterprise led expansion"));
        assert!(read_output.text.contains("| Metric | Value |"));
        assert!(read_output.text.contains("| ARR | $2.1M |"));
    }

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

    #[tokio::test]
    async fn test_edit_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );

        let path = tmp
            .path()
            .join("editable.pptx")
            .to_str()
            .unwrap()
            .to_string();

        let write_tool = WritePptxTool::new(service.clone());
        write_tool
            .call(WritePptxArgs {
                path: path.clone(),
                slides: vec![PptxSlideSpec {
                    title: Some("Original Title".to_string()),
                    shapes: vec![PptxShapeSpec::TextBox {
                        x: 0.8,
                        y: 1.7,
                        width: 8.0,
                        height: 1.2,
                        text: "Original body".to_string(),
                        style: None,
                    }],
                }],
            })
            .await
            .unwrap();

        let edit_tool = EditPptxTool::new(service.clone());
        let edit_output = edit_tool
            .call(EditPptxArgs {
                path: path.clone(),
                output_path: None,
                operations: vec![
                    EditPptxOperation::SetSlideTitle {
                        slide: 1,
                        title: "Updated Title".to_string(),
                    },
                    EditPptxOperation::AddBulletList {
                        slide: 1,
                        x: 0.8,
                        y: 3.0,
                        width: 8.0,
                        height: 1.8,
                        items: vec!["First update".into(), "Second update".into()],
                        style: None,
                    },
                ],
            })
            .await
            .unwrap();

        assert_eq!(edit_output.operations_applied, 2);
        assert_eq!(edit_output.slide_count, 1);

        let read_tool = ReadPptxTool::new(service);
        let read_output = read_tool
            .call(ReadPptxArgs {
                path,
                include_notes: None,
                max_chars: None,
            })
            .await
            .unwrap();

        assert!(read_output.text.contains("## Slide 1: Updated Title"));
        assert!(read_output.text.contains("Original body"));
        assert!(read_output.text.contains("First update"));
        assert!(read_output.text.contains("Second update"));
    }
}
