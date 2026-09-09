# Upstream rig-core patch proposal: mark the latest message inside `finalize_openrouter_request_body`

Status: **drafted, not submitted**. This repository has no push access to
`rig-core` in the session that wrote this file. Once a human with access to
that repo is available, this document is the starting point for the actual
PR — see "Submission checklist" at the end.

Tracks: [AGE-239](https://linear.app/agents-research/issue/AGE-239) (audit
finding B3, `docs/message-path-debt.md`). Origin of the workaround this
would let chatty delete: [AGE-205](https://linear.app/agents-research/issue/AGE-205).

Verified against the vendored source at
`~/.cargo/registry/src/index.crates.io-*/rig-core-0.42.0/` (the version
pinned in this workspace's `Cargo.lock`). Line numbers below are for that
exact version; re-check them against whatever `rig-core` revision the PR is
actually opened against, since upstream `main` has almost certainly moved.

## (a) Problem statement

`PromptCachingHttpClient::rewrite`
(`crates/chatty-core/src/factories/agent_factory/prompt_cache_http.rs:89-104`)
sits underneath rig's HTTP client and, on every `POST …/chat/completions`
call, does a second full parse-mutate-serialize pass over the request body
rig already built: it deserializes the JSON, calls `mark_latest_message` to
add a `cache_control: {"type": "ephemeral"}` breakpoint to the last
user/assistant message, and re-serializes. For a conversation with image or
PDF attachments in its history, that is a second full pass over a body that
can run to megabytes, once per provider request (i.e. once per turn *and*
once per tool round-trip within a turn).

rig already holds the same body as a `serde_json::Value` inside
`finalize_openrouter_request_body`
(`rig-core-0.42.0/src/providers/openrouter/completion.rs:1398`), and that
function's own `apply_prompt_caching` helper (line 1351) already walks
`messages` to mark the *system* message with the same
`cache_control: {"type": "ephemeral"}` shape, when `with_prompt_caching()`
was opted into on the completion model. Marking the *latest* user/assistant
message in the same pass — mirroring chatty's `mark_latest_message` — is a
small, additive change: no second JSON pass, no separate `HttpClientExt`
wrapper, no header patching for `content-length` after the rewrite.

Why chatty needs the latest-message breakpoint at all: OpenRouter's
provider-side explicit-cache-control models only reuse a cached prefix if
the *newest* request shares a marked prefix with the *previous* one.
`with_prompt_caching()` alone marks the system message (preamble + tool
defs), which is necessary but not sufficient — the growing conversation
history in `messages` never gets a breakpoint of its own, so only the
system-prompt portion of each request is ever a cache hit. Two breakpoints
total (system + latest message) stays within Anthropic's four-breakpoint
limit through OpenRouter.

## (b) Proposed diff

This is hand-written against the real 0.42.0 source (not machine-generated
via `git diff`), and kept minimal: it adds one new opt-in bool alongside the
existing `prompt_caching` bool rather than changing what `prompt_caching`
already means, so it cannot change behavior for any existing caller that
only wants the system-message breakpoint.

### 1. `src/providers/openai/completion/mod.rs`

Add a second flag next to the existing `prompt_caching` bool, both on the
per-model builder state (`GenericCompletionModel`) and the per-request
options snapshot (`CompletionModelOptions`) it hands to provider
extensions:

```diff
 pub struct GenericCompletionModel<Ext = super::OpenAICompletionsExt, H = reqwest::Client> {
     pub(crate) client: crate::client::Client<Ext, H>,
     pub model: String,
     pub(crate) strict_tools: bool,
     pub(crate) tool_result_array_content: bool,
     pub(crate) prompt_caching: bool,
+    /// Whether the OpenRouter extension should also mark the latest
+    /// user/assistant message as a cache breakpoint (`with_prompt_caching`
+    /// only marks the system message). No-op for providers that don't
+    /// consume it.
+    pub(crate) prompt_caching_history: bool,
 }
```

```diff
     pub fn new(client: crate::client::Client<Ext, H>, model: impl Into<String>) -> Self {
         Self {
             client,
             model: model.into(),
             strict_tools: false,
             tool_result_array_content: false,
             prompt_caching: false,
+            prompt_caching_history: false,
         }
     }
```

```diff
 pub struct CompletionModelOptions {
     /// Whether tool schemas should be sanitized for strict-mode validation.
     pub strict_tools: bool,
     /// Whether tool-result messages should serialize their content as arrays.
     pub tool_result_array_content: bool,
     /// Whether the model requested provider-specific prompt caching markers.
     pub prompt_caching: bool,
+    /// Whether the model also requested a moving breakpoint on the latest
+    /// user/assistant message, on top of the system-message breakpoint
+    /// `prompt_caching` already covers. Currently only consumed by the
+    /// OpenRouter extension.
+    pub prompt_caching_history: bool,
 }
```

Both places that build a `CompletionModelOptions` from `self` need the new
field threaded through — `raw_completion_with_request_id`
(`mod.rs:2240-2244`) and `raw_stream` (`completion/streaming.rs:326-330`):

```diff
         let options = CompletionModelOptions {
             strict_tools: self.strict_tools,
             tool_result_array_content: self.tool_result_array_content,
             prompt_caching: self.prompt_caching,
+            prompt_caching_history: self.prompt_caching_history,
         };
```
(applies identically at both call sites)

### 2. `src/providers/openrouter/completion.rs`

Port chatty's `mark_latest_message` next to `apply_prompt_caching`, thread
the new option through `finalize_openrouter_request_body`, and add the
builder method:

```diff
+/// Add `cache_control: {"type": "ephemeral"}` to the last content block of
+/// the most recent user or assistant message.
+///
+/// A string `content` becomes a one-element text-block array. Messages
+/// whose content is absent (an assistant turn that only carried tool
+/// calls) or a `tool` result are skipped in favour of the message before
+/// them, so a tool-loop request still caches everything up to its last
+/// model/user exchange. Idempotent: re-running this on an
+/// already-marked body overwrites the same `cache_control` value in place
+/// rather than adding a second marker.
+pub(super) fn mark_latest_message(body: &mut serde_json::Value) {
+    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
+        return;
+    };
+    for message in messages.iter_mut().rev() {
+        let Some(obj) = message.as_object_mut() else {
+            continue;
+        };
+        let role = obj.get("role").and_then(|r| r.as_str());
+        if !matches!(role, Some("user") | Some("assistant")) {
+            continue;
+        }
+        match obj.get_mut("content") {
+            Some(serde_json::Value::String(text)) if !text.is_empty() => {
+                let text = std::mem::take(text);
+                obj.insert(
+                    "content".to_string(),
+                    serde_json::json!([{
+                        "type": "text",
+                        "text": text,
+                        "cache_control": { "type": "ephemeral" }
+                    }]),
+                );
+                return;
+            }
+            Some(serde_json::Value::Array(blocks)) => {
+                if let Some(serde_json::Value::Object(last)) = blocks.last_mut() {
+                    last.insert(
+                        "cache_control".to_string(),
+                        serde_json::json!({ "type": "ephemeral" }),
+                    );
+                    return;
+                }
+            }
+            _ => {}
+        }
+    }
+}
+
 pub(super) fn finalize_openrouter_request_body(body: &mut serde_json::Value, prompt_caching: bool) {
     if prompt_caching {
         apply_prompt_caching(body);
     }
+    if prompt_caching_history {
+        mark_latest_message(body);
+    }

     // The shared assistant message serializes hidden reasoning under the
```

The signature itself needs the second parameter:

```diff
-pub(super) fn finalize_openrouter_request_body(body: &mut serde_json::Value, prompt_caching: bool) {
+pub(super) fn finalize_openrouter_request_body(
+    body: &mut serde_json::Value,
+    prompt_caching: bool,
+    prompt_caching_history: bool,
+) {
```

Its two callers:

```diff
     fn finalize_request_body_with_options(
         &self,
         body: &mut serde_json::Value,
         options: openai::completion::CompletionModelOptions,
     ) -> Result<(), CompletionError> {
-        finalize_openrouter_request_body(body, options.prompt_caching);
+        finalize_openrouter_request_body(
+            body,
+            options.prompt_caching,
+            options.prompt_caching_history,
+        );
         Ok(())
     }
```

```diff
 #[cfg(test)]
 pub(super) fn final_request_body(
     request: &OpenrouterCompletionRequest,
     prompt_caching: bool,
+    prompt_caching_history: bool,
 ) -> Result<serde_json::Value, CompletionError> {
     let mut body = serde_json::to_value(request)?;
-    finalize_openrouter_request_body(&mut body, prompt_caching);
+    finalize_openrouter_request_body(&mut body, prompt_caching, prompt_caching_history);
     Ok(body)
 }
```

...which means the three existing call sites of `final_request_body` in
this file's own test module (`test_final_request_body_applies_prompt_caching_to_converted_completion_request`
and `test_final_request_body_preserves_stream_flag_when_prompt_caching_enabled`,
around lines 4449/4456/4475) each need a trailing `false` added to keep
testing exactly what they test today:

```diff
-        let body = final_request_body(&request, true).expect("request body should serialize");
+        let body = final_request_body(&request, true, false).expect("request body should serialize");
```
(three occurrences, mechanical)

And the builder method, next to `with_prompt_caching()`:

```diff
 impl<H> openai::completion::GenericCompletionModel<OpenRouterExt, H> {
     /// Enable explicit prompt caching for supported OpenRouter models.
     ///
     /// Adds `cache_control: {"type": "ephemeral"}` to the system-prompt
     /// block so subsequent turns that share the same system prefix can be
     /// billed at the cache-hit rate when the selected model/provider supports
     /// explicit cache breakpoints.
     pub fn with_prompt_caching(mut self) -> Self {
         self.prompt_caching = true;
         self
     }
+
+    /// Also mark the latest user/assistant message as a cache breakpoint,
+    /// on top of the system-message breakpoint `with_prompt_caching`
+    /// already adds. OpenRouter's explicit-cache-control providers only
+    /// reuse a cached prefix when the newest request shares a marked
+    /// prefix with the previous one, so without this the conversation
+    /// history itself is never a cache hit — only the system prompt is.
+    /// No-op unless [`with_prompt_caching`](Self::with_prompt_caching) is
+    /// also enabled.
+    pub fn with_prompt_caching_history(mut self) -> Self {
+        self.prompt_caching_history = true;
+        self
+    }
 }
```

**Open design question for whoever opens the PR**: should
`with_prompt_caching_history()` be a separate opt-in (as drafted above,
matching the issue's "or add `prompt_caching_history()`" phrasing), or
should `with_prompt_caching()` simply always do both? Chatty always wants
both together, and no other rig caller opts into `prompt_caching` today (it
was added for chatty's own AGE-205), so there is no compatibility reason to
keep them separate — collapsing them into one flag on `with_prompt_caching()`
would be a smaller diff (no new field, no new builder method, no second
plumbed-through bool). This document keeps them separate defensively, but
a maintainer with a fuller view of `with_prompt_caching()`'s other callers
should feel free to collapse it.

## (c) Adapted tests

Chatty's own unit tests for `mark_latest_message` and `rewrite`
(`crates/chatty-core/src/factories/agent_factory/prompt_cache_http.rs`)
adapted into this file's existing `#[cfg(test)] mod tests` block, matching
its `json!`-macro style and its `apply_prompt_caching` tests immediately
above:

```rust
#[test]
fn test_mark_latest_message_string_content_becomes_a_marked_text_block() {
    let mut body = json!({ "messages": [
        { "role": "system", "content": "sys" },
        { "role": "user", "content": "hello" }
    ]});
    mark_latest_message(&mut body);
    assert_eq!(
        body["messages"][1]["content"],
        json!([{ "type": "text", "text": "hello", "cache_control": { "type": "ephemeral" } }])
    );
    // The system message is apply_prompt_caching's breakpoint; this helper
    // leaves it alone.
    assert_eq!(body["messages"][0]["content"], json!("sys"));
}

#[test]
fn test_mark_latest_message_array_content_marks_its_last_block_only() {
    let mut body = json!({ "messages": [
        { "role": "user", "content": [
            { "type": "text", "text": "look" },
            { "type": "image_url", "image_url": { "url": "data:..." } }
        ] }
    ]});
    mark_latest_message(&mut body);
    let blocks = body["messages"][0]["content"].as_array().unwrap();
    assert!(blocks[0].get("cache_control").is_none());
    assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
}

#[test]
fn test_mark_latest_message_tool_results_and_empty_assistant_turns_fall_back() {
    let mut body = json!({ "messages": [
        { "role": "user", "content": "run it" },
        { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
        { "role": "tool", "tool_call_id": "c1", "content": "ok" }
    ]});
    mark_latest_message(&mut body);
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert!(body["messages"][1]["content"].is_null());
    assert_eq!(body["messages"][2]["content"], json!("ok"));
}

#[test]
fn test_mark_latest_message_nothing_to_mark_is_noop() {
    let mut empty = json!({ "messages": [] });
    let before = empty.clone();
    mark_latest_message(&mut empty);
    assert_eq!(empty, before);

    let mut no_messages_key = json!({ "model": "x" });
    let before = no_messages_key.clone();
    mark_latest_message(&mut no_messages_key);
    assert_eq!(no_messages_key, before);

    let mut only_tool = json!({ "messages": [
        { "role": "tool", "content": "only" }
    ]});
    let before = only_tool.clone();
    mark_latest_message(&mut only_tool);
    assert_eq!(only_tool, before);
}

/// The combined-shape invariant this whole change exists for: system
/// message and latest message both marked, exactly two breakpoints, in a
/// single serialize pass.
#[test]
fn test_final_request_body_applies_both_system_and_history_breakpoints() {
    let request = OpenrouterCompletionRequest::try_from(OpenRouterRequestParams {
        model: "anthropic/claude-3.5-sonnet",
        request: prompt_caching_completion_request(),
        strict_tools: false,
    })
    .expect("request conversion should succeed");

    let body = final_request_body(&request, true, true).expect("request body should serialize");

    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral",
        "system message breakpoint from apply_prompt_caching"
    );
    let user_message = body["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(
        user_message["content"][0]["cache_control"]["type"],
        "ephemeral",
        "latest-message breakpoint from mark_latest_message"
    );

    // prompt_caching_history without prompt_caching (unlikely combination,
    // but the two flags are independent): the system message stays plain.
    let body = final_request_body(&request, false, true).expect("request body should serialize");
    assert!(
        body["messages"][0]["content"][0].get("cache_control").is_none()
    );
}

/// Re-finalizing an already-marked body must not add a third breakpoint —
/// this is what lets a wrapper like chatty's `PromptCachingHttpClient`
/// be deleted outright once it can call this instead: the two are meant
/// to be behaviorally identical, including on a resend of an
/// already-marked history within the same tool loop.
#[test]
fn test_finalize_openrouter_request_body_is_idempotent_on_an_already_marked_body() {
    let mut body = json!({ "messages": [
        { "role": "system", "content": "sys" },
        { "role": "user", "content": "hello" }
    ]});
    finalize_openrouter_request_body(&mut body, true, true);
    let once = body.clone();
    finalize_openrouter_request_body(&mut body, true, true);
    assert_eq!(body, once, "second finalize pass must be a no-op");
}
```

Note: `prompt_caching_completion_request()` and `OpenrouterCompletionRequest`
already exist in this test module (lines 4424-4438); the new tests reuse
them rather than redefining fixtures.

## (d) Suggested PR

**Title:**
```
openrouter: mark the latest message as a prompt-cache breakpoint too
```

**Description:**
```
`with_prompt_caching()` marks the system message (preamble + tool
definitions) as an explicit `cache_control` breakpoint, but OpenRouter's
explicit-cache-control providers only reuse a cached prefix when the
*newest* request shares a marked prefix with the previous one. Without a
breakpoint on the growing conversation history, only the system-prompt
portion of each request is ever a cache hit.

Downstream consumers who need the history breakpoint today have to bolt
it on with a custom `HttpClientExt` that re-parses and re-serializes the
whole request body a second time per call (see chatty's
`PromptCachingHttpClient`, linked in the issue this PR closes). This PR
adds the same marking inside `finalize_openrouter_request_body`, which
already holds the body as `serde_json::Value` for `apply_prompt_caching`
to use — no second JSON pass, and the wrapper becomes unnecessary.

Adds `with_prompt_caching_history()` as a separate opt-in alongside
`with_prompt_caching()` (see the "Open design question" note in the
PR conversation / linked doc if you'd rather fold it into the existing
flag — I don't have visibility into every other consumer of
`with_prompt_caching()` to know if that's safe).

Tests adapted from the downstream project's own unit tests for the same
logic, which has been running in production against OpenRouter for
several months.

Closes: <chatty issue link, e.g. https://linear.app/agents-research/issue/AGE-239>
```

## Submission checklist (for whoever has rig-core access)

1. Clone `rig-core` at (or rebase onto) whatever revision is current; re-verify every line number and function name above still matches — this document was written against `0.42.0` and upstream `main` will have moved.
2. Apply the diff in section (b); resolve the "Open design question" one way or the other.
3. Add the tests in section (c) to `src/providers/openrouter/completion.rs`'s existing `#[cfg(test)] mod tests` block.
4. Run rig-core's own test suite; open the PR using the title/description in section (d).
5. Once merged and released, bump chatty's `rig-core` dependency, delete
   `crates/chatty-core/src/factories/agent_factory/prompt_cache_http.rs`
   (including `PromptCachingHttpClient`), switch the OpenRouter agent
   builder to `.with_prompt_caching().with_prompt_caching_history()` (or
   whatever the merged API ended up being), and delete the pinning test
   this repo added at the same time as this document — its only job is to
   hold the fallback's shape stable until this point.
6. Link the merged PR on
   [AGE-205](https://linear.app/agents-research/issue/AGE-205) and close
   out [AGE-239](https://linear.app/agents-research/issue/AGE-239).
