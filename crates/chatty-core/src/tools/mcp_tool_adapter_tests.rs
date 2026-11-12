//! The adapter against an in-process rmcp 3 server over an in-memory pipe.

use std::sync::Arc;
use std::time::Duration;

use rig_agent::tool::{ToolContext, ToolSet};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleClient, RunningService, ServiceExt};
use rmcp::{RoleServer, ServerHandler};
use serde_json::json;

use super::*;

struct TestServer;

fn object_schema() -> Arc<JsonObject> {
    let schema = json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
    });
    Arc::new(schema.as_object().cloned().unwrap())
}

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(vec![
            Tool::new("echo", "Echo the text back", object_schema()),
            Tool::new("fail", "Always reports an error", object_schema()),
            Tool::new("structured", "Returns structured content", object_schema()),
            Tool::new("hang", "Never answers", object_schema()),
        ])))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let text = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("text"))
            .and_then(|text| text.as_str())
            .unwrap_or("<no arguments>")
            .to_string();
        let result = match request.name.as_ref() {
            "echo" => CallToolResult::success(vec![ContentBlock::text(text)]),
            "fail" => CallToolResult::error(vec![ContentBlock::text("boom")]),
            "structured" => CallToolResult::structured(json!({ "answer": 42 })),
            "hang" => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                CallToolResult::success(vec![])
            }
            other => return Err(ErrorData::invalid_params(format!("no tool {other}"), None)),
        };
        Ok(result.into())
    }
}

async fn connect() -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let server = TestServer.serve(server_io).await.expect("server starts");
        let _ = server.waiting().await;
    });
    ().serve(client_io).await.expect("client connects")
}

async fn tool_set(client: &RunningService<RoleClient, ()>, timeout: Duration) -> ToolSet {
    let tools = client.list_all_tools().await.expect("tools/list");
    let mut set = ToolSet::default();
    for tool in tools {
        set.add_dynamic_tool(mcp_dynamic_tool_with_timeout(
            tool,
            client.peer().clone(),
            timeout,
        ));
    }
    set
}

#[tokio::test]
async fn lists_server_tools_as_rig_definitions() {
    let client = connect().await;
    let set = tool_set(&client, MCP_TOOL_TIMEOUT).await;

    let definitions = set.tool_definitions();
    let names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["echo", "fail", "structured", "hang"]);
    let echo = &definitions[0];
    assert_eq!(echo.description, "Echo the text back");
    assert_eq!(
        echo.parameters,
        serde_json::Value::Object((*object_schema()).clone())
    );
}

#[tokio::test]
async fn call_round_trips_arguments_and_text() {
    let client = connect().await;
    let set = tool_set(&client, MCP_TOOL_TIMEOUT).await;

    let result = set
        .execute("echo", r#"{"text":"hello"}"#, &mut ToolContext::new())
        .await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(result.output().as_text(), Some("hello"));

    // `null` is a call without arguments, not a refusal.
    let result = set.execute("echo", "null", &mut ToolContext::new()).await;
    assert_eq!(result.output().as_text(), Some("<no arguments>"));
}

#[tokio::test]
async fn non_object_arguments_are_refused_before_the_call() {
    let client = connect().await;
    let set = tool_set(&client, MCP_TOOL_TIMEOUT).await;

    let result = set.execute("echo", "[1,2]", &mut ToolContext::new()).await;
    let error = result.error().expect("refused");
    assert_eq!(
        error.message(),
        "MCP tool 'echo' received invalid arguments: expected a JSON object or null, got array"
    );
}

#[tokio::test]
async fn is_error_result_is_a_failed_call_carrying_the_tool_output() {
    let client = connect().await;
    let set = tool_set(&client, MCP_TOOL_TIMEOUT).await;

    let result = set.execute("fail", "{}", &mut ToolContext::new()).await;
    let error = result.error().expect("failed call");
    assert_eq!(
        error.message(),
        "MCP tool 'fail' reported an execution error"
    );
    assert_eq!(error.model_output().as_text(), Some("boom"));
}

#[tokio::test]
async fn structured_content_replaces_its_text_copy() {
    let client = connect().await;
    let set = tool_set(&client, MCP_TOOL_TIMEOUT).await;

    let mut context = ToolContext::new();
    let result = set.execute("structured", "{}", &mut context).await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(
        result.output().as_content(),
        [ToolResultContent::json(json!({ "answer": 42 }))]
    );
}

#[tokio::test]
async fn a_call_past_its_deadline_is_a_timeout() {
    let client = connect().await;
    let set = tool_set(&client, Duration::from_millis(200)).await;

    let result = set.execute("hang", "{}", &mut ToolContext::new()).await;
    let error = result.error().expect("timed out");
    assert_eq!(error.message(), "MCP tool 'hang' timed out after 200ms");
}

#[tokio::test]
async fn a_closed_server_is_a_request_failure() {
    let client = connect().await;
    let set = tool_set(&client, MCP_TOOL_TIMEOUT).await;
    client.cancel().await.expect("client closes");

    let result = set.execute("echo", "{}", &mut ToolContext::new()).await;
    let error = result.error().expect("request failed");
    assert!(
        error
            .message()
            .starts_with("MCP tool 'echo' request failed: "),
        "{}",
        error.message()
    );
}
