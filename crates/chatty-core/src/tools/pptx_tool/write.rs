use std::fs;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use pptx_writer::Presentation;
use pptx_writer::WriteXml;
use pptx_writer::dml::fill::FillFormat;
use pptx_writer::dml::line::LineFormat;
use pptx_writer::enums::shapes::PpPlaceholderType;
use pptx_writer::shapes::{AutoShape, PlaceholderFormat, ShapeTree};
use pptx_writer::table::Table;
use pptx_writer::text::{BulletFormat, Paragraph, RgbColor, TextFrame};
use pptx_writer::units::{Emu, Inches, PlaceholderIndex, ShapeId};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

use crate::services::filesystem_service::FileSystemService;

use super::PptxToolError;

const DEFAULT_TITLE_X: f64 = 0.6;
const DEFAULT_TITLE_Y: f64 = 0.4;
const DEFAULT_TITLE_WIDTH: f64 = 8.2;
const DEFAULT_TITLE_HEIGHT: f64 = 0.7;
const DEFAULT_TITLE_FONT_SIZE: f64 = 28.0;

#[derive(Deserialize, Serialize)]
pub struct WritePptxArgs {
    pub path: String,
    pub slides: Vec<PptxSlideSpec>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PptxSlideSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub shapes: Vec<PptxShapeSpec>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PptxShapeSpec {
    TextBox {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        text: String,
        #[serde(default)]
        style: Option<TextStyleSpec>,
    },
    BulletList {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        items: Vec<String>,
        #[serde(default)]
        style: Option<TextStyleSpec>,
    },
    Table {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        rows: Vec<Vec<String>>,
    },
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct TextStyleSpec {
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WritePptxOutput {
    pub path: String,
    pub slide_count: usize,
    pub shapes_written: usize,
    pub bytes_written: usize,
}

#[derive(Clone)]
pub struct WritePptxTool {
    service: Arc<FileSystemService>,
}

impl WritePptxTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for WritePptxTool {
    const NAME: &'static str = "write_pptx";
    type Error = PptxToolError;
    type Args = WritePptxArgs;
    type Output = WritePptxOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "write_pptx".to_string(),
            description: "Create a PowerPoint presentation (.pptx) from structured JSON slides.\n\
                         Supports slide titles plus three shape types: text_box, bullet_list, and table.\n\
                         Shape coordinates and sizes use inches.\n\
                         \n\
                         Supported shape payloads:\n\
                         - text_box: positioned multi-paragraph text\n\
                         - bullet_list: positioned bullet items\n\
                         - table: positioned rows/columns of string data\n\
                         \n\
                         Example:\n\
                         {\"path\":\"deck.pptx\",\"slides\":[{\"title\":\"Quarterly Review\",\"shapes\":[{\"type\":\"bullet_list\",\"x\":0.8,\"y\":1.7,\"width\":8.0,\"height\":2.2,\"items\":[\"Revenue grew 18%\",\"Enterprise led expansion\"]},{\"type\":\"table\",\"x\":0.8,\"y\":4.3,\"width\":8.0,\"height\":1.2,\"rows\":[[\"Metric\",\"Value\"],[\"ARR\",\"$2.1M\"]]}]}]}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to write the .pptx file, relative to workspace root or absolute within workspace"
                    },
                    "slides": {
                        "type": "array",
                        "description": "Slides to include in the presentation",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "Optional slide title rendered near the top of the slide"
                                },
                                "shapes": {
                                    "type": "array",
                                    "items": {
                                        "anyOf": [
                                            {
                                                "type": "object",
                                                "properties": {
                                                    "type": { "type": "string", "enum": ["text_box"] },
                                                    "x": { "type": "number" },
                                                    "y": { "type": "number" },
                                                    "width": { "type": "number" },
                                                    "height": { "type": "number" },
                                                    "text": { "type": "string" },
                                                    "style": {
                                                        "type": "object",
                                                        "properties": {
                                                            "font_size": { "type": "number" },
                                                            "bold": { "type": "boolean" },
                                                            "italic": { "type": "boolean" },
                                                            "color": { "type": "string", "description": "Hex RGB, with or without leading #, e.g. FF0000" }
                                                        }
                                                    }
                                                },
                                                "required": ["type", "x", "y", "width", "height", "text"]
                                            },
                                            {
                                                "type": "object",
                                                "properties": {
                                                    "type": { "type": "string", "enum": ["bullet_list"] },
                                                    "x": { "type": "number" },
                                                    "y": { "type": "number" },
                                                    "width": { "type": "number" },
                                                    "height": { "type": "number" },
                                                    "items": {
                                                        "type": "array",
                                                        "items": { "type": "string" }
                                                    },
                                                    "style": {
                                                        "type": "object",
                                                        "properties": {
                                                            "font_size": { "type": "number" },
                                                            "bold": { "type": "boolean" },
                                                            "italic": { "type": "boolean" },
                                                            "color": { "type": "string", "description": "Hex RGB, with or without leading #, e.g. FF0000" }
                                                        }
                                                    }
                                                },
                                                "required": ["type", "x", "y", "width", "height", "items"]
                                            },
                                            {
                                                "type": "object",
                                                "properties": {
                                                    "type": { "type": "string", "enum": ["table"] },
                                                    "x": { "type": "number" },
                                                    "y": { "type": "number" },
                                                    "width": { "type": "number" },
                                                    "height": { "type": "number" },
                                                    "rows": {
                                                        "type": "array",
                                                        "items": {
                                                            "type": "array",
                                                            "items": { "type": "string" }
                                                        }
                                                    }
                                                },
                                                "required": ["type", "x", "y", "width", "height", "rows"]
                                            }
                                        ]
                                    }
                                }
                            },
                            "required": ["shapes"]
                        }
                    }
                },
                "required": ["path", "slides"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.slides.is_empty() {
            return Err(anyhow!("slides must contain at least one slide").into());
        }

        let canonical = self.service.resolve_new_path(&args.path).await?;
        if let Some(parent) = canonical.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
        }

        let (pptx_bytes, shapes_written) = build_presentation(&args.slides)?;
        let bytes_written = pptx_bytes.len();

        fs::write(&canonical, &pptx_bytes)
            .with_context(|| format!("Failed to write '{}'", canonical.display()))?;

        Ok(WritePptxOutput {
            path: canonical.display().to_string(),
            slide_count: args.slides.len(),
            shapes_written,
            bytes_written,
        })
    }
}

fn build_presentation(slides: &[PptxSlideSpec]) -> Result<(Vec<u8>, usize), PptxToolError> {
    let mut presentation = Presentation::new().context("Failed to create PPTX presentation")?;
    let layouts = presentation
        .slide_layouts()
        .context("Failed to load PPTX slide layouts")?;
    let layout = layouts
        .first()
        .ok_or_else(|| anyhow!("The PPTX template does not expose any slide layouts"))?
        .clone();

    let mut shapes_written = 0usize;

    for slide in slides {
        let slide_ref = presentation
            .add_slide(&layout)
            .context("Failed to add slide to presentation")?;
        let existing_xml = presentation
            .slide_xml(&slide_ref)
            .context("Failed to read slide XML")?
            .to_vec();
        let (updated_xml, written_for_slide) = populate_slide(existing_xml, slide)?;
        *presentation
            .slide_xml_mut(&slide_ref)
            .context("Failed to update slide XML")? = updated_xml;
        shapes_written += written_for_slide;
    }

    let bytes = presentation
        .to_bytes()
        .context("Failed to serialize PPTX presentation")?;
    Ok((bytes, shapes_written))
}

fn populate_slide(
    slide_xml: Vec<u8>,
    slide: &PptxSlideSpec,
) -> Result<(Vec<u8>, usize), PptxToolError> {
    let tree = ShapeTree::from_slide_xml(&slide_xml).context("Failed to parse slide XML")?;
    let mut next_shape_id = tree.max_shape_id().0.saturating_add(1);
    let mut updated_xml = slide_xml;
    let mut shapes_written = 0usize;

    if let Some(title) = slide
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        let shape = build_title_shape(next_shape_id, title)?;
        updated_xml = insert_shape_xml(&updated_xml, &shape.to_xml_string())?;
        next_shape_id += 1;
        shapes_written += 1;
    }

    for shape in &slide.shapes {
        let shape_xml = build_shape_xml(next_shape_id, shape)?;
        updated_xml = insert_shape_xml(&updated_xml, &shape_xml)?;
        next_shape_id += 1;
        shapes_written += 1;
    }

    Ok((updated_xml, shapes_written))
}

fn build_title_shape(shape_id: u32, title: &str) -> Result<AutoShape, PptxToolError> {
    let mut shape = AutoShape::textbox(
        ShapeId(shape_id),
        format!("Title {shape_id}"),
        inches_to_emu(DEFAULT_TITLE_X, "slide.title.x", false)?,
        inches_to_emu(DEFAULT_TITLE_Y, "slide.title.y", false)?,
        inches_to_emu(DEFAULT_TITLE_WIDTH, "slide.title.width", true)?,
        inches_to_emu(DEFAULT_TITLE_HEIGHT, "slide.title.height", true)?,
    );
    shape.placeholder = Some(PlaceholderFormat {
        ph_type: Some(PpPlaceholderType::Title),
        idx: PlaceholderIndex(0),
        orient: None,
        sz: None,
    });
    shape.set_fill(FillFormat::no_fill());
    shape.set_line(LineFormat {
        fill: Some(FillFormat::no_fill()),
        ..LineFormat::new()
    });
    shape.text_frame = Some(build_text_frame(
        title,
        &TextStyleSpec {
            font_size: Some(DEFAULT_TITLE_FONT_SIZE),
            bold: Some(true),
            italic: None,
            color: None,
        },
    )?);
    Ok(shape)
}

fn build_shape_xml(shape_id: u32, shape: &PptxShapeSpec) -> Result<String, PptxToolError> {
    match shape {
        PptxShapeSpec::TextBox {
            x,
            y,
            width,
            height,
            text,
            style,
        } => {
            let mut shape = AutoShape::textbox(
                ShapeId(shape_id),
                format!("TextBox {shape_id}"),
                inches_to_emu(*x, "text_box.x", false)?,
                inches_to_emu(*y, "text_box.y", false)?,
                inches_to_emu(*width, "text_box.width", true)?,
                inches_to_emu(*height, "text_box.height", true)?,
            );
            shape.set_fill(FillFormat::no_fill());
            shape.set_line(LineFormat {
                fill: Some(FillFormat::no_fill()),
                ..LineFormat::new()
            });
            shape.text_frame = Some(build_text_frame(
                text,
                style.as_ref().unwrap_or(&TextStyleSpec::default()),
            )?);
            Ok(shape.to_xml_string())
        }
        PptxShapeSpec::BulletList {
            x,
            y,
            width,
            height,
            items,
            style,
        } => {
            if items.is_empty() {
                return Err(anyhow!("bullet_list.items must contain at least one entry").into());
            }

            let mut shape = AutoShape::textbox(
                ShapeId(shape_id),
                format!("BulletList {shape_id}"),
                inches_to_emu(*x, "bullet_list.x", false)?,
                inches_to_emu(*y, "bullet_list.y", false)?,
                inches_to_emu(*width, "bullet_list.width", true)?,
                inches_to_emu(*height, "bullet_list.height", true)?,
            );
            shape.set_fill(FillFormat::no_fill());
            shape.set_line(LineFormat {
                fill: Some(FillFormat::no_fill()),
                ..LineFormat::new()
            });
            shape.text_frame = Some(build_bullet_list_frame(
                items,
                style.as_ref().unwrap_or(&TextStyleSpec::default()),
            )?);
            Ok(shape.to_xml_string())
        }
        PptxShapeSpec::Table {
            x,
            y,
            width,
            height,
            rows,
        } => build_table_shape_xml(shape_id, *x, *y, *width, *height, rows),
    }
}

fn build_text_frame(text: &str, style: &TextStyleSpec) -> Result<TextFrame, PptxToolError> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut frame = TextFrame::new();
    frame.paragraphs_mut()[0].clear();

    if lines.is_empty() {
        return Ok(frame);
    }

    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            let paragraph = &mut frame.paragraphs_mut()[0];
            if !line.is_empty() {
                let run = paragraph.add_run();
                apply_text_style(run.font_mut(), style)?;
                run.set_text(line);
            }
        } else {
            let paragraph = frame.add_paragraph();
            if !line.is_empty() {
                let run = paragraph.add_run();
                apply_text_style(run.font_mut(), style)?;
                run.set_text(line);
            }
        }
    }

    Ok(frame)
}

fn build_bullet_list_frame(
    items: &[String],
    style: &TextStyleSpec,
) -> Result<TextFrame, PptxToolError> {
    let mut frame = TextFrame::new();
    frame.paragraphs_mut()[0].clear();

    for (index, item) in items.iter().enumerate() {
        if index == 0 {
            populate_bullet_paragraph(&mut frame.paragraphs_mut()[0], item, style)?;
        } else {
            let paragraph = frame.add_paragraph();
            populate_bullet_paragraph(paragraph, item, style)?;
        }
    }

    Ok(frame)
}

fn populate_bullet_paragraph(
    paragraph: &mut Paragraph,
    text: &str,
    style: &TextStyleSpec,
) -> Result<(), PptxToolError> {
    paragraph.set_bullet(BulletFormat::Character('•'));
    let run = paragraph.add_run();
    apply_text_style(run.font_mut(), style)?;
    run.set_text(text);
    Ok(())
}

fn build_table_shape_xml(
    shape_id: u32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rows: &[Vec<String>],
) -> Result<String, PptxToolError> {
    if rows.is_empty() {
        return Err(anyhow!("table.rows must contain at least one row").into());
    }
    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return Err(anyhow!("table.rows must contain at least one column").into());
    }

    let width_emu = inches_to_emu(width, "table.width", true)?;
    let height_emu = inches_to_emu(height, "table.height", true)?;
    let row_height = Emu(height_emu.0 / rows.len() as i64);
    let mut table = Table::new(rows.len(), col_count, width_emu, row_height);

    for (row_index, row) in rows.iter().enumerate() {
        for col_index in 0..col_count {
            let value = row.get(col_index).map(String::as_str).unwrap_or("");
            table.cell_mut(row_index, col_index).set_text(value);
        }
    }

    Ok(format!(
        r#"<p:graphicFrame xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:nvGraphicFramePr><p:cNvPr id="{shape_id}" name="Table {shape_id}"/><p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm>{}</p:graphicFrame>"#,
        inches_to_emu(x, "table.x", false)?.0,
        inches_to_emu(y, "table.y", false)?.0,
        width_emu.0,
        height_emu.0,
        table.to_graphic_data_xml(),
    ))
}

fn apply_text_style(
    font: &mut pptx_writer::text::Font,
    style: &TextStyleSpec,
) -> Result<(), PptxToolError> {
    if let Some(size) = style.font_size {
        font.set_size(size).map_err(anyhow::Error::from)?;
    }
    font.bold = style.bold;
    font.italic = style.italic;
    if let Some(color) = style.color.as_deref() {
        font.color = Some(parse_rgb_color(color)?);
    }
    Ok(())
}

fn parse_rgb_color(color: &str) -> Result<RgbColor, PptxToolError> {
    let normalized = color.trim().trim_start_matches('#');
    RgbColor::from_hex(normalized)
        .map_err(anyhow::Error::from)
        .map_err(Into::into)
}

fn inches_to_emu(value: f64, field: &str, strictly_positive: bool) -> Result<Emu, PptxToolError> {
    if !value.is_finite() {
        return Err(anyhow!("{field} must be a finite number").into());
    }
    if value < 0.0 {
        return Err(anyhow!("{field} must be 0 or greater").into());
    }
    if strictly_positive && value <= 0.0 {
        return Err(anyhow!("{field} must be greater than 0").into());
    }
    Ok(Inches(value).into())
}

fn insert_shape_xml(slide_xml: &[u8], shape_xml: &str) -> Result<Vec<u8>, PptxToolError> {
    let slide_str = std::str::from_utf8(slide_xml).context("Slide XML is not valid UTF-8")?;
    let insert_at = slide_str
        .find("</p:spTree>")
        .ok_or_else(|| anyhow!("Slide XML does not contain </p:spTree>"))?;

    let mut updated = Vec::with_capacity(slide_xml.len() + shape_xml.len());
    updated.extend_from_slice(&slide_xml[..insert_at]);
    updated.extend_from_slice(shape_xml.as_bytes());
    updated.extend_from_slice(&slide_xml[insert_at..]);
    Ok(updated)
}
