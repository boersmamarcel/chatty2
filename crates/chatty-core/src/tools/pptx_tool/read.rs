use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::services::filesystem_service::FileSystemService;

use super::PptxToolError;

#[derive(Deserialize, Serialize)]
pub struct ReadPptxArgs {
    pub path: String,
    /// Include speaker notes for each slide. Defaults to false.
    #[serde(default)]
    pub include_notes: Option<bool>,
    /// Maximum characters to return. Defaults to 50_000.
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReadPptxOutput {
    pub path: String,
    pub slide_count: usize,
    pub text: String,
    pub char_count: usize,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct ReadPptxTool {
    service: Arc<FileSystemService>,
}

impl ReadPptxTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ReadPptxTool {
    const NAME: &'static str = "read_pptx";
    type Error = PptxToolError;
    type Args = ReadPptxArgs;
    type Output = ReadPptxOutput;

    fn description(&self) -> String {
        "Read a PowerPoint presentation (.pptx) and return its text content.\n\
                         Returns slide titles, body text, and tables formatted as markdown sections.\n\
                         \n\
                         Use this for .pptx files — do NOT use read_file (returns binary garbage).\n\
                         \n\
                         Examples:\n\
                         - Read full presentation: {\"path\": \"slides.pptx\"}\n\
                         - Include speaker notes: {\"path\": \"slides.pptx\", \"include_notes\": true}\n\
                         - Limit output: {\"path\": \"slides.pptx\", \"max_chars\": 10000}"
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the .pptx file, relative to workspace root or absolute within workspace"
                },
                "include_notes": {
                    "type": "boolean",
                    "description": "Include speaker notes for each slide. Defaults to false."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return. Defaults to 50000."
                }
            },
            "required": ["path"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let (canonical, bytes) = self.service.read_binary_bytes(&args.path).await?;
        let max_chars = args.max_chars.unwrap_or(50_000);
        let include_notes = args.include_notes.unwrap_or(false);
        let requested_path = args.path.clone();
        let (slide_count, full_text) = tokio::task::spawn_blocking(move || {
            parse_pptx_bytes(bytes, include_notes, &requested_path)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse PPTX '{}': {}", args.path, e))??;
        let char_count = full_text.chars().count();
        let truncated = char_count > max_chars;
        let text = if truncated {
            full_text.chars().take(max_chars).collect::<String>() + "\n\n[... truncated ...]"
        } else {
            full_text
        };

        Ok(ReadPptxOutput {
            path: canonical.display().to_string(),
            slide_count,
            text,
            char_count,
            truncated,
        })
    }
}

/// One slide's extracted content.
///
/// The structured form behind `read_pptx`'s markdown. Since AGE-343 the
/// artifact panel's Rendered tab shows rasterised slides instead of these
/// fields; the panel keeps using them only to fill its Source tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PptxSlide {
    /// 1-based position in the deck's slide order.
    pub number: usize,
    pub title: Option<String>,
    /// Body text, one entry per text shape, in z-order. The title placeholder
    /// is not repeated here.
    pub body: Vec<String>,
    /// Tables already rendered as markdown.
    pub tables: Vec<String>,
    /// Only populated when the caller asked for notes.
    pub notes: Option<String>,
}

/// Extract every slide from a `.pptx` byte buffer — one `rpptx` open, shared
/// by `read_pptx` and the desktop artifact panel's Source tab.
fn parse_pptx_slides(
    bytes: Vec<u8>,
    include_notes: bool,
    requested_path: &str,
) -> anyhow::Result<Vec<PptxSlide>> {
    let deck = rpptx::Presentation::from_bytes(&bytes)
        .with_context(|| format!("'{}' is not a readable PPTX file", requested_path))?;
    Ok(slides_from_deck(&deck, include_notes))
}

fn slides_from_deck(deck: &rpptx::Presentation, include_notes: bool) -> Vec<PptxSlide> {
    deck.slides()
        .enumerate()
        .map(|(index, slide)| {
            let title_shape = slide.title();
            let title = title_shape
                .as_ref()
                .and_then(|shape| shape.text())
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());

            let mut body = Vec::new();
            let mut tables = Vec::new();
            collect_shapes(slide.shapes(), title_shape, &mut body, &mut tables);

            let notes = include_notes
                .then(|| slide.notes_text())
                .flatten()
                .map(|notes| notes.trim().to_string())
                .filter(|notes| !notes.is_empty());

            PptxSlide {
                number: index + 1,
                title,
                body,
                tables,
                notes,
            }
        })
        .collect()
}

/// Walk one shape tree in z-order, splitting text shapes from tables.
///
/// Groups are descended into: a deck that puts its body copy inside a group
/// would otherwise read as an empty slide. `skip` drops the title placeholder,
/// which [`PptxSlide::title`] already carries.
fn collect_shapes<'a>(
    shapes: impl Iterator<Item = rpptx::ShapeRef<'a>>,
    skip: Option<rpptx::ShapeRef<'a>>,
    body: &mut Vec<String>,
    tables: &mut Vec<String>,
) {
    for shape in shapes {
        if Some(shape) == skip {
            continue;
        }
        if let Some(table) = shape.table() {
            let markdown = table_as_markdown(&table);
            if !markdown.is_empty() {
                tables.push(markdown);
            }
            continue;
        }
        if shape.kind() == rpptx::ShapeKind::Group {
            collect_shapes(shape.children(), skip, body, tables);
            continue;
        }
        if let Some(text) = shape.text() {
            let text = text.trim();
            if !text.is_empty() {
                body.push(text.to_string());
            }
        }
    }
}

/// Render a slide table as a markdown table, first row as the header.
fn table_as_markdown(table: &rpptx::TableRef<'_>) -> String {
    let columns = table.column_count();
    if columns == 0 || table.row_count() == 0 {
        return String::new();
    }
    let mut lines = Vec::with_capacity(table.row_count() + 1);
    for row in 0..table.row_count() {
        let cells: Vec<String> = (0..columns)
            .map(|column| {
                table
                    .cell(row, column)
                    .map(|cell| cell.text().trim().replace('\n', " "))
                    .unwrap_or_default()
            })
            .collect();
        lines.push(format!("| {} |", cells.join(" | ")));
        if row == 0 {
            let separator = vec!["---"; columns].join(" | ");
            lines.push(format!("| {} |", separator));
        }
    }
    lines.join("\n")
}

/// Read a `.pptx` off disk and extract its slides.
///
/// Reads the file directly: callers that must stay inside the workspace go
/// through `FileSystemService` first, as `read_pptx` does.
pub fn read_pptx_slides(path: &Path, include_notes: bool) -> anyhow::Result<Vec<PptxSlide>> {
    let deck = rpptx::Presentation::open(path)
        .with_context(|| format!("Failed to read PPTX '{}'", path.display()))?;
    Ok(slides_from_deck(&deck, include_notes))
}

/// The markdown `read_pptx` returns, rendered from already-extracted slides.
pub fn pptx_slides_to_text(slides: &[PptxSlide]) -> String {
    let sections: Vec<String> = slides
        .iter()
        .map(|slide| {
            let mut section = format!("## Slide {}", slide.number);
            if let Some(title) = &slide.title {
                section.push_str(&format!(": {}", title));
            }
            section.push('\n');

            let body = slide.body.join("\n\n");
            if !body.trim().is_empty() {
                section.push('\n');
                section.push_str(body.trim());
                section.push('\n');
            }

            for table_md in &slide.tables {
                section.push('\n');
                section.push_str(table_md);
                section.push('\n');
            }

            if let Some(notes) = &slide.notes {
                section.push_str("\n_Notes:_ ");
                section.push_str(notes.trim());
                section.push('\n');
            }

            section
        })
        .collect();
    sections.join("\n")
}

fn parse_pptx_bytes(
    bytes: Vec<u8>,
    include_notes: bool,
    requested_path: &str,
) -> anyhow::Result<(usize, String)> {
    let slides = parse_pptx_slides(bytes, include_notes, requested_path)?;
    let text = pptx_slides_to_text(&slides);
    Ok((slides.len(), text))
}
