use std::io::Read;
use std::sync::Arc;

use anyhow::Context;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
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

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_pptx".to_string(),
            description: "Read a PowerPoint presentation (.pptx) and return its text content.\n\
                         Returns slide titles, body text, and tables formatted as markdown sections.\n\
                         \n\
                         Use this for .pptx files — do NOT use read_file (returns binary garbage).\n\
                         \n\
                         Examples:\n\
                         - Read full presentation: {\"path\": \"slides.pptx\"}\n\
                         - Include speaker notes: {\"path\": \"slides.pptx\", \"include_notes\": true}\n\
                         - Limit output: {\"path\": \"slides.pptx\", \"max_chars\": 10000}"
                .to_string(),
            parameters: serde_json::json!({
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
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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

fn parse_pptx_bytes(
    bytes: Vec<u8>,
    include_notes: bool,
    requested_path: &str,
) -> anyhow::Result<(usize, String)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .with_context(|| format!("'{}' is not a valid PPTX/ZIP file", requested_path))?;

    // Collect and sort slide entries: ppt/slides/slide*.xml
    let file_names: Vec<String> = archive.file_names().map(|s| s.to_string()).collect();
    let mut slide_entries: Vec<(usize, String)> = file_names
        .iter()
        .filter_map(|name| parse_slide_number(name).map(|n| (n, name.clone())))
        .collect();
    slide_entries.sort_by_key(|(n, _)| *n);

    let slide_count = slide_entries.len();
    let mut sections: Vec<String> = Vec::new();

    for (slide_num, slide_name) in &slide_entries {
        let slide_xml = read_zip_entry(&mut archive, slide_name)?;
        let content = extract_slide_content(&slide_xml)?;

        let mut section = format!("## Slide {}", slide_num);
        if let Some(title) = &content.title {
            section.push_str(&format!(": {}", title));
        }
        section.push('\n');

        if !content.body.trim().is_empty() {
            section.push('\n');
            section.push_str(content.body.trim());
            section.push('\n');
        }

        for table_md in &content.tables {
            section.push('\n');
            section.push_str(table_md);
            section.push('\n');
        }

        if include_notes {
            let notes_name = slide_name.replace("ppt/slides/slide", "ppt/notesSlides/notesSlide");
            if file_names.contains(&notes_name)
                && let Ok(notes_xml) = read_zip_entry(&mut archive, &notes_name)
            {
                let notes_text = extract_notes_text(&notes_xml);
                if !notes_text.trim().is_empty() {
                    section.push_str("\n_Notes:_ ");
                    section.push_str(notes_text.trim());
                    section.push('\n');
                }
            }
        }

        sections.push(section);
    }

    Ok((slide_count, sections.join("\n")))
}

/// Parse slide number from "ppt/slides/slide3.xml" → Some(3).
fn parse_slide_number(name: &str) -> Option<usize> {
    let stem = name.strip_prefix("ppt/slides/slide")?;
    let stem = stem.strip_suffix(".xml")?;
    stem.parse::<usize>().ok()
}

/// Read a ZIP entry into a String.
fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> anyhow::Result<String> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("Entry '{}' not found in archive", name))?;
    let mut content = String::new();
    entry
        .read_to_string(&mut content)
        .with_context(|| format!("Failed to read entry '{}'", name))?;
    Ok(content)
}

struct SlideContent {
    title: Option<String>,
    body: String,
    tables: Vec<String>,
}

fn extract_slide_content(xml: &str) -> anyhow::Result<SlideContent> {
    let doc =
        roxmltree::Document::parse(xml).map_err(|e| anyhow::anyhow!("XML parse error: {}", e))?;
    let root = doc.root_element();

    let mut title: Option<String> = None;
    let mut body_parts: Vec<String> = Vec::new();
    let mut tables: Vec<String> = Vec::new();

    // Process shapes (p:sp)
    for sp in root.descendants().filter(|n| n.tag_name().name() == "sp") {
        let text = extract_txbody_text(&sp);
        if !text.trim().is_empty() {
            if is_title_placeholder(&sp) && title.is_none() {
                title = Some(text.trim().to_string());
            } else {
                body_parts.push(text.trim().to_string());
            }
        }
    }

    // Process tables (a:tbl inside p:graphicFrame — separate from p:sp shapes)
    for tbl in root.descendants().filter(|n| n.tag_name().name() == "tbl") {
        let md = render_table_as_markdown(&tbl);
        if !md.is_empty() {
            tables.push(md);
        }
    }

    Ok(SlideContent {
        title,
        body: body_parts.join("\n\n"),
        tables,
    })
}

/// Returns true if the shape contains a title or centered-title placeholder.
fn is_title_placeholder(sp: &roxmltree::Node) -> bool {
    sp.descendants().any(|n| {
        if n.tag_name().name() == "ph" {
            let t = n.attribute("type").unwrap_or("");
            t == "title" || t == "ctrTitle"
        } else {
            false
        }
    })
}

/// Extract paragraphs from a p:txBody inside a p:sp.
fn extract_txbody_text(sp: &roxmltree::Node) -> String {
    let txbody = match sp.children().find(|n| n.tag_name().name() == "txBody") {
        Some(n) => n,
        None => return String::new(),
    };

    let paragraphs: Vec<String> = txbody
        .children()
        .filter(|n| n.tag_name().name() == "p")
        .filter_map(|p_node| {
            let text: String = p_node
                .descendants()
                .filter(|n| n.tag_name().name() == "t")
                .filter_map(|t| t.text())
                .collect::<Vec<_>>()
                .join("");
            if text.trim().is_empty() {
                None
            } else {
                Some(text.trim().to_string())
            }
        })
        .collect();

    paragraphs.join("\n")
}

/// Render an a:tbl table node as a markdown table.
fn render_table_as_markdown(tbl: &roxmltree::Node) -> String {
    let rows: Vec<Vec<String>> = tbl
        .children()
        .filter(|n| n.tag_name().name() == "tr")
        .map(|tr| {
            tr.children()
                .filter(|n| n.tag_name().name() == "tc")
                .map(|tc| {
                    let text: String = tc
                        .descendants()
                        .filter(|n| n.tag_name().name() == "t")
                        .filter_map(|t| t.text())
                        .collect::<Vec<_>>()
                        .join("");
                    text.trim().to_string()
                })
                .collect()
        })
        .filter(|row: &Vec<String>| !row.is_empty())
        .collect();

    if rows.is_empty() {
        return String::new();
    }

    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let padded: Vec<String> = (0..col_count)
            .map(|c| row.get(c).cloned().unwrap_or_default())
            .collect();
        lines.push(format!("| {} |", padded.join(" | ")));
        if i == 0 {
            let sep = (0..col_count)
                .map(|_| "---")
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("| {} |", sep));
        }
    }
    lines.join("\n")
}

/// Extract notes body text (skips slide-number placeholders).
fn extract_notes_text(xml: &str) -> String {
    let doc = match roxmltree::Document::parse(xml) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };
    let root = doc.root_element();

    root.descendants()
        .filter(|n| n.tag_name().name() == "sp")
        .filter(|sp| {
            // Only extract body-type placeholders (skip sldNum, dt, etc.)
            let ph_type = sp
                .descendants()
                .find(|n| n.tag_name().name() == "ph")
                .and_then(|n| n.attribute("type"))
                .unwrap_or("body");
            ph_type == "body"
        })
        .map(|sp| extract_txbody_text(&sp))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
