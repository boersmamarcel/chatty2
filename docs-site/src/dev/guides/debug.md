# Debug

**When to read this:** Something is wrong at runtime — a stuck stream, missing or duplicated text, a layout glitch, a tool that "failed" with no message, a cache hit rate that never rises — and you need the switches that expose what happened.

## Goal

Find the log line, overlay field or file that explains the symptom, using only what the binaries already emit.

## Prerequisites

- A debug build launched from a terminal (`cargo run -p chatty-gpui` / `cargo run -p chatty-tui`), so you can set environment variables and see stderr.
- For rendering bugs: [Rendering pipeline](../architecture/rendering-system.md). For stream bugs: [Stream lifecycle](../architecture/stream-manager.md).

## Steps

### 1. Turn on logs

Both binaries use `tracing` with an `EnvFilter`, so `RUST_LOG` narrows or widens what you see.

| Binary | Where logs go | Default level |
|--------|---------------|---------------|
| `chatty` (desktop) | stderr of the process. An `ErrorCollectorLayer` also copies every `warn` and `error` event into the in-app error log dialog | `info` |
| `chatty-tui` (interactive) | `<data dir>/chatty/chatty-tui.log` — `~/.local/share/chatty/` on Linux (`$XDG_DATA_HOME` if set), `~/Library/Application Support/chatty/` on macOS, `%APPDATA%\chatty\` on Windows. The file is truncated on every start | `warn` |
| `chatty-tui --headless` / `--pipe` | Nowhere: logging is suppressed so stdout carries only the answer | — |

Useful filters:

```bash
RUST_LOG=debug cargo run -p chatty-tui                         # everything at debug
RUST_LOG="warn,chatty_core=debug" cargo run -p chatty-gpui     # core only
RUST_LOG="info,chatty_gpui::render=trace" cargo run -p chatty-gpui 2>&1 | tee /tmp/chatty-render.log
```

The desktop render path logs under the targets `chatty_gpui::render::list`, `::message`, `::stream` and `::handler`; all four match `chatty_gpui::render`. Other environment variables the binaries read are in the [environment variables reference](../reference/env-vars.md).

### 2. The `CHATTY_DEBUG_UI` overlay

Set `CHATTY_DEBUG_UI=1` (any value other than empty or `0`) before launching the desktop app to get a monospace overlay in the top-right of the chat pane, one row per message. The flag is read once at process start and costs nothing when unset.

```text
ChatView debug
  msgs: 4 visible / 5 total   awaiting: false   skeleton: false   filtered: 1
  [0] User       s=0 m=0 ti=0 c=23  trace=none
  [1] Assistant  s=1 m=1 ti=2 c=148  trace=live
```

| Field | Meaning |
|-------|---------|
| `visible` / `total` / `filtered` | Messages the list renders, all messages, and how many the empty-streaming-message filter hides |
| `awaiting`, `skeleton` | `is_awaiting_response()` — waiting for the first token, thinking indicator shown |
| `s` | `is_streaming` |
| `m` | `is_markdown` |
| `ti` | Number of items in `live_trace` (streaming) or the `SystemTraceView` (finalized) |
| `c` | `content.len()` |
| `trace` | `live` while streaming, `open` / `empty` once a `SystemTraceView` exists, `none` otherwise |

`awaiting: true` together with `s=1 c=0` on the last assistant row is the expected "waiting for the first token" state; the layout jump when the first token replaces the thinking indicator is what usually gets reported as "whitespace".

### 3. Trace a rendering bug

Capture a log with the render targets at `trace` (the `tee` command in step 1), reproduce, then grep:

```bash
grep -E "render_message_list|render_message |interleave_segment|append_assistant_text|finalize_assistant_message" /tmp/chatty-render.log
grep '"conversation_id":"<id>"' /tmp/chatty-render.log     # one conversation
```

Events in the order they fire during a streaming turn:

| Event | Fired from | Useful fields |
|-------|------------|---------------|
| `start_assistant_message` | [`chat_view/mod.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/chatty/views/chat_view/mod.rs) | `total_messages` |
| `append_assistant_text` | `chat_view/mod.rs` | `delta_len`, `content_len_before`, `new_content_len`, `message_idx`; a `warn` variant `append_assistant_text dropped` means no streaming parent existed |
| `tool_call_started` | [`chat_view/handlers.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/chatty/views/chat_view/handlers.rs) | `tool_id`, `tool_name`, `text_before_len`, `had_trace_view`, `live_trace_items` |
| `render_message_list` | `chat_view/mod.rs` | `total`, `visible`, `is_awaiting`, `thinking_visible` |
| `render_message` | [`message_component.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/chatty/views/message_component.rs) | `index`, `role`, `is_streaming`, `is_markdown`, `should_interleave`, `content_len`, `trace_items`, `attachments` |
| `interleave_segment` | `message_component.rs` (`render_interleaved_content`) | one per tool call: `tool_idx`, `tool_name`, `text_before_len`, `last_text_end`, `will_render_segment`; then a final `kind=remaining` with `remaining_after_tools_len` |
| `finalize_assistant_message` | `chat_view/mod.rs` | `had_live_trace`, `cleared_streaming_cache`, `content_len` |

What to look for:

- **Whitespace during streaming.** A `render_message_list` transition from `visible=1 … is_awaiting=true thinking_visible=true` to `visible=2 … is_awaiting=false`; the frame between them is the jump. With tools involved, `render_message` showing `content_len=0 trace_items>0` followed by a large `content_len` is the reflow from "trace only" to "text + trace".
- **Overlapping or duplicated text.** Across the `interleave_segment` events of one message, `text_before_len` must be non-decreasing and each `last_text_end` must equal the previous `text_before_len`. If not, two segments render overlapping byte ranges.

Use the existing target convention when adding events (`target: "chatty_gpui::render::<area>"`), structured fields rather than formatted strings, `trace!` for per-render events, `debug!` for one-or-two-per-turn transitions, `warn!` only for states that should not happen.

### 4. Stuck or stalled streams

Every stream goes through `StreamManager`; the loop itself is `run_stream_loop` in [`stream_processor.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/services/stream_processor.rs) and is shared with the TUI, so a lifecycle bug is a core bug.

- **Cancellation** is a flag (`Arc<AtomicBool>`) checked at the top of every iteration, not a task drop. A "Stop" that does nothing means the flag never reached the loop, or a stale `StreamEnded` from a superseded stream (each registration carries an epoch; older epochs are ignored).
- **The stall watchdog** wakes every `STALL_TICK` (5 s). After `STALL_TIMEOUT` (180 s) with no chunk and no sub-agent progress it logs `Stream produced nothing for too long; ending the turn as stalled` at `warn` and ends the turn with the error text `The model stopped responding (no output for 3 minutes)…`. A long tool call is silence from the stream's point of view, which is why the timeout is generous.
- The expected event order for each scenario is pinned by the characterization goldens; diff a failing run against them ([Test](./test.md)).

### 5. A tool "failed" with no message

If the transcript or the model sees the literal text `the tool failed`, the tool's `map_error` bypassed `map_tool_error`. Every `impl Tool` must route errors through `crate::tools::map_tool_error(Self::NAME, error)`, which writes `Error: {tool_name}: {message}`. That prefix is also the only failure signal the stream carries — `llm_service::tool_result_looks_like_error` sniffs it — so a tool that drops it is filed as a success and its error prose lands in `output`. See [Your first change](../start/first-change.md).

### 6. Prompt caching that does not hit

Each provider request logs one line:

```text
LLM completion call usage turn=2 input=412 cache_read=18220 cache_write=0 output=96 hit_rate=98
```

`input` is the uncached share of the prompt, so `input + cache_read + cache_write` is the whole prompt regardless of provider convention. A `hit_rate` that stays low from the second request onwards means the prompt prefix is changing between requests — the preamble, the tool block, or the message history is not byte-stable. Never sum calls first and divide later; the rate is per request. Background in [Token budget](../architecture/token-tracking.md) and the token-usage section of [Contributing patterns](../contributing-patterns.md).

### 7. Browser tool output

`browser_console` and `browser_network` return a short summary plus a `dump_path`. The full capture is written to `<workspace>/.chatty/browser/console-<uuid>.log` or `network-<uuid>.log` only when there is something to write; grep the dump when the summary says entries were elided. When a browser tool call appears in the transcript, the desktop docks a live screencast panel next to it, so you can watch the agent drive the page.

### 8. Reading a `system_trace`

Assistant messages persist a `system_trace` JSON column alongside the text in `<data dir>/chatty/conversations.db` (SQLite; same data directory as the TUI log above). Its shape is `SystemTrace` from [`models/message_types.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/models/message_types.rs):

```json
{
  "items": [
    { "Thinking":  { "content": "…", "summary": "…", "duration": null, "state": "Completed" } },
    { "ToolCall":  { "tool_name": "read_file", "…": "…" } },
    { "ApprovalPrompt": { "…": "…" } },
    { "ClarificationPrompt": { "…": "…" } }
  ],
  "total_duration": null,
  "active_tool_index": null
}
```

The transcript renders these items; the ATIF and JSONL exporters read them. The tool-call and tool-result *messages* rig recorded for the turn are persisted as separate rows in front of the final text entry (they carry no trace), so when you count or export turns skip them with `services::is_tool_message` and count exchanges with `services::exchange_count`.

## Verify

You can name the log line, overlay row or file that shows the bug, and the fix makes that evidence change.

## Checklist

- [ ] Reproduced with `RUST_LOG` narrowed to the relevant target
- [ ] Rendering bugs: overlay and render trace captured for the same run
- [ ] Stream bugs: checked the cancel flag, the epoch and the watchdog line before touching the loop
- [ ] Tool failures: confirmed the `Error: <tool>:` prefix is present

## Common issues

| Symptom | Check |
|---------|-------|
| Stream never ends | StreamManager status and cancel flag; watchdog `warn` after 180 s of silence |
| Text duplicated | Conversation model vs `ChatView` state; `interleave_segment` accounting |
| Tool UI stuck | The `ToolCallStarted` and tool-result `StreamManagerEvent`s reached the view for the active conversation |
| Whitespace or overlap | `render_message_list` transitions; markdown cache cleared on finalize |
| "the tool failed" | Tool bypassed `map_tool_error` |
| Cache hit rate low | Prompt prefix changes between requests |
| No TUI log file | You are in `--headless` or `--pipe` mode; logging is off there |
