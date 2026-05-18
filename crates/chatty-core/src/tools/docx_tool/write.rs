use std::io::Cursor;
use std::sync::Arc;

use anyhow::Context;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};

use crate::services::filesystem_service::FileSystemService;

use super::DocxToolError;

#[derive(Deserialize, Serialize)]
pub struct WriteDocxArgs {
    pub path: String,
    /// Document content in simplified markdown:
    /// - `# Heading 1`, `## Heading 2`, `### Heading 3`
    /// - Plain paragraphs (blank-line separated)
    /// - `- item` for bullet lists
    /// - `| col1 | col2 |` markdown tables
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct WriteDocxOutput {
    pub path: String,
    pub bytes_written: usize,
    pub paragraphs_written: usize,
}

#[derive(Clone)]
pub struct WriteDocxTool {
    service: Arc<FileSystemService>,
}

impl WriteDocxTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for WriteDocxTool {
    const NAME: &'static str = "write_docx";
    type Error = DocxToolError;
    type Args = WriteDocxArgs;
    type Output = WriteDocxOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "write_docx".to_string(),
            description: "Create a Word document (.docx) from simplified markdown content.\n\
                         Supports headings (#/##/###), paragraphs, bullet lists (- item), and tables.\n\
                         \n\
                         Content format:\n\
                         - Headings: lines starting with `#`, `##`, or `###`\n\
                         - Paragraphs: plain text separated by blank lines\n\
                         - Bullet list: lines starting with `- ` or `* `\n\
                         - Table: `| col1 | col2 |` rows with separator `|---|---|`\n\
                         \n\
                         Examples:\n\
                         - Simple doc: {\"path\": \"report.docx\", \"content\": \"# Title\\n\\nParagraph text.\"}\n\
                         - With table: {\"path\": \"data.docx\", \"content\": \"# Summary\\n\\n| Name | Value |\\n|---|---|\\n| Alice | 100 |\"}".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to write the .docx file, relative to workspace root or absolute within workspace"
                    },
                    "content": {
                        "type": "string",
                        "description": "Document content in simplified markdown (headings, paragraphs, bullets, tables)"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_new_path(&args.path).await?;

        // Ensure parent directory exists
        if let Some(parent) = canonical.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
        }

        let (docx_bytes, paragraphs_written) = build_docx(&args.content)?;
        let bytes_written = docx_bytes.len();

        std::fs::write(&canonical, &docx_bytes)
            .with_context(|| format!("Failed to write '{}'", canonical.display()))?;

        Ok(WriteDocxOutput {
            path: canonical.display().to_string(),
            bytes_written,
            paragraphs_written,
        })
    }
}

/// Parse simplified markdown content and build a DOCX document, returning the raw bytes.
fn build_docx(content: &str) -> Result<(Vec<u8>, usize), DocxToolError> {
    use docx_rs::*;

    let mut doc = Docx::new();
    let mut paragraphs_written = 0usize;

    // Parse blocks: blank-line separated
    let blocks = split_into_blocks(content);

    for block in &blocks {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(table) = try_parse_table(block) {
            doc = doc.add_table(table);
            paragraphs_written += 1;
        } else if is_bullet_list(block) {
            for line in block.lines() {
                let text = line
                    .trim_start_matches("- ")
                    .trim_start_matches("* ")
                    .trim();
                let para = Paragraph::new()
                    .add_run(Run::new().add_text(text))
                    .style("ListParagraph");
                doc = doc.add_paragraph(para);
                paragraphs_written += 1;
            }
        } else if let Some(level) = heading_level(trimmed) {
            let text = trimmed.trim_start_matches('#').trim();
            let style = match level {
                1 => "Heading1",
                2 => "Heading2",
                3 => "Heading3",
                4 => "Heading4",
                _ => "Heading5",
            };
            let para = Paragraph::new()
                .add_run(Run::new().add_text(text))
                .style(style);
            doc = doc.add_paragraph(para);
            paragraphs_written += 1;
        } else {
            // Regular paragraph (may span multiple lines — join with space)
            let text = block
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let para = Paragraph::new().add_run(Run::new().add_text(&text));
            doc = doc.add_paragraph(para);
            paragraphs_written += 1;
        }
    }

    let mut buf = Cursor::new(Vec::<u8>::new());
    doc.build()
        .pack(&mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to pack DOCX: {}", e))?;
    let bytes = buf.into_inner();

    Ok((bytes, paragraphs_written))
}

/// Split content on blank lines into logical blocks.
fn split_into_blocks(content: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
}

fn heading_level(line: &str) -> Option<u8> {
    if line.starts_with("##### ") {
        Some(5)
    } else if line.starts_with("#### ") {
        Some(4)
    } else if line.starts_with("### ") {
        Some(3)
    } else if line.starts_with("## ") {
        Some(2)
    } else if line.starts_with("# ") {
        Some(1)
    } else {
        None
    }
}

fn is_bullet_list(block: &str) -> bool {
    block
        .lines()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* "))
}

/// Try to parse a markdown table block. Returns None if it doesn't look like a table.
fn try_parse_table(block: &str) -> Option<docx_rs::Table> {
    use docx_rs::*;

    let lines: Vec<&str> = block.lines().collect();
    if lines.len() < 2 {
        return None;
    }

    // First line must start with |
    if !lines[0].trim().starts_with('|') {
        return None;
    }

    // Collect data rows (skip separator lines like |---|---|)
    let data_lines: Vec<Vec<String>> = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('|')
                && !t
                    .trim_matches('|')
                    .chars()
                    .all(|c| c == '-' || c == ' ' || c == ':' || c == '|')
        })
        .map(|l| {
            l.trim()
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();

    if data_lines.is_empty() {
        return None;
    }

    let col_count = data_lines.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return None;
    }

    let mut table_rows: Vec<TableRow> = Vec::new();

    for (row_idx, row_data) in data_lines.iter().enumerate() {
        let mut cells: Vec<TableCell> = Vec::new();
        for col_idx in 0..col_count {
            let text = row_data.get(col_idx).map(|s| s.as_str()).unwrap_or("");
            let mut run = Run::new().add_text(text);
            if row_idx == 0 {
                run = run.bold();
            }
            let cell = TableCell::new().add_paragraph(Paragraph::new().add_run(run));
            cells.push(cell);
        }
        table_rows.push(TableRow::new(cells));
    }

    Some(Table::new(table_rows))
}
