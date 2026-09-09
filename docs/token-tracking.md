# Token tracking & context window display

**When to read this:** You need to know how the footer's context-window estimate is
computed, when it is refreshed, and how the provider's real counts get folded in.

Chatty estimates token usage before each LLM call and updates the estimate with the
provider's real counts after the response. What happens when the estimate crosses the
budget is described in [context-compaction.md](context-compaction.md).

## Architecture

```
send_message() / handle_regeneration()
  └─ run_llm_stream()
       ├─ gather_snapshot_inputs()        # GPUI thread — reads globals, warms cache
       │    └─ tokio::spawn_blocking      # background — BPE-counts history + user message
       │         └─ GlobalTokenBudget::publish()   # writes to watch::Sender
       │              └─ main.rs watcher task      # detects channel change
       │                   └─ cx.refresh_windows() # triggers re-render
       │                        └─ TokenContextBarView reads receiver.borrow()
       │
       └─ stream_prompt()                 # starts in parallel with token counting
            └─ finalize_completed_stream()
                 └─ GlobalTokenBudget::update_with_actuals()  # patches snapshot with real counts
```

Core types live in `crates/chatty-core/src/token_budget/` (`snapshot.rs`,
`counter.rs`, `cache.rs`, `summarizer.rs`); the GPUI glue is
`crates/chatty-gpui/src/chatty/token_budget/manager.rs` and the footer view is
`crates/chatty-gpui/src/chatty/views/footer/token_context_bar_view.rs`.

## Data flow: estimated snapshot

### 1. `gather_snapshot_inputs()` — GPUI thread, synchronous

Reads from globals before handing off to the background thread:

- Active conversation's `model_identifier`, `max_context_window`, `preamble`
- `response_reserve` from `TokenTrackingSettings` (default 4096)
- Tool count from `ExecutionSettingsModel` + enabled MCP servers (`build_tool_hint()`)
- Warms `GlobalTokenBudget::cache` for preamble and tool tokens (hash-checked; BPE only
  when content changes)

Returns `None` (skips counting) if `max_context_window` is not configured for the model.

### 2. `compute_snapshot_background()` — `tokio::spawn_blocking`

Runs BPE token counting off the UI thread:

- **Preamble** — counted via BPE if the cache is cold; reused if the hash matches
- **Tool definitions** — `counter.estimate_tool_tokens(tool_count)`, a per-schema
  estimate rather than a count of the real schemas
- **Conversation history** — `count_history()`: a BPE count of the text content of
  every `rig_core::completion::Message` entry, with non-text parts charged a fixed
  per-item estimate (see "Known limitations"); counted fresh every turn
- **Latest user message** — plain text from `UserContent::Text` variants
  (`extract_user_message_text()`); images and PDFs are skipped

Publishes the completed `TokenBudgetSnapshot` to `GlobalTokenBudget::sender`.

### 3. Watch channel → window refresh

A task spawned in `main.rs` loops on `receiver.changed().await` and calls
`cx.refresh_windows()` whenever a new snapshot arrives, bridging the tokio channel
into GPUI's render cycle so `TokenContextBarView` (a `RenderOnce` element) re-renders
with fresh data.

## Data flow: actual counts

After the stream completes, `finalize_completed_stream()` takes the provider's real
counts from `StreamEnded { token_usage: Option<TokenUsage>, .. }` and calls:

```rust
cx.global::<GlobalTokenBudget>().update_with_actuals(input_tokens, output_tokens);
```

`update_with_actuals()` uses `watch::Sender::send_modify` to patch the existing
snapshot in place, setting `actual_input_tokens` and `actual_output_tokens`. The
watcher sees the change and triggers another re-render, so the popover shows actual
counts next to the estimate.

## Research connection (GEPA / ACE)

Context headroom and token cost feed optimizer economics ([cost model](research/cost-model.md))
and ACE playbook growth bounds. See [app ↔ research bridge](research/app-research-bridge.md#context-window--token-budget).

## `TokenBudgetSnapshot`

```rust
pub struct TokenBudgetSnapshot {
    pub computed_at: Instant,
    pub model_context_limit: usize,          // from ModelConfig.max_context_window
    pub response_reserve: usize,             // from TokenTrackingSettings
    pub preamble_tokens: usize,              // BPE or cache
    pub tool_definitions_tokens: usize,      // estimated from tool count
    pub conversation_history_tokens: usize,  // BPE of history *before* current message
    pub latest_user_message_tokens: usize,   // BPE of current user text only
    pub actual_input_tokens: Option<usize>,  // set after stream ends
    pub actual_output_tokens: Option<usize>,
    pub conversation_id: String,
}
```

Key derived values:

| Method | Formula |
|:-------|:--------|
| `effective_budget()` | `model_context_limit - response_reserve` |
| `estimated_total()` | `preamble + tools + history + user_msg` |
| `remaining()` | `effective_budget - estimated_total` |
| `utilization()` | `estimated_total / effective_budget` (clamped 0–1) |
| `estimation_delta()` | `actual_input - estimated_total` (signed; `Some` only when actuals present) |

`check_pressure(snapshot, settings)` turns a snapshot into a `ContextPressureEvent`
(`HighPressure` at `high_threshold`, `CriticalPressure` at `critical_threshold`).

## `TokenCounter` — accuracy notes

Uses tiktoken-rs static BPE instances (initialised once globally, ~50 ms first call):

| Encoding | Models |
|:---------|:-------|
| `o200k_base` | `gpt-4o*`, `o1*`, `o3*`, `o4*` |
| `cl100k_base` | Everything else (Claude, Gemini, Mistral, Ollama, GPT-4) |

Accuracy by provider:
- **OpenAI cl100k/o200k families** — exact
- **Claude** — ±5% (cl100k approximation)
- **Gemini** — ±10–15% (SentencePiece differs significantly; always labelled `~`)
- **Mistral / Ollama** — ±5–10%

**Known limitations:**
- The *latest user message* component skips images and PDFs entirely —
  `extract_user_message_text()` only concatenates `UserContent::Text` variants.
  *History* counting differs: `TokenCounter::count_message()` walks each message's
  content parts and BPE-counts the text (plain text, tool-result text, tool-call
  name and arguments), substituting a fixed `NON_TEXT_CONTENT_TOKENS` estimate
  (1000) for each image, document, audio or video part and each non-text tool-result
  item. Before AGE-229 it JSON-serialised and fully BPE-tokenized the base64
  payload, which was slow and produced a number unrelated to what a provider
  actually bills (images are priced per pixel or tile). Either way, a conversation
  with many large images is only ever a rough estimate.
- History is counted *before* the new user message is appended to the conversation
  model, so the snapshot reflects the state at send time.
- Tool results in history (e.g. web fetch responses) *are* counted, via
  `count_message()`'s per-content-part walk rather than a full-message JSON
  serialisation.
- `count_history()` still re-tokenizes every entry on every call.
  `chatty_core::token_budget::cache::HistoryTokenCache` caches each entry's count
  keyed by its index and a content hash, so a caller that holds one across turns
  recounts only the entries that changed. As of AGE-229 it exists and is unit-tested
  in chatty-core, but `compute_snapshot_background()` in the desktop manager does not
  yet hold one, so `GlobalTokenBudget` still does a full recount on every send.

## `CachedTokenCounts`

Preamble and tool tokens rarely change between turns. The cache stores the last BPE
count and invalidates on content-hash mismatch:

```
cache.preamble_tokens(&preamble_str, &counter)
    → hash(preamble) == stored_hash? return cached_count : recount + store
```

Tool tokens use a compact hint string encoding the tool configuration
(`build_tool_hint()`), hashed the same way.

## `TokenTrackingSettings` global

```rust
pub struct TokenTrackingSettings {
    pub enabled: bool,                          // show bar (default: true)
    pub response_reserve: usize,                // output headroom (default: 4096)
    pub high_threshold: f64,                    // amber at 0.70
    pub critical_threshold: f64,                // red at 0.90
    pub auto_summarize: bool,                   // auto-summarize at critical (default: false)
    pub summarization_model_id: Option<String>, // override model for summarization
}
```

> [!NOTE]
> `TokenTrackingSettings` is **not persisted**. The struct derives `Serialize` /
> `Deserialize`, but no repository reads or writes it and no settings page edits it;
> `main.rs` installs `TokenTrackingSettings::default()` at startup and that is the value
> the app runs with.

**Read by:**
- `gather_snapshot_inputs()` — `response_reserve`
- `check_pressure()` — `high_threshold`, `critical_threshold`
- `finalize_completed_stream()` — `auto_summarize`
- `TokenContextBarView` — `should_show_bar()` (hides the bar when `enabled = false`)

## Footer bar

The bar shows one segment per snapshot component — preamble, tools, history, latest
message, remaining — each sized as its share of `effective_budget()`. The border
switches to the warning colour at `high_threshold` and the critical colour at
`critical_threshold`.

The popover lists the estimate and utilisation, one row per component, the provider's
actual counts with the signed estimation delta once the stream has ended, and session
totals (cumulative input/output/cache tokens and cost from `ConversationTokenUsage`,
which is separate from the snapshot). Manual summarisation is the `/compact` slash
command, not a button in the bar.

**Stale snapshot guard:** `read_budget_snapshot()` checks
`snap.conversation_id == active_conversation_id`. On conversation switch,
`load_conversation()` calls `GlobalTokenBudget::clear()` (publishes `None`), so the bar
shows an empty state until a snapshot for the new conversation arrives.
