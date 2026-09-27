//! S3 rows 3.10–3.14: what the gateway enforces across protocols.

use std::time::{Duration, Instant};

use chatty_wasm_runtime::test_support::FakeResponse;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::harness::{Gateway, Module, record};

fn chat(content: &str) -> Value {
    json!({ "model": "m", "messages": [{ "role": "user", "content": content }] })
}

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

/// 3.10 — a protocol a module's `[protocols]` turns off is not served for
/// it: `mcp = false` makes `/mcp/{m}` (and its `/sse`) a 404, and likewise
/// for `openai_compat` and `a2a`. The protocols left on still answer.
#[tokio::test(flavor = "multi_thread")]
async fn s3_10_disabled_protocol_is_404() {
    let gw = Gateway::start(
        vec![
            Module::fixture("echo-agent")
                .named("no-mcp")
                .protocols(true, false, true),
            Module::fixture("echo-agent")
                .named("no-openai")
                .protocols(false, true, true),
            Module::fixture("echo-agent")
                .named("no-a2a")
                .protocols(true, true, false),
        ],
        vec![],
    )
    .await;

    let (status, body) = gw.post("/mcp/no-mcp", &mcp_list()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "/mcp/no-mcp: {body}");
    let sse = gw.http.get(gw.url("/mcp/no-mcp/sse")).send().await.unwrap();
    assert_eq!(sse.status(), StatusCode::NOT_FOUND, "/mcp/no-mcp/sse");
    let (status, body) = gw.post("/v1/no-mcp/chat/completions", &chat("hi")).await;
    assert_eq!(status, StatusCode::OK, "openai stays on: {body}");

    let (status, body) = gw.post("/v1/no-openai/chat/completions", &chat("hi")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "/v1/no-openai: {body}");
    let routed =
        json!({ "model": "module:no-openai", "messages": [{ "role": "user", "content": "hi" }] });
    let (status, body) = gw.post("/v1/chat/completions", &routed).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "model-routed no-openai: {body}"
    );
    let (status, body) = gw.post("/mcp/no-openai", &mcp_list()).await;
    assert_eq!(status, StatusCode::OK, "mcp stays on: {body}");

    let (status, body) = gw.post("/a2a/no-a2a", &a2a_send("hi")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "/a2a/no-a2a: {body}");
    let card = gw
        .http
        .get(gw.url("/a2a/no-a2a/.well-known/agent.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(card.status(), StatusCode::NOT_FOUND, "no-a2a agent card");
}

/// 3.11 — twenty concurrent requests to module B while module A is inside a
/// 2 s `slow-host` call: B is not held up by A. Records B's p50/p95; B's
/// p95 must stay under 200 ms. Before PL-H4 one registry write lock
/// serialized every call to every module (F10), so B waited out A; each
/// module now has its own lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_11_slow_module_does_not_block_another() {
    const A_DELAY: Duration = Duration::from_secs(2);
    let gw = Gateway::start(
        vec![Module::fixture("slow-host"), Module::shipped("echo-agent")],
        vec![FakeResponse::Delay(A_DELAY, "a done".into())],
    )
    .await;

    let slow = {
        let (http, url) = (gw.http.clone(), gw.url("/v1/slow-host/chat/completions"));
        tokio::spawn(async move { http.post(url).json(&chat("go")).send().await })
    };
    // Let A get into its guest call before B's burst starts.
    for _ in 0..100 {
        if !gw.llm.calls().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(gw.llm.calls().len(), 1, "module A is inside its slow call");

    let burst = (0..20).map(|i| {
        let (http, url) = (gw.http.clone(), gw.url("/v1/echo-agent/chat/completions"));
        async move {
            let started = Instant::now();
            let resp = http.post(url).json(&chat(&format!("b{i}"))).send().await;
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
        &format!("module B under a {A_DELAY:?} module-A call: p50 {p50:?}, p95 {p95:?}"),
    );

    let a = slow.await.unwrap().expect("module A answers");
    assert_eq!(a.status(), StatusCode::OK);
    assert!(
        p95 < Duration::from_millis(200),
        "module B's p95 was {p95:?} (p50 {p50:?}) while module A ran a {A_DELAY:?} call"
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
/// protocol's POST route, and the gateway keeps serving.
#[tokio::test(flavor = "multi_thread")]
async fn s3_12_oversized_request_body_is_413() {
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;
    // Built as bytes around one 100 MiB string: serializing it through
    // serde_json in a debug build costs seconds per route.
    let huge = "x".repeat(100 << 20);
    let wrap = |before: &str, after: &str| [before, huge.as_str(), after].concat().into_bytes();
    let bodies = [
        (
            "/v1/echo-agent/chat/completions",
            wrap(
                r#"{"model":"m","messages":[{"role":"user","content":""#,
                r#""}]}"#,
            ),
        ),
        (
            "/mcp/echo-agent",
            wrap(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"input":""#,
                r#""}}}"#,
            ),
        ),
        (
            "/a2a/echo-agent",
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

    let (status, body) = gw
        .post("/v1/echo-agent/chat/completions", &chat("still here"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "Echo: still here");
}

/// 3.12 — `huge-output` returning 50 MiB is bounded: a clean error status
/// with a small body (PL-D3 caps a guest's output at 1 MB per call; PL-H4
/// maps that to an error), not 50 MiB relayed to the caller.
#[tokio::test(flavor = "multi_thread")]
async fn s3_12_huge_output_is_bounded() {
    let gw = Gateway::start(vec![Module::fixture("huge-output")], vec![]).await;

    for (path, body) in [
        ("/v1/huge-output/chat/completions", chat("50")),
        ("/a2a/huge-output", a2a_send("50")),
    ] {
        let resp = gw.http.post(gw.url(path)).json(&body).send().await.unwrap();
        let status = resp.status();
        let bytes = resp.bytes().await.unwrap();
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).into_owned();
        assert!(
            bytes.len() < 1 << 20,
            "{path}: {} bytes relayed ({status})",
            bytes.len()
        );
        // A JSON-RPC route may carry the error in the body.
        let is_error =
            !status.is_success() || text.contains("\"error\"") || text.contains("failed");
        assert!(
            is_error,
            "{path}: answered {status} without an error: {text}"
        );
    }
}

/// POST a chat to echo-agent with the given `Host` and optional `Origin`.
async fn chat_with_headers(gw: &Gateway, host: &str, origin: Option<&str>) -> StatusCode {
    let mut req = gw
        .http
        .post(gw.url("/v1/echo-agent/chat/completions"))
        .header(reqwest::header::HOST, host)
        .json(&chat("rebind"));
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
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;
    let port = authority(&gw).rsplit(':').next().unwrap().to_string();

    let rebound_host = format!("evil.example:{port}");
    let rebound_origin = format!("http://evil.example:{port}");
    for (host, origin) in [
        (rebound_host.as_str(), None),
        (rebound_host.as_str(), Some(rebound_origin.as_str())),
        (authority(&gw), Some(rebound_origin.as_str())),
        (authority(&gw), Some("null")),
    ] {
        let status = chat_with_headers(&gw, host, origin).await;
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
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;
    let port = authority(&gw).rsplit(':').next().unwrap().to_string();

    let localhost = format!("localhost:{port}");
    let origin = format!("http://localhost:{port}");
    for (host, origin) in [
        (authority(&gw), None),
        (localhost.as_str(), None),
        (localhost.as_str(), Some(origin.as_str())),
    ] {
        let status = chat_with_headers(&gw, host, origin).await;
        assert_eq!(status, StatusCode::OK, "Host {host}, Origin {origin:?}");
    }
}

/// 3.14 — a participant registered under a module's name. The documented
/// precedence (`docs/a2a-and-wasm-modules.md`, gateway README): on the A2A
/// routes a live participant is looked up first and shadows the module;
/// participants speak only A2A, so the OpenAI and MCP routes still reach the
/// module; and once the participant disconnects the module answers A2A
/// again.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn s3_14_participant_shadows_module_on_a2a_only() {
    use chatty_core::services::a2a_client::A2aClient;
    use chatty_protocol_gateway::participant::{
        BrokerFrame, ParticipantCard, ParticipantConnection, TaskState,
    };

    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;
    let card = ParticipantCard {
        name: "echo-agent".into(),
        description: "the participant, not the module".into(),
        version: "0.1.0".into(),
        ..Default::default()
    };
    let mut conn = ParticipantConnection::register(&gw.socket, card)
        .await
        .expect("a participant may take a module's name");
    let participant = tokio::spawn(async move {
        let Some(BrokerFrame::Task { task_id, text, .. }) = conn.next_frame().await.unwrap() else {
            panic!("expected a task");
        };
        conn.artifact(&task_id, format!("participant got: {text}"), true)
            .await
            .unwrap();
        conn.finish(&task_id, TaskState::Completed, None, None)
            .await
            .unwrap();
        conn // dropped by the caller, which deregisters it
    });

    let client = A2aClient::new();
    let agent = gw.a2a_agent("echo-agent");
    let card = gw
        .http
        .get(gw.url("/a2a/echo-agent/.well-known/agent.json"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(card["description"], "the participant, not the module");
    let answer = client.send_message(&agent, "hi").await.unwrap();
    assert_eq!(answer, "participant got: hi", "A2A reaches the participant");

    let (status, body) = gw
        .post("/v1/echo-agent/chat/completions", &chat("hi"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["choices"][0]["message"]["content"], "Echo: hi",
        "OpenAI still reaches the module"
    );
    let (status, body) = gw.post("/mcp/echo-agent", &mcp_list()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["result"]["tools"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["name"] == "echo")),
        "MCP still reaches the module: {body}"
    );

    drop(participant.await.unwrap());
    let mut answer = String::new();
    for _ in 0..100 {
        match client.send_message(&agent, "hi").await {
            Ok(text) if text == "Echo: hi" => {
                answer = text;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    assert_eq!(
        answer, "Echo: hi",
        "the module answers A2A once the participant is gone"
    );
}
