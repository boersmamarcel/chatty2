//! S3 rows 3.10–3.14: what the gateway enforces across protocols.

use std::time::{Duration, Instant};

use chatty_wasm_runtime::test_support::FakeResponse;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::harness::{Gateway, Module, record};

fn a2a_send(text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": { "message": { "parts": [{ "kind": "text", "text": text }] } }
    })
}

fn mcp_list() -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} })
}

/// A `tools/call` of `tool` with `{"input": input}`.
fn mcp_call(tool: &str, input: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": { "input": input } }
    })
}

/// 3.10 — a plugin whose `[protocols]` turns MCP off is not served: `mcp =
/// false` makes `/mcp/{m}` (and its `/sse`) a 404, while a plugin with it on
/// answers. MCP is the only protocol a plugin is served on (PL-U3): neither
/// has an OpenAI or A2A route.
#[tokio::test(flavor = "multi_thread")]
async fn s3_10_disabled_protocol_is_404() {
    let gw = Gateway::start(
        vec![
            Module::fixture("echo").named("no-mcp").mcp(false),
            Module::fixture("echo").named("with-mcp").mcp(true),
        ],
        vec![],
    )
    .await;

    let (status, body) = gw.post("/mcp/no-mcp", &mcp_list()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "/mcp/no-mcp: {body}");
    let sse = gw.http.get(gw.url("/mcp/no-mcp/sse")).send().await.unwrap();
    assert_eq!(sse.status(), StatusCode::NOT_FOUND, "/mcp/no-mcp/sse");
    let (status, body) = gw.post("/mcp/with-mcp", &mcp_list()).await;
    assert_eq!(status, StatusCode::OK, "mcp on: {body}");

    for name in ["no-mcp", "with-mcp"] {
        let (status, body) = gw.post(&format!("/a2a/{name}"), &a2a_send("hi")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "/a2a/{name}: {body}");
        let card = gw
            .http
            .get(gw.url(&format!("/a2a/{name}/.well-known/agent.json")))
            .send()
            .await
            .unwrap();
        assert_eq!(card.status(), StatusCode::NOT_FOUND, "{name} agent card");
        let chat = json!({ "model": name, "messages": [{ "role": "user", "content": "hi" }] });
        let (status, _) = gw
            .post(&format!("/v1/{name}/chat/completions"), &chat)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "/v1/{name}");
    }
}

/// 3.11 — twenty concurrent tool calls to plugin B while plugin A is inside
/// a 2 s `slow-host` call: B is not held up by A. Records B's p50/p95; B's
/// p95 must stay under 200 ms. Before PL-H4 one registry write lock
/// serialized every call to every module (F10), so B waited out A; each
/// module now has its own lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_11_slow_module_does_not_block_another() {
    const A_DELAY: Duration = Duration::from_secs(2);
    let gw = Gateway::start(
        vec![
            Module::fixture("slow-host").granted("llm"),
            Module::shipped("echo"),
        ],
        vec![FakeResponse::Delay(A_DELAY, "a done".into())],
    )
    .await;

    let slow = {
        let (http, url) = (gw.http.clone(), gw.url("/mcp/slow-host"));
        tokio::spawn(async move { http.post(url).json(&mcp_call("ask", "go")).send().await })
    };
    // Let A get into its guest call before B's burst starts.
    for _ in 0..100 {
        if !gw.llm.calls().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(gw.llm.calls().len(), 1, "plugin A is inside its slow call");

    let burst = (0..20).map(|i| {
        let (http, url) = (gw.http.clone(), gw.url("/mcp/echo"));
        async move {
            let started = Instant::now();
            let resp = http
                .post(url)
                .json(&mcp_call("echo", &format!("b{i}")))
                .send()
                .await;
            let status = resp
                .map(|r| r.status())
                .unwrap_or_else(|e| panic!("B {i}: {e}"));
            assert_eq!(status, StatusCode::OK, "B {i}");
            started.elapsed()
        }
    });
    let mut latencies = futures::future::join_all(burst).await;
    latencies.sort();
    let percentile = |p: usize| latencies[(latencies.len() * p / 100).min(latencies.len() - 1)];
    let (p50, p95) = (percentile(50), percentile(95));
    record(
        "3.11 s3_11_slow_module_does_not_block_another",
        &format!("plugin B under a {A_DELAY:?} plugin-A call: p50 {p50:?}, p95 {p95:?}"),
    );

    let a = slow.await.unwrap().expect("plugin A answers");
    assert_eq!(a.status(), StatusCode::OK);
    assert!(
        p95 < Duration::from_millis(200),
        "plugin B's p95 was {p95:?} (p50 {p50:?}) while plugin A ran a {A_DELAY:?} call"
    );
}

/// POST `body` over a raw socket and return the response's status code.
///
/// Not through reqwest: a server that refuses an oversized body answers
/// before reading it and closes, and an HTTP client still writing the body
/// reports that as a send error instead of the answer. Like curl, this keeps
/// reading whatever the server said.
async fn raw_post_status(gw: &Gateway, path: &str, body: Vec<u8>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::TcpStream::connect(authority(gw)).await.unwrap();
    let (mut read, mut write) = stream.into_split();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        authority(gw),
        body.len()
    );
    let writer = tokio::spawn(async move {
        // The server may stop reading at any point; that is the point.
        let _ = write.write_all(head.as_bytes()).await;
        let _ = write.write_all(&body).await;
    });

    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    let status_line = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => response.extend_from_slice(&buf[..n]),
            }
            if response.windows(2).any(|w| w == b"\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&response)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    })
    .await
    .unwrap_or_else(|_| panic!("{path}: no answer within thirty seconds"));
    writer.abort();

    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("{path}: no HTTP status line in {status_line:?}"))
}

/// 3.12 — a 100 MiB request body is refused with a clean 413 on every
/// POST route (the MCP route a plugin is served on, and the A2A route an
/// agent is), and the gateway keeps serving.
#[tokio::test(flavor = "multi_thread")]
async fn s3_12_oversized_request_body_is_413() {
    let gw = Gateway::start(vec![Module::shipped("echo")], vec![]).await;
    // Built as bytes around one 100 MiB string: serializing it through
    // serde_json in a debug build costs seconds per route.
    let huge = "x".repeat(100 << 20);
    let wrap = |before: &str, after: &str| [before, huge.as_str(), after].concat().into_bytes();
    let bodies = [
        (
            "/mcp/echo",
            wrap(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"input":""#,
                r#""}}}"#,
            ),
        ),
        (
            "/a2a/echo",
            wrap(
                r#"{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"parts":[{"kind":"text","text":""#,
                r#""}]}}}"#,
            ),
        ),
    ];
    for (path, body) in bodies {
        let status = raw_post_status(&gw, path, body).await;
        assert_eq!(status, 413, "{path}: a 100 MiB body");
    }

    let (status, body) = gw.post("/mcp/echo", &mcp_call("echo", "still here")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["content"][0]["text"], "still here");
}

/// 3.12 — `huge-output` returning 50 MiB is bounded: a clean error status
/// with a small body (PL-D3 caps a guest's output at 1 MB per call; PL-H4
/// maps that to an error), not 50 MiB relayed to the caller.
#[tokio::test(flavor = "multi_thread")]
async fn s3_12_huge_output_is_bounded() {
    let gw = Gateway::start(vec![Module::fixture("huge-output")], vec![]).await;

    let path = "/mcp/huge-output";
    let resp = gw
        .http
        .post(gw.url(path))
        .json(&mcp_call("emit", "50"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.bytes().await.unwrap();
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).into_owned();
    assert!(
        bytes.len() < 1 << 20,
        "{path}: {} bytes relayed ({status})",
        bytes.len()
    );
    // A JSON-RPC route may carry the error in the body.
    let is_error = !status.is_success() || text.contains("\"error\"");
    assert!(
        is_error,
        "{path}: answered {status} without an error: {text}"
    );
}

/// POST an MCP `tools/list` to echo with the given `Host` and optional
/// `Origin`.
async fn list_with_headers(gw: &Gateway, host: &str, origin: Option<&str>) -> StatusCode {
    let mut req = gw
        .http
        .post(gw.url("/mcp/echo"))
        .header(reqwest::header::HOST, host)
        .json(&mcp_list());
    if let Some(origin) = origin {
        req = req.header(reqwest::header::ORIGIN, origin);
    }
    req.send().await.unwrap().status()
}

/// The `host:port` part of the gateway's base URL.
fn authority(gw: &Gateway) -> &str {
    gw.base.trim_start_matches("http://")
}

/// 3.13 — DNS rebinding. Decision: **reject** a request whose `Host` is not
/// a loopback name, or whose `Origin` is present and not loopback.
///
/// "Accepted risk because the socket is loopback-only" does not hold: DNS
/// rebinding is exactly the attack that reaches a loopback socket, through
/// the user's browser (a page on `evil.example` re-resolves its own name to
/// 127.0.0.1 and is then same-origin with the gateway). What it reaches here
/// is every module, whose `llm::complete` spends the user's provider key,
/// and every published virtual agent, which starts a local chatty worker.
/// The MCP transport spec requires servers to validate `Origin` for this
/// reason. PL-D3 (AGE-595) set the plugin trust boundary — local directory
/// trusted, downloads signed, capabilities denied by default — and that
/// boundary assumes only local processes can call the gateway; this check is
/// what makes that assumption true.
#[tokio::test(flavor = "multi_thread")]
async fn s3_13_non_loopback_host_or_origin_is_rejected() {
    let gw = Gateway::start(vec![Module::shipped("echo")], vec![]).await;
    let port = authority(&gw).rsplit(':').next().unwrap().to_string();

    let rebound_host = format!("evil.example:{port}");
    let rebound_origin = format!("http://evil.example:{port}");
    for (host, origin) in [
        (rebound_host.as_str(), None),
        (rebound_host.as_str(), Some(rebound_origin.as_str())),
        (authority(&gw), Some(rebound_origin.as_str())),
        (authority(&gw), Some("null")),
    ] {
        let status = list_with_headers(&gw, host, origin).await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::MISDIRECTED_REQUEST,
            "Host {host}, Origin {origin:?}: answered {status}"
        );
    }
}

/// 3.13 — the loopback names a local client really sends keep working, with
/// or without a loopback `Origin`.
#[tokio::test(flavor = "multi_thread")]
async fn s3_13_loopback_host_is_served() {
    let gw = Gateway::start(vec![Module::shipped("echo")], vec![]).await;
    let port = authority(&gw).rsplit(':').next().unwrap().to_string();

    let localhost = format!("localhost:{port}");
    let origin = format!("http://localhost:{port}");
    for (host, origin) in [
        (authority(&gw), None),
        (localhost.as_str(), None),
        (localhost.as_str(), Some(origin.as_str())),
    ] {
        let status = list_with_headers(&gw, host, origin).await;
        assert_eq!(status, StatusCode::OK, "Host {host}, Origin {origin:?}");
    }
}

/// 3.14 — a participant claiming a plugin's name. Since ADR-0020 the name
/// is not the participant's to claim: the broker names it `<spec>-<n>` and
/// ignores the card's, so a participant whose card says `echo` is admitted
/// as `echo-0`, and `echo` stays the plugin, served over MCP only (PL-U3).
///
/// Checked on the registry directly rather than over A2A: since BI-7 a role
/// — `echo-0` included — is reached over its own connection, never over
/// loopback, so an A2A client can no longer be the one proving it is
/// addressable (`participant_socket.rs`'s `card_name_is_ignored` is the
/// fuller version of this same check).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn s3_14_participant_cannot_take_a_modules_name() {
    use chatty_protocol_gateway::participant::{
        ParticipantCard, ParticipantConnection, open_connection,
    };

    let gw = Gateway::start(vec![Module::shipped("echo")], vec![]).await;
    let card = ParticipantCard {
        name: "echo".into(),
        description: "the participant, not the plugin".into(),
        version: "0.1.0".into(),
        ..Default::default()
    };
    let connection = open_connection(&gw.participants, "echo", None).unwrap();
    connection.worker_end.set_nonblocking(true).unwrap();
    let stream = tokio::net::UnixStream::from_std(connection.worker_end).unwrap();
    let conn = ParticipantConnection::hello_over(stream, card)
        .await
        .expect("the broker welcomes the participant");
    assert_eq!(conn.name(), "echo-0", "the card's name is ignored");
    assert!(
        gw.participants.is_registered("echo-0"),
        "the participant is live under the assigned name"
    );
    assert!(
        !gw.participants.is_registered("echo"),
        "`echo` was never taken"
    );

    // While the participant is connected, `echo` is still the plugin: no
    // agent of that name on A2A (never had one), and its tools on MCP.
    // `echo-0` is a role now reached only over its connection: loopback
    // refuses it exactly as it refuses any other role (BI-7).
    let card = gw
        .http
        .get(gw.url("/a2a/echo/.well-known/agent.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        card.status(),
        StatusCode::NOT_FOUND,
        "no agent named `echo`"
    );
    let role_card = gw
        .http
        .get(gw.url("/a2a/echo-0/.well-known/agent.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        role_card.status(),
        StatusCode::FORBIDDEN,
        "a role is reached over its connection, not loopback"
    );
    let (status, body) = gw.post("/mcp/echo", &mcp_list()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["result"]["tools"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["name"] == "echo")),
        "MCP still reaches the plugin: {body}"
    );
    drop(conn);
}
