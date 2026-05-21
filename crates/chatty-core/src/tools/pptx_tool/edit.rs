use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use anyhow::{Context, anyhow};
use pptx_writer::media::Image;
use pptx_writer::shapes::ShapeTree;
use pptx_writer::{Inches, Presentation};
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

fn image_path_schema() -> Value {
    serde_json::json!({
        "type": "string",
        "description": "Path to an image file (png, jpg, jpeg, gif, bmp, tiff, webp, svg)"
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
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": operation_type_discriminator_schema("add_image"),
                                "slide": { "type": "integer", "description": "1-based slide index" },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "width": { "type": "number" },
                                "height": { "type": "number" },
                                "image_path": image_path_schema()
                            },
                            "required": ["type", "slide", "x", "y", "width", "height", "image_path"]
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
    AddImage {
        slide: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        image_path: String,
    },
}

impl EditPptxOperation {
    fn slide_number(&self) -> usize {
        match self {
            Self::SetSlideTitle { slide, .. }
            | Self::AddTextBox { slide, .. }
            | Self::AddBulletList { slide, .. }
            | Self::AddTable { slide, .. }
            | Self::AddImage { slide, .. } => *slide,
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
                         Supported operations: set_slide_title, add_text_box, add_bullet_list, add_table, add_image.\n\
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
        let mut operations = args.operations.clone();
        for op in &mut operations {
            if let EditPptxOperation::AddImage { image_path, .. } = op {
                let resolved = self.service.resolve_path(image_path).await?;
                *image_path = resolved.display().to_string();
            }
        }
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

    let xml_ops: Vec<EditPptxOperation> = operations
        .iter()
        .filter(|op| !matches!(op, EditPptxOperation::AddImage { .. }))
        .cloned()
        .collect();
    let image_ops: Vec<EditPptxOperation> = operations
        .iter()
        .filter(|op| matches!(op, EditPptxOperation::AddImage { .. }))
        .cloned()
        .collect();

    let mut current_bytes = if xml_ops.is_empty() {
        archive.into_inner().into_inner()
    } else {
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
                let ops_for_slide: Vec<&EditPptxOperation> = xml_ops
                    .iter()
                    .filter(|op| op.slide_number() == slide_no)
                    .collect();
                if !ops_for_slide.is_empty() {
                    let slide_xml = String::from_utf8(content).with_context(|| {
                        format!("Slide {} XML entry '{}' is not valid UTF-8", slide_no, name)
                    })?;
                    let edited_xml = apply_slide_operations(&slide_xml, &ops_for_slide)?;
                    writer.write_all(edited_xml.as_bytes()).with_context(|| {
                        format!("Failed to write edited slide entry '{}'", name)
                    })?;
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
        output_cursor.into_inner()
    };

    if !image_ops.is_empty() {
        current_bytes = apply_image_operations(current_bytes, &image_ops)?;
    }

    Ok((current_bytes, available_slides.len()))
}

const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

fn apply_image_operations(
    pptx_bytes: Vec<u8>,
    operations: &[EditPptxOperation],
) -> Result<Vec<u8>, PptxToolError> {
    let mut presentation = Presentation::from_bytes(&pptx_bytes)
        .map_err(|e| anyhow!("Failed to parse PPTX for image insertion: {}", e))?;
    let slides = presentation
        .slides()
        .map_err(|e| anyhow!("Failed to resolve slide references: {}", e))?;

    for op in operations {
        if let EditPptxOperation::AddImage {
            slide,
            x,
            y,
            width,
            height,
            image_path,
        } = op
        {
            let slide_ref = slides.get(slide.saturating_sub(1)).ok_or_else(|| {
                anyhow!(
                    "Slide {} does not exist for add_image operation ({} slides available)",
                    slide,
                    slides.len()
                )
            })?;

            let image = Image::from_file(image_path).map_err(|e| {
                anyhow!(
                    "Failed to load image '{}' for add_image operation: {}",
                    image_path,
                    e
                )
            })?;
            let image_partname = presentation
                .add_image(&image)
                .map_err(|e| anyhow!("Failed to add image media part: {}", e))?;
            let image_part_uri = pptx_writer::opc::PackURI::new(&image_partname).map_err(|e| {
                anyhow!(
                    "Invalid generated image part URI '{}': {}",
                    image_partname,
                    e
                )
            })?;

            let target_ref = image_part_uri.relative_ref(slide_ref.partname.base_uri());
            let slide_part = presentation
                .package_mut()
                .part_mut(&slide_ref.partname)
                .ok_or_else(|| anyhow!("Slide part '{}' not found", slide_ref.partname.as_str()))?;
            let image_r_id = slide_part
                .rels
                .add_relationship(IMAGE_REL_TYPE, &target_ref, false);

            let updated_slide_xml = ShapeTree::add_picture(
                &slide_part.blob,
                &image_r_id,
                Inches(*x).into(),
                Inches(*y).into(),
                Inches(*width).into(),
                Inches(*height).into(),
            )
            .map_err(|e| anyhow!("Failed to add image shape to slide {}: {}", slide, e))?;
            slide_part.blob = updated_slide_xml;
        }
    }

    presentation
        .to_bytes()
        .map_err(|e| anyhow!("Failed to serialize PPTX after image insertion: {}", e).into())
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
            EditPptxOperation::AddImage { .. } => {}
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
