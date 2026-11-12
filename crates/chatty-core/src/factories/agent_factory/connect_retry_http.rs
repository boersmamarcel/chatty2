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
//! tail if it alone is more ([`compact_tool_results`]), and the text of the
//! earlier messages halved the same way ([`compact_message_texts`]; the
//! task's first message, the latest user message and the last message are
//! kept) — and sent again. A request refused again is compacted further and
//! resent, up to [`OVERFLOW_RETRIES`] times (AGE-994): the guard had already
//! one-lined the old tool results, so a long conversation's bulk is
//! assistant text, and one halving of its tool results was not enough. Each
//! refusal is recorded on the agent's [`WindowOverflows`], so its guard
//! keeps more headroom on every later request. The last refusal is the
//! caller's.
//!
//! # A request that asks for more output than the window leaves (AGE-985)
//!
//! One more shape of the same refusal: the prompt fits, but the prompt plus
//! the requested `max_tokens` does not (vLLM: `you requested 30135 output
//! tokens and your prompt contains at least 2634 input tokens`, one over a
//! 32,768 window). Compacting tool results cannot help; the request's output
//! is lowered instead ([`lower_output_cap`]) by the overshoot plus a margin
//! for the tokenizer gap ([`OutputMargin`]), and the request is sent once
//! more. OpenAI's `max_tokens is too large … supports at most N` is handled
//! too: the cap goes down to N.
//!
//! vLLM's input figure is only a lower bound (AGE-988): it stops counting the
//! prompt once it is past the room the requested output left, and says `at
//! least 583 input tokens` of a 19k-token prompt, or gives its length in
//! characters. So the lowered cap is also bounded by the request's own count:
//! the window less the counted body and the margin on it. And the cap a
//! refusal names may not be chatty's: hive's egress adds one, sized to the
//! budget left, to a request that carries none (run #4). The retry then
//! carries the lowered cap itself, which an egress that only ever lowers a
//! cap keeps.

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;
use serde_json::Value;

use crate::services::context_shaper::{
    MIN_CLAMPED_REPLY_TOKENS, OutputMargin, WindowOverflows, head_and_tail, omitted_marker,
};
use crate::token_budget::counter::TokenCounter;

/// Re-sends after the first failed send. With the default backoff the
/// retries wait 1 + 2 + 4 + 8 = 15 s in all: long enough for a pooled
/// connection race or a brief overload, short of a server restart, which
/// headless's own (slower) retry of the turn covers.
pub const CONNECT_RETRY_ATTEMPTS: usize = 4;

/// Further sends a request refused as over the model's window gets, each
/// smaller than the last (AGE-994).
pub const OVERFLOW_RETRIES: usize = 3;
const DEFAULT_BACKOFF: Duration = Duration::from_secs(1);

/// Wraps an HTTP client so a request whose send failed at the connection
/// level is sent again, up to [`CONNECT_RETRY_ATTEMPTS`] times with
/// doubling backoff.
///
/// `Default` is required by rig's completion-model bounds on the HTTP
/// client type, not used by chatty itself.
#[derive(Clone, Debug)]
pub struct ConnectRetryHttpClient<I = rig_reqwest::ReqwestClient> {
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
    "max_tokens is too large",
    "max_completion_tokens is too large",
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

/// A message's text slots: its `content` when that is a string, else each
/// text part's `text` (`text`, Responses `input_text`/`output_text`) —
/// never a tool result block.
fn text_slots(item: &mut Value) -> Vec<&mut Value> {
    match item.get_mut("content") {
        Some(content @ Value::String(_)) => vec![content],
        Some(Value::Array(parts)) => parts
            .iter_mut()
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "input_text" | "output_text")
                )
            })
            .filter_map(|part| part.get_mut("text"))
            .filter(|text| text.is_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a request item is a user message with text of its own (not only
/// tool results).
fn is_user_text(item: &Value) -> bool {
    if item.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    match item.get("content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("text" | "input_text")
            )
        }),
        _ => false,
    }
}

/// Shrink the text of a refused request's earlier messages, for a retry
/// (AGE-994): every user or assistant message but the first user message
/// (the task), the latest user message and the last message, oldest first,
/// each replaced by a one-line stub until their text is at most half what it
/// was. Tool calls and results are left to [`compact_tool_results`], so no
/// round-trip loses a half. Returns whether anything changed.
pub(crate) fn compact_message_texts(body: &mut Value) -> bool {
    let list = if body.get("messages").is_some() {
        body.get_mut("messages")
    } else {
        body.get_mut("input")
    };
    let Some(Value::Array(items)) = list else {
        return false;
    };
    let first_user = items.iter().position(is_user_text);
    let latest_user = items.iter().rposition(is_user_text);
    let last = items.len().saturating_sub(1);
    let mut slots: Vec<&mut Value> = items
        .iter_mut()
        .enumerate()
        .filter(|(i, item)| {
            Some(*i) != first_user
                && Some(*i) != latest_user
                && *i != last
                && matches!(
                    item.get("role").and_then(Value::as_str),
                    Some("user" | "assistant")
                )
        })
        .flat_map(|(_, item)| text_slots(item))
        .collect();
    let lens: Vec<usize> = slots
        .iter()
        .map(|slot| slot.as_str().map_or(0, str::len))
        .collect();
    let total: usize = lens.iter().sum();
    let target = total / 2;
    let mut remaining = total;
    for (slot, len) in slots.iter_mut().zip(lens) {
        if remaining <= target {
            break;
        }
        let stub = format!(
            "[earlier message text dropped — {len} chars; the request did not fit the \
             model's context window]"
        );
        if stub.len() < len {
            remaining -= len - stub.len();
            **slot = Value::String(stub);
        }
    }
    remaining < total
}

/// `body` compacted for a retry — its tool results and its earlier
/// messages' text — or `None` when it is not JSON or has nothing left to
/// compact.
fn compacted(body: &Bytes) -> Option<Bytes> {
    let mut json: Value = serde_json::from_slice(body).ok()?;
    let results = compact_tool_results(&mut json);
    let texts = compact_message_texts(&mut json);
    if !(results || texts) {
        return None;
    }
    serde_json::to_vec(&json).ok().map(Bytes::from)
}

/// The number written right before `marker` in `text` (`"30135 output
/// tokens"` with `" output tokens"`), searching from byte `from`.
fn number_before(text: &str, marker: &str, from: usize) -> Option<usize> {
    let end = from + text.get(from..)?.find(marker)?;
    let digits: String = text[..end]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// The number written right after `marker` in `text`.
fn number_after(text: &str, marker: &str) -> Option<usize> {
    let start = text.find(marker)? + marker.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    digits.parse().ok()
}

/// Phrasings that say the requested `max_tokens` itself was refused.
const OUTPUT_CAP_PHRASES: &[&str] = &[
    "max_tokens is too large",
    "max_completion_tokens is too large",
    "'max_tokens' or 'max_completion_tokens' is too large",
];

/// What a refusal says about the request's output cap.
#[derive(Debug, PartialEq)]
enum OutputCapRefusal {
    /// Not about the output (or the prompt alone is over the window, or
    /// there is no room worth asking for): compacting is the remedy.
    NotOutput,
    /// About the output cap, but too little is said to pick a lower one:
    /// neither a retry nor compaction would change the outcome.
    Unfixable,
    /// Retry with the cap lowered to this.
    Lower(usize),
}

/// Read a refusal message (any case) for what it asks of the output cap of
/// a request counted at `input_tokens` on our side. Wordings handled:
///
/// - vLLM: `maximum context length is W tokens. However, you requested N
///   output tokens and your prompt contains at least M input tokens`, the
///   older `you requested T tokens (M in the messages, N in the
///   completion)`, and `'max_tokens' or 'max_completion_tokens' is too
///   large: N. … your request has M input tokens`: N less the overshoot
///   `N + M - W`, less the margin on M, but never under
///   [`MIN_CLAMPED_REPLY_TOKENS`] (a reply that short is the input's
///   problem: compact it); and never over the window less `input_tokens`
///   and the margin on it, since vLLM's M is a lower bound (AGE-988). Any
///   input wording does, or none: `your prompt contains C characters (more
///   than …, which is the upper bound for M input tokens)` is one;
/// - OpenRouter: `you requested about T tokens (M of text input, N in the
///   output)`: the same;
/// - OpenAI: `max_tokens is too large: N. This model supports at most L
///   completion tokens`: L.
fn output_cap_refusal(
    message: &str,
    input_tokens: usize,
    margin: OutputMargin,
) -> OutputCapRefusal {
    let text = message.to_ascii_lowercase();
    let too_large = OUTPUT_CAP_PHRASES.iter().any(|p| text.contains(p));
    if let Some(limit) = number_after(&text, "supports at most ")
        && (text.contains("max_tokens") || text.contains("completion tokens"))
    {
        return if limit > 0 {
            OutputCapRefusal::Lower(limit)
        } else {
            OutputCapRefusal::Unfixable
        };
    }
    let output = number_before(&text, " output tokens", 0)
        .or_else(|| number_before(&text, " in the output", 0))
        .or_else(|| number_before(&text, " in the completion", 0))
        .or_else(|| number_after(&text, "is too large: "));
    let input = number_before(&text, " input tokens", 0)
        .or_else(|| number_before(&text, " of text input", 0))
        .or_else(|| number_before(&text, " in the messages", 0));
    let window = number_after(&text, "maximum context length is ");
    let (Some(output), Some(window)) = (output, window) else {
        return if too_large {
            OutputCapRefusal::Unfixable
        } else {
            OutputCapRefusal::NotOutput
        };
    };
    // The room the window leaves past an input of `n` tokens and its margin:
    // the provider's figure and our count are both at most the real input.
    let room = |n: usize| window.checked_sub(n + margin.for_input(n));
    let lowered = room(input.unwrap_or(0)).zip(room(input_tokens));
    match lowered.map(|(reported, counted)| reported.min(counted)) {
        Some(lowered) if lowered < output && lowered >= MIN_CLAMPED_REPLY_TOKENS => {
            OutputCapRefusal::Lower(lowered)
        }
        _ => OutputCapRefusal::NotOutput,
    }
}

/// `body` with its output cap (`max_tokens`, `max_completion_tokens`,
/// `max_output_tokens`) lowered to what the provider's refusal `message`
/// asks for — or `max_tokens` added, when the body carries none and the cap
/// refused was added on the way (AGE-988) — or `None` when the refusal is
/// not about the output or the cap is already no larger.
pub(crate) fn lower_output_cap(body: &Bytes, message: &str) -> Option<Bytes> {
    let mut json: Value = serde_json::from_slice(body).ok()?;
    let model = json
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The whole body, JSON and all: more than the prompt the model sees, so
    // the room it leaves errs on the small side.
    let counted = TokenCounter::for_model(model).count(&String::from_utf8_lossy(body));
    let OutputCapRefusal::Lower(lowered) =
        output_cap_refusal(message, counted, OutputMargin::default())
    else {
        return None;
    };
    let object = json.as_object_mut()?;
    let fields = ["max_tokens", "max_completion_tokens", "max_output_tokens"];
    if !fields.iter().any(|field| object.contains_key(*field)) {
        object.insert("max_tokens".into(), Value::from(lowered as u64));
        return serde_json::to_vec(&json).ok().map(Bytes::from);
    }
    let mut changed = false;
    for field in fields {
        if let Some(slot) = object.get_mut(field)
            && slot.as_u64().is_some_and(|n| n as usize > lowered)
        {
            *slot = Value::from(lowered as u64);
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    serde_json::to_vec(&json).ok().map(Bytes::from)
}

/// The refusal message of a context-overflow error.
fn overflow_message(error: &http_client::Error) -> &str {
    match error {
        http_client::Error::InvalidStatusCodeWithDetails { body, .. } => body,
        _ => "",
    }
}

/// Send `body` with connection retries; when the provider refuses it as
/// over its window, count that on `overflows` and send it again: with its
/// output cap lowered when the refusal says the requested output was what
/// did not fit (AGE-985), else compacted — further on every refusal, up to
/// [`OVERFLOW_RETRIES`] times (AGE-994).
async fn send_fitting<R, F, Fut>(
    backoff: Duration,
    overflows: &WindowOverflows,
    mut body: Bytes,
    mut send: F,
) -> http_client::Result<R>
where
    F: FnMut(Bytes) -> Fut,
    Fut: Future<Output = http_client::Result<R>>,
{
    let mut retry = 0;
    loop {
        let error = match with_connect_retries(backoff, || send(body.clone())).await {
            Err(error) if retry < OVERFLOW_RETRIES && is_context_overflow(&error) => error,
            result => return result,
        };
        let message = overflow_message(&error);
        let (smaller, path) = if let Some(lowered) = lower_output_cap(&body, message) {
            (lowered, "with a lower max_tokens")
        } else if output_cap_refusal(message, 0, OutputMargin::default())
            == OutputCapRefusal::Unfixable
        {
            // The refusal is about the output cap and names no number to
            // lower it to: compacting would change nothing and would raise
            // the headroom of every later request.
            return Err(error);
        } else if let Some(compacted) = compacted(&body) {
            // Only a compaction counts as a window overflow: it is the
            // input that was too large, so later requests keep more
            // headroom. A lowered cap says nothing about the input.
            overflows.record();
            (compacted, "compacted")
        } else {
            return Err(error);
        };
        tracing::warn!(
            retry = retry + 1,
            bytes_before = body.len(),
            bytes_after = smaller.len(),
            "Model request refused as over the context window; sending it again {path}"
        );
        body = smaller;
        retry += 1;
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
        ConnectRetryHttpClient::with_backoff(inner.into(), Duration::from_millis(5))
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
        http_client::Error::non_success_with_details(
            reqwest::StatusCode::from_u16(status).unwrap(),
            Default::default(),
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

    /// AGE-994: a compacted request refused again is compacted further and
    /// sent again, each time smaller, until one is answered.
    #[tokio::test]
    async fn a_refused_retry_is_compacted_further() {
        let (url, bodies) = refusing_server(2);
        let overflows = WindowOverflows::default();
        let client = client().reporting_overflows(overflows.clone());
        let response = client
            .send_streaming(with_tool_results(&url))
            .await
            .expect("the third request is answered");
        assert_eq!(response.status(), 200);
        let bodies = bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 3, "two refusals, two retries");
        assert!(bodies[2].len() < bodies[1].len() && bodies[1].len() < bodies[0].len());
        assert_eq!(overflows.count(), 2);
    }

    /// The retries are bounded: the last refusal is the caller's error.
    #[tokio::test]
    async fn overflow_retries_are_bounded() {
        let (url, bodies) = refusing_server(usize::MAX);
        let error = client()
            .send_streaming(with_tool_results(&url))
            .await
            .map(|response| response.status())
            .expect_err("always refused");
        assert!(is_context_overflow(&error), "{error}");
        assert_eq!(bodies.lock().unwrap().len(), 1 + OVERFLOW_RETRIES);
    }

    /// AGE-994: the earlier messages' text is halved too, oldest first; the
    /// system prompt, the task, the latest user message and the last
    /// message are kept, and so is every tool call.
    #[test]
    fn compaction_halves_the_earlier_messages_text() {
        let long = |tag: &str| format!("{tag}-START {} {tag}-END", "y".repeat(4_000));
        let mut body = serde_json::json!({ "messages": [
            { "role": "system", "content": long("SYS") },
            { "role": "user", "content": long("TASK") },
            { "role": "assistant", "content": long("A1"), "tool_calls": [
                { "id": "a", "type": "function", "function": { "name": "f", "arguments": "{}" } }
            ] },
            { "role": "tool", "tool_call_id": "a", "content": "ok" },
            { "role": "assistant", "content": [{ "type": "text", "text": long("A2") }] },
            { "role": "assistant", "content": long("A3") },
            { "role": "user", "content": long("CONTINUE") },
            { "role": "assistant", "content": long("LAST") },
        ]});
        let before = body.clone();
        assert!(compact_message_texts(&mut body));
        let m = &body["messages"];
        assert!(
            m[2]["content"]
                .as_str()
                .unwrap()
                .starts_with("[earlier message text dropped")
        );
        assert_eq!(m[2]["tool_calls"], before["messages"][2]["tool_calls"]);
        assert!(
            m[4]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("[earlier message text dropped")
        );
        assert_eq!(m[5], before["messages"][5], "half is reached by then");
        for i in [0, 1, 3, 6, 7] {
            assert_eq!(m[i], before["messages"][i]);
        }

        let mut short = serde_json::json!({ "messages": [
            { "role": "user", "content": "task" },
            { "role": "assistant", "content": "ok" },
        ]});
        assert!(!compact_message_texts(&mut short), "nothing to take");
    }

    /// A status the server sent back is an answer, not a lost connection.
    #[test]
    fn an_http_status_is_not_a_connection_failure() {
        let error = http_client::Error::non_success_with_details(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            Default::default(),
            "overloaded".into(),
        );
        assert!(!is_connection_failure(&error));
    }

    // ── AGE-985: a request asking for more output than the window leaves ──

    fn body_with_cap(field: &str, cap: u64) -> Bytes {
        Bytes::from(serde_json::json!({ "model": "m", "messages": [], field: cap }).to_string())
    }

    fn cap_of(body: &Bytes, field: &str) -> u64 {
        serde_json::from_slice::<Value>(body).unwrap()[field]
            .as_u64()
            .unwrap()
    }

    /// The incident's vLLM refusal: 2,634 + 30,135 = 32,769, one over. The
    /// cap goes down by that one, plus 10% of the input and the slack.
    #[test]
    fn the_vllm_wording_lowers_the_cap_by_the_overshoot_and_the_margin() {
        let message = "This model's maximum context length is 32768 tokens. However, you \
            requested 30135 output tokens and your prompt contains at least 2634 input \
            tokens, for a total of at least 32769 tokens. Please reduce the length of \
            the input prompt or the number of requested output tokens.";
        assert!(says_context_overflow(message));
        let lowered = lower_output_cap(&body_with_cap("max_tokens", 30_135), message).unwrap();
        let margin = OutputMargin::default().for_input(2_634) as u64;
        assert!(margin >= 263 + 256);
        assert_eq!(cap_of(&lowered, "max_tokens"), 30_135 - 1 - margin);
        // The new request fits by a margin the tokenizer gap cannot eat.
        assert!(2_634 + cap_of(&lowered, "max_tokens") + margin <= 32_768);
    }

    #[test]
    fn other_providers_wordings_are_handled() {
        // OpenRouter: the output share is named in parentheses.
        let openrouter = "This endpoint's maximum context length is 32768 tokens. However, you \
            requested about 35000 tokens (2000 of text input, 33000 in the output). Please \
            reduce the length of either one, or use the \"middle-out\" transform";
        assert!(says_context_overflow(openrouter));
        let lowered = lower_output_cap(&body_with_cap("max_tokens", 33_000), openrouter).unwrap();
        let cap = cap_of(&lowered, "max_tokens");
        assert!(cap <= 33_000 - 2_232 - 200 && cap > 28_000, "{cap}");

        // OpenAI: the model's real limit on completion tokens.
        let openai = "max_tokens is too large: 40000. This model supports at most 16384 \
            completion tokens, whereas you provided 40000.";
        assert!(says_context_overflow(openai));
        let lowered =
            lower_output_cap(&body_with_cap("max_completion_tokens", 40_000), openai).unwrap();
        assert_eq!(cap_of(&lowered, "max_completion_tokens"), 16_384);
    }

    #[test]
    fn a_refusal_that_is_not_about_the_output_lowers_nothing() {
        // The prompt alone is over: no output cap helps.
        let prompt_only = "maximum context length is 32768 tokens. However, you requested 0 \
            output tokens and your prompt contains at least 40000 input tokens";
        assert!(lower_output_cap(&body_with_cap("max_tokens", 0), prompt_only).is_none());
        // A cap already low enough.
        let message = "maximum context length is 32768 tokens. However, you requested 30135 \
            output tokens and your prompt contains at least 2634 input tokens";
        assert!(lower_output_cap(&body_with_cap("max_tokens", 100), message).is_none());
        // A body without a cap: the one refused was added on the way, and
        // the retry carries its own (AGE-988).
        let none = Bytes::from(r#"{"messages":[]}"#);
        let added = lower_output_cap(&none, message).expect("a cap added");
        assert_eq!(
            cap_of(&added, "max_tokens"),
            30_135 - 1 - OutputMargin::default().for_input(2_634) as u64
        );
        // No room left at all.
        let hopeless = "maximum context length is 32768 tokens. However, you requested 100 \
            output tokens and your prompt contains at least 32700 input tokens";
        assert!(lower_output_cap(&body_with_cap("max_tokens", 100), hopeless).is_none());
    }

    /// More vLLM wordings, and a floor on the lowered cap.
    #[test]
    fn more_vllm_wordings_and_a_floor_on_the_lowered_cap() {
        let new = "'max_tokens' or 'max_completion_tokens' is too large: 30135. This model's \
            maximum context length is 32768 tokens and your request has 2634 input tokens \
            (30135 > 32768 - 2634).";
        assert!(says_context_overflow(new));
        let lowered = lower_output_cap(&body_with_cap("max_tokens", 30_135), new).unwrap();
        let margin = OutputMargin::default().for_input(2_634) as u64;
        assert_eq!(cap_of(&lowered, "max_tokens"), 30_135 - 1 - margin);

        let old = "This model's maximum context length is 32768 tokens. However, you requested \
            32769 tokens (2634 in the messages, 30135 in the completion). Please reduce the \
            length of the messages or completion.";
        let lowered = lower_output_cap(&body_with_cap("max_tokens", 30_135), old).unwrap();
        assert_eq!(cap_of(&lowered, "max_tokens"), 30_135 - 1 - margin);

        // Input near the window: 32768 - 29400 - margin is under the floor,
        // so no output retry; compaction is the remedy.
        let crowded = "maximum context length is 32768 tokens. However, you requested 3500 \
            output tokens and your prompt contains at least 29400 input tokens";
        assert!(lower_output_cap(&body_with_cap("max_tokens", 3_500), crowded).is_none());
        assert_eq!(
            output_cap_refusal(crowded, 0, OutputMargin::default()),
            OutputCapRefusal::NotOutput
        );
    }

    fn refusal(message: &str) -> http_client::Error {
        http_client::Error::non_success_with_details(
            reqwest::StatusCode::BAD_REQUEST,
            Default::default(),
            message.to_string(),
        )
    }

    /// Sends `body` through `send_fitting` against a server that refuses the
    /// first request with `message`; returns the bodies sent and the
    /// overflows counted.
    async fn refused_once(body: Bytes, message: &str) -> (Vec<Bytes>, usize, bool) {
        let overflows = WindowOverflows::default();
        let sent = std::sync::Mutex::new(Vec::new());
        let result = send_fitting(Duration::ZERO, &overflows, body, |b| {
            let mut sent = sent.lock().unwrap();
            sent.push(b);
            let first = sent.len() == 1;
            let message = message.to_string();
            async move {
                if first {
                    Err(refusal(&message))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        (
            sent.into_inner().unwrap(),
            overflows.count(),
            result.is_ok(),
        )
    }

    /// An output-cap refusal that names nothing to lower to is returned as
    /// it is: no second request, no compaction, no overflow counted.
    #[tokio::test]
    async fn an_unlowerable_output_cap_refusal_neither_compacts_nor_counts() {
        let body = Bytes::from(
            serde_json::json!({ "max_tokens": 40_000, "messages": [
                { "role": "tool", "content": "x".repeat(5_000) },
                { "role": "tool", "content": "y".repeat(5_000) }
            ] })
            .to_string(),
        );
        let (sent, count, ok) = refused_once(body, "max_tokens is too large: 40000.").await;
        assert_eq!(sent.len(), 1, "no retry");
        assert_eq!(count, 0);
        assert!(!ok);
    }

    /// A lowered cap is a retry that counts no overflow; a compaction does.
    #[tokio::test]
    async fn only_a_compaction_counts_a_window_overflow() {
        let message = "maximum context length is 32768 tokens. However, you requested 30135 \
            output tokens and your prompt contains at least 2634 input tokens";
        let (sent, count, ok) = refused_once(body_with_cap("max_tokens", 30_135), message).await;
        assert!(ok);
        assert_eq!((sent.len(), count), (2, 0));
        assert!(cap_of(&sent[1], "max_tokens") < 30_135);

        let tools = Bytes::from(
            serde_json::json!({ "messages": [
                { "role": "tool", "content": "x".repeat(5_000) },
                { "role": "tool", "content": "y".repeat(5_000) }
            ] })
            .to_string(),
        );
        let overflow = "maximum context length is 32768 tokens. However, you requested 0 \
            output tokens and your prompt contains at least 32769 input tokens";
        let (sent, count, ok) = refused_once(tools, overflow).await;
        assert!(ok);
        assert_eq!((sent.len(), count), (2, 1));
        assert!(sent[1].len() < sent[0].len());
    }

    // ── AGE-988: run #4's refusals, a cap added downstream ──────────────

    /// A request like run #4's leader's: about 76k characters of history
    /// (about 23k tokens by cl100k) and no `max_tokens` of its own.
    fn run4_body() -> Bytes {
        let rows: String = (0..2_000)
            .map(|i| {
                format!(
                    "Merchant Crossfit_Hanna paid a fee of {}.{:02} EUR. ",
                    i % 97,
                    i % 100
                )
            })
            .collect();
        let rows = &rows[..rows.len().min(76_000)];
        Bytes::from(
            serde_json::json!({ "model": "RedHatAI/Qwen3.8-27B-INT4", "messages": [
                { "role": "system", "content": "You analyse payments data." },
                { "role": "user", "content": "Total the fees." },
                { "role": "tool", "content": rows }
            ] })
            .to_string(),
        )
    }

    fn tokens_of(body: &Bytes) -> u64 {
        crate::token_budget::counter::TokenCounter::for_model("m")
            .count(&String::from_utf8_lossy(body)) as u64
    }

    /// Both run #4 wordings: vLLM names only a lower bound on the input
    /// (`at least 583`, `upper bound for 143`), so the retry's cap is
    /// counted from the request itself, with the margin, and added to the
    /// body that had none. That request fits the window.
    #[test]
    fn the_run_4_refusals_are_answered_by_a_cap_that_fits() {
        let worker = "This model's maximum context length is 32768 tokens. However, you \
            requested 32186 output tokens and your prompt contains at least 583 input tokens, \
            for a total of at least 32769 tokens. Please reduce the length of the input \
            prompt or the number of requested output tokens. (parameter=input_tokens, \
            value=583)";
        let leader = "This model's maximum context length is 32768 tokens. However, you \
            requested 32625 output tokens and your prompt contains 76061 characters (more \
            than 18304 characters, which is the upper bound for 143 input tokens). Please \
            reduce the length of the input prompt or the number of requested output tokens. \
            (parameter=input_text, value=76061)";
        let body = run4_body();
        let input = tokens_of(&body);
        assert!(input > 15_000 && input < 25_000, "fixture: {input} tokens");
        for message in [worker, leader] {
            assert!(says_context_overflow(message));
            let lowered = lower_output_cap(&body, message).expect("a cap to retry with");
            let cap = cap_of(&lowered, "max_tokens");
            let margin = OutputMargin::default().for_input(input as usize) as u64;
            assert!(cap >= MIN_CLAMPED_REPLY_TOKENS as u64, "{cap}");
            assert!(input + margin + cap <= 32_768, "{input} + {margin} + {cap}");
        }
    }

    /// The retry goes out once, with that cap; no overflow is counted (the
    /// input was not the problem).
    #[tokio::test]
    async fn a_refusal_of_a_cap_added_downstream_is_retried_with_a_counted_cap() {
        let leader = "maximum context length is 32768 tokens. However, you requested 32625 \
            output tokens and your prompt contains 76061 characters (more than 18304 \
            characters, which is the upper bound for 143 input tokens).";
        let (sent, count, ok) = refused_once(run4_body(), leader).await;
        assert!(ok);
        assert_eq!((sent.len(), count), (2, 0));
        assert!(serde_json::from_slice::<Value>(&sent[0]).unwrap()["max_tokens"].is_null());
        assert!(cap_of(&sent[1], "max_tokens") + tokens_of(&sent[0]) < 32_768);
    }
}
