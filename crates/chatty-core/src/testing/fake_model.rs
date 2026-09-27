//! A localhost model server that answers from a script and records every
//! request (AGE-632).
//!
//! Two modes share one daemon:
//!
//! - **Canned** ([`FakeDaemon::ollama`], [`FakeDaemon::openai_compatible`]):
//!   raw response bodies served in arrival order, whatever the request. This
//!   is what `session::append_only_prefix` drives, where the exact bytes of
//!   each reply are the fixture.
//! - **Scripted** ([`FakeDaemon::scripted`]): a [`Script`] maps a routing key
//!   to a queue of [`Reply`]s, so several agents — a leader and its workers,
//!   each a separate process — share one server and each gets its own
//!   answers. A key matches a request whose `model` field equals it, or whose
//!   system prompt contains it; the first matching route wins. A request no
//!   route matches, or whose route has run dry, gets HTTP 500 with a body
//!   naming the key, so a missing script fails the test loudly instead of
//!   hanging it.
//!
//! Replies go out as OpenAI-compatible SSE on `…/chat/completions` and as
//! Ollama NDJSON on `/api/chat`: rig only reads the body, so the content type
//! and the record shape are the whole difference.
//!
//! Every request body is recorded with the time it arrived and the time its
//! response was written, which is what [`FakeDaemon::max_concurrency`] reads:
//! the peak number of requests in flight at once. One daemon is one model
//! endpoint; a test about two endpoints starts two.
//!
//! Blocking, one OS thread per connection: the workspace's tokio does not
//! carry the `net` feature everywhere, and a socket server is not what any
//! test built on this is about. Every response closes its connection, so
//! each request is one connection and arrival order is well defined.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// One step of a scripted route.
///
/// `Text`, `ToolCalls` and `Error` answer a request; `Usage` and `Delay`
/// shape the answer that follows them in the same queue.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// End the provider turn with this assistant text.
    Text(String),
    /// Answer with these tool calls, `(name, arguments)`, in order.
    ToolCalls(Vec<(String, serde_json::Value)>),
    /// The usage the next answer reports, instead of [`DEFAULT_USAGE`].
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
    },
    /// Wait this many milliseconds, then serve the next reply.
    Delay(u64),
    /// Answer with this HTTP status and an error body.
    Error(u16),
}

impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// One tool call.
    pub fn tool_call(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self::ToolCalls(vec![(name.into(), arguments)])
    }
}

/// `(input, output, cache_read)` reported by an answer no `Reply::Usage`
/// preceded.
pub const DEFAULT_USAGE: (u64, u64, u64) = (10, 5, 0);

/// Routing keys, each with its queue of replies.
#[derive(Debug, Clone, Default)]
pub struct Script {
    routes: Vec<(String, VecDeque<Reply>)>,
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer requests `key` matches with `replies`, in order.
    pub fn route(
        mut self,
        key: impl Into<String>,
        replies: impl IntoIterator<Item = Reply>,
    ) -> Self {
        self.routes
            .push((key.into(), replies.into_iter().collect::<VecDeque<_>>()));
        self
    }

    /// The first route whose key matches, by `model` equality or as a
    /// substring of the system prompt.
    fn matching(&self, model: &str, system: &str) -> Option<usize> {
        self.routes
            .iter()
            .position(|(key, _)| model == key || system.contains(key.as_str()))
    }
}

/// One request as the daemon received it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// The request path, e.g. `/v1/chat/completions`.
    pub path: String,
    /// The route that answered it, `None` when none matched (or in canned
    /// mode, which has no routes).
    pub key: Option<String>,
    /// The body exactly as the client sent it.
    pub body: Vec<u8>,
    pub arrived: Instant,
    /// When the response was written; `None` while it is in flight.
    pub finished: Option<Instant>,
}

impl RecordedRequest {
    /// The body as JSON, `Null` when it is not.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

enum Mode {
    Canned {
        content_type: &'static str,
        responses: VecDeque<String>,
    },
    Scripted(Script),
}

#[derive(Default)]
struct Log {
    requests: Vec<RecordedRequest>,
    in_flight: usize,
    peak: usize,
    /// Numbers tool-call ids, so they are stable run to run.
    calls: usize,
}

struct State {
    mode: Mutex<Mode>,
    log: Mutex<Log>,
}

/// The fake model server. It serves until the process exits.
pub struct FakeDaemon {
    port: u16,
    state: Arc<State>,
}

impl FakeDaemon {
    /// An Ollama daemon: `responses` are NDJSON records, one per request.
    pub fn ollama(responses: Vec<String>) -> Self {
        Self::start(Mode::Canned {
            content_type: "application/x-ndjson",
            responses: responses.into(),
        })
    }

    /// An OpenAI-compatible daemon, as OpenRouter's client sees it:
    /// `responses` are SSE streams, one per request.
    pub fn openai_compatible(responses: Vec<String>) -> Self {
        Self::start(Mode::Canned {
            content_type: "text/event-stream",
            responses: responses.into(),
        })
    }

    /// A daemon that answers from `script`.
    pub fn scripted(script: Script) -> Self {
        Self::start(Mode::Scripted(script))
    }

    fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is available");
        let port = listener.local_addr().expect("bound address").port();
        let state = Arc::new(State {
            mode: Mutex::new(mode),
            log: Mutex::default(),
        });

        let serving = state.clone();
        std::thread::spawn(move || {
            for connection in listener.incoming() {
                let Ok(connection) = connection else {
                    break;
                };
                let state = serving.clone();
                std::thread::spawn(move || serve(connection, &state));
            }
        });

        Self { port, state }
    }

    /// `http://127.0.0.1:<port>`, with no path.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The recorded bodies, in arrival order.
    pub fn bodies(&self) -> Vec<Vec<u8>> {
        self.requests().into_iter().map(|r| r.body).collect()
    }

    /// Every request so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.log.lock().requests.clone()
    }

    /// The requests the route `key` answered, in arrival order.
    pub fn requests_for(&self, key: &str) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.key.as_deref() == Some(key))
            .collect()
    }

    /// The most requests that were ever in flight at once.
    pub fn max_concurrency(&self) -> usize {
        self.state.log.lock().peak
    }
}

/// Answer one connection: read the request, record it, reply, close.
fn serve(mut connection: TcpStream, state: &State) {
    let Some(request) = read_request(&mut connection) else {
        return;
    };
    let index = {
        let mut log = state.log.lock();
        log.in_flight += 1;
        log.peak = log.peak.max(log.in_flight);
        log.requests.push(RecordedRequest {
            path: request.path.clone(),
            key: None,
            body: request.body.clone(),
            arrived: Instant::now(),
            finished: None,
        });
        log.requests.len() - 1
    };

    let response = respond(&request, state, index);
    let _ = connection.write_all(&response);
    let _ = connection.flush();

    let mut log = state.log.lock();
    log.in_flight -= 1;
    log.requests[index].finished = Some(Instant::now());
}

/// The whole HTTP response for `request`, the `index`th one received.
fn respond(request: &Request, state: &State, index: usize) -> Vec<u8> {
    let (content_type, replies, key) = {
        let mut mode = state.mode.lock();
        match &mut *mode {
            Mode::Canned {
                content_type,
                responses,
            } => {
                return match responses.pop_front() {
                    Some(body) => http(200, content_type, &body),
                    None => http(500, "text/plain", "fake model: no canned response left"),
                };
            }
            Mode::Scripted(script) => {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null);
                let model = body["model"].as_str().unwrap_or_default().to_string();
                let system = system_prompt(&body);
                let Some(at) = script.matching(&model, &system) else {
                    return http(
                        500,
                        "text/plain",
                        &format!(
                            "fake model: no script route for model '{model}' on {} \
                             (system prompt starts {:?})",
                            request.path,
                            system.chars().take(120).collect::<String>()
                        ),
                    );
                };
                let (key, queue) = &mut script.routes[at];
                // Take everything up to and including the next answer now,
                // so concurrent requests on one route never share a reply.
                let mut taken = Vec::new();
                while let Some(reply) = queue.pop_front() {
                    let answers = matches!(
                        reply,
                        Reply::Text(_) | Reply::ToolCalls(_) | Reply::Error(_)
                    );
                    taken.push(reply);
                    if answers {
                        break;
                    }
                }
                let ollama = request.path.ends_with("/api/chat");
                let content_type = if ollama {
                    "application/x-ndjson"
                } else {
                    "text/event-stream"
                };
                (content_type, taken, key.clone())
            }
        }
    };
    state.log.lock().requests[index].key = Some(key.clone());

    let mut usage = DEFAULT_USAGE;
    for reply in replies {
        match reply {
            Reply::Delay(ms) => std::thread::sleep(Duration::from_millis(ms)),
            Reply::Usage {
                input,
                output,
                cache_read,
            } => usage = (input, output, cache_read),
            Reply::Error(status) => {
                return http(
                    status,
                    "application/json",
                    &serde_json::json!({ "error": { "message": format!("scripted error {status}") } })
                        .to_string(),
                );
            }
            Reply::Text(text) => {
                let body = if content_type == "text/event-stream" {
                    sse_text(&text, usage)
                } else {
                    ndjson(
                        serde_json::json!({ "role": "assistant", "content": text }),
                        usage,
                    )
                };
                return http(200, content_type, &body);
            }
            Reply::ToolCalls(calls) => {
                let first = {
                    let mut log = state.log.lock();
                    let first = log.calls;
                    log.calls += calls.len();
                    first
                };
                let body = if content_type == "text/event-stream" {
                    sse_tool_calls(&calls, first, usage)
                } else {
                    let calls: Vec<_> = calls
                        .iter()
                        .map(|(name, arguments)| {
                            serde_json::json!({ "function": { "name": name, "arguments": arguments } })
                        })
                        .collect();
                    ndjson(
                        serde_json::json!({ "role": "assistant", "content": "", "tool_calls": calls }),
                        usage,
                    )
                };
                return http(200, content_type, &body);
            }
        }
    }
    http(
        500,
        "text/plain",
        &format!("fake model: the script for key '{key}' is exhausted"),
    )
}

/// The system prompt of a chat request, OpenAI or Ollama shaped: the first
/// `system` message, its content a string or a list of text parts.
fn system_prompt(body: &serde_json::Value) -> String {
    let Some(message) = body["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|m| m["role"] == "system"))
    else {
        return String::new();
    };
    match &message["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        _ => String::new(),
    }
}

fn http(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    let reason = if status == 200 { "OK" } else { "Scripted" };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// The SSE frames rig's OpenAI-compatible decoder reads, one `data:` event
/// per frame plus the `[DONE]` sentinel.
pub fn sse_stream(frames: &[serde_json::Value]) -> String {
    frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .chain(std::iter::once("data: [DONE]\n\n".to_string()))
        .collect()
}

/// OpenAI's convention: `prompt_tokens` includes the cached share.
fn sse_usage((input, output, cache_read): (u64, u64, u64)) -> serde_json::Value {
    serde_json::json!({
        "prompt_tokens": input + cache_read,
        "completion_tokens": output,
        "total_tokens": input + cache_read + output,
        "prompt_tokens_details": { "cached_tokens": cache_read }
    })
}

fn sse_text(text: &str, usage: (u64, u64, u64)) -> String {
    sse_stream(&[
        serde_json::json!({
            "id": "gen-fake", "model": "fake",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text },
                "finish_reason": null }]
        }),
        serde_json::json!({
            "id": "gen-fake", "model": "fake",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": sse_usage(usage)
        }),
    ])
}

fn sse_tool_calls(
    calls: &[(String, serde_json::Value)],
    first_id: usize,
    usage: (u64, u64, u64),
) -> String {
    let tool_calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (name, arguments))| {
            serde_json::json!({
                "index": index, "id": format!("call_{}", first_id + index), "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() }
            })
        })
        .collect();
    sse_stream(&[
        serde_json::json!({
            "id": "gen-fake", "model": "fake",
            "choices": [{ "index": 0, "delta": {
                "role": "assistant", "content": "", "tool_calls": tool_calls
            }, "finish_reason": null }]
        }),
        serde_json::json!({
            "id": "gen-fake", "model": "fake",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": sse_usage(usage)
        }),
    ])
}

fn ndjson(message: serde_json::Value, (input, output, _): (u64, u64, u64)) -> String {
    serde_json::json!({
        "model": "fake",
        "created_at": "2026-01-01T00:00:00Z",
        "message": message,
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": input,
        "eval_count": output
    })
    .to_string()
        + "\n"
}

struct Request {
    path: String,
    body: Vec<u8>,
}

/// Read one HTTP request, or `None` if the peer hung up first.
fn read_request(connection: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut head: Option<(usize, usize, String)> = None;

    loop {
        if head.is_none()
            && let Some(position) = find(&buffer, b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&buffer[..position]).into_owned();
            let path = headers
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            head = Some((position + 4, content_length_of(&headers), path));
        }
        if let Some((start, length, path)) = &head
            && buffer.len() >= start + length
        {
            return Some(Request {
                path: path.clone(),
                body: buffer[*start..start + length].to_vec(),
            });
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

/// The `Content-Length`, 0 when absent (a bodyless `GET`).
fn content_length_of(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        })
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(daemon: &FakeDaemon, path: &str, body: serde_json::Value) -> (u16, String) {
        post_to(daemon.port, path, body)
    }

    fn post_to(port: u16, path: &str, body: serde_json::Value) -> (u16, String) {
        let body = body.to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (status, body)
    }

    fn chat(model: &str, system: &str) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "messages": [{ "role": "system", "content": system }, { "role": "user", "content": "hi" }]
        })
    }

    #[test]
    fn fake_model_unmatched_request_fails_loudly() {
        let daemon = FakeDaemon::scripted(Script::new().route("leader", [Reply::text("ok")]));

        let (status, body) = post(
            &daemon,
            "/v1/chat/completions",
            chat("worker-x", "You help."),
        );

        assert_eq!(status, 500);
        assert!(
            body.contains("'worker-x'"),
            "the body names the key: {body}"
        );
        assert_eq!(
            daemon.requests().len(),
            1,
            "an unmatched request is still recorded"
        );
        assert_eq!(daemon.requests()[0].key, None);
    }

    #[test]
    fn an_exhausted_route_fails_loudly_too() {
        let daemon = FakeDaemon::scripted(Script::new().route("m", [Reply::text("once")]));
        assert_eq!(post(&daemon, "/chat/completions", chat("m", "")).0, 200);

        let (status, body) = post(&daemon, "/chat/completions", chat("m", ""));
        assert_eq!(status, 500);
        assert!(body.contains("'m' is exhausted"), "{body}");
    }

    #[test]
    fn routes_by_model_or_system_prompt_substring() {
        let daemon = FakeDaemon::scripted(
            Script::new()
                .route("You are the reviewer", [Reply::text("from system")])
                .route("coder-model", [Reply::text("from model")]),
        );

        let (_, by_model) = post(
            &daemon,
            "/chat/completions",
            chat("coder-model", "You code."),
        );
        let (_, by_system) = post(
            &daemon,
            "/chat/completions",
            chat("any", "Hello. You are the reviewer."),
        );

        assert!(by_model.contains("from model"), "{by_model}");
        assert!(by_system.contains("from system"), "{by_system}");
        assert_eq!(daemon.requests_for("coder-model").len(), 1);
        assert_eq!(daemon.requests_for("You are the reviewer").len(), 1);
    }

    #[test]
    fn replies_are_sse_or_ndjson_by_path_with_scripted_usage_and_errors() {
        let daemon = FakeDaemon::scripted(Script::new().route(
            "m",
            [
                Reply::Usage {
                    input: 7,
                    output: 3,
                    cache_read: 2,
                },
                Reply::tool_call("read_file", serde_json::json!({ "path": "a" })),
                Reply::text("done"),
                Reply::Error(429),
            ],
        ));

        let (_, sse) = post(&daemon, "/v1/chat/completions", chat("m", ""));
        assert!(sse.starts_with("data: "), "{sse}");
        assert!(sse.contains(r#""name":"read_file""#), "{sse}");
        assert!(
            sse.contains(r#""prompt_tokens":9"#),
            "cached counts in the prompt: {sse}"
        );
        assert!(sse.ends_with("data: [DONE]\n\n"));

        let (_, ndjson) = post(&daemon, "/api/chat", chat("m", ""));
        let record: serde_json::Value = serde_json::from_str(ndjson.trim()).unwrap();
        assert_eq!(record["message"]["content"], "done");
        assert_eq!(record["prompt_eval_count"], DEFAULT_USAGE.0);

        assert_eq!(post(&daemon, "/v1/chat/completions", chat("m", "")).0, 429);
    }

    #[test]
    fn max_concurrency_counts_requests_in_flight() {
        let daemon = FakeDaemon::scripted(Script::new().route(
            "m",
            [
                Reply::Delay(300),
                Reply::text("a"),
                Reply::Delay(300),
                Reply::text("b"),
            ],
        ));
        let port = daemon.port;
        let handles: Vec<_> = (0..2)
            .map(|_| {
                std::thread::spawn(move || post_to(port, "/chat/completions", chat("m", "")).0)
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 200);
        }

        assert_eq!(daemon.max_concurrency(), 2);
        assert!(daemon.requests().iter().all(|r| r.finished.is_some()));
    }
}
