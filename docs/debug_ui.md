# Debugging chat-view rendering

This guide focuses on diagnosing the two rendering bugs that have been
hardest to reproduce in `crates/chatty-gpui/src/chatty/views/chat_view/`:

1. **Excessive vertical whitespace during streaming** — large empty gaps
   appear inside an assistant message while tool traces are streaming
   in, then collapse once the stream finalizes.
2. **Overlapping content** — two siblings (e.g. a table from the
   previous message and a heading from the next) visibly draw on top of
   each other for one or more frames.

Both bugs are intermittent and tend to require a specific tool-call /
streaming sequence to trigger. The instrumentation below is designed
so that when you do reproduce them you can capture enough state to
narrow down which render path is at fault.

---

## TL;DR — capture a trace next time it happens

```bash
RUST_LOG="info,chatty_gpui::render=trace" \
CHATTY_DEBUG_UI=1 \
  cargo run -p chatty-gpui 2>&1 | tee /tmp/chatty-render.log
```

Then reproduce the issue and grep:

```bash
grep -E "render_message_list|finalize_assistant_message|append_assistant_text|render_interleaved|skeleton_visible" /tmp/chatty-render.log
```

The overlay in the top-right of the window (enabled by
`CHATTY_DEBUG_UI=1`) shows the same state live, with one row per
visible message.

---

## What `chatty_gpui::render` traces

All render-path logging routes through the `chatty_gpui::render` tracing
target (and submodules: `chatty_gpui::render::list`,
`chatty_gpui::render::message`, `chatty_gpui::render::stream`). Filter
to just those events with:

```bash
RUST_LOG="warn,chatty_gpui::render=trace"
```

Key events, in the order they fire during a streaming turn:

| Event (in `target = "chatty_gpui::render::*"`) | Fired from | Useful fields |
|---|---|---|
| `start_assistant_message` | `chat_view/mod.rs::start_assistant_message` | `total_messages` |
| `append_assistant_text` | `chat_view/mod.rs::append_assistant_text` | `delta_len`, `new_content_len`, `last_msg_streaming` |
| `tool_call_started` | `chat_view/handlers.rs::handle_tool_call_started` | `tool_id`, `tool_name`, `had_trace_view`, `live_trace_items` |
| `render_message_list` | `chat_view/mod.rs::render_message_list` | `total`, `visible`, `filtered`, `is_awaiting`, `skeleton_visible` |
| `render_message` | `message_component.rs::render_message` | `index`, `role`, `is_streaming`, `is_markdown`, `should_interleave`, `content_len`, `trace_items` |
| `interleave_segment` | `message_component.rs::render_interleaved_content` | `tool_idx`, `tool_name`, `text_before_len`, `last_text_end`, `remaining_after_tools_len` |
| `finalize_assistant_message` | `chat_view/mod.rs::finalize_assistant_message` | `had_live_trace`, `cleared_streaming_cache` |

Each event includes the conversation id where relevant, so you can
filter to a single conversation:

```bash
grep '"conversation_id":"abc123"' /tmp/chatty-render.log
```

---

## The `CHATTY_DEBUG_UI` overlay

Set `CHATTY_DEBUG_UI=1` (any non-empty value works) before launching the
app to enable a small monospace overlay in the top-right of every chat
window. It lists, for each visible message:

```
ChatView debug
  msgs: 4 visible / 5 total   awaiting: false   skeleton: false
  [0] User       n=23
  [1] Assistant  s=1 m=1 ti=2 c=148  trace=open
  [2] Sub-agent  s=0 c=812
  ...
```

Legend:

- `s=1` — `is_streaming`
- `m=1` — `is_markdown`
- `ti=N` — number of items in `live_trace` (or `system_trace_view`)
- `c=N` — `content.len()`
- `trace=open|closed|none` — `SystemTraceView.is_collapsed`
- `filtered` (in the header) — count of messages excluded by the
  empty-streaming-message filter at `chat_view/mod.rs::render_message_list`

If you see `skeleton: true` simultaneously with `s=1 c=0` for the last
assistant message, that is the *expected* "awaiting first token" state
described in bug #1 below — the skeleton and the message are mutually
exclusive but the layout jump between the two is the perceived
"whitespace".

The overlay only renders when the env var is set; it is otherwise
zero-cost.

---

## Bug #1: excessive whitespace during streaming

### Where it happens

`crates/chatty-gpui/src/chatty/views/chat_view/mod.rs` lines ~595–740.
The same predicate appears in two places:

```rust
// is_awaiting_response — fires skeleton when true
msg.is_streaming
    && msg.content.is_empty()
    && !msg.live_trace.as_ref().is_some_and(|t| t.has_items())

// visible_messages filter — *excludes* the message when the same predicate is true
```

So while we're waiting for the very first token (or first trace item),
the empty streaming message is **hidden** and the loading skeleton is
shown instead. The skeleton is ~80px tall
(`crates/chatty-gpui/src/chatty/views/chat_view/start_screen.rs::render_loading_skeleton`).
When the first token arrives the message becomes visible and the
skeleton disappears in the same frame, which on slow streams reads as
a "block of whitespace that collapses".

When tool traces are involved the symptom is worse, because the trace
adds items *before* any assistant text — the message becomes visible
(filter passes) but `content.is_empty()` is still true, and on the next
text chunk the message has to relayout from "trace box only" to "text +
trace box", which can shift a lot of vertical space.

### How to confirm with the new traces

Look for a transition like this in the log:

```
chatty_gpui::render::list  render_message_list total=2 visible=1 filtered=1 is_awaiting=true skeleton_visible=true
chatty_gpui::render::list  render_message_list total=2 visible=2 filtered=0 is_awaiting=false skeleton_visible=false
```

The frame in between those two events is where the visible jump
happens.

For the tool-trace variant, look for:

```
chatty_gpui::render::message render_message index=1 is_streaming=true content_len=0 trace_items=2 should_interleave=true
chatty_gpui::render::message render_message index=1 is_streaming=true content_len=37 trace_items=2 should_interleave=true
```

If `content_len` jumps from 0 to a large value in one frame *and*
`trace_items` is non-zero, the renderer is reflowing trace items
relative to the new text — that reflow is what produces the apparent
whitespace.

### Likely fixes (not applied — needs reproduction)

- Render an inline placeholder for the empty streaming assistant
  message *instead* of the separate floating skeleton, so the layout
  doesn't have to swap between "skeleton block" and "message block".
- Stop wrapping streaming markdown elements in an extra `flex_col`
  div (`message_component.rs` lines ~338–347 and ~729–742) once we
  confirm GPUI no longer needs the original workaround.

---

## Bug #2: overlapping content

### Where it happens

The most likely culprits, in priority order:

1. **Streaming → finalized layout switch.**
   In `message_component.rs::render_message`, streaming markdown wraps
   children in `div().flex().flex_col().w_full()` (line ~729); the
   finalized path returns elements *directly* as `container.children(...)`
   on a container that is **not** explicitly flex_col. The container
   is created at line ~684:

   ```rust
   let mut container = div().max_w(relative(1.)).p_3().rounded_lg();
   ```

   The transition from "wrapped flex_col" to "unwrapped block" on
   finalize changes the layout context and has been observed to leave
   stale geometry for one frame.

   Mitigation applied in this commit: the container is now created
   with `.flex().flex_col().w_full()` so the layout context is the
   same on both sides of the transition. The streaming-only wrapper
   is left in place as a belt-and-braces guard.

2. **`render_interleaved_content` text-segment accounting.**
   `last_text_end` is advanced by `tool_call.text_before.len()`, which
   is frozen at the moment the tool call started. If the model emits
   text → tool → more text → tool, the second tool's `text_before`
   *includes* the first segment. With the new
   `chatty_gpui::render::message::interleave_segment` events you can
   verify that `text_before_len` is monotonically non-decreasing across
   tool calls and that `last_text_end == previous text_before_len`. If
   not, two segments are being rendered in overlapping byte ranges
   (i.e., the same text twice) which can look like overlap.

3. **Scroll container.** The scrollable region is
   `div().overflow_scroll().size_full()` wrapping
   `div().p_4().flex().flex_col().gap_4()`. GPUI requires `min_h_0`
   on flex children for scroll to compute heights correctly; that
   *is* set on the outer wrapper. If you see overlap only after
   resizing the window, suspect this.

### How to confirm with the new traces

```
grep "interleave_segment\|render_message " /tmp/chatty-render.log
```

For each rendered assistant message, you should see one
`render_message` event followed by N `interleave_segment` events (one
per tool call) and then a final `interleave_segment` with
`remaining_after_tools_len=...`. If two `interleave_segment` events
for the same `tool_idx` report different `text_before_len` between
frames, that is a smoking gun for the accounting bug.

---

## Reproducing the bugs reliably

What has worked in past investigations:

1. Pick a conversation that uses `pdf_to_image` or `daytona_run` —
   these tools dump multi-page output that combines with markdown
   tables, which is what most reports involve.
2. Set the streaming chunk size as small as possible (the screenshot
   reports come from network-throttled or slow-provider sessions).
3. Use a model whose first response is a markdown table — tables have
   intrinsic-width children which exacerbate layout misses.
4. Toggle `CHATTY_DEBUG_UI=1` on and record a short screen capture
   alongside `/tmp/chatty-render.log`; correlate the frame number with
   the timestamps in the log.

---

## Code map — where to look first

| Symptom in the screenshot | First file to read |
|---|---|
| Skeleton flashes before first token | `chat_view/mod.rs::is_awaiting_response`, `start_screen.rs::render_loading_skeleton` |
| Whitespace grows as tool calls arrive | `message_component.rs::render_interleaved_content` |
| Tool-call card overlapping with text | `message_component.rs::render_message` (the streaming/finalized branch around line ~714 and ~779) |
| Whole messages overlap (table on top of next heading) | `chat_view/mod.rs::render_message_list` (scroll container + flex_col children) |
| Sub-agent progress block overlaps with main response | `chat_view/sub_agent.rs::finalize_sub_agent_progress` |

---

## Adding more tracing

If you need a new event:

- Use the existing target convention: `target: "chatty_gpui::render::<area>"`
  (`list`, `message`, `stream`, `handler`).
- Prefer structured fields (`field = %value`) over string formatting
  so `grep` and the env-filter work cleanly.
- Use `trace!` for per-render events, `debug!` for state transitions
  that fire 1–2× per turn, `warn!` only for genuinely unexpected
  states.

The default subscriber (in `crates/chatty-gpui/src/main.rs`) already
respects `RUST_LOG`, so no setup changes are needed to enable new
events.
