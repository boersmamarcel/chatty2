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
//! The OpenRouter path is recorded too (AGE-291), against a fake
//! chat-completions daemon, because that is the production path and the
//! moving breakpoint is a rewrite by design: the message that was last in
//! turn N carries `cache_control` there and does not in turn N+1. On those
//! bytes the append-only property fails at exactly that message and holds
//! again once the markers are stripped — see
//! [`openrouter_moving_breakpoint_rewrites_the_previously_last_message`].
//! Whether the provider hashes the marker into the cached prefix is the
//! measurement AGE-291 owes; `factories::agent_factory::cache_breakpoint_probe`
//! is the harness for it.
//!
//! Two variants — a small tool result and one over the context guard's 8 KB
//! per-tool cap. Both hold: the guard (`services::context_shaper`, AGE-504)
//! only rewrites a request that would not fit the model's window, and these
//! fixtures fit, so what rig sends is the recorded history byte for byte.
//! What the guard does to a request that does not fit is
//! [`the_context_guard_shapes_inside_the_tool_loop`]'s subject.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use similar::TextDiff;

use super::*;
use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

// ── Fake provider daemons ────────────────────────────────────────────────────

/// A localhost daemon that records request bodies and replays canned
/// responses, one per connection, in order.
///
/// Blocking, on its own OS thread: the workspace's tokio does not carry the
/// `net` feature, and a socket server is not what this test is about. The
/// same daemon plays Ollama (`/api/chat`, NDJSON) and an OpenAI-compatible
/// endpoint (`/chat/completions`, SSE): rig only reads the body, so the
/// content type is the whole difference.
struct FakeDaemon {
    port: u16,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeDaemon {
    /// Serve `responses` in order, one per request, as `content_type`.
    fn start(content_type: &'static str, responses: Vec<String>) -> Self {
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
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
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

    /// An Ollama daemon: one NDJSON record per `POST /api/chat`.
    fn ollama(responses: Vec<String>) -> Self {
        Self::start("application/x-ndjson", responses)
    }

    /// An OpenAI-compatible daemon, as OpenRouter's client sees it: one SSE
    /// stream per `POST /chat/completions`.
    fn openai_compatible(responses: Vec<String>) -> Self {
        Self::start("text/event-stream", responses)
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

/// The SSE frames rig's OpenAI-compatible decoder reads, one `data:` event
/// per line plus the `[DONE]` sentinel.
fn sse_stream(frames: &[serde_json::Value]) -> String {
    frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .chain(std::iter::once("data: [DONE]\n\n".to_string()))
        .collect()
}

fn sse_usage() -> serde_json::Value {
    serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 10,
        "total_tokens": 110,
        "prompt_tokens_details": { "cached_tokens": 0 }
    })
}

/// One chat-completions stream whose assistant turn calls `list_directory`.
fn sse_tool_call_response(path: &str) -> String {
    let arguments = serde_json::json!({ "path": path }).to_string();
    sse_stream(&[
        serde_json::json!({
            "id": "gen-1", "model": "anthropic/claude-haiku-4.5",
            "choices": [{ "index": 0, "delta": {
                "role": "assistant", "content": "",
                "tool_calls": [{ "index": 0, "id": "call_1", "type": "function",
                    "function": { "name": "list_directory", "arguments": arguments } }]
            }, "finish_reason": null }]
        }),
        serde_json::json!({
            "id": "gen-1", "model": "anthropic/claude-haiku-4.5",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": sse_usage()
        }),
    ])
}

/// One chat-completions stream that ends the provider turn with plain text.
fn sse_text_response(text: &str) -> String {
    sse_stream(&[
        serde_json::json!({
            "id": "gen-2", "model": "anthropic/claude-haiku-4.5",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text },
                "finish_reason": null }]
        }),
        serde_json::json!({
            "id": "gen-2", "model": "anthropic/claude-haiku-4.5",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": sse_usage()
        }),
    ])
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

/// The Ollama fixture: rig sends the body unmodified.
async fn session_against(daemon: &FakeDaemon, workspace: &Path) -> AgentSession {
    let model_config = ModelConfig::new(
        "age-277".to_string(),
        "Prefix Fixture".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    );
    let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    session_with(&model_config, &provider_config, workspace).await
}

/// The OpenRouter fixture: the production path, whose client is built on
/// `PromptCachingHttpClient` and rewrites every chat-completions body.
async fn session_against_openrouter(daemon: &FakeDaemon, workspace: &Path) -> AgentSession {
    let model_config = ModelConfig::new(
        "age-291".to_string(),
        "Breakpoint Fixture".to_string(),
        ProviderType::OpenRouter,
        "anthropic/claude-haiku-4.5".to_string(),
    );
    let provider_config = ProviderConfig::new("OpenRouter".to_string(), ProviderType::OpenRouter)
        .with_api_key("age-291-fixture-key".to_string())
        .with_base_url(daemon.base_url());
    session_with(&model_config, &provider_config, workspace).await
}

async fn session_with(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    workspace: &Path,
) -> AgentSession {
    let _ = crate::init_repositories();

    let settings = execution_settings(workspace);
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings.clone(),
        surface: StreamSurface::InteractiveTui,
        loop_guard: false,
    });

    session
        .create_conversation(
            "c1".to_string(),
            "New Chat".to_string(),
            model_config,
            provider_config,
            AgentBuildContext::from_services(AgentServices {
                exec_settings: Some(settings),
                ..AgentServices::default()
            }),
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

/// The `messages` array of `body`, one element per message, as the bytes rig
/// sent them.
///
/// Each element is sliced out of the body with `RawValue` rather than parsed
/// and re-serialized: this workspace's `serde_json` has no `preserve_order`,
/// so a round trip would sort keys and turn this into a structural comparison.
/// Prefix caching is byte-level, and so is this.
fn message_elements(body: &[u8]) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Body<'a> {
        #[serde(borrow)]
        messages: Vec<&'a serde_json::value::RawValue>,
    }
    let request: Body<'_> = serde_json::from_slice(body).expect("the request body is JSON");
    request
        .messages
        .iter()
        .map(|message| message.get().to_string())
        .collect()
}

/// `later` must contain `earlier`'s messages as an exact leading run.
fn assert_append_only(earlier: &[u8], later: &[u8]) {
    if let Some(Rewrite { index, diff }) = first_rewritten_message(earlier, later) {
        panic!(
            "message {index} was rewritten between turns, so every cache \
             block after it is invalidated:\n{diff}"
        );
    }
}

/// The first message of `earlier` that `later` does not carry byte for byte.
struct Rewrite {
    index: usize,
    diff: String,
}

/// The first message `later` rewrote, or `None` when `later` extends `earlier`.
fn first_rewritten_message(earlier: &[u8], later: &[u8]) -> Option<Rewrite> {
    let earlier = message_elements(earlier);
    let later = message_elements(later);

    assert!(
        later.len() >= earlier.len(),
        "turn N+1 sent {} messages, fewer than turn N's {}",
        later.len(),
        earlier.len()
    );

    earlier
        .iter()
        .zip(later.iter())
        .enumerate()
        .find(|(_, (before, after))| before != after)
        .map(|(index, (before, after))| Rewrite {
            index,
            diff: message_diff(before, after),
        })
}

/// Indexes of the messages carrying a `cache_control` marker on any content
/// block: rig's system breakpoint and this layer's moving one (AGE-205).
fn breakpoint_indexes(body: &[u8]) -> Vec<usize> {
    let request: serde_json::Value = serde_json::from_slice(body).expect("JSON body");
    request["messages"]
        .as_array()
        .expect("a messages array")
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|b| b.get("cache_control").is_some()))
        })
        .map(|(index, _)| index)
        .collect()
}

/// `body` with every `cache_control` marker removed and a one-block text
/// content folded back to the string it was before the marker was added, so
/// two requests can be compared on the content the provider bills for.
fn without_breakpoints(body: &[u8]) -> Vec<u8> {
    let mut request: serde_json::Value = serde_json::from_slice(body).expect("JSON body");
    for message in request["messages"]
        .as_array_mut()
        .expect("a messages array")
    {
        let Some(blocks) = message["content"].as_array_mut() else {
            continue;
        };
        for block in blocks.iter_mut() {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
        if let [block] = blocks.as_slice()
            && block["type"] == "text"
            && let Some(text) = block["text"].as_str()
        {
            message["content"] = serde_json::Value::String(text.to_string());
        }
    }
    serde_json::to_vec(&request).expect("the body re-serializes")
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
    let daemon = FakeDaemon::ollama(vec![
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

/// The same two turns with a tool result over the context guard's 8 KB
/// per-tool cap.
///
/// The guard sees the oversized result on turn 2 (it is history by then) and
/// leaves it alone, because the request fits the model's window: the cap is a
/// pressure measure, not a per-message rule. That is what keeps turn 2's
/// prefix identical to turn 1's. A guard that trimmed every oversized result
/// on sight would break this test, and with it every cache block after the
/// first big tool call — which is why capping under budget stays off the
/// table until AGE-279 trims once, where the result is recorded.
#[tokio::test]
async fn append_only_prefix_survives_a_large_tool_result() {
    let workspace = workspace_with_payload(150);
    let daemon = FakeDaemon::ollama(vec![
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

/// The same two turns on the production path (AGE-291): the bodies are the
/// ones `PromptCachingHttpClient` emits, after the rewrite.
///
/// Each request carries two breakpoints — rig's on the system message and the
/// moving one on the latest user/assistant message. Between turn 1's last
/// request and turn 2's first, the moving one leaves the message it was on, so
/// on the literal bytes the append-only property fails at exactly that message
/// and at no other; with the markers stripped it holds. Whether the provider
/// counts the marker as content is what
/// `factories::agent_factory::cache_breakpoint_probe` measures.
#[tokio::test]
async fn openrouter_moving_breakpoint_rewrites_the_previously_last_message() {
    let workspace = workspace_with_payload(3);
    let daemon = FakeDaemon::openai_compatible(vec![
        sse_tool_call_response("payload"),
        sse_text_response("Three files."),
        sse_text_response("Still three."),
    ]);
    let mut session = session_against_openrouter(&daemon, workspace.path()).await;

    let first = run_and_commit_turn(&mut session, "what is in payload?").await;
    assert_no_stream_error(&first, "turn 1");
    let second = run_and_commit_turn(&mut session, "and now?").await;
    assert_no_stream_error(&second, "turn 2");

    let bodies = daemon.bodies();
    assert_eq!(bodies.len(), 3, "expected three provider requests");

    let turn_1 = breakpoint_indexes(&bodies[1]);
    let turn_2 = breakpoint_indexes(&bodies[2]);
    assert_eq!(turn_1.len(), 2, "turn 1 breakpoints: {turn_1:?}");
    assert_eq!(turn_2.len(), 2, "turn 2 breakpoints: {turn_2:?}");
    assert_eq!(
        (turn_1[0], turn_2[0]),
        (0, 0),
        "rig's system breakpoint stays put"
    );
    let previously_last = turn_1[1];
    assert!(
        turn_2[1] > previously_last,
        "the moving breakpoint advanced from message {previously_last} to {}",
        turn_2[1]
    );

    let rewrite = first_rewritten_message(&bodies[1], &bodies[2])
        .expect("moving the breakpoint rewrites the message it left");
    assert_eq!(
        rewrite.index, previously_last,
        "the first rewritten message is the one the breakpoint left:\n{}",
        rewrite.diff
    );
    eprintln!(
        "AGE-291: on the OpenRouter path message {} is rewritten between turns:\n{}",
        rewrite.index, rewrite.diff
    );

    // The marker is the only difference: on the content the provider bills
    // for, turn 2 still extends turn 1.
    assert_append_only(
        &without_breakpoints(&bodies[1]),
        &without_breakpoints(&bodies[2]),
    );
}

/// A session against the Ollama daemon whose model has `window` tokens and a
/// 64-token reply reserve, for the guard tests below.
async fn session_with_window(daemon: &FakeDaemon, workspace: &Path, window: i32) -> AgentSession {
    let mut model_config = ModelConfig::new(
        "age-504".to_string(),
        "Guard Fixture".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    );
    model_config.max_context_window = Some(window);
    model_config.max_tokens = Some(64);
    let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    session_with(&model_config, &provider_config, workspace).await
}

fn guard_of(session: &AgentSession) -> crate::services::ContextShaper {
    session
        .conversation()
        .expect("the fixture conversation")
        .agent()
        .context_shaper()
        .clone()
}

/// The guard's job (AGE-504), part one: results that fit the recording cap
/// but together outgrow the window get a shorter history on the next model
/// call, inside the tool loop, with the message the model is answering — the
/// newest result — sent exactly as recorded.
///
/// The daemon scripts six identical tool calls. Each 15 KB listing is under
/// the recording cap (the window is chosen so), so every result is recorded
/// whole; by the last request five of them are history and do not fit, so
/// the guard shapes them while the sixth — the prompt — goes out untouched.
#[tokio::test]
async fn the_context_guard_shapes_inside_the_tool_loop() {
    let workspace = workspace_with_payload(150);
    let daemon = FakeDaemon::ollama(vec![
        tool_call_response("payload"),
        tool_call_response("payload"),
        tool_call_response("payload"),
        tool_call_response("payload"),
        tool_call_response("payload"),
        tool_call_response("payload"),
        text_response("A lot of files, six times."),
    ]);
    let mut session = session_with_window(&daemon, workspace.path(), 32_000).await;

    let events = run_and_commit_turn(&mut session, "what is in payload?").await;
    assert_no_stream_error(&events, "the turn");

    let bodies = daemon.bodies();
    assert_eq!(bodies.len(), 7, "six tool calls, then the answer");

    // Guard the premise: the result is over the 8 KB cap the shaping stages
    // use and under the recording cap, so it is recorded whole.
    let raw = message_elements(&bodies[1]);
    let result = raw
        .iter()
        .find(|m| m.contains("\"role\":\"tool\""))
        .expect("request 2 carries the tool result");
    assert!(
        result.len() > 8_192,
        "the fixture's tool result exceeds 8 KB"
    );
    assert!(
        !result.contains("[tool result truncated"),
        "recorded whole: {}…",
        &result[..200]
    );
    let guard = guard_of(&session);
    assert!(
        guard.counter().count(result) <= guard.recording_cap(),
        "fixture: the result must be under the recording cap {}",
        guard.recording_cap()
    );

    let last_request = message_elements(&bodies[6]);
    let shaped_at: Vec<usize> = last_request
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.contains("[tool result truncated")
                || m.contains("[compacted]")
                || m.contains("snipped")
        })
        .map(|(i, _)| i)
        .collect();
    assert!(
        !shaped_at.is_empty(),
        "five results in history do not fit a {}-token budget, so the guard shaped the request; sizes: {:?}",
        guard.history_budget(0),
        last_request.iter().map(|m| m.len()).collect::<Vec<_>>()
    );
    assert_eq!(
        last_request.last().map(String::len),
        Some(result.len()),
        "the newest result is the prompt and goes out as recorded"
    );
}

/// The guard's job, part two: a result larger than the recording cap is cut
/// down once, where it is recorded, so it is the same bytes on every later
/// request — the append-only property holds for it — and the conversation
/// persists the recorded form.
///
/// The fixture window leaves a budget the 12 KB listing exceeds half of, so
/// it is cut at recording; the cut form then fits the budget, so request 3
/// is not shaped and carries it unchanged.
#[tokio::test]
async fn a_result_over_the_recording_cap_is_recorded_truncated_once() {
    let workspace = workspace_with_payload(150);
    let daemon = FakeDaemon::ollama(vec![
        tool_call_response("payload"),
        tool_call_response("payload"),
        text_response("A lot of files, twice."),
    ]);
    let mut session = session_with_window(&daemon, workspace.path(), 16_000).await;

    let events = run_and_commit_turn(&mut session, "what is in payload?").await;
    assert_no_stream_error(&events, "the turn");

    let bodies = daemon.bodies();
    assert_eq!(bodies.len(), 3, "two tool calls, then the answer");
    let header = "[tool result truncated — kept ";
    let guard = guard_of(&session);
    assert!(
        guard.recording_cap() > crate::services::context_shaper::RECORDING_CAP_FLOOR_TOKENS
            && guard.recording_cap() <= guard.history_budget(0),
        "fixture: the cap ({}) sits inside the budget ({})",
        guard.recording_cap(),
        guard.history_budget(0)
    );

    let second = message_elements(&bodies[1]);
    let recorded = second
        .iter()
        .find(|m| m.contains(header))
        .expect("request 2 carries the first result cut to the recording cap");
    assert!(recorded.len() < 8_192, "cut well under the raw 12 KB");
    let third = message_elements(&bodies[2]);
    assert_eq!(
        third.get(second.len() - 1),
        Some(recorded),
        "request 3 carries the same bytes: cut once, at recording"
    );

    let persisted = session.conversation().expect("conversation").messages();
    assert!(
        persisted
            .iter()
            .any(|m| serde_json::to_string(m).unwrap().contains(header)),
        "the conversation persists the recorded form"
    );
}
