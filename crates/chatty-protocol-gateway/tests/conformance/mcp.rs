//! S3 rows 3.5–3.6: MCP, by hand and through real clients.

use std::time::Duration;

use reqwest::StatusCode;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};

use crate::harness::{Gateway, Module, on_path, run_client, skip};

/// Twelve amounts, enough for benford's distribution tool to answer.
fn amounts() -> Value {
    json!([
        123.4, 187.0, 1450.0, 212.5, 2999.0, 31.0, 44.0, 1200.0, 5.5, 67.0, 1.9, 910.0
    ])
}

async fn rpc(gw: &Gateway, module: &str, id: u64, method: &str, params: Value) -> Value {
    let (status, body) = gw
        .post(
            &format!("/mcp/{module}"),
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{method}: {body}");
    assert!(body.get("error").is_none(), "{method}: {body}");
    body
}

/// The text of a `tools/call` result's first content block.
fn call_text(body: &Value) -> &str {
    body.pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no text content in {body}"))
}

/// 3.5 — `initialize` → `tools/list` → `tools/call echo {"input":"hi"}`
/// answers `hi`, as echo's own schema (`{input: string}`) promises.
///
/// The gateway hands the guest the `arguments` object serialized once, which
/// is what the WIT contract says `args` is ("JSON-encoded arguments") and
/// what `s3_05_mcp_tool_args_receive_the_arguments_json` pins; echo-agent
/// reads `input` out of it. (This row was red because echo-agent returned
/// that JSON verbatim; PL-H4 fixed the module, not the gateway.)
#[tokio::test(flavor = "multi_thread")]
async fn s3_05_mcp_session_echo_returns_input() {
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;

    let init = rpc(
        &gw,
        "echo-agent",
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "conformance", "version": "0" },
        }),
    )
    .await;
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    let (status, _) = gw
        .post(
            "/mcp/echo-agent",
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let list = rpc(&gw, "echo-agent", 2, "tools/list", json!({})).await;
    let echo = list["result"]["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|t| t["name"] == "echo"))
        .unwrap_or_else(|| panic!("no `echo` tool in {list}"));
    assert_eq!(echo["inputSchema"]["required"], json!(["input"]));

    let call = rpc(
        &gw,
        "echo-agent",
        3,
        "tools/call",
        json!({ "name": "echo", "arguments": { "input": "hi" } }),
    )
    .await;
    assert_eq!(call_text(&call), "hi", "{call}");
}

/// 3.5, the argument half that holds: a guest's `invoke_tool` receives the
/// `arguments` object as one JSON document — not wrapped, not re-quoted —
/// and a module that parses it (benford) answers correctly over MCP.
#[tokio::test(flavor = "multi_thread")]
async fn s3_05_mcp_tool_args_receive_the_arguments_json() {
    let gw = Gateway::start(
        vec![
            Module::fixture("tool-args"),
            Module::shipped("benford-agent"),
        ],
        vec![],
    )
    .await;

    let arguments = json!({ "input": "hi", "n": 2, "nested": { "list": [1, "two"] } });
    let call = rpc(
        &gw,
        "tool-args",
        1,
        "tools/call",
        json!({ "name": "echo_args", "arguments": arguments }),
    )
    .await;
    let received: Value = serde_json::from_str(call_text(&call))
        .unwrap_or_else(|e| panic!("the guest's args are not JSON ({e}): {call}"));
    assert_eq!(received, arguments, "the guest gets the arguments object");

    let call = rpc(
        &gw,
        "benford-agent",
        2,
        "tools/call",
        json!({ "name": "compute_benford_distribution", "arguments": { "numbers": amounts() } }),
    )
    .await;
    let result: Value = serde_json::from_str(call_text(&call)).unwrap();
    assert_eq!(result["total_analyzed"], 12, "{result}");
}

/// 3.6 — chatty's own MCP client (rmcp's streamable HTTP transport, as
/// `mcp_service` connects to a module's server) initializes, lists and calls
/// tools on every module.
#[tokio::test(flavor = "multi_thread")]
async fn s3_06_mcp_rmcp_client_streamable_http() {
    let gw = Gateway::start(
        vec![
            Module::shipped("echo-agent"),
            Module::shipped("benford-agent"),
            Module::fixture("tool-args"),
        ],
        vec![],
    )
    .await;

    let cases = [
        ("echo-agent", "reverse", json!({ "input": "abc" })),
        (
            "benford-agent",
            "compute_benford_distribution",
            json!({ "numbers": amounts() }),
        ),
        ("tool-args", "echo_args", json!({ "input": "hi" })),
    ];
    for (module, tool, arguments) in cases {
        let transport = StreamableHttpClientTransport::from_uri(gw.url(&format!("/mcp/{module}")));
        let client = match tokio::time::timeout(Duration::from_secs(10), ().serve(transport)).await
        {
            Ok(Ok(client)) => client,
            Ok(Err(e)) => panic!("{module}: initialize failed: {e}"),
            Err(_) => panic!("{module}: initialize timed out"),
        };

        let tools = client
            .list_all_tools()
            .await
            .unwrap_or_else(|e| panic!("{module}: tools/list failed: {e}"));
        assert!(
            tools.iter().any(|t| t.name == tool),
            "{module}: `{tool}` listed"
        );

        let Value::Object(arguments) = arguments else {
            unreachable!()
        };
        let result = client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
            .await
            .unwrap_or_else(|e| panic!("{module}: tools/call {tool} failed: {e}"));
        assert_ne!(result.is_error, Some(true), "{module}: {result:?}");
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_else(|| panic!("{module}: no text content: {result:?}"));
        assert!(!text.is_empty(), "{module}: empty result");

        client.cancel().await.ok();
    }
}

/// 3.6 — `/mcp/{m}/sse` is a transport, so it stays open after announcing
/// its endpoint (before PL-H4 it sent the `endpoint` event and closed).
#[tokio::test(flavor = "multi_thread")]
async fn s3_06_mcp_sse_stream_stays_open() {
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;

    let mut resp = gw
        .http
        .get(gw.url("/mcp/echo-agent/sse"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut seen = String::new();
    while !seen.contains("\n\n") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("the endpoint event within five seconds")
            .unwrap();
        match chunk {
            Some(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            None => panic!("the stream closed before the endpoint event: {seen:?}"),
        }
    }
    assert!(seen.contains("event: endpoint"), "{seen:?}");

    // Open means: no end of stream for a while (keep-alives are fine).
    match tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match resp.chunk().await {
                Ok(Some(_)) => continue,
                other => return other,
            }
        }
    })
    .await
    {
        Err(_) => {}
        Ok(end) => panic!("/sse ended after its endpoint event: {end:?}"),
    }
}

/// `node --version`, e.g. `v22.23.2`.
#[derive(Debug)]
struct NodeVersion(String);

impl NodeVersion {
    fn major_minor(&self) -> Option<(u32, u32)> {
        let mut parts = self.0.trim().trim_start_matches('v').split('.');
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    }
}

fn node_version() -> Option<NodeVersion> {
    let output = std::process::Command::new(on_path("node")?)
        .arg("--version")
        .output()
        .ok()?;
    Some(NodeVersion(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// The MCP Inspector's CLI, pinned: the reference client of the MCP project.
const INSPECTOR: &str = "@modelcontextprotocol/inspector@2.8.0";

/// Run the Inspector CLI against `url` over `transport`, returning stdout.
async fn inspector(
    row: &str,
    npx: &std::path::Path,
    url: &str,
    transport: &str,
    args: &[&str],
) -> String {
    let mut command = tokio::process::Command::new(npx);
    command
        .args(["-y", INSPECTOR, "--cli", url, "--transport", transport])
        .args(["--connect-timeout", "5000"])
        .args(args);
    let output = run_client(row, command, Duration::from_secs(300)).await;
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Both Inspector calls (`tools/list`, `tools/call`) over one transport.
async fn inspector_lists_and_calls(row: &str, transport: &str, path_suffix: &str) {
    let Some(npx) = on_path("npx") else {
        skip(
            row,
            "`npx` is not on PATH, so the MCP Inspector CLI cannot run",
        );
        return;
    };
    // The Inspector declares `node >= 22.19` and fails on older runtimes
    // (Node 18: "CustomEvent is not defined").
    let node = node_version();
    if node
        .as_ref()
        .and_then(|v| v.major_minor())
        .is_none_or(|v| v < (22, 19))
    {
        skip(
            row,
            &format!("the MCP Inspector needs Node >= 22.19; `node --version` gave {node:?}"),
        );
        return;
    }
    let gw = Gateway::start(vec![Module::fixture("tool-args")], vec![]).await;
    let url = gw.url(&format!("/mcp/tool-args{path_suffix}"));

    let listed = inspector(row, &npx, &url, transport, &["--method", "tools/list"]).await;
    assert!(
        listed.contains("echo_args"),
        "{row}: tools/list printed {listed}"
    );

    let called = inspector(
        row,
        &npx,
        &url,
        transport,
        &[
            "--method",
            "tools/call",
            "--tool-name",
            "echo_args",
            "--tool-arg",
            "input=from-inspector",
        ],
    )
    .await;
    assert!(
        called.contains("from-inspector"),
        "{row}: tools/call printed {called}"
    );
}

/// 3.6 — the MCP Inspector CLI over streamable HTTP (`POST /mcp/{m}`).
/// Skips (printing why) when `npx` is absent; `npx` fetches the pinned
/// package on first use.
#[tokio::test(flavor = "multi_thread")]
async fn s3_06_mcp_inspector_cli_streamable_http() {
    inspector_lists_and_calls("3.6 s3_06_mcp_inspector_cli_streamable_http", "http", "").await;
}

/// 3.6 — the MCP Inspector CLI over the SSE transport (`GET /mcp/{m}/sse`).
#[tokio::test(flavor = "multi_thread")]
async fn s3_06_mcp_inspector_cli_sse() {
    inspector_lists_and_calls("3.6 s3_06_mcp_inspector_cli_sse", "sse", "/sse").await;
}
