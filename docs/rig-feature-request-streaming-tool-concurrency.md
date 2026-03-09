# Feature Request: Add `with_tool_concurrency()` to Streaming Path

## Summary

The non-streaming `PromptRequest` supports concurrent tool execution via `with_tool_concurrency(N)` backed by `buffer_unordered(N)`. The streaming `StreamingPromptRequest` lacks this capability and executes all tool calls sequentially, even when the LLM explicitly batches multiple tool calls in a single assistant turn.

This proposal adds the same `with_tool_concurrency()` builder method to `StreamingPromptRequest`, enabling parallel tool execution in the streaming path.

## Motivation

When an LLM emits multiple `tool_use` blocks in a single assistant message (e.g., "read these 3 files" → 3 concurrent `read_file` calls), it is signaling that these operations are independent and can run in parallel. Today:

- **Non-streaming path**: Honors this intent via `buffer_unordered(concurrency)` (`mod.rs:568`)
- **Streaming path**: Ignores it — each tool call is `await`ed inline before the next one starts (`streaming.rs:533`)

For tools with I/O-bound latency (shell commands, file reads, HTTP requests, MCP server calls), sequential execution can multiply wall-clock time by the number of tool calls. A batch of 4 tool calls that each take 2 seconds costs 8 seconds sequentially vs ~2 seconds concurrently.

### Real-world use case

In [Chatty](https://github.com/boersmamarcel/chatty2), a desktop chat application built on rig-core, we use the streaming path exclusively (users expect to see tokens as they arrive). When a user asks the LLM to run multiple shell commands or read multiple files, each tool call blocks the next — leading to noticeably slower responses despite the LLM correctly batching them.

## Current Architecture

### Non-streaming (`PromptRequest`) — already supports concurrency

```rust
// mod.rs — PromptRequest struct
pub struct PromptRequest<'a, S, M, P> {
    // ...
    concurrency: usize,  // defaults to 1
    // ...
}

// Builder method
pub fn with_tool_concurrency(mut self, concurrency: usize) -> Self {
    self.concurrency = concurrency;
    self
}

// Tool execution (mod.rs:442-572)
let tool_content = stream::iter(tool_calls)
    .map(|choice| async move {
        // execute tool, run hooks, record span
    })
    .buffer_unordered(self.concurrency)  // ← concurrent execution
    .collect::<Vec<_>>()
    .await;
```

### Streaming (`StreamingPromptRequest`) — sequential only

```rust
// streaming.rs — NO concurrency field
pub struct StreamingPromptRequest<M, P> {
    prompt: Message,
    chat_history: Option<Vec<Message>>,
    max_turns: usize,
    // ... no concurrency field
    hook: Option<P>,
}

// Tool execution (streaming.rs:458-544)
Ok(StreamedAssistantContent::ToolCall { tool_call, .. }) => {
    // yield ToolCall event to stream (UI sees "tool started")

    let tool_result = tool_server_handle
        .call_tool(&tool_call.function.name, &tool_args)
        .await;  // ← blocks here until complete

    // yield ToolResult to stream
    // only THEN does the next tool call start
}
```

## Proposed Design

### API addition

Add a `concurrency` field and builder method to `StreamingPromptRequest`, mirroring the non-streaming path:

```rust
pub struct StreamingPromptRequest<M, P> {
    // ... existing fields ...
    concurrency: usize,  // NEW — defaults to 1
}

pub fn with_tool_concurrency(mut self, concurrency: usize) -> Self {
    self.concurrency = concurrency;
    self
}
```

### Execution strategy: deferred batch execution

When `concurrency > 1`, tool calls within a single assistant turn are collected and executed concurrently after the assistant's streamed response completes:

1. **During stream parsing**: When a `ToolCall` arrives, yield `StreamAssistantItem(ToolCall)` immediately (so the consumer can show "tool started" in the UI), but **defer execution** by pushing the tool call into a `pending_tool_calls` buffer.

2. **After the inner stream ends** (all assistant content for this turn has been received): Execute all pending tool calls concurrently using `buffer_unordered(concurrency)`.

3. **Yield results**: As each tool completes, yield `StreamUserItem(ToolResult)` — results arrive as they finish (not necessarily in order).

4. **Continue multi-turn loop**: Once all tool results are collected, append them to chat history and proceed to the next LLM turn as usual.

```rust
// Pseudocode for the deferred batch approach
let mut pending_tool_calls = Vec::new();

while let Some(item) = inner_stream.next().await {
    match item {
        ToolCall { tool_call, .. } => {
            yield Ok(StreamAssistantItem(ToolCall(..)));  // immediate UI feedback
            if concurrency > 1 {
                pending_tool_calls.push((tool_call, call_id, span));
            } else {
                // existing sequential behavior (zero change for default)
                let result = tool_server_handle.call_tool(..).await;
                yield Ok(StreamUserItem(ToolResult(..)));
            }
        }
        // ... text chunks, etc.
    }
}

// After inner stream ends: execute deferred tool calls concurrently
if !pending_tool_calls.is_empty() {
    let results = stream::iter(pending_tool_calls)
        .map(|(tc, id, span)| async move {
            // execute tool, run hooks, record span
            // (same logic as non-streaming mod.rs:472-565)
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;

    for result in results {
        yield Ok(StreamUserItem(ToolResult(..)));
    }
}
```

### Key design decisions

| Decision | Rationale |
|:---------|:----------|
| Default `concurrency: 1` | Zero behavior change for existing users |
| Deferred execution (not inline) | Tool calls must be fully parsed from the stream before execution can begin; also avoids interleaving stream parsing with concurrent I/O |
| `buffer_unordered` (same as non-streaming) | Proven pattern already in the codebase; limits max parallelism while allowing results to arrive out-of-order |
| Yield `ToolCall` immediately, defer execution | Consumer gets early notification for UI feedback (e.g., "running tool X...") |
| Hook callbacks preserved per tool call | Each tool call still triggers `on_tool_call_start` / `on_tool_call_end` hooks, just potentially overlapping in time |

### Stream item ordering

With `concurrency > 1`, the stream ordering changes slightly:

**Before (sequential, `concurrency=1`):**
```
AssistantItem(Text("I'll read both files"))
AssistantItem(ToolCall("read_file", "foo.rs"))     # tool A announced
UserItem(ToolResult("read_file", "contents of foo")) # tool A result
AssistantItem(ToolCall("read_file", "bar.rs"))     # tool B announced
UserItem(ToolResult("read_file", "contents of bar")) # tool B result
```

**After (concurrent, `concurrency=4`):**
```
AssistantItem(Text("I'll read both files"))
AssistantItem(ToolCall("read_file", "foo.rs"))     # tool A announced
AssistantItem(ToolCall("read_file", "bar.rs"))     # tool B announced
UserItem(ToolResult("read_file", "contents of bar")) # tool B finishes first
UserItem(ToolResult("read_file", "contents of foo")) # tool A finishes second
```

This is the same semantic contract as the non-streaming path — tool results are keyed by `tool_call_id`, so ordering of results does not affect correctness.

## Scope

### What changes

| File | Change |
|:-----|:-------|
| `streaming.rs` | Add `concurrency` field, `with_tool_concurrency()` builder, deferred batch execution in `send()` |

### What does NOT change

- `PromptRequest` (non-streaming) — already has this feature
- Default behavior (`concurrency=1`) — identical to current sequential execution
- Hook contract — hooks still fire per tool call
- Stream item types — no new variants needed
- Chat history construction — tool results are still collected before the next turn

## Alternatives Considered

1. **Inline concurrent execution during stream parsing**: More complex, risks interleaving stream parsing with tool I/O. The deferred approach is simpler and matches how the non-streaming path already works (collect all tool calls, then execute).

2. **Yielding results in original order**: Would require buffering completed results until earlier tools finish. Adds complexity for no real benefit — consumers already key on `tool_call_id`.

3. **Separate `StreamingPromptRequestConcurrent` type**: Unnecessary — a single field with a default of 1 is simpler and mirrors the existing non-streaming API.

## Usage Example

```rust
let mut stream = agent
    .stream_prompt(user_message)
    .with_history(history)
    .multi_turn(10)
    .with_tool_concurrency(4)  // NEW — run up to 4 tools in parallel
    .await;

while let Some(item) = stream.next().await {
    match item {
        Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => { /* render */ }
        Ok(MultiTurnStreamItem::StreamUserItem(content)) => { /* tool result */ }
        Err(e) => { /* handle error */ }
    }
}
```
