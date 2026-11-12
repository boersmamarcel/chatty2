//! MCP server tools as rig tools, on rmcp 3 (AGE-1101).
//!
//! rig-agent 0.42's `rmcp` feature carried this adapter but requires rmcp
//! ^2, and rmcp < 3.9 has RUSTSEC-2026-0457 and RUSTSEC-2026-0462. This is
//! that adapter's behaviour, kept so the model sees the same tools and
//! results: the MCP name, description and input schema as the definition;
//! a 300 s per-call deadline that cancels the request on the server; and the
//! same error kinds and messages (invalid arguments, timeout, request
//! failure, a tool that reports `isError`).
//!
//! One difference: rig 0.42 retired a tool from the registry once its
//! server's transport closed, and its `DynamicTool` has no such hook. A
//! closed server's tool now stays offered until the agent is rebuilt, and a
//! call to it fails with "request failed: Transport closed".

use std::time::Duration;

use rig_agent::tool::{DynamicTool, ToolContext, ToolExecutionError, ToolOutput};
use rig_core::tool::ContextValue;
use rig_core::message::{ImageMediaType, MimeType, ToolResultContent};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, ContentBlock,
    JsonObject, RequestMetaObject, ResourceContents, ServerResult,
};
use rmcp::service::{PeerRequestOptions, RequestHandle, RoleClient, ServerSink};

/// Per-call deadline, as rig 0.42's `DEFAULT_MCP_TOOL_TIMEOUT`: a response
/// the transport loses would otherwise hang the turn.
pub const MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a best-effort cancellation may take after a call timed out.
const CANCELLATION_GRACE_PERIOD: Duration = Duration::from_secs(1);

/// The rig tool for one tool of the MCP server behind `sink`.
pub fn mcp_dynamic_tool(definition: rmcp::model::Tool, sink: ServerSink) -> DynamicTool {
    mcp_dynamic_tool_with_timeout(definition, sink, MCP_TOOL_TIMEOUT)
}

fn mcp_dynamic_tool_with_timeout(
    definition: rmcp::model::Tool,
    sink: ServerSink,
    timeout: Duration,
) -> DynamicTool {
    let name = definition.name.to_string();
    let description = definition.description.as_deref().unwrap_or("").to_string();
    let parameters = definition.schema_as_json_value();
    let tool_name = name.clone();
    // An MCP tool always has a name; rig 0.46 refuses an empty one.
    let name = rig_core::message::ToolName::new(name).unwrap_or_else(|_| {
        rig_core::message::ToolName::new("unnamed_mcp_tool").expect("a non-empty name")
    });
    DynamicTool::new_with_context(
        name,
        description,
        parameters,
        move |context: &mut ToolContext, args| {
            let name = tool_name.clone();
            let sink = sink.clone();
            Box::pin(async move { execute(&name, &sink, timeout, context, args).await })
        },
    )
}

/// The `_meta` an MCP call carries, when the host put one in the tool
/// context.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct McpRequestMeta(pub RequestMetaObject);

impl ContextValue for McpRequestMeta {
    const KEY: &'static str = "mcp.request_meta";
}

/// An MCP call's `structuredContent`, host-only in the tool context.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct McpStructuredContent(pub serde_json::Value);

impl ContextValue for McpStructuredContent {
    const KEY: &'static str = "mcp.structured_content";
}

/// An MCP call's untouched response, host-only in the tool context.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct McpCallToolResult(pub CallToolResult);

impl ContextValue for McpCallToolResult {
    const KEY: &'static str = "mcp.call_tool_result";
}

async fn execute(
    name: &str,
    sink: &ServerSink,
    timeout: Duration,
    context: &mut ToolContext,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments = mcp_arguments(args).map_err(|kind| {
        ToolExecutionError::invalid_args(format!(
            "MCP tool '{name}' received invalid arguments: \
             expected a JSON object or null, got {kind}"
        ))
    })?;
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = arguments;
    request.meta = context
        .get::<McpRequestMeta>()
        .ok()
        .flatten()
        .map(|meta| meta.0);

    let result = match call_tool(sink, request, timeout).await {
        Ok(result) => result,
        Err(error @ rmcp::ServiceError::Timeout { .. }) => {
            return Err(ToolExecutionError::timeout(format!(
                "MCP tool '{name}' timed out after {timeout:?}"
            ))
            .with_source(error));
        }
        Err(error) => {
            return Err(ToolExecutionError::provider(format!(
                "MCP tool '{name}' request failed: {error}"
            ))
            .with_source(error));
        }
    };

    // Host-only: result hooks can read the untouched response.
    if let Some(structured) = result.structured_content.clone()
        && let Err(error) = context.insert_result(McpStructuredContent(structured))
    {
        tracing::warn!(tool = name, %error, "MCP structured content not kept in the tool context");
    }
    if let Err(error) = context.insert_result(McpCallToolResult(result.clone())) {
        tracing::warn!(tool = name, %error, "MCP response not kept in the tool context");
    }

    let output = mcp_result_output(&result)?;
    if result.is_error == Some(true) {
        Err(
            ToolExecutionError::other(format!("MCP tool '{name}' reported an execution error"))
                .with_model_output(output),
        )
    } else {
        Ok(output)
    }
}

/// `null` is a call without arguments, an object is the arguments; any
/// other JSON is refused rather than sent as a no-argument call. `Err` is
/// the JSON kind that was given.
fn mcp_arguments(value: serde_json::Value) -> Result<Option<JsonObject>, &'static str> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Object(arguments) => Ok(Some(arguments)),
        serde_json::Value::Bool(_) => Err("boolean"),
        serde_json::Value::Number(_) => Err("number"),
        serde_json::Value::String(_) => Err("string"),
        serde_json::Value::Array(_) => Err("array"),
    }
}

/// One `tools/call` within `timeout`. On timeout the server is told to
/// cancel, off the caller's deadline and bounded by
/// [`CANCELLATION_GRACE_PERIOD`].
async fn call_tool(
    sink: &ServerSink,
    params: CallToolRequestParams,
    timeout: Duration,
) -> Result<CallToolResult, rmcp::ServiceError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
    let mut handle = tokio::time::timeout_at(
        deadline,
        sink.send_cancellable_request(request, PeerRequestOptions::no_options()),
    )
    .await
    .map_err(|_| rmcp::ServiceError::Timeout { timeout })??;

    let response = match tokio::time::timeout_at(deadline, &mut handle.rx).await {
        Ok(response) => response.map_err(|_| rmcp::ServiceError::TransportClosed)??,
        Err(_) => {
            cancel_timed_out(handle);
            return Err(rmcp::ServiceError::Timeout { timeout });
        }
    };
    match response {
        ServerResult::CallToolResult(result) => Ok(result),
        _ => Err(rmcp::ServiceError::UnexpectedResponse),
    }
}

fn cancel_timed_out(handle: RequestHandle<RoleClient>) {
    let reason = RequestHandle::<RoleClient>::REQUEST_TIMEOUT_REASON.to_owned();
    let cancel = handle.cancel(Some(reason));
    tokio::spawn(async move {
        let _ = tokio::time::timeout(CANCELLATION_GRACE_PERIOD, cancel).await;
    });
}

fn content_block_as_json(content: &ContentBlock) -> Result<ToolResultContent, ToolExecutionError> {
    serde_json::to_value(content)
        .map(ToolResultContent::json)
        .map_err(|error| {
            ToolExecutionError::provider(format!(
                "failed to preserve an MCP content block as JSON: {error}"
            ))
            .with_source(error)
        })
}

/// Text and images become rig content; every other block (resources,
/// links, audio, future variants) is kept whole as JSON rather than
/// flattened to its body.
fn content_block_to_tool_content(
    content: &ContentBlock,
) -> Result<ToolResultContent, ToolExecutionError> {
    match content {
        ContentBlock::Text(text) => Ok(ToolResultContent::text(text.text.clone())),
        ContentBlock::Image(image) => match ImageMediaType::from_mime_type(&image.mime_type) {
            Some(media_type) => Ok(ToolResultContent::image_base64(
                image.data.clone(),
                Some(media_type),
                None,
            )),
            None => content_block_as_json(content),
        },
        ContentBlock::Resource(resource) => match &resource.resource {
            ResourceContents::BlobResourceContents {
                mime_type, blob, ..
            } => match mime_type
                .as_deref()
                .and_then(ImageMediaType::from_mime_type)
            {
                Some(media_type) => Ok(ToolResultContent::image_base64(
                    blob.clone(),
                    Some(media_type),
                    None,
                )),
                None => content_block_as_json(content),
            },
            _ => content_block_as_json(content),
        },
        _ => content_block_as_json(content),
    }
}

/// The model's view of a result, in the server's block order. A text block
/// that only repeats `structuredContent` (rmcp's compatibility copy) is
/// replaced by the structured value; otherwise the structured value comes
/// first. An empty result is one text block.
fn mcp_result_output(result: &CallToolResult) -> Result<ToolOutput, ToolExecutionError> {
    let structured = result.structured_content.as_ref();
    let canonical_fallback = structured.map(serde_json::Value::to_string);
    let mut replaced_fallback = false;
    let mut mapped = Vec::with_capacity(result.content.len());

    for block in &result.content {
        let is_fallback = !replaced_fallback
            && matches!(
                (block, canonical_fallback.as_deref()),
                (ContentBlock::Text(text), Some(fallback)) if text.text == fallback
            );
        if is_fallback && let Some(structured) = structured {
            mapped.push(ToolResultContent::json(structured.clone()));
            replaced_fallback = true;
        } else {
            mapped.push(content_block_to_tool_content(block)?);
        }
    }

    if let Some(structured) = structured
        && !replaced_fallback
    {
        mapped.insert(0, ToolResultContent::json(structured.clone()));
    }

    if !mapped.is_empty() {
        return ToolOutput::content(mapped);
    }
    if result.is_error == Some(true) {
        Ok(ToolOutput::text("the MCP tool reported an error"))
    } else {
        Ok(ToolOutput::text(""))
    }
}

#[cfg(test)]
#[path = "mcp_tool_adapter_tests.rs"]
mod tests;
