use base64::Engine;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use rmcp::model::CallToolRequestParams;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;

/// Error type for publish module tool operations
#[derive(Debug, thiserror::Error)]
pub enum PublishModuleToolError {
    #[error("Publish error: {0}")]
    OperationError(#[from] anyhow::Error),
}

#[derive(Deserialize, Serialize)]
pub struct PublishModuleArgs {
    /// Path to the .wasm file to publish (relative to workspace or absolute)
    pub wasm_path: String,
    /// TOML manifest string with module metadata (name, display_name, description, version, etc.)
    pub manifest_toml: String,
}

#[derive(Debug, Serialize)]
pub struct PublishModuleOutput {
    pub success: bool,
    pub message: String,
}

/// A composite tool that reads a WASM binary from disk, base64-encodes it,
/// and publishes it to the hive registry via the MCP `publish_module` tool.
///
/// This avoids shuttling large base64 blobs through the LLM context window.
#[derive(Clone)]
pub struct PublishModuleTool {
    server_sink: Arc<rmcp::service::ServerSink>,
    workspace_dir: Option<String>,
}

impl PublishModuleTool {
    pub fn new(server_sink: rmcp::service::ServerSink, workspace_dir: Option<String>) -> Self {
        Self {
            server_sink: Arc::new(server_sink),
            workspace_dir,
        }
    }

    fn resolve_path(&self, path: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(path);
        if p.is_absolute() {
            p
        } else if let Some(ref ws) = self.workspace_dir {
            std::path::PathBuf::from(ws).join(path)
        } else {
            p
        }
    }
}

impl Tool for PublishModuleTool {
    const NAME: &'static str = "publish_wasm_module";
    type Error = PublishModuleToolError;
    type Args = PublishModuleArgs;
    type Output = PublishModuleOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "publish_wasm_module".to_string(),
            description: "Publish a WASM module to the hive registry. \
                         Reads the binary file from disk, base64-encodes it, \
                         and uploads it together with a TOML manifest via MCP. \
                         The manifest should be a flat TOML string with fields: \
                         name, display_name, description, version (required), \
                         plus optional: license, tags, category, pricing_model.\n\
                         \n\
                         Example:\n\
                         {\n\
                           \"wasm_path\": \"/path/to/module.wasm\",\n\
                           \"manifest_toml\": \"name = \\\"my-module\\\"\\n\
                             display_name = \\\"My Module\\\"\\n\
                             description = \\\"A demo module\\\"\\n\
                             version = \\\"0.1.0\\\"\\n\
                             license = \\\"MIT\\\"\\n\
                             tags = [\\\"demo\\\"]\\n\
                             category = \\\"utility\\\"\\n\
                             pricing_model = \\\"free\\\"\"\n\
                         }"
            .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "wasm_path": {
                        "type": "string",
                        "description": "Path to the .wasm file (absolute or relative to workspace)"
                    },
                    "manifest_toml": {
                        "type": "string",
                        "description": "Flat TOML string with module metadata (name, display_name, description, version required)"
                    }
                },
                "required": ["wasm_path", "manifest_toml"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.resolve_path(&args.wasm_path);

        // Read WASM binary from disk
        let wasm_bytes = tokio::fs::read(&path).await.map_err(|e| {
            anyhow::anyhow!("Failed to read WASM file at {}: {}", path.display(), e)
        })?;

        if wasm_bytes.len() < 4 || &wasm_bytes[..4] != b"\x00asm" {
            return Err(anyhow::anyhow!(
                "File at {} does not appear to be a valid WASM module (bad magic number)",
                path.display()
            )
            .into());
        }

        tracing::info!(
            path = %path.display(),
            size = wasm_bytes.len(),
            "Read WASM binary, encoding as base64 for MCP publish"
        );

        // Base64-encode (standard, with padding — hive server handles both)
        let wasm_b64 = base64::engine::general_purpose::STANDARD.encode(&wasm_bytes);

        // Build the MCP tool call arguments
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "manifest_toml".to_string(),
            serde_json::Value::String(args.manifest_toml),
        );
        arguments.insert(
            "wasm_base64".to_string(),
            serde_json::Value::String(wasm_b64),
        );

        let params = CallToolRequestParams {
            meta: None,
            name: Cow::Borrowed("publish_module"),
            arguments: Some(arguments),
            task: None,
        };

        // Call publish_module via MCP ServerSink
        let result = self
            .server_sink
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("MCP call_tool failed: {}", e))?;

        // Extract text from result
        let text = result
            .content
            .iter()
            .filter_map(|c| match c.raw {
                rmcp::model::RawContent::Text(ref t) => Some(t.text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let success = !result.is_error.unwrap_or(false);

        Ok(PublishModuleOutput {
            success,
            message: text,
        })
    }
}
