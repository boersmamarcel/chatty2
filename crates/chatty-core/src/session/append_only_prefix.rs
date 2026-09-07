//! Turn N+1's provider request must extend turn N's, byte for byte (AGE-277).
//!
//! Prompt caching is a byte-prefix property: the provider reuses the cached
//! prefix only up to the first byte that differs. Rewriting an earlier message
//! — a moved compaction window, a re-trimmed tool result — invalidates every
//! cache block after it, so the whole conversation is re-billed on every turn
//! (ADR-0010 §2b). ADR-0010 makes that worse, not better: a hosted turn is a
//! fresh process with nothing but the provider cache to fall back on.
//!
//! The bytes compared here are the real ones. The session drives a real
//! `AgentClient` against a fake Ollama daemon on localhost, which records the
//! body of every `POST /api/chat` exactly as rig serialized it. Ollama is the
//! provider under test because its client sends the body unmodified; the
//! OpenRouter path rewrites it in flight to move the prompt-cache breakpoint
//! (AGE-205), which is a deliberate mutation of the last message and would
//! mask the one this test is looking for.
//!
//! Two variants — a small tool result and one over the context shaper's 8 KB
//! per-tool cap. Both hold on `main` today. AGE-277 expected the second to
//! fail; it does not, and the reason is worth knowing before AGE-279 touches
//! the shaper: stage 1's cap only ever sees `ToolResultContent::Text`, and rig
//! records a typed tool's output as `ToolResultContent::Json`, so the cap is
//! inert for every native chatty tool. See
//! [`shaper_trims_text_tool_results_but_not_json_ones`].

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use similar::TextDiff;

use super::*;
use crate::factories::agent_factory::AgentBuildContext;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

// ── Fake Ollama daemon ───────────────────────────────────────────────────────

/// A localhost daemon that records request bodies and replays canned NDJSON.
///
/// Blocking, on its own OS thread: the workspace's tokio does not carry the
/// `net` feature, and a socket server is not what this test is about.
struct FakeOllama {
    port: u16,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeOllama {
    /// Serve `responses` in order, one per `POST /api/chat`.
    fn start(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is available");
        let port = listener.local_addr().expect("bound address").port();
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let sink = bodies.clone();

        std::thread::spawn(move || {
            for (connection, response) in listener.incoming().zip(responses) {
                let Ok(mut connection) = connection else {
                    break;
                };
                match read_request_body(&mut connection) {
                    Some(body) => sink.lock().push(body),
                    None => break,
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                );
                let _ = connection.write_all(head.as_bytes());
                let _ = connection.write_all(response.as_bytes());
                let _ = connection.flush();
            }
        });

        Self { port, bodies }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The recorded bodies, in the order the daemon received them.
    fn bodies(&self) -> Vec<Vec<u8>> {
        self.bodies.lock().clone()
    }
}

/// Read one HTTP request and return its body, or `None` if the peer hung up.
fn read_request_body(connection: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut headers_end: Option<usize> = None;
    let mut content_length = 0usize;

    loop {
        if headers_end.is_none()
            && let Some(position) = find(&buffer, b"\r\n\r\n")
        {
            content_length = content_length_of(&buffer[..position])
                .expect("rig sends a Content-Length body, not a chunked one");
            headers_end = Some(position + 4);
        }
        if let Some(start) = headers_end
            && buffer.len() >= start + content_length
        {
            return Some(buffer[start..start + content_length].to_vec());
        }
        match connection.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn content_length_of(headers: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        })
        .and_then(|(_, value)| value.trim().parse().ok())
}

/// One `/api/chat` NDJSON record whose assistant message calls `list_directory`.
fn tool_call_response(path: &str) -> String {
    serde_json::json!({
        "model": "llama3.2",
        "created_at": "2026-01-01T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": { "name": "list_directory", "arguments": { "path": path } }
            }]
        },
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": 100,
        "eval_count": 10
    })
    .to_string()
        + "\n"
}

/// One `/api/chat` NDJSON record that ends the provider turn with plain text.
fn text_response(text: &str) -> String {
    serde_json::json!({
        "model": "llama3.2",
        "created_at": "2026-01-01T00:00:00Z",
        "message": { "role": "assistant", "content": text },
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": 120,
        "eval_count": 12
    })
    .to_string()
        + "\n"
}

// ── Workspace fixture ────────────────────────────────────────────────────────

/// A workspace whose `payload` directory holds `files` empty files with long
/// names, so `list_directory` returns a result of a predictable size.
///
/// 150 entries put the serialized result well over the shaper's 8 KB cap;
/// 3 keep it well under.
fn workspace_with_payload(files: usize) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("a temp workspace");
    let payload = workspace.path().join("payload");
    std::fs::create_dir_all(&payload).expect("payload directory");
    for index in 0..files {
        // A long, stable name: the result's size must not depend on the
        // temp directory's randomly generated path.
        let name = format!("entry-{index:04}-{}.txt", "p".repeat(48));
        std::fs::write(payload.join(name), b"").expect("payload file");
    }
    workspace
}

// ── Session fixture ──────────────────────────────────────────────────────────

fn execution_settings(workspace: &Path) -> ExecutionSettingsModel {
    ExecutionSettingsModel {
        workspace_dir: Some(workspace.to_string_lossy().into_owned()),
        // The fake daemon is not the internet; keeping fetch off also keeps
        // the tool block smaller and the bodies easier to read on failure.
        fetch_enabled: false,
        ..ExecutionSettingsModel::default()
    }
}

async fn session_against(daemon: &FakeOllama, workspace: &Path) -> AgentSession {
    let _ = crate::init_repositories();

    let settings = execution_settings(workspace);
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings.clone(),
        surface: StreamSurface::InteractiveTui,
        loop_guard: false,
    });

    let model_config = ModelConfig::new(
        "age-277".to_string(),
        "Prefix Fixture".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    );
    let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());

    session
        .create_conversation(
            "c1".to_string(),
            "New Chat".to_string(),
            &model_config,
            &provider_config,
            AgentBuildContext {
                mcp_tools: None,
                exec_settings: Some(settings),
                pending_approvals: None,
                pending_clarifications: None,
                pending_write_approvals: None,
                pending_artifacts: None,
                shell_session: None,
                user_secrets: Vec::new(),
                theme_colors: None,
                memory_service: None,
                skill_service: None,
                search_settings: None,
                embedding_service: None,
                allow_sub_agent: false,
                module_agents: Vec::new(),
                gateway_port: None,
                remote_agents: Vec::new(),
                available_model_ids: Vec::new(),
                conversation_id: None,
            },
        )
        .await
        .expect("the fixture conversation builds");

    session
}

/// Drive one real turn to completion and commit it, as an owner would.
async fn run_and_commit_turn(session: &mut AgentSession, text: &str) -> Vec<SessionEvent> {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text(text), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the turn starts");
    turn.await;

    let events = Rc::try_unwrap(events).unwrap().into_inner();
    for event in &events {
        session.apply(event);
    }
    session.finish_turn(None, vec![]);
    events
}

fn assert_no_stream_error(events: &[SessionEvent], label: &str) {
    if let Some(SessionEvent::Error(error)) = events
        .iter()
        .find(|event| matches!(event, SessionEvent::Error(_)))
    {
        panic!("{label} failed: {error:?}");
    }
}

// ── The prefix assertion ─────────────────────────────────────────────────────

/// The `messages` array of `body`, one serialized element per message.
///
/// `serde_json` is built with `preserve_order` in this workspace, so
/// re-serializing a parsed element reproduces the body's own key order and
/// compact spacing: this is a byte comparison of each message, not a
/// structural one.
fn message_elements(body: &[u8]) -> Vec<String> {
    let request: serde_json::Value =
        serde_json::from_slice(body).expect("the request body is JSON");
    request["messages"]
        .as_array()
        .expect("the request body carries a messages array")
        .iter()
        .map(|message| serde_json::to_string(message).expect("a message re-serializes"))
        .collect()
}

/// `later` must contain `earlier`'s messages as an exact leading run.
fn assert_append_only(earlier: &[u8], later: &[u8]) {
    let earlier = message_elements(earlier);
    let later = message_elements(later);

    assert!(
        later.len() >= earlier.len(),
        "turn N+1 sent {} messages, fewer than turn N's {}",
        later.len(),
        earlier.len()
    );

    for (index, (before, after)) in earlier.iter().zip(later.iter()).enumerate() {
        if before != after {
            panic!(
                "message {index} was rewritten between turns, so every cache \
                 block after it is invalidated:\n{}",
                message_diff(before, after)
            );
        }
    }
}

/// A unified diff of one rewritten message. Split on `,` as well as newlines so
/// a compact one-line JSON message still diffs readably.
fn message_diff(before: &str, after: &str) -> String {
    let before = before.replace(',', ",\n");
    let after = after.replace(',', ",\n");
    let diff = TextDiff::from_lines(&before, &after);
    let mut unified = diff.unified_diff();
    unified.header("turn N", "turn N+1").to_string()
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Two turns, a tool result comfortably under the shaper's per-tool cap: the
/// second turn's request extends the first's, byte for byte.
#[tokio::test]
async fn append_only_prefix_holds_for_a_small_tool_result() {
    let workspace = workspace_with_payload(3);
    let daemon = FakeOllama::start(vec![
        tool_call_response("payload"),
        text_response("Three files."),
        text_response("Still three."),
    ]);
    let mut session = session_against(&daemon, workspace.path()).await;

    let first = run_and_commit_turn(&mut session, "what is in payload?").await;
    assert_no_stream_error(&first, "turn 1");
    let second = run_and_commit_turn(&mut session, "and now?").await;
    assert_no_stream_error(&second, "turn 2");

    let bodies = daemon.bodies();
    assert_eq!(
        bodies.len(),
        3,
        "expected two provider requests in turn 1 (tool call, then answer) \
         and one in turn 2"
    );

    assert_append_only(&bodies[1], &bodies[2]);
}

/// The same two turns with a tool result over the context shaper's 8 KB
/// per-tool cap.
///
/// AGE-277 expected this to fail on `main`: stage 1 of the shaper
/// (`services/context_shaper.rs`, `stage1_budget_reduction`) runs over the
/// whole history before *every* stream, so turn 2 should replace turn 1's
/// already-sent tool result with a `[tool result truncated — N chars]` stub and
/// break the prefix. It passes instead, and the reason is a second gap rather
/// than the absence of the first: `trim_tool_result_content` matches only
/// `ToolResultContent::Text`, and rig records a typed tool's output as
/// `ToolResultContent::Json`. Every native chatty tool is typed, so the 8 KB cap
/// currently applies to almost nothing — see
/// [`the shaper leaves a JSON tool result alone`](shaper_trims_text_tool_results_but_not_json_ones).
///
/// So this is a live regression test, not an aspiration. When AGE-279 makes the
/// shaper trim JSON results too, it must trim them **once, where the result is
/// recorded** — trimming again per turn breaks this test, and with it every
/// cache block after the first oversized tool call.
#[tokio::test]
async fn append_only_prefix_survives_a_large_tool_result() {
    let workspace = workspace_with_payload(150);
    let daemon = FakeOllama::start(vec![
        tool_call_response("payload"),
        text_response("A lot of files."),
        text_response("Still a lot."),
    ]);
    let mut session = session_against(&daemon, workspace.path()).await;

    let first = run_and_commit_turn(&mut session, "what is in payload?").await;
    assert_no_stream_error(&first, "turn 1");
    let second = run_and_commit_turn(&mut session, "and now?").await;
    assert_no_stream_error(&second, "turn 2");

    let bodies = daemon.bodies();
    assert_eq!(bodies.len(), 3, "expected three provider requests");

    // Guard the premise: without a tool result over the cap this test would
    // pass for the wrong reason.
    let oversized = message_elements(&bodies[1])
        .iter()
        .any(|message| message.len() > 8_192);
    assert!(
        oversized,
        "the fixture's tool result did not exceed the shaper's 8 KB cap"
    );

    assert_append_only(&bodies[1], &bodies[2]);
}

/// Why the test above passes today, in the shaper's own terms.
///
/// Stage 1 caps a tool result at 8 KB, but only when the result arrived as
/// `ToolResultContent::Text`. rig records a typed tool's output as
/// `ToolResultContent::Json`, and every native chatty tool is typed, so the cap
/// is inert for them and only bites MCP results, which are text.
///
/// Recorded here rather than fixed: the shaper is AGE-279's to change, and the
/// fix has to be "trim once, at the point the result is recorded", or the
/// append-only property above goes with it.
#[tokio::test]
async fn shaper_trims_text_tool_results_but_not_json_ones() {
    use rig_core::completion::message::{Text, ToolCallId, ToolResult, ToolResultContent};

    let payload = "x".repeat(20_000);
    let tool_result = |id: &str, content: ToolResultContent| Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            call: ToolCallId::new(id).unwrap(),
            provider: None,
            name: "list_directory".to_string(),
            content: vec![content],
        })],
    };

    let history = vec![
        tool_result("text-result", ToolResultContent::Text(Text::new(&payload))),
        tool_result(
            "json-result",
            ToolResultContent::Json {
                value: serde_json::json!({ "content": payload }),
            },
        ),
    ];

    let shaped = shape_context(history, &ContextShaperSettings::default(), None).await;

    let sizes: Vec<usize> = shaped
        .messages
        .iter()
        .map(|message| serde_json::to_string(message).unwrap().len())
        .collect();
    assert!(
        sizes[0] < 1_000,
        "a text tool result over the cap is trimmed, and was {} bytes",
        sizes[0]
    );
    assert!(
        sizes[1] > 20_000,
        "a JSON tool result over the cap is left whole — the cap does not \
         reach typed tool output (AGE-279); it was {} bytes",
        sizes[1]
    );
}
