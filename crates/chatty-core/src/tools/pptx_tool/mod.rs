mod read;
mod write;

pub use read::{PptxSlide, PptxTextBlock, ReadPptxTool, pptx_slides_to_text, read_pptx_slides};
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

    use rig_agent::tool::{Tool, ToolContext};

    use crate::services::filesystem_service::FileSystemService;

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
            .call(
                &mut ToolContext::new(),
                WritePptxArgs {
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
                },
            )
            .await
            .unwrap();

        assert_eq!(write_output.slide_count, 1);
        assert_eq!(write_output.shapes_written, 3);
        assert!(write_output.bytes_written > 0);
        assert!(std::path::Path::new(&path).exists());

        let read_tool = ReadPptxTool::new(service);
        let read_output = read_tool
            .call(
                &mut ToolContext::new(),
                ReadPptxArgs {
                    path,
                    include_notes: None,
                    max_chars: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(read_output.slide_count, 1);
        assert!(read_output.text.contains("## Slide 1: Quarterly Review"));
        assert!(read_output.text.contains("Revenue grew 18%"));
        assert!(read_output.text.contains("Enterprise led expansion"));
        assert!(read_output.text.contains("| Metric | Value |"));
        assert!(read_output.text.contains("| ARR | $2.1M |"));
    }

    /// Slide spec helper for the extraction tests: a titled slide with one
    /// bullet list.
    fn bulleted_slide(title: &str, items: &[&str]) -> PptxSlideSpec {
        PptxSlideSpec {
            title: Some(title.to_string()),
            shapes: vec![PptxShapeSpec::BulletList {
                x: 0.8,
                y: 1.7,
                width: 8.0,
                height: 2.2,
                items: items.iter().map(|s| s.to_string()).collect(),
                style: None,
            }],
        }
    }

    /// AGE-138: the artifact panel pages a deck a slide at a time, so it needs
    /// the slide list — not just the flattened markdown `read_pptx` returns.
    #[tokio::test]
    async fn slides_extract_title_bullets_and_tables_per_slide() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let path = tmp.path().join("deck.pptx");

        WritePptxTool::new(service)
            .call(
                &mut ToolContext::new(),
                WritePptxArgs {
                    path: path.to_str().unwrap().to_string(),
                    slides: vec![
                        bulleted_slide("Opening", &["First point", "Second point"]),
                        PptxSlideSpec {
                            title: Some("Numbers".to_string()),
                            shapes: vec![PptxShapeSpec::Table {
                                x: 0.8,
                                y: 4.3,
                                width: 8.0,
                                height: 1.2,
                                rows: vec![
                                    vec!["Metric".into(), "Value".into()],
                                    vec!["ARR".into(), "$2.1M".into()],
                                ],
                            }],
                        },
                        bulleted_slide("Closing", &["Thanks"]),
                    ],
                },
            )
            .await
            .unwrap();

        let slides = read_pptx_slides(&path, false).expect("extract slides");
        assert_eq!(slides.len(), 3, "one entry per slide, in deck order");
        assert_eq!(
            slides.iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        assert_eq!(slides[0].title.as_deref(), Some("Opening"));
        assert_eq!(slides[0].body.len(), 1, "one bullet-list shape");
        assert_eq!(
            slides[0].body[0].lines,
            vec!["First point".to_string(), "Second point".to_string()],
            "each bullet stays its own line, not one run-on paragraph"
        );
        assert!(
            slides[0].body[0].bulleted,
            "write_pptx's bullet_list must come back marked as bulleted"
        );
        assert!(slides[0].tables.is_empty());

        assert_eq!(slides[1].title.as_deref(), Some("Numbers"));
        assert_eq!(slides[1].tables.len(), 1);
        assert!(slides[1].tables[0].contains("| Metric | Value |"));
        assert!(slides[1].tables[0].contains("| ARR | $2.1M |"));

        assert_eq!(slides[2].title.as_deref(), Some("Closing"));
    }

    /// `read_pptx`'s wire format, pinned to a literal.
    ///
    /// Comparing the tool's output against `pptx_slides_to_text` would be true
    /// by construction — after AGE-138 the tool *is* that composition — so the
    /// expected text is written out here instead. Every byte of the section
    /// header, the blank-line spacing and the markdown table is part of what
    /// the model and the panel's Source tab see; changing any of it is a
    /// behaviour change to `read_pptx` and must fail here.
    const GOLDEN_DECK_TEXT: &str = "\
## Slide 1: Opening

First point
Second point

## Slide 2: Numbers

| Metric | Value |
| --- | --- |
| ARR | $2.1M |
";

    #[tokio::test]
    async fn read_pptx_text_format_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let path = tmp.path().join("deck.pptx");

        WritePptxTool::new(service.clone())
            .call(
                &mut ToolContext::new(),
                WritePptxArgs {
                    path: path.to_str().unwrap().to_string(),
                    slides: vec![
                        bulleted_slide("Opening", &["First point", "Second point"]),
                        PptxSlideSpec {
                            title: Some("Numbers".to_string()),
                            shapes: vec![PptxShapeSpec::Table {
                                x: 0.8,
                                y: 4.3,
                                width: 8.0,
                                height: 1.2,
                                rows: vec![
                                    vec!["Metric".into(), "Value".into()],
                                    vec!["ARR".into(), "$2.1M".into()],
                                ],
                            }],
                        },
                    ],
                },
            )
            .await
            .unwrap();

        let read_output = ReadPptxTool::new(service)
            .call(
                &mut ToolContext::new(),
                ReadPptxArgs {
                    path: path.to_str().unwrap().to_string(),
                    include_notes: None,
                    max_chars: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(read_output.slide_count, 2);
        assert_eq!(read_output.text, GOLDEN_DECK_TEXT);
        assert_eq!(read_output.char_count, GOLDEN_DECK_TEXT.chars().count());
        assert!(!read_output.truncated);

        // The panel's Source tab renders the same bytes from the slide list.
        let slides = read_pptx_slides(&path, false).expect("extract slides");
        assert_eq!(pptx_slides_to_text(&slides), GOLDEN_DECK_TEXT);
    }

    /// A truncated/garbage file must surface an error the panel can show as a
    /// muted one-liner, never a panic.
    #[test]
    fn unreadable_pptx_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("broken.pptx");
        std::fs::write(&path, b"not a zip at all").unwrap();
        let err = read_pptx_slides(&path, false).expect_err("garbage is not a deck");
        assert!(
            err.to_string().contains("not a valid PPTX/ZIP file"),
            "unexpected message: {err}"
        );

        let missing = tmp.path().join("gone.pptx");
        assert!(read_pptx_slides(&missing, false).is_err());
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
            .call(
                &mut ToolContext::new(),
                ReadPptxArgs {
                    path: tmp.path().join("nope.pptx").to_str().unwrap().to_string(),
                    include_notes: None,
                    max_chars: None,
                },
            )
            .await;
        assert!(result.is_err());
    }
}
