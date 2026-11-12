//! HTTP layer that adds the prompt-cache breakpoints (AGE-205).
//!
//! The system message carries `cache_control` on its last content block,
//! which caches the preamble and tool definitions ([`mark_system_message`]:
//! the shape rig 0.42's `with_prompt_caching()` wrote; rig 0.46 only offers
//! a top-level `cache_control`, AGE-1151). The conversation history only
//! caches if the *latest* message carries a breakpoint too, so each request
//! re-uses the prefix the previous one wrote. rig has no hook for that: its
//! per-turn `RequestPatch` merges `additional_params` at the top level of
//! the body and cannot annotate an element of `messages`. This wrapper sits
//! under rig's client instead and rewrites the serialized chat-completions
//! body on its way out.
//!
//! A streamed request also gets `stream_options.include_usage`, so an
//! OpenAI-compatible server behind this client reports its token usage
//! (see [`request_stream_usage`]).
//!
//! Only `POST …/chat/completions` bodies are touched. Everything else
//! (model listing, key verification, multipart uploads) passes straight
//! through to `reqwest`.

use std::future::Future;

use bytes::Bytes;
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, Method, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;

use super::connect_retry_http::ConnectRetryHttpClient;

/// HTTP client that marks the system message and the latest user/assistant
/// message of every chat-completions request as prompt-cache breakpoints,
/// and sends a request again when the connection failed before any response
/// ([`ConnectRetryHttpClient`]).
#[derive(Clone, Debug, Default)]
pub struct PromptCachingHttpClient {
    inner: ConnectRetryHttpClient,
    /// Whether the latest message gets the moving breakpoint; off only for
    /// the cache probe's control arm, which keeps the system one alone.
    moving: bool,
}

impl PromptCachingHttpClient {
    pub fn new(inner: ConnectRetryHttpClient) -> Self {
        Self {
            inner,
            moving: true,
        }
    }

    /// The system breakpoint alone, for the cache probe's control arm.
    #[cfg(test)]
    pub(crate) fn system_breakpoint_only(inner: ConnectRetryHttpClient) -> Self {
        Self {
            inner,
            moving: false,
        }
    }
}

/// Mark the first system message as a prompt-cache breakpoint: its `content`
/// becomes text blocks, the last one carrying `cache_control: {"type":
/// "ephemeral"}`. This is the marker rig 0.42's OpenRouter client wrote
/// (`with_prompt_caching()`), and a top-level `cache_control` (rig 0.46's
/// spelling of the same opt-in) moves there. Returns whether anything
/// changed.
pub(crate) fn mark_system_message(body: &mut serde_json::Value) -> bool {
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    let top_level = obj.remove("cache_control").is_some();
    let Some(system) = obj
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .and_then(|messages| {
            messages
                .iter_mut()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        })
        .and_then(|m| m.as_object_mut())
    else {
        return top_level;
    };
    match system.get_mut("content") {
        Some(serde_json::Value::String(text)) => {
            let text = std::mem::take(text);
            system.insert(
                "content".to_string(),
                serde_json::json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": { "type": "ephemeral" }
                }]),
            );
            true
        }
        Some(serde_json::Value::Array(blocks)) => match blocks.last_mut() {
            Some(serde_json::Value::Object(last)) => {
                last.insert(
                    "cache_control".to_string(),
                    serde_json::json!({ "type": "ephemeral" }),
                );
                true
            }
            _ => top_level,
        },
        _ => top_level,
    }
}

/// Add `cache_control: {"type": "ephemeral"}` to the last content block of
/// the most recent user or assistant message.
///
/// A string `content` becomes a one-element text-block array. Messages whose
/// content is absent (an assistant turn that only carried tool calls) or a
/// `tool` result are skipped in favour of the message before them, so a
/// tool-loop request still caches everything up to its last model/user
/// exchange. Returns whether anything changed.
pub(crate) fn mark_latest_message(body: &mut serde_json::Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return false;
    };
    for message in messages.iter_mut().rev() {
        let Some(obj) = message.as_object_mut() else {
            continue;
        };
        let role = obj.get("role").and_then(|r| r.as_str());
        if !matches!(role, Some("user") | Some("assistant")) {
            continue;
        }
        match obj.get_mut("content") {
            Some(serde_json::Value::String(text)) if !text.is_empty() => {
                let text = std::mem::take(text);
                obj.insert(
                    "content".to_string(),
                    serde_json::json!([{
                        "type": "text",
                        "text": text,
                        "cache_control": { "type": "ephemeral" }
                    }]),
                );
                return true;
            }
            Some(serde_json::Value::Array(blocks)) => {
                if let Some(serde_json::Value::Object(last)) = blocks.last_mut() {
                    last.insert(
                        "cache_control".to_string(),
                        serde_json::json!({ "type": "ephemeral" }),
                    );
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Ask a streamed chat-completions request for its usage.
///
/// rig's OpenRouter client leaves `stream_options` out (OpenRouter reports
/// usage on the final chunk unasked), but the same client is how chatty
/// talks to every OpenAI-compatible server (`--openai-compat-url`), and
/// vLLM, llama.cpp and the like send no usage on a stream unless
/// `stream_options.include_usage` is set: every call counted zero tokens.
/// A caller's own `include_usage` is left as it is. Returns whether
/// anything changed.
pub(crate) fn request_stream_usage(body: &mut serde_json::Value) -> bool {
    if body.get("stream").and_then(|s| s.as_bool()) != Some(true) {
        return false;
    }
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    let options = obj
        .entry("stream_options")
        .or_insert_with(|| serde_json::json!({}));
    let Some(options) = options.as_object_mut() else {
        return false;
    };
    if options.contains_key("include_usage") {
        return false;
    }
    options.insert("include_usage".to_string(), serde_json::Value::Bool(true));
    true
}

/// Rewrite a chat-completions request body; pass any other request through.
fn rewrite<T: Into<Bytes>>(req: Request<T>, moving: bool) -> Request<Bytes> {
    let (mut parts, body) = req.into_parts();
    let body: Bytes = body.into();
    let is_chat_completion =
        parts.method == Method::POST && parts.uri.path().ends_with("/chat/completions");
    if is_chat_completion
        && let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body)
        && (mark_system_message(&mut json)
            | (moving && mark_latest_message(&mut json))
            | request_stream_usage(&mut json))
        && let Ok(rewritten) = serde_json::to_vec(&json)
    {
        // reqwest derives the length from the body it is given.
        parts.headers.remove("content-length");
        return Request::from_parts(parts, Bytes::from(rewritten));
    }
    Request::from_parts(parts, body)
}

impl HttpClientExt for PromptCachingHttpClient {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.inner.send(rewrite(req, self.moving))
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.inner.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        self.inner.send_streaming(rewrite(req, self.moving))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ephemeral() -> serde_json::Value {
        json!({ "type": "ephemeral" })
    }

    #[test]
    fn string_content_becomes_a_marked_text_block() {
        let mut body = json!({ "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "hello" }
        ]});
        assert!(mark_latest_message(&mut body));
        assert_eq!(
            body["messages"][1]["content"],
            json!([{ "type": "text", "text": "hello", "cache_control": { "type": "ephemeral" } }])
        );
        // The system message is rig's breakpoint; this layer leaves it alone.
        assert_eq!(body["messages"][0]["content"], json!("sys"));
    }

    /// AGE-1151: rig 0.42's `with_prompt_caching()` marker, restored on
    /// rig 0.46's body: a string system prompt becomes one marked text block.
    #[test]
    fn a_string_system_prompt_becomes_a_marked_text_block() {
        let mut body = json!({ "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "hello" }
        ]});
        assert!(mark_system_message(&mut body));
        assert_eq!(
            body["messages"][0]["content"],
            json!([{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }])
        );
        assert_eq!(body["messages"][1]["content"], json!("hello"));
    }

    /// AGE-1151: rig 0.46 sends the system prompt as text blocks and, when
    /// asked to cache, a top-level `cache_control`; the marker goes on the
    /// last system block instead, where rig 0.42 put it.
    #[test]
    fn a_top_level_cache_marker_moves_onto_the_system_block() {
        let mut body = json!({
            "cache_control": { "type": "ephemeral" },
            "messages": [
                { "role": "system", "content": [{ "type": "text", "text": "sys" }] },
                { "role": "user", "content": "hello" }
            ]
        });
        assert!(mark_system_message(&mut body));
        assert_eq!(
            body,
            json!({ "messages": [
                { "role": "system", "content": [
                    { "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }
                ]},
                { "role": "user", "content": "hello" }
            ]})
        );
    }

    #[test]
    fn array_content_marks_its_last_block_only() {
        let mut body = json!({ "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "look" },
                { "type": "image_url", "image_url": { "url": "data:..." } }
            ] }
        ]});
        assert!(mark_latest_message(&mut body));
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert!(blocks[0].get("cache_control").is_none());
        assert_eq!(blocks[1]["cache_control"], ephemeral());
    }

    #[test]
    fn tool_results_and_empty_assistant_turns_fall_back_to_the_previous_message() {
        let mut body = json!({ "messages": [
            { "role": "user", "content": "run it" },
            { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
            { "role": "tool", "tool_call_id": "c1", "content": "ok" }
        ]});
        assert!(mark_latest_message(&mut body));
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            ephemeral()
        );
        assert!(body["messages"][1]["content"].is_null());
        assert_eq!(body["messages"][2]["content"], json!("ok"));
    }

    #[test]
    fn a_streamed_request_asks_for_its_usage() {
        let mut body = json!({ "stream": true, "messages": [] });
        assert!(request_stream_usage(&mut body));
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        // Idempotent, and a caller's own choice stands.
        assert!(!request_stream_usage(&mut body));
        let mut body =
            json!({ "stream": true, "stream_options": { "include_usage": false, "x": 1 } });
        assert!(!request_stream_usage(&mut body));
        assert_eq!(
            body["stream_options"],
            json!({ "include_usage": false, "x": 1 })
        );
        let mut body = json!({ "stream": true, "stream_options": { "x": 1 } });
        assert!(request_stream_usage(&mut body));
        assert_eq!(
            body["stream_options"],
            json!({ "include_usage": true, "x": 1 })
        );
    }

    #[test]
    fn a_blocking_request_is_left_alone() {
        let mut body = json!({ "messages": [] });
        assert!(!request_stream_usage(&mut body));
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn nothing_to_mark_reports_false() {
        assert!(!mark_latest_message(&mut json!({ "messages": [] })));
        assert!(!mark_latest_message(&mut json!({ "model": "x" })));
        assert!(!mark_latest_message(&mut json!({ "messages": [
            { "role": "tool", "content": "only" }
        ]})));
    }

    #[test]
    fn rewrite_touches_only_chat_completion_posts() {
        let body = json!({ "messages": [{ "role": "user", "content": "hi" }] }).to_string();
        let chat = Request::builder()
            .method(Method::POST)
            .uri("https://openrouter.ai/api/v1/chat/completions")
            .header("content-length", body.len().to_string())
            .body(Bytes::from(body.clone()))
            .unwrap();
        let out = rewrite(chat, true);
        let json: serde_json::Value = serde_json::from_slice(out.body()).unwrap();
        assert_eq!(
            json["messages"][0]["content"][0]["cache_control"],
            ephemeral()
        );
        assert!(out.headers().get("content-length").is_none());

        let listing = Request::builder()
            .method(Method::GET)
            .uri("https://openrouter.ai/api/v1/models")
            .body(Bytes::from(body.clone()))
            .unwrap();
        assert_eq!(rewrite(listing, true).body(), &Bytes::from(body));
    }

    /// Counts `cache_control` markers across every message's content blocks.
    fn count_cache_control_breakpoints(body: &serde_json::Value) -> usize {
        body["messages"]
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|m| m.get("content")?.as_array())
                    .flatten()
                    .filter(|block| block.get("cache_control").is_some())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Pins the two-breakpoint shape this wrapper exists for (AGE-205,
    /// AGE-239): the system message (the body as rig 0.46 sends it, its
    /// system content a text-block array without a marker) and the latest
    /// user/assistant message. A follow-up tool-loop turn resends the
    /// growing, already-marked history through `rewrite()` again — that must
    /// not add a third breakpoint or touch the existing ones.
    #[test]
    fn rewrite_yields_exactly_two_breakpoints_and_is_idempotent() {
        let body = json!({ "messages": [
            { "role": "system", "content": [{ "type": "text", "text": "sys" }] },
            { "role": "user", "content": "first turn" },
            { "role": "assistant", "content": "reply" },
            { "role": "user", "content": "second turn" }
        ]})
        .to_string();

        let request = |body: String| {
            Request::builder()
                .method(Method::POST)
                .uri("https://openrouter.ai/api/v1/chat/completions")
                .body(Bytes::from(body))
                .unwrap()
        };

        let out = rewrite(request(body), true);
        let rewritten: serde_json::Value = serde_json::from_slice(out.body()).unwrap();
        assert_eq!(count_cache_control_breakpoints(&rewritten), 2);

        // Re-send the already-marked body, as the next provider request in
        // the same tool loop would.
        let out_again = rewrite(request(rewritten.to_string()), true);
        let rewritten_again: serde_json::Value = serde_json::from_slice(out_again.body()).unwrap();
        assert_eq!(
            count_cache_control_breakpoints(&rewritten_again),
            2,
            "re-marking an already-marked block must not add a third breakpoint"
        );
        assert_eq!(
            rewritten, rewritten_again,
            "rewriting an already-marked body must be a no-op"
        );
    }
}
