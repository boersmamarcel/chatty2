//! HTTP layer that re-sends a model request the connection lost before any
//! response arrived.
//!
//! A local server (vLLM, Ollama) under load drops or refuses connections now
//! and then: reqwest reports `error sending request for url (…)` before a
//! single response byte. rig ends the whole multi-turn run on that error, so
//! every tool round-trip of the run so far went with it and headless had to
//! continue from a history that no longer held them. Nothing was sent back
//! yet, so sending the identical request again is safe, and doing it here —
//! under rig — keeps the run itself alive.
//!
//! Only the send is retried. An error after the response started (a body
//! stream cut mid-answer) has already reached the model loop as partial
//! output and is the session's to recover.
//!
//! # A request over the model's window (AGE-978)
//!
//! The same holds for a request the provider refuses as larger than its
//! context window (vLLM's `maximum context length is 32768 tokens`, and the
//! other providers' wordings, [`says_context_overflow`]): rig would end the
//! run, and a delegated worker's whole invocation with it. The context guard
//! sizes every request to fit, but by an estimate, and on some text the
//! model's own tokenizer counts far more. So the refused request is
//! compacted here — the oldest tool results replaced by a one-line stub
//! until their text is half what it was, the newest cut to its head and
//! tail if it alone is more ([`compact_tool_results`]) — and sent once more.
//! The refusal is recorded on the agent's [`WindowOverflows`], so its guard
//! keeps more headroom on every later request. A second refusal is the
//! caller's, as before.

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;
use serde_json::Value;

use crate::services::context_shaper::{WindowOverflows, head_and_tail, omitted_marker};

/// Re-sends after the first failed send. With the default backoff the
/// retries wait 1 + 2 + 4 + 8 = 15 s in all: long enough for a pooled
/// connection race or a brief overload, short of a server restart, which
/// headless's own (slower) retry of the turn covers.
pub const CONNECT_RETRY_ATTEMPTS: usize = 4;
const DEFAULT_BACKOFF: Duration = Duration::from_secs(1);

/// Wraps an HTTP client so a request whose send failed at the connection
/// level is sent again, up to [`CONNECT_RETRY_ATTEMPTS`] times with
/// doubling backoff.
///
/// `Default` is required by rig's completion-model bounds on the HTTP
/// client type, not used by chatty itself.
#[derive(Clone, Debug)]
pub struct ConnectRetryHttpClient<I = reqwest::Client> {
    inner: I,
    backoff: Duration,
    /// Where a request refused as over the window is counted (AGE-978).
    overflows: WindowOverflows,
}

impl<I> ConnectRetryHttpClient<I> {
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            backoff: DEFAULT_BACKOFF,
            overflows: WindowOverflows::default(),
        }
    }

    /// Count requests refused as over the window on `overflows` — the
    /// agent's context guard's, so it keeps more headroom after one.
    pub fn reporting_overflows(mut self, overflows: WindowOverflows) -> Self {
        self.overflows = overflows;
        self
    }

    /// The first retry's delay (each later one doubles it).
    #[cfg(test)]
    fn with_backoff(inner: I, backoff: Duration) -> Self {
        Self {
            inner,
            backoff,
            overflows: WindowOverflows::default(),
        }
    }
}

impl<I: Default> Default for ConnectRetryHttpClient<I> {
    fn default() -> Self {
        Self::new(I::default())
    }
}

/// Whether `error` is a send that never got a response: a refused, reset or
/// dropped connection. An HTTP status (the server answered) is not one.
fn is_connection_failure(error: &http_client::Error) -> bool {
    let http_client::Error::Instance(inner) = error else {
        return false;
    };
    inner
        .downcast_ref::<reqwest::Error>()
        .is_some_and(|e| e.status().is_none() && (e.is_connect() || e.is_request()))
}

/// Run `send` until it succeeds, fails with anything but a connection
/// failure, or has been retried [`CONNECT_RETRY_ATTEMPTS`] times.
async fn with_connect_retries<R, F, Fut>(backoff: Duration, mut send: F) -> http_client::Result<R>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = http_client::Result<R>>,
{
    let mut delay = backoff;
    let mut retries = 0;
    loop {
        match send().await {
            Err(error) if retries < CONNECT_RETRY_ATTEMPTS && is_connection_failure(&error) => {
                retries += 1;
                tracing::warn!(
                    %error,
                    retry = retries,
                    of = CONNECT_RETRY_ATTEMPTS,
                    delay_ms = delay.as_millis() as u64,
                    "Model request failed before a response; sending it again"
                );
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            result => return result,
        }
    }
}

/// Wordings a provider refuses a request over its context window with,
/// lowercase: vLLM, OpenAI and OpenRouter (`maximum context length`, the
/// `context_length_exceeded` code), Anthropic (`prompt is too long`),
/// llama.cpp (`exceeds the available context size`, `exceed_context_size`).
const CONTEXT_OVERFLOW_PHRASES: &[&str] = &[
    "maximum context length",
    "context_length_exceeded",
    "context length exceeded",
    "prompt is too long",
    "exceeds the available context size",
    "exceed_context_size",
    "exceeds the context window",
];

/// Whether a provider's error body says the request was over its context
/// window.
pub(crate) fn says_context_overflow(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    CONTEXT_OVERFLOW_PHRASES
        .iter()
        .any(|phrase| body.contains(phrase))
}

/// Whether `error` is a provider refusing the request as over its context
/// window: a 400 (or 413) whose body says so.
fn is_context_overflow(error: &http_client::Error) -> bool {
    let (status, body) = match error {
        http_client::Error::InvalidStatusCodeWithMessage(status, body) => (*status, body),
        http_client::Error::InvalidStatusCodeWithDetails { status, body, .. } => (*status, body),
        _ => return false,
    };
    matches!(status.as_u16(), 400 | 413) && says_context_overflow(body)
}

/// Every tool result's payload in a request body, oldest first: an OpenAI
/// or Ollama `role: "tool"` message's `content`, a Responses
/// `function_call_output`'s `output`, an Anthropic-style `tool_result`
/// block's `content`.
fn tool_result_slots(body: &mut Value) -> Vec<&mut Value> {
    let list = if body.get("messages").is_some() {
        body.get_mut("messages")
    } else {
        body.get_mut("input")
    };
    let Some(Value::Array(items)) = list else {
        return Vec::new();
    };
    let mut slots = Vec::new();
    for item in items.iter_mut() {
        let field = if item.get("role").and_then(Value::as_str) == Some("tool") {
            Some("content")
        } else if item.get("type").and_then(Value::as_str) == Some("function_call_output") {
            Some("output")
        } else {
            None
        };
        if let Some(field) = field {
            slots.extend(item.get_mut(field));
        } else if let Some(Value::Array(blocks)) = item.get_mut("content") {
            for block in blocks.iter_mut() {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    slots.extend(block.get_mut("content"));
                }
            }
        }
    }
    slots
}

/// A tool result payload's text: a string, or a list of text parts joined.
fn slot_text(slot: &Value) -> String {
    match slot {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Shrink the tool results of a request body the provider refused as over
/// its window, for the one retry: oldest first, each replaced by a one-line
/// stub, until their text is at most half what it was; when the newest one
/// alone is more than that, it keeps its head and tail with the omitted
/// marker in between. Returns whether anything changed.
pub(crate) fn compact_tool_results(body: &mut Value) -> bool {
    let mut slots = tool_result_slots(body);
    let lens: Vec<usize> = slots.iter().map(|slot| slot_text(slot).len()).collect();
    let total: usize = lens.iter().sum();
    let target = total / 2;
    let mut remaining = total;
    let last = slots.len().saturating_sub(1);
    for (i, slot) in slots.iter_mut().enumerate() {
        if remaining <= target {
            break;
        }
        let replacement = if i == last {
            let text = slot_text(slot);
            let keep = text.len().saturating_sub(remaining - target);
            let (head, tail) = head_and_tail(&text, keep);
            let omitted = text.len() - head.len() - tail.len();
            format!("{head}{}{tail}", omitted_marker(omitted))
        } else {
            format!(
                "[tool result dropped — {} chars; the request did not fit the model's \
                 context window. Call the tool again if you still need it.]",
                lens[i]
            )
        };
        if replacement.len() < lens[i] {
            remaining -= lens[i] - replacement.len();
            **slot = Value::String(replacement);
        }
    }
    remaining < total
}

/// `body` with its tool results compacted for a retry, or `None` when it is
/// not JSON or has nothing to compact.
fn compacted(body: &Bytes) -> Option<Bytes> {
    let mut json: Value = serde_json::from_slice(body).ok()?;
    if !compact_tool_results(&mut json) {
        return None;
    }
    serde_json::to_vec(&json).ok().map(Bytes::from)
}

/// Send `body` with connection retries; when the provider refuses it as
/// over its window, count that on `overflows` and send it once more with
/// its tool results compacted.
async fn send_fitting<R, F, Fut>(
    backoff: Duration,
    overflows: &WindowOverflows,
    body: Bytes,
    mut send: F,
) -> http_client::Result<R>
where
    F: FnMut(Bytes) -> Fut,
    Fut: Future<Output = http_client::Result<R>>,
{
    match with_connect_retries(backoff, || send(body.clone())).await {
        Err(error) if is_context_overflow(&error) => {
            let Some(smaller) = compacted(&body) else {
                return Err(error);
            };
            overflows.record();
            tracing::warn!(
                bytes_before = body.len(),
                bytes_after = smaller.len(),
                "Model request refused as over the context window; sending it once more \
                 with its oldest tool results compacted"
            );
            with_connect_retries(backoff, || send(smaller.clone())).await
        }
        result => result,
    }
}

impl<I> HttpClientExt for ConnectRetryHttpClient<I>
where
    I: HttpClientExt + Clone + Send + Sync + 'static,
{
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let backoff = self.backoff;
        let overflows = self.overflows.clone();
        let (mut parts, body) = req.into_parts();
        // reqwest derives the length from the body, and a compacted body
        // is shorter than the one the header was set for.
        parts.headers.remove(reqwest::header::CONTENT_LENGTH);
        let body: Bytes = body.into();
        async move {
            send_fitting(backoff, &overflows, body, |body| {
                inner.send(Request::from_parts(parts.clone(), body))
            })
            .await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        // A multipart form is consumed by the send; it cannot go twice.
        self.inner.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let inner = self.inner.clone();
        let backoff = self.backoff;
        let overflows = self.overflows.clone();
        let (mut parts, body) = req.into_parts();
        // reqwest derives the length from the body, and a compacted body
        // is shorter than the one the header was set for.
        parts.headers.remove(reqwest::header::CONTENT_LENGTH);
        let body: Bytes = body.into();
        async move {
            send_fitting(backoff, &overflows, body, |body| {
                inner.send_streaming(Request::from_parts(parts.clone(), body))
            })
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A local server that drops the first `drops` connections without a
    /// response (what reqwest reports as "error sending request") and then
    /// answers 200. Returns its URL and the count of connections accepted.
    fn flaky_server(drops: usize) -> (String, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                if n < drops {
                    drop(stream);
                    continue;
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                      Content-Length: 12\r\nConnection: close\r\n\r\ndata: [DONE]",
                );
            }
        });
        (url, accepted)
    }

    fn post(url: &str) -> Request<Bytes> {
        Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "application/json")
            .body(Bytes::from_static(br#"{"messages":[]}"#))
            .unwrap()
    }

    fn client() -> ConnectRetryHttpClient {
        // No pooling: every attempt is a fresh connection the server counts.
        let inner = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        ConnectRetryHttpClient::with_backoff(inner, Duration::from_millis(5))
    }

    /// The failure seen in the offline benchmark: the connection dropped
    /// before a response. The same request goes out again and succeeds.
    #[tokio::test]
    async fn a_dropped_connection_is_sent_again() {
        let (url, accepted) = flaky_server(2);
        let response = client()
            .send_streaming(post(&url))
            .await
            .expect("the third attempt answers");
        assert_eq!(response.status(), 200);
        assert_eq!(accepted.load(Ordering::SeqCst), 3);
    }

    /// A server that never answers: the retries stop after the bound and
    /// the connection error is what the caller sees.
    #[tokio::test]
    async fn retries_are_bounded() {
        let (url, accepted) = flaky_server(usize::MAX);
        let error = client()
            .send_streaming(post(&url))
            .await
            .map(|response| response.status())
            .expect_err("a server that never answers still fails");
        assert!(is_connection_failure(&error), "{error}");
        assert_eq!(accepted.load(Ordering::SeqCst), 1 + CONNECT_RETRY_ATTEMPTS);
    }

    /// Nothing listening at all (connection refused) is retried too, and
    /// still ends in an error.
    #[tokio::test]
    async fn a_refused_connection_is_a_connection_failure() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = client()
            .send(post(&format!(
                "http://127.0.0.1:{port}/v1/chat/completions"
            )))
            .await
            .map(|_: Response<LazyBody<Bytes>>| ())
            .expect_err("nothing listens there");
        assert!(is_connection_failure(&error), "{error}");
    }

    // ── AGE-978: a request over the model's window ──────────────────────

    /// vLLM's refusal, as the AGE-900 acceptance run logged it 33 times.
    const VLLM_OVERFLOW: &str = r#"{"error":{"message":"This model's maximum context length is 32768 tokens. However, you requested 0 output tokens and your prompt contains at least 32769 input tokens, for a total of at least 32769 tokens. Please reduce the length of the input prompt or the number of requested output tokens. (parameter=input_tokens, value=32769)","type":"BadRequestError","param":"input_tokens","code":400}}"#;

    fn status_error(status: u16, body: &str) -> http_client::Error {
        http_client::Error::InvalidStatusCodeWithMessage(
            reqwest::StatusCode::from_u16(status).unwrap(),
            body.to_string(),
        )
    }

    /// Each provider's wording of "over the window" is one; other 400s, and
    /// the wording under another status, are not.
    #[test]
    fn a_context_overflow_is_recognised_in_every_providers_wording() {
        let refusals = [
            VLLM_OVERFLOW,
            // OpenAI and Azure.
            r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130512 tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
            // OpenRouter.
            r#"{"error":{"message":"This endpoint's maximum context length is 32768 tokens. However, you requested about 40211 tokens (40211 of text input). Please reduce the length of either one, or use the \"middle-out\" transform to compress your prompt automatically.","code":400}}"#,
            // Anthropic, directly or forwarded.
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210417 tokens > 200000 maximum"}}"#,
            // llama.cpp's server.
            r#"{"error":{"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":40000,"n_ctx":32768}}"#,
        ];
        for body in refusals {
            assert!(
                is_context_overflow(&status_error(400, body)),
                "not recognised: {body}"
            );
        }
        assert!(is_context_overflow(
            &http_client::Error::InvalidStatusCodeWithDetails {
                status: reqwest::StatusCode::BAD_REQUEST,
                body: VLLM_OVERFLOW.to_string(),
                headers: Box::default(),
            }
        ));
        assert!(!is_context_overflow(&status_error(
            400,
            r#"{"error":{"message":"messages with role 'tool' must be a response to a preceding message with 'tool_calls'"}}"#
        )));
        assert!(!is_context_overflow(&status_error(500, VLLM_OVERFLOW)));
        assert!(!is_context_overflow(&status_error(429, VLLM_OVERFLOW)));
    }

    fn tool_text(n: usize, tag: &str) -> String {
        format!("{tag}-START {} {tag}-END", "x".repeat(n))
    }

    /// Chat completions: the oldest tool results become stubs until the
    /// results are half their size; everything else is left as it was.
    #[test]
    fn compaction_stubs_the_oldest_tool_results_first() {
        let mut body = serde_json::json!({ "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "task" },
            { "role": "assistant", "tool_calls": [] },
            { "role": "tool", "tool_call_id": "a", "content": tool_text(4_000, "A") },
            { "role": "assistant", "tool_calls": [] },
            { "role": "tool", "tool_call_id": "b", "content": [{ "type": "text", "text": tool_text(4_000, "B") }] },
            { "role": "assistant", "tool_calls": [] },
            { "role": "tool", "tool_call_id": "c", "content": tool_text(4_000, "C") },
        ]});
        let before = body.clone();
        assert!(compact_tool_results(&mut body));
        let m = &body["messages"];
        let a = m[3]["content"].as_str().unwrap();
        let b = m[5]["content"].as_str().unwrap();
        assert!(a.starts_with("[tool result dropped"), "{a}");
        assert!(b.starts_with("[tool result dropped"), "{b}");
        assert_eq!(m[7], before["messages"][7], "the newest result is whole");
        for i in [0, 1, 2, 4, 6] {
            assert_eq!(m[i], before["messages"][i]);
        }
        assert_eq!(m[3]["tool_call_id"], "a", "only the payload changes");
    }

    /// When the newest result alone is over half, it keeps its head and
    /// tail with the marker that says how to read the rest.
    #[test]
    fn compaction_cuts_a_newest_result_that_is_most_of_the_request() {
        let mut body = serde_json::json!({ "messages": [
            { "role": "tool", "tool_call_id": "a", "content": tool_text(100, "A") },
            { "role": "tool", "tool_call_id": "b", "content": tool_text(20_000, "B") },
        ]});
        assert!(compact_tool_results(&mut body));
        let b = body["messages"][1]["content"].as_str().unwrap();
        assert!(
            b.starts_with("B-START") && b.ends_with("B-END"),
            "head and tail kept"
        );
        assert!(b.contains("omitted here"), "{}", &b[..200]);
        let total = slot_text(&body["messages"][0]["content"]).len() + b.len();
        assert!(
            total <= (20_000 + 100 + 30) / 2 + 400,
            "about half: {total}"
        );
    }

    /// The Responses API's `function_call_output` and Anthropic-style
    /// `tool_result` blocks are tool results too; a body with none, or not
    /// JSON, has nothing to compact.
    #[test]
    fn compaction_finds_tool_results_on_every_wire() {
        let mut responses = serde_json::json!({ "input": [
            { "type": "function_call_output", "call_id": "a", "output": tool_text(4_000, "A") },
            { "type": "function_call_output", "call_id": "b", "output": tool_text(4_000, "B") },
        ]});
        assert!(compact_tool_results(&mut responses));
        assert!(
            responses["input"][0]["output"]
                .as_str()
                .unwrap()
                .starts_with("[tool result dropped")
        );

        let mut blocks = serde_json::json!({ "messages": [
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "a", "content": tool_text(4_000, "A") },
                { "type": "tool_result", "tool_use_id": "b", "content": tool_text(4_000, "B") },
            ]},
        ]});
        assert!(compact_tool_results(&mut blocks));
        assert!(
            blocks["messages"][0]["content"][0]["content"]
                .as_str()
                .unwrap()
                .starts_with("[tool result dropped")
        );

        let mut plain = serde_json::json!({ "messages": [{ "role": "user", "content": "hi" }] });
        assert!(!compact_tool_results(&mut plain));
        assert!(compacted(&Bytes::from_static(b"not json")).is_none());
    }

    /// A local server that refuses the first `refusals` requests as over
    /// its window, as vLLM does, then answers 200; it records each body.
    fn refusing_server(refusals: usize) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = Vec::new();
                let mut buf = [0u8; 65_536];
                // Read the head, then as much body as it announces.
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).into_owned();
                    if let Some(at) = text.find("\r\n\r\n") {
                        let length = text[..at]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if request.len() >= at + 4 + length || n == 0 {
                            seen.lock().unwrap().push(text[at + 4..].to_string());
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let n = seen.lock().unwrap().len();
                let response = if n <= refusals {
                    format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{VLLM_OVERFLOW}",
                        VLLM_OVERFLOW.len()
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: 12\r\nConnection: close\r\n\r\ndata: [DONE]"
                        .to_string()
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (url, bodies)
    }

    fn with_tool_results(url: &str) -> Request<Bytes> {
        let body = serde_json::json!({ "messages": [
            { "role": "user", "content": "task" },
            { "role": "tool", "tool_call_id": "a", "content": tool_text(4_000, "A") },
            { "role": "tool", "tool_call_id": "b", "content": tool_text(4_000, "B") },
        ]});
        Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "application/json")
            .body(Bytes::from(body.to_string()))
            .unwrap()
    }

    /// AGE-978: a request refused as over the window is sent once more with
    /// its oldest tool result compacted, the refusal is counted, and the
    /// caller sees the answer, not the 400.
    #[tokio::test]
    async fn a_context_overflow_is_compacted_and_sent_once_more() {
        let (url, bodies) = refusing_server(1);
        let overflows = WindowOverflows::default();
        let client = client().reporting_overflows(overflows.clone());
        let response = client
            .send_streaming(with_tool_results(&url))
            .await
            .expect("the compacted retry is answered");
        assert_eq!(response.status(), 200);
        let bodies = bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 2, "one refusal, one retry");
        assert!(bodies[0].contains("A-END"));
        let retry: Value = serde_json::from_str(&bodies[1]).unwrap();
        assert!(
            retry["messages"][1]["content"]
                .as_str()
                .unwrap()
                .starts_with("[tool result dropped"),
            "the oldest result is compacted"
        );
        assert!(bodies[1].contains("B-END"), "the newest is kept");
        assert_eq!(overflows.count(), 1);
    }

    /// One retry only: a compacted request refused again is the caller's
    /// error, as before.
    #[tokio::test]
    async fn a_second_overflow_is_returned() {
        let (url, bodies) = refusing_server(usize::MAX);
        let error = client()
            .send_streaming(with_tool_results(&url))
            .await
            .map(|response| response.status())
            .expect_err("refused twice");
        assert!(is_context_overflow(&error), "{error}");
        assert_eq!(bodies.lock().unwrap().len(), 2);
    }

    /// A status the server sent back is an answer, not a lost connection.
    #[test]
    fn an_http_status_is_not_a_connection_failure() {
        let error = http_client::Error::InvalidStatusCodeWithMessage(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "overloaded".into(),
        );
        assert!(!is_connection_failure(&error));
    }
}
