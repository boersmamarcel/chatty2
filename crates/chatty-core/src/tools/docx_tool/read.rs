use std::sync::Arc;

use anyhow::Context;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

use crate::services::filesystem_service::FileSystemService;

use super::DocxToolError;

#[derive(Deserialize, Serialize)]
pub struct ReadDocxArgs {
    pub path: String,
    /// Whether to include tables as markdown. Defaults to true.
    #[serde(default)]
    pub include_tables: Option<bool>,
    /// Maximum characters to return. Defaults to 50_000.
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReadDocxOutput {
    pub path: String,
    pub text: String,
    pub char_count: usize,
    pub truncated: bool,
    pub paragraph_count: usize,
}

#[derive(Clone)]
pub struct ReadDocxTool {
    service: Arc<FileSystemService>,
}

impl ReadDocxTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ReadDocxTool {
    const NAME: &'static str = "read_docx";
    type Error = DocxToolError;
    type Args = ReadDocxArgs;
    type Output = ReadDocxOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_docx".to_string(),
            description: "Read a Word document (.docx) and return its text content as markdown.\n\
                         Returns paragraphs, headings, and optionally tables formatted as markdown.\n\
                         \n\
                         Use this for .docx files — do NOT use read_file (binary garbage).\n\
                         \n\
                         Examples:\n\
                         - Read full document: {\"path\": \"report.docx\"}\n\
                         - Read without tables: {\"path\": \"report.docx\", \"include_tables\": false}\n\
                         - Limit output: {\"path\": \"report.docx\", \"max_chars\": 10000}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the .docx file, relative to workspace root or absolute within workspace"
                    },
                    "include_tables": {
                        "type": "boolean",
                        "description": "Include table content as markdown tables. Defaults to true."
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Maximum characters to return. Defaults to 50000."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let max_chars = args.max_chars.unwrap_or(50_000);
        let include_tables = args.include_tables.unwrap_or(true);

        // Read file bytes
        let bytes = std::fs::read(&canonical)
            .with_context(|| format!("Failed to read '{}'", canonical.display()))?;

        // Parse DOCX
        let docx = docx_rs::read_docx(&bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse DOCX '{}': {:?}", args.path, e))?;

        let mut output_parts: Vec<String> = Vec::new();
        let mut paragraph_count = 0usize;

        for child in &docx.document.children {
            match child {
                docx_rs::DocumentChild::Paragraph(para) => {
                    let rendered = render_paragraph(para);
                    if !rendered.is_empty() {
                        output_parts.push(rendered);
                        paragraph_count += 1;
                    }
                }
                docx_rs::DocumentChild::Table(table) if include_tables => {
                    let rendered = render_table(table);
                    if !rendered.is_empty() {
                        output_parts.push(rendered);
                        paragraph_count += 1;
                    }
                }
                _ => {}
            }
        }

        let full_text = output_parts.join("\n\n");
        let char_count = full_text.chars().count();
        let truncated = char_count > max_chars;
        let text = if truncated {
            full_text.chars().take(max_chars).collect::<String>()
                + "\n\n[... truncated ...]"
        } else {
            full_text
        };

        Ok(ReadDocxOutput {
            path: canonical.display().to_string(),
            text,
            char_count,
            truncated,
            paragraph_count,
        })
    }
}

/// Render a docx paragraph to a markdown string.
fn render_paragraph(para: &docx_rs::Paragraph) -> String {
    // Detect heading style from paragraph properties
    let heading_level = para
        .property
        .style
        .as_ref()
        .and_then(|s| heading_level_from_style(&s.val));

    let text = collect_paragraph_text(para);
    if text.trim().is_empty() {
        return String::new();
    }

    match heading_level {
        Some(1) => format!("# {}", text.trim()),
        Some(2) => format!("## {}", text.trim()),
        Some(3) => format!("### {}", text.trim()),
        Some(4) => format!("#### {}", text.trim()),
        Some(5) => format!("##### {}", text.trim()),
        _ => text.trim().to_string(),
    }
}

fn heading_level_from_style(style: &str) -> Option<u8> {
    match style {
        "Heading1" | "heading1" | "Heading 1" => Some(1),
        "Heading2" | "heading2" | "Heading 2" => Some(2),
        "Heading3" | "heading3" | "Heading 3" => Some(3),
        "Heading4" | "heading4" | "Heading 4" => Some(4),
        "Heading5" | "heading5" | "Heading 5" => Some(5),
        _ => {
            // Handle "Heading1", "Heading2" etc. with numeric suffix
            if let Some(stripped) = style.strip_prefix("Heading") {
                stripped.trim().parse::<u8>().ok()
            } else {
                None
            }
        }
    }
}

fn collect_paragraph_text(para: &docx_rs::Paragraph) -> String {
    let mut text = String::new();
    for child in &para.children {
        match child {
            docx_rs::ParagraphChild::Run(run) => {
                for run_child in &run.children {
                    if let docx_rs::RunChild::Text(t) = run_child {
                        text.push_str(&t.text);
                    }
                }
            }
            docx_rs::ParagraphChild::Hyperlink(link) => {
                for para_child in &link.children {
                    if let docx_rs::ParagraphChild::Run(run) = para_child {
                        for run_child in &run.children {
                            if let docx_rs::RunChild::Text(t) = run_child {
                                text.push_str(&t.text);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    text
}

/// Render a DOCX table as a markdown table string.
fn render_table(table: &docx_rs::Table) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for row_child in &table.rows {
        if let docx_rs::TableChild::TableRow(row) = row_child {
            let mut cells: Vec<String> = Vec::new();
            for cell_child in &row.cells {
                if let docx_rs::TableRowChild::TableCell(cell) = cell_child {
                    let cell_text = cell
                        .children
                        .iter()
                        .filter_map(|c| {
                            if let docx_rs::TableCellContent::Paragraph(p) = c {
                                let t = collect_paragraph_text(p);
                                if t.trim().is_empty() { None } else { Some(t.trim().to_string()) }
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    cells.push(cell_text);
                }
            }
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
    }

    if rows.is_empty() {
        return String::new();
    }

    // Build markdown table
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();

    for (i, row) in rows.iter().enumerate() {
        let padded: Vec<String> = (0..col_count)
            .map(|c| row.get(c).cloned().unwrap_or_default())
            .collect();
        lines.push(format!("| {} |", padded.join(" | ")));
        if i == 0 {
            // separator
            let sep = (0..col_count).map(|_| "---").collect::<Vec<_>>().join(" | ");
            lines.push(format!("| {} |", sep));
        }
    }

    lines.join("\n")
}
