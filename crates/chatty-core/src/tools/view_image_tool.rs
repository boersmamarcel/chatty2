//! `view_image`: show an image in the workspace to the agent's own model.
//!
//! `add_attachment` displays a file to the *user* after the stream ends; the
//! agent never sees it. An agent that makes a picture (a screenshot of the
//! page it built, a chart, a diagram) has no way to look at it. This tool
//! returns the image in the tool result, as rig's `ToolResultContent::Image`:
//! where a provider takes images in a tool message it stays there, and where
//! it does not rig moves it into the next user message (`result_images`). So
//! the tool only has to read, check and return the file.
//!
//! It is offered only to an agent whose model accepts images and that may
//! read the workspace; the file is read through the same path gate as
//! `read_file`.

use std::sync::Arc;

use base64::Engine as _;
#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError, ToolOutput};
use rig_core::message::{DocumentSourceKind, Image, ImageMediaType, ToolResultContent};
use serde::Deserialize;

use crate::services::filesystem_service::FileSystemService;
use crate::tools::ToolError;

/// The largest image this tool returns: a screenshot is a few hundred KB, and
/// a larger file is nearly always something that should be scaled down first.
pub const MAX_VIEW_IMAGE_BYTES: u64 = 3 * 1024 * 1024;

#[derive(Deserialize)]
pub struct ViewImageArgs {
    pub path: String,
    /// What to look for, echoed back with the image so the model keeps it in view.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone)]
pub struct ViewImageTool {
    service: Arc<FileSystemService>,
}

impl ViewImageTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

/// The tool result for `bytes` named `name`: a line of text and the image.
/// The type is read from the bytes, never from the file name.
pub fn image_output(name: &str, bytes: &[u8], note: Option<&str>) -> Result<ToolOutput, ToolError> {
    let media_type = ImageMediaType::sniff(bytes).ok_or_else(|| {
        ToolError::OperationFailed(format!(
            "'{name}' is not a PNG, JPEG, GIF or WebP image (checked by its content, not its name)"
        ))
    })?;
    let mut text = format!("Image '{name}' ({} KB).", bytes.len().div_ceil(1000));
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        text.push_str(&format!(" Looking for: {note}"));
    }
    let image = Image {
        data: DocumentSourceKind::base64(base64::engine::general_purpose::STANDARD.encode(bytes)),
        media_type: Some(media_type),
        detail: None,
        native: None,
    };
    ToolOutput::content(vec![
        ToolResultContent::text(text),
        ToolResultContent::Image(image),
    ])
    .map_err(|error| ToolError::OperationFailed(error.to_string()))
}

impl Tool for ViewImageTool {
    const NAME: &'static str = "view_image";
    type Error = ToolError;
    type Args = ViewImageArgs;
    type Output = ToolOutput;

    fn description(&self) -> String {
        "Look at an image file in the workspace with your own eyes: a screenshot of a page you \
         built, a chart, a diagram, a photo. The image is shown to you in the result, so you can \
         judge it yourself (layout, spacing, readable text, contrast, anything cut off).\n\
         \n\
         PNG, JPEG, GIF and WebP, up to 3 MB; a larger file should be scaled down first. Use it \
         on pictures you or a tool produced, and say what you saw, not only that you looked.\n\
         \n\
         Example: {\"path\": \"shots/home-desktop.png\", \"note\": \"is any text cut off or low contrast?\"}"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file, relative to the workspace root or absolute within the workspace"
                },
                "note": {
                    "type": "string",
                    "description": "Optional: what you want to check in the image"
                }
            },
            "required": ["path"]
        })
    }

    /// The real failure text stays in front of the model (see `map_tool_error`).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let size = tokio::fs::metadata(&canonical)
            .await
            .map_err(|error| ToolError::OperationFailed(format!("'{}': {error}", args.path)))?
            .len();
        if size > MAX_VIEW_IMAGE_BYTES {
            return Err(ToolError::OperationFailed(format!(
                "'{}' is {} KB; the limit is {} KB. Scale it down (a smaller viewport, or a JPEG) and look again",
                args.path,
                size / 1000,
                MAX_VIEW_IMAGE_BYTES / 1000
            )));
        }
        let bytes = tokio::fs::read(&canonical)
            .await
            .map_err(|error| ToolError::OperationFailed(format!("'{}': {error}", args.path)))?;
        image_output(&args.path, &bytes, args.note.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-a-png";

    #[test]
    fn a_png_comes_back_as_text_and_an_image() {
        let output = image_output("shots/a.png", PNG, Some("is text cut off?")).unwrap();
        let content = output.as_content();
        assert_eq!(content.len(), 2);
        assert!(content[0].as_text().unwrap().contains("Looking for: is text cut off?"));
        match &content[1] {
            ToolResultContent::Image(image) => {
                assert_eq!(image.media_type, Some(ImageMediaType::PNG));
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn the_type_comes_from_the_bytes_not_the_name() {
        assert!(image_output("fake.png", b"not an image at all", None).is_err());
        let jpeg = image_output("a.txt", b"\xff\xd8\xff\xe0jpeg", None).unwrap();
        match &jpeg.as_content()[1] {
            ToolResultContent::Image(image) => assert_eq!(image.media_type, Some(ImageMediaType::JPEG)),
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn the_note_is_optional() {
        let output = image_output("a.png", PNG, None).unwrap();
        assert!(!output.as_content()[0].as_text().unwrap().contains("Looking for"));
        let output = image_output("a.png", PNG, Some("   ")).unwrap();
        assert!(!output.as_content()[0].as_text().unwrap().contains("Looking for"));
    }

    #[test]
    fn the_tool_definition_asks_for_a_path() {
        let definition = tool_definition(&ViewImageTool::new(test_service()));
        assert_eq!(definition.parameters["required"][0], "path");
        assert!(definition.description.contains("your own eyes"));
    }

    fn test_service() -> Arc<FileSystemService> {
        let dir = std::env::temp_dir();
        Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(FileSystemService::new(dir.to_str().unwrap()))
                .unwrap(),
        )
    }
}
