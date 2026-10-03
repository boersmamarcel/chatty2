use base64::Engine;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use rmcp::model::CallToolRequestParams;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::tools::ToolError;

#[derive(Deserialize, Serialize)]
pub struct PublishModuleArgs {
    /// Path to the .wasm file to publish (relative to workspace or absolute)
    pub wasm_path: String,
    /// TOML manifest string with module metadata (name, display_name, description, version, etc.)
    pub manifest_toml: String,
    /// The step-up request a first call filed (CX-0b): once the person has
    /// approved it, the second call passes its id to publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_up_request_id: Option<String>,
}

/// A module's name and version from its manifest, flat or under `[module]`,
/// as hive-registry reads them: the target and value its `module_publish`
/// step-up binds.
fn manifest_name_version(manifest_toml: &str) -> Result<(String, String), anyhow::Error> {
    let top: toml::Table = manifest_toml
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid manifest TOML: {e}"))?;
    let listing = match top.get("module") {
        Some(toml::Value::Table(module)) => module,
        _ => &top,
    };
    let field = |key: &str| {
        listing
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("The manifest has no `{key}`"))
    };
    Ok((field("name")?, field("version")?))
}

/// The JSON a registry MCP tool answered with, from its text content.
fn tool_json(result: &rmcp::model::CallToolResult) -> Result<serde_json::Value, anyhow::Error> {
    let text = result_text(result);
    if result.is_error.unwrap_or(false) {
        return Err(anyhow::anyhow!("{text}"));
    }
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("Unexpected registry answer: {e}"))
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            rmcp::model::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the tool does with a step-up request's status: publish with the
/// assertion, or tell the person what to do next.
#[derive(Debug, PartialEq, Eq)]
enum StepUpNext {
    Publish(String),
    Wait(String),
}

/// Read a `get_step_up_request` answer for `name`@`version`.
fn step_up_next(status: &serde_json::Value, name: &str, version: &str) -> StepUpNext {
    let field = |key: &str| status.get(key).and_then(|v| v.as_str()).unwrap_or_default();
    if field("action") != "module_publish" || field("target") != name || field("value") != version
    {
        return StepUpNext::Wait(format!(
            "That step-up request is not for publishing {name}@{version}. \
             Call publish_wasm_module again without step_up_request_id to file a new one."
        ));
    }
    match (field("status"), status.get("assertion").and_then(|v| v.as_str())) {
        ("approved", Some(assertion)) => StepUpNext::Publish(assertion.to_string()),
        ("expired", _) => StepUpNext::Wait(format!(
            "The approval for publishing {name}@{version} expired. \
             Call publish_wasm_module again without step_up_request_id to file a new one."
        )),
        _ => StepUpNext::Wait(format!(
            "Publishing {name}@{version} is still waiting for approval. Ask the user to open {} \
             in their browser, sign in again and approve it, then call publish_wasm_module \
             again with the same step_up_request_id.",
            field("page")
        )),
    }
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

    /// Call registry MCP tool `name` with `arguments` (a JSON object).
    async fn call_registry(
        &self,
        name: &'static str,
        arguments: serde_json::Value,
    ) -> Result<rmcp::model::CallToolResult, anyhow::Error> {
        let serde_json::Value::Object(arguments) = arguments else {
            unreachable!("registry tool arguments are a JSON object");
        };
        let params = CallToolRequestParams::new(name).with_arguments(arguments);
        self.server_sink
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("MCP call_tool {name} failed: {e}"))
    }

    fn resolve_path(&self, path: &str) -> Result<std::path::PathBuf, anyhow::Error> {
        let p = std::path::PathBuf::from(path);
        let resolved = if p.is_absolute() {
            p
        } else if let Some(ref ws) = self.workspace_dir {
            std::path::PathBuf::from(ws).join(path)
        } else {
            p
        };

        // Canonicalize to resolve symlinks and ../ components, then enforce
        // that the result lives inside the workspace directory.
        if let Some(ref ws) = self.workspace_dir {
            let ws_canon = std::path::PathBuf::from(ws)
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("Cannot resolve workspace dir: {e}"))?;
            let resolved_canon = resolved
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("Cannot resolve path {}: {e}", resolved.display()))?;
            if !resolved_canon.starts_with(&ws_canon) {
                return Err(anyhow::anyhow!(
                    "Path {} is outside the workspace ({})",
                    resolved_canon.display(),
                    ws_canon.display()
                ));
            }
            Ok(resolved_canon)
        } else {
            Ok(resolved)
        }
    }
}

impl Tool for PublishModuleTool {
    const NAME: &'static str = "publish_wasm_module";
    type Error = ToolError;
    type Args = PublishModuleArgs;
    type Output = PublishModuleOutput;

    fn description(&self) -> String {
        "Publish a WASM module to the hive registry. \
                         Reads the binary file from disk, base64-encodes it, \
                         and uploads it together with a TOML manifest via MCP. \
                         Every publish needs the user's approval (a step-up): \
                         the first call files the request and answers with a page \
                         for the user to open, sign in again and approve on, and \
                         a step_up_request_id; after they approve, call again with \
                         the same arguments plus that step_up_request_id. \
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
        .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "wasm_path": {
                    "type": "string",
                    "description": "Path to the .wasm file (absolute or relative to workspace)"
                },
                "manifest_toml": {
                    "type": "string",
                    "description": "Flat TOML string with module metadata (name, display_name, description, version required)"
                },
                "step_up_request_id": {
                    "type": "string",
                    "description": "The id the first call returned, once the user has approved the publish"
                }
            },
            "required": ["wasm_path", "manifest_toml"]
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
        let path = self
            .resolve_path(&args.wasm_path)
            .map_err(|e| anyhow::anyhow!("Path resolution failed: {e}"))?;

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

        // CX-0: a publish needs a `module_publish` step-up for exactly this
        // name and version. The first call files it and hands the approval
        // to the person; the second collects the assertion and publishes.
        let (name, version) = manifest_name_version(&args.manifest_toml)?;
        let Some(request_id) = args.step_up_request_id else {
            let filed = self
                .call_registry(
                    "request_step_up",
                    serde_json::json!({
                        "action": "module_publish",
                        "target": name,
                        "value": version,
                    }),
                )
                .await?;
            let filed = tool_json(&filed)?;
            let id = filed.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let page = filed.get("page").and_then(|v| v.as_str()).unwrap_or_default();
            return Ok(PublishModuleOutput {
                success: false,
                message: format!(
                    "Publishing {name}@{version} needs the user's approval. Ask them to open \
                     {page} in their browser, sign in again and approve it. Then call \
                     publish_wasm_module again with the same arguments and \
                     step_up_request_id = \"{id}\"."
                ),
            });
        };
        let status = self
            .call_registry(
                "get_step_up_request",
                serde_json::json!({ "id": request_id }),
            )
            .await?;
        let assertion = match step_up_next(&tool_json(&status)?, &name, &version) {
            StepUpNext::Publish(assertion) => assertion,
            StepUpNext::Wait(message) => {
                return Ok(PublishModuleOutput {
                    success: false,
                    message,
                });
            }
        };

        // Base64-encode (standard, with padding — hive server handles both)
        let wasm_b64 = base64::engine::general_purpose::STANDARD.encode(&wasm_bytes);

        let result = self
            .call_registry(
                "publish_module",
                serde_json::json!({
                    "manifest_toml": args.manifest_toml,
                    "wasm_base64": wasm_b64,
                    "step_up_assertion": assertion,
                }),
            )
            .await?;
        let text = result_text(&result);
        let success = !result.is_error.unwrap_or(false);

        Ok(PublishModuleOutput {
            success,
            message: text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_step_up_binds_the_manifests_name_and_version_flat_or_sectioned() {
        let flat = "name = \"benford\"\nversion = \"1.2.0\"\ndescription = \"d\"";
        assert_eq!(
            manifest_name_version(flat).unwrap(),
            ("benford".to_string(), "1.2.0".to_string())
        );
        let sectioned = "[module]\nname = \"benford\"\nversion = \"1.3.0\"";
        assert_eq!(
            manifest_name_version(sectioned).unwrap(),
            ("benford".to_string(), "1.3.0".to_string())
        );
        assert!(manifest_name_version("name = \"x\"").is_err());
    }

    #[test]
    fn only_an_approved_request_for_this_module_and_version_publishes() {
        let status = |action: &str, target: &str, value: &str, state: &str| {
            json!({
                "id": "r", "action": action, "target": target, "value": value,
                "status": state, "page": "http://localhost:8080/step-up",
                "assertion": (state == "approved").then_some("a-1"),
            })
        };
        assert_eq!(
            step_up_next(
                &status("module_publish", "m", "1.0.0", "approved"),
                "m",
                "1.0.0"
            ),
            StepUpNext::Publish("a-1".to_string())
        );
        for (other, why) in [
            (status("module_publish", "m", "1.0.0", "pending"), "waiting"),
            (status("module_publish", "m", "1.0.0", "expired"), "expired"),
            (status("module_publish", "m", "2.0.0", "approved"), "not for"),
            (status("module_publish", "n", "1.0.0", "approved"), "not for"),
            (status("pricing_set", "m", "1.0.0", "approved"), "not for"),
        ] {
            match step_up_next(&other, "m", "1.0.0") {
                StepUpNext::Wait(message) => assert!(message.contains(why), "{message}"),
                StepUpNext::Publish(_) => panic!("published on {other}"),
            }
        }
    }
}
