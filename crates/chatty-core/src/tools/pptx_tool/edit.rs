use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use anyhow::{Context, anyhow};
use pptx_writer::shapes::ShapeTree;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zip::write::SimpleFileOptions;

use crate::services::filesystem_service::FileSystemService;

use super::PptxToolError;
use super::write::{
    PptxShapeSpec, TextStyleSpec, build_shape_xml, build_title_shape_xml, insert_shape_xml,
};

fn operation_type_discriminator_schema(operation: &str) -> Value {
    serde_json::json!({
        "type": "string",
        "enum": [operation]
    })
}

fn text_style_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "font_size": { "type": "number" },
            "bold": { "type": "boolean" },
            "italic": { "type": "boolean" },
            "color": { "type": "string", "description": "Hex RGB, with or without leading #, e.g. FF0000" }
        }
    })
}

fn table_rows_schema() -> Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "array",
            "items": { "type": "string" }
        }
    })
}

fn edit_pptx_parameters_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the existing .pptx file"
            },
            "output_path": {
                "type": "string",
                "description": "Optional output path for edited presentation. If omitted, edits in place."
            },
            "operations": {
                "type": "array",
                "description": "Ordered PPTX edit operations to apply",
                "items": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "type": operation_type_discriminator_schema("set_slide_title"),
                                "slide": { "type": "integer", "description": "1-based slide index" },
                                "title": { "type": "string" }
                            },
                            "required": ["type", "slide", "title"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": operation_type_discriminator_schema("add_text_box"),
                                "slide": { "type": "integer", "description": "1-based slide index" },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "width": { "type": "number" },
                                "height": { "type": "number" },
                                "text": { "type": "string" },
                                "style": text_style_schema()
                            },
                            "required": ["type", "slide", "x", "y", "width", "height", "text"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": operation_type_discriminator_schema("add_bullet_list"),
                                "slide": { "type": "integer", "description": "1-based slide index" },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "width": { "type": "number" },
                                "height": { "type": "number" },
                                "items": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                },
                                "style": text_style_schema()
                            },
                            "required": ["type", "slide", "x", "y", "width", "height", "items"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": operation_type_discriminator_schema("add_table"),
                                "slide": { "type": "integer", "description": "1-based slide index" },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "width": { "type": "number" },
                                "height": { "type": "number" },
                                "rows": table_rows_schema()
                            },
                            "required": ["type", "slide", "x", "y", "width", "height", "rows"]
                        }
                    ]
                }
            }
        },
        "required": ["path", "operations"]
    })
}

#[derive(Deserialize, Serialize)]
pub struct EditPptxArgs {
    pub path: String,
    #[serde(default)]
    pub output_path: Option<String>,
    pub operations: Vec<EditPptxOperation>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditPptxOperation {
    SetSlideTitle {
        slide: usize,
        title: String,
    },
    AddTextBox {
        slide: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        text: String,
        #[serde(default)]
        style: Option<TextStyleSpec>,
    },
    AddBulletList {
        slide: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        items: Vec<String>,
        #[serde(default)]
        style: Option<TextStyleSpec>,
    },
    AddTable {
        slide: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        rows: Vec<Vec<String>>,
    },
}

impl EditPptxOperation {
    fn slide_number(&self) -> usize {
        match self {
            Self::SetSlideTitle { slide, .. }
            | Self::AddTextBox { slide, .. }
            | Self::AddBulletList { slide, .. }
            | Self::AddTable { slide, .. } => *slide,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EditPptxOutput {
    pub path: String,
    pub slide_count: usize,
    pub operations_applied: usize,
    pub message: String,
}

#[derive(Clone)]
pub struct EditPptxTool {
    service: Arc<FileSystemService>,
}

impl EditPptxTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for EditPptxTool {
    const NAME: &'static str = "edit_pptx";
    type Error = PptxToolError;
    type Args = EditPptxArgs;
    type Output = EditPptxOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "edit_pptx".to_string(),
            description: "Edit an existing PowerPoint presentation (.pptx) with style-preserving operations.\n\
                         Applies targeted XML-level updates to specific slides while preserving the template and existing slide formatting.\n\
                         \n\
                         Supported operations: set_slide_title, add_text_box, add_bullet_list, add_table.\n\
                         \n\
                         Examples:\n\
                         - Update title: {\"path\":\"deck.pptx\",\"operations\":[{\"type\":\"set_slide_title\",\"slide\":1,\"title\":\"Q2 Review\"}]}\n\
                         - Add text box: {\"path\":\"deck.pptx\",\"operations\":[{\"type\":\"add_text_box\",\"slide\":2,\"x\":0.8,\"y\":1.8,\"width\":8.0,\"height\":1.5,\"text\":\"Updated guidance\"}]}"
                .to_string(),
            parameters: edit_pptx_parameters_schema(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let output_canonical = if let Some(ref out) = args.output_path {
            self.service.resolve_new_path(out).await?
        } else {
            canonical.clone()
        };

        let source_bytes = std::fs::read(&canonical)
            .with_context(|| format!("Failed to read '{}'", canonical.display()))?;
        let operations = args.operations.clone();
        let requested_path = args.path.clone();
        let (edited_bytes, slide_count) = tokio::task::spawn_blocking(move || {
            apply_style_preserving_edits(source_bytes, &operations, &requested_path)
        })
        .await
        .map_err(|e| anyhow!("Failed to edit PPTX '{}': {}", args.path, e))??;

        if let Some(parent) = output_canonical.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create output directory '{}'", parent.display())
            })?;
        }
        std::fs::write(&output_canonical, edited_bytes)
            .with_context(|| format!("Failed to write '{}'", output_canonical.display()))?;

        Ok(EditPptxOutput {
            path: output_canonical.display().to_string(),
            slide_count,
            operations_applied: args.operations.len(),
            message: format!(
                "Applied {} operation(s) to '{}' and saved to '{}'.",
                args.operations.len(),
                canonical.display(),
                output_canonical.display()
            ),
        })
    }
}

fn apply_style_preserving_edits(
    pptx_bytes: Vec<u8>,
    operations: &[EditPptxOperation],
    requested_path: &str,
) -> Result<(Vec<u8>, usize), PptxToolError> {
    if operations.is_empty() {
        return Err(anyhow!("operations must contain at least one operation").into());
    }

    let cursor = Cursor::new(pptx_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .with_context(|| format!("'{}' is not a valid PPTX/ZIP file", requested_path))?;

    let file_names: Vec<String> = archive.file_names().map(|s| s.to_string()).collect();
    let mut available_slides: Vec<usize> = file_names
        .iter()
        .filter_map(|name| parse_slide_number(name))
        .collect();
    available_slides.sort_unstable();
    available_slides.dedup();

    if available_slides.is_empty() {
        return Err(anyhow!("No slides found in '{}'", requested_path).into());
    }

    for op in operations {
        let slide = op.slide_number();
        if !available_slides.contains(&slide) {
            return Err(anyhow!(
                "Slide {} does not exist. Available slides: {:?}",
                slide,
                available_slides
            )
            .into());
        }
    }

    let output = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(output);

    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .with_context(|| format!("Failed to read archive entry at index {}", idx))?;
        let name = entry.name().to_string();
        let compression = entry.compression();
        let unix_mode = entry.unix_mode().unwrap_or(0o644);
        let is_dir = entry.is_dir();

        let options = SimpleFileOptions::default()
            .compression_method(compression)
            .unix_permissions(unix_mode);

        if is_dir {
            writer
                .add_directory(name.clone(), options)
                .with_context(|| format!("Failed to add directory entry '{}'", name))?;
            continue;
        }

        writer
            .start_file(name.clone(), options)
            .with_context(|| format!("Failed to start archive entry '{}'", name))?;
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .with_context(|| format!("Failed to read archive entry '{}'", name))?;

        if let Some(slide_no) = parse_slide_number(&name) {
            let ops_for_slide: Vec<&EditPptxOperation> = operations
                .iter()
                .filter(|op| op.slide_number() == slide_no)
                .collect();
            if !ops_for_slide.is_empty() {
                let slide_xml = String::from_utf8(content).with_context(|| {
                    format!("Slide {} XML entry '{}' is not valid UTF-8", slide_no, name)
                })?;
                let edited_xml = apply_slide_operations(&slide_xml, &ops_for_slide)?;
                writer
                    .write_all(edited_xml.as_bytes())
                    .with_context(|| format!("Failed to write edited slide entry '{}'", name))?;
                continue;
            }
        }

        writer
            .write_all(&content)
            .with_context(|| format!("Failed to copy archive entry '{}'", name))?;
    }

    let output_cursor = writer
        .finish()
        .context("Failed to finalize edited PPTX archive")?;
    Ok((output_cursor.into_inner(), available_slides.len()))
}

fn apply_slide_operations(
    slide_xml: &str,
    operations: &[&EditPptxOperation],
) -> Result<String, PptxToolError> {
    let mut updated = slide_xml.as_bytes().to_vec();

    for op in operations {
        match op {
            EditPptxOperation::SetSlideTitle { title, .. } => {
                let xml = std::str::from_utf8(&updated)
                    .context("Slide XML is not valid UTF-8 during title update")?;
                if let Some(replaced) = replace_title_text(xml, title) {
                    updated = replaced.into_bytes();
                } else {
                    let title_xml = build_title_shape_xml(next_shape_id(&updated)?, title)?;
                    updated = insert_shape_xml(&updated, &title_xml)?;
                }
            }
            EditPptxOperation::AddTextBox {
                x,
                y,
                width,
                height,
                text,
                style,
                ..
            } => {
                let shape = PptxShapeSpec::TextBox {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    text: text.clone(),
                    style: style.clone(),
                };
                let shape_xml = build_shape_xml(next_shape_id(&updated)?, &shape)?;
                updated = insert_shape_xml(&updated, &shape_xml)?;
            }
            EditPptxOperation::AddBulletList {
                x,
                y,
                width,
                height,
                items,
                style,
                ..
            } => {
                let shape = PptxShapeSpec::BulletList {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    items: items.clone(),
                    style: style.clone(),
                };
                let shape_xml = build_shape_xml(next_shape_id(&updated)?, &shape)?;
                updated = insert_shape_xml(&updated, &shape_xml)?;
            }
            EditPptxOperation::AddTable {
                x,
                y,
                width,
                height,
                rows,
                ..
            } => {
                let shape = PptxShapeSpec::Table {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    rows: rows.clone(),
                };
                let shape_xml = build_shape_xml(next_shape_id(&updated)?, &shape)?;
                updated = insert_shape_xml(&updated, &shape_xml)?;
            }
        }
    }

    String::from_utf8(updated)
        .map_err(|e| anyhow!("Edited slide XML is not valid UTF-8: {}", e).into())
}

fn next_shape_id(slide_xml: &[u8]) -> Result<u32, PptxToolError> {
    let tree = ShapeTree::from_slide_xml(slide_xml).context("Failed to parse slide XML")?;
    tree.max_shape_id()
        .0
        .checked_add(1)
        .ok_or_else(|| anyhow!("No available shape IDs remain on this slide").into())
}

/// Parse slide number from "ppt/slides/slide3.xml" -> Some(3).
fn parse_slide_number(name: &str) -> Option<usize> {
    let stem = name.strip_prefix("ppt/slides/slide")?;
    let stem = stem.strip_suffix(".xml")?;
    stem.parse::<usize>().ok()
}

fn replace_title_text(slide_xml: &str, title: &str) -> Option<String> {
    let mut cursor = 0usize;
    while let Some(start_rel) = slide_xml[cursor..].find("<p:sp>") {
        let start = cursor + start_rel;
        let end_rel = slide_xml[start..].find("</p:sp>")?;
        let end = start + end_rel + "</p:sp>".len();
        let shape = &slide_xml[start..end];

        if !shape.contains("type=\"title\"") && !shape.contains("type=\"ctrTitle\"") {
            cursor = end;
            continue;
        }

        let escaped = escape_xml_text(title);
        let replaced_shape = replace_text_runs_in_shape(shape, &escaped)?;

        let mut replaced = String::with_capacity(
            slide_xml.len() + replaced_shape.len().saturating_sub(shape.len()),
        );
        replaced.push_str(&slide_xml[..start]);
        replaced.push_str(&replaced_shape);
        replaced.push_str(&slide_xml[end..]);
        return Some(replaced);
    }

    None
}

fn replace_text_runs_in_shape(shape_xml: &str, escaped_title: &str) -> Option<String> {
    let mut cursor = 0usize;
    let mut replaced_any = false;
    let mut wrote_title = false;
    let mut out = String::with_capacity(shape_xml.len() + escaped_title.len());

    while let Some(start_rel) = shape_xml[cursor..].find("<a:t>") {
        let start = cursor + start_rel;
        let content_start = start + "<a:t>".len();
        let end_rel = shape_xml[content_start..].find("</a:t>")?;
        let content_end = content_start + end_rel;

        out.push_str(&shape_xml[cursor..content_start]);
        if !wrote_title {
            out.push_str(escaped_title);
            wrote_title = true;
        }
        cursor = content_end;
        replaced_any = true;
    }

    if !replaced_any {
        return None;
    }

    out.push_str(&shape_xml[cursor..]);
    Some(out)
}

fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::edit_pptx_parameters_schema;
    use rig_core::completion::ToolDefinition;
    use rig_core::providers::gemini::completion::gemini_api_types::{Schema, Tool};
    use serde_json::Value;

    fn assert_no_empty_types(schema: &Schema) {
        assert!(
            !schema.r#type.is_empty(),
            "Gemini schema type should never be empty"
        );

        if let Some(items) = &schema.items {
            assert_no_empty_types(items);
        }

        if let Some(properties) = &schema.properties {
            for property_schema in properties.values() {
                assert_no_empty_types(property_schema);
            }
        }
    }

    #[test]
    fn edit_pptx_schema_uses_anyof_not_oneof() {
        let schema = edit_pptx_parameters_schema();
        assert!(
            schema["properties"]["operations"]["items"]
                .get("anyOf")
                .is_some()
        );
        assert!(
            schema["properties"]["operations"]["items"]
                .get("oneOf")
                .is_none()
        );
    }

    #[test]
    fn edit_pptx_schema_operation_type_tags_are_string_enums() {
        let schema = edit_pptx_parameters_schema();
        let variants = schema["properties"]["operations"]["items"]["anyOf"]
            .as_array()
            .expect("operations.anyOf should be an array");

        for variant in variants {
            let type_schema = &variant["properties"]["type"];
            assert_eq!(type_schema["type"], Value::String("string".to_string()));
            assert_eq!(type_schema["enum"].as_array().map(Vec::len), Some(1));
        }
    }

    #[test]
    fn edit_pptx_schema_converts_to_gemini_without_empty_types() {
        let gemini_tool = Tool::try_from(ToolDefinition {
            name: "edit_pptx".to_string(),
            description: "Edit an existing PPTX file".to_string(),
            parameters: edit_pptx_parameters_schema(),
        })
        .expect("edit_pptx schema should convert for Gemini");

        let parameters = gemini_tool.function_declarations[0]
            .parameters
            .as_ref()
            .expect("edit_pptx should expose Gemini parameters");

        assert_no_empty_types(parameters);
    }
}
