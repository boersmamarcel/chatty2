//! HTTP layer that adds the moving prompt-cache breakpoint (AGE-205).
//!
//! rig's `with_prompt_caching()` marks the system message with
//! `cache_control`, which caches the preamble and tool definitions. The
//! conversation history only caches if the *latest* message carries a
//! breakpoint too, so each request re-uses the prefix the previous one
//! wrote. rig has no hook for that: its per-turn `RequestPatch` merges
//! `additional_params` at the top level of the body and cannot annotate an
//! element of `messages`. This wrapper sits under rig's client instead and
//! rewrites the serialized chat-completions body on its way out.
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

/// `reqwest::Client` that marks the latest user/assistant message of every
/// chat-completions request as a prompt-cache breakpoint.
///
/// `Default` is required by rig's completion-model bounds on the HTTP client
/// type, not used by chatty itself.
#[derive(Clone, Debug, Default)]
pub struct PromptCachingHttpClient {
    inner: reqwest::Client,
}

impl PromptCachingHttpClient {
    pub fn new(inner: reqwest::Client) -> Self {
        Self { inner }
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

/// Rewrite a chat-completions request body; pass any other request through.
fn rewrite<T: Into<Bytes>>(req: Request<T>) -> Request<Bytes> {
    let (mut parts, body) = req.into_parts();
    let body: Bytes = body.into();
    let is_chat_completion =
        parts.method == Method::POST && parts.uri.path().ends_with("/chat/completions");
    if is_chat_completion
        && let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body)
        && mark_latest_message(&mut json)
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
        self.inner.send(rewrite(req))
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
        self.inner.send_streaming(rewrite(req))
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
        let out = rewrite(chat);
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
        assert_eq!(rewrite(listing).body(), &Bytes::from(body));
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
    /// AGE-239): rig's own `apply_prompt_caching` already marks the system
    /// message before this wrapper ever sees the body (simulated here, since
    /// that step happens upstream in rig-core, not in this file), and this
    /// wrapper marks the latest user/assistant message. A follow-up
    /// tool-loop turn resends the growing, already-marked history through
    /// `rewrite()` again — that must not add a third breakpoint or touch the
    /// existing one, so this wrapper's fallback can be dropped outright once
    /// rig grows the upstream equivalent (AGE-239) without re-checking this
    /// invariant by hand.
    #[test]
    fn rewrite_combined_with_rigs_system_breakpoint_yields_exactly_two_and_is_idempotent() {
        let body = json!({ "messages": [
            { "role": "system", "content": [
                { "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }
            ]},
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

        let out = rewrite(request(body));
        let rewritten: serde_json::Value = serde_json::from_slice(out.body()).unwrap();
        assert_eq!(count_cache_control_breakpoints(&rewritten), 2);

        // Re-send the already-marked body, as the next provider request in
        // the same tool loop would.
        let out_again = rewrite(request(rewritten.to_string()));
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
