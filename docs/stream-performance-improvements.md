# Stream Flow Performance Improvements

This document records stream-path improvements identified from the message-flow analysis,
why they help, and the expected impact. The first improvement has been implemented in this
change; the remaining items are deliberately scoped as follow-up opportunities.

## Implemented: One shared stream loop for GPUI and TUI

### Problem

Before this change, the TUI used `chatty_core::services::run_stream_loop()` while the GPUI
frontend carried a separate inline `tokio::select!` loop in
`crates/chatty-gpui/src/chatty/controllers/app_controller/message_ops.rs`.

That duplicated loop was responsible for:

- checking the cancellation `Arc<AtomicBool>`;
- polling the LLM `ResponseStream`;
- prioritizing `invoke_agent` progress events;
- forwarding chunks to frontend-specific state;
- draining late sub-agent progress after the stream ended.

The duplicated logic was not just a maintenance issue. Stream loops are latency-sensitive:
small inconsistencies in cancellation checks, progress draining, or break conditions can
produce dropped progress events, extra work after cancellation, or divergent behavior between
frontends.

### Change

The GPUI frontend now implements a `GpuiStreamHandler` adapter for the existing
`StreamChunkHandler` trait. `run_llm_stream()` delegates polling/cancellation/progress
draining to `chatty_core::services::run_stream_loop()` and keeps only GPUI-specific effects
inside the handler:

- text chunks append to `ConversationsStore.streaming_message`;
- all chunks forward to `StreamManager`;
- sub-agent progress updates `ConversationsStore` and the active `ChatView`;
- Azure 401/Unauthorized detection is recorded in the handler and refreshed after the shared
  loop returns.

The shared loop also drains any progress events that arrive immediately before a break, so
frontends no longer need bespoke post-loop drain code.

### Rust-specific benefits

- **Monomorphized handler dispatch:** `run_stream_loop()` accepts `&mut impl StreamChunkHandler`,
  so the GPUI and TUI handlers are statically dispatched. There is no `dyn Trait` allocation
  or virtual call overhead in the hot loop.
- **Borrowed frontend state:** `GpuiStreamHandler` borrows `&mut AsyncApp` for the lifetime of
  the loop instead of wrapping UI state in extra `Arc<Mutex<_>>` layers.
- **Single cancellation branch:** cancellation is checked in exactly one shared loop using
  `AtomicBool::load(Ordering::Relaxed)`, which is appropriate because the flag is a simple
  cross-task stop signal and does not guard memory invariants.
- **Fewer duplicated clones and branches:** the old inline loop had separate per-chunk and
  post-chunk `match` blocks. The handler now classifies the chunk once and forwards it once.

### Estimated impact

| Area | Estimate | Notes |
|------|----------|-------|
| Runtime throughput | Low but positive | The LLM/network dominates, but removing duplicate branch structure and post-loop bespoke logic reduces CPU work per chunk slightly. |
| UI latency consistency | Medium | Progress events and cancellation now follow one shared path across frontends. |
| Memory usage | Low | No new heap-backed abstraction; handler is stack-allocated and statically dispatched. |
| Maintenance cost | High | Stream-loop behavior now lives in one place, reducing future bug-fix duplication. |

## Follow-up opportunity: route sub-agent progress through StreamManager

Sub-agent progress still updates `ConversationsStore` and `ChatView` directly from
`GpuiStreamHandler`. That preserves current behavior, but it means sub-agent progress is not
part of the typed `StreamManagerEvent` event stream.

An elegant next step would add:

- `StreamManagerEvent::SubAgentStarted`;
- `StreamManagerEvent::SubAgentText`;
- `StreamManagerEvent::SubAgentFinished`.

Then the stream handler would forward progress to StreamManager exactly like LLM chunks. This
would make background conversation restoration more uniform and reduce direct `ChatView`
coupling in the streaming path.

**Estimated impact:** medium correctness/architecture improvement, low raw speed impact.

## Follow-up opportunity: typed stream keys instead of `"__pending__"`

`StreamManager` still uses the magic string `"__pending__"` to represent streams that have
started before a conversation ID exists. A typed enum would make this safer:

```rust
enum StreamKey {
    Pending,
    Conversation(String),
}
```

This would remove repeated string comparisons and make invalid states harder to express.

**Estimated impact:** low speed impact, medium correctness/readability impact.

## Follow-up opportunity: eliminate dual trace ownership

The final trace is still extracted from `ChatView` first and falls back to
`ConversationsStore.streaming_trace` when the user switched conversations. Making
`ConversationsStore.streaming_trace` the single in-progress trace owner would reduce memory
duplication and remove one synchronization point.

**Estimated impact:** low-to-medium memory improvement for trace-heavy streams, medium
architecture improvement.
