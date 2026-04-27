# Token Tracking & Context Window Display

Chatty estimates token usage before each LLM call and updates with real counts after the response.

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

## File Layout

| File | Purpose |
|:-----|:--------|
| `crates/chatty-core/src/token_budget/snapshot.rs` | `TokenBudgetSnapshot`, `ComponentFractions`, `ContextStatus`, `ContextPressureEvent` |
| `crates/chatty-core/src/token_budget/counter.rs` | `TokenCounter` — tiktoken-rs BPE wrapper, provider-aware |
| `crates/chatty-core/src/token_budget/cache.rs` | `CachedTokenCounts` — hash-keyed cache for preamble + tool tokens |
| `crates/chatty-gpui/src/chatty/token_budget/manager.rs` | `GlobalTokenBudget`, `gather_snapshot_inputs()`, `compute_snapshot_background()`, `check_pressure()` |
| `crates/chatty-core/src/token_budget/summarizer.rs` | Stub — future conversation summarization |
| `crates/chatty-core/src/settings/models/token_tracking_settings.rs` | `TokenTrackingSettings` GPUI global |
| `crates/chatty-gpui/src/chatty/views/footer/token_context_bar_view.rs` | Footer bar and popover UI |

## Data Flow: Estimated Snapshot

### 1. `gather_snapshot_inputs()` — GPUI thread, synchronous

Reads from globals before handing off to the background thread:

- Active conversation's `model_identifier`, `max_context_window`, `preamble`
- `response_reserve` from `TokenTrackingSettings` (default 4096)
- Tool count from `ExecutionSettingsModel` + enabled MCP servers
- Warms `GlobalTokenBudget::cache` for preamble and tool tokens (hash-checked; BPE only when content changes)

Returns `None` (skips counting) if `max_context_window` is not configured for the model.

### 2. `compute_snapshot_background()` — `tokio::spawn_blocking`

Runs BPE token counting off the UI thread:

- **Preamble** — counted via BPE if cache cold; reused if hash matches
- **Tool definitions** — estimated as `tool_count × tokens_per_sample_schema` (BPE-counted once on a representative schema)
- **Conversation history** — full BPE count of all `rig::completion::Message` entries serialised to JSON; counted fresh every turn
- **Latest user message** — plain text extracted from `UserContent::Text` variants; images/PDFs skipped

Publishes the completed `TokenBudgetSnapshot` to `GlobalTokenBudget::sender`.

### 3. Watch channel → window refresh

A background task spawned in `main.rs` loops on `receiver.changed().await` and calls `cx.refresh_windows()` whenever a new snapshot arrives. This bridges the tokio channel into GPUI's render cycle so `TokenContextBarView` (a `RenderOnce` element) re-renders with the fresh data.

## Data Flow: Actual Counts

After the LLM stream completes, `finalize_completed_stream()` receives the provider's real token counts from `StreamEnded { token_usage, api_turn_count }` and calls:

```rust
cx.global::<GlobalTokenBudget>().update_with_actuals(input_tokens, output_tokens);
```

`update_with_actuals()` uses `watch::Sender::send_modify` to atomically patch the existing snapshot in-place, setting `actual_input_tokens` and `actual_output_tokens`. The watcher detects this change and triggers another re-render, showing actual counts in the popover alongside the estimates.

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

## `TokenCounter` — Accuracy Notes

Uses tiktoken-rs static BPE instances (initialised once globally, ~50 ms first call):

| Encoding | Models |
|:---------|:-------|
| `o200k_base` | `gpt-4o*`, `o1-*`, `o3-*`, `o4-*` |
| `cl100k_base` | Everything else (Claude, Gemini, Mistral, Ollama, GPT-4) |

Accuracy by provider:
- **OpenAI cl100k/o200k families** — exact
- **Claude** — ±5% (cl100k approximation)
- **Gemini** — ±10–15% (SentencePiece differs significantly; always labelled `~`)
- **Mistral / Ollama** — ±5–10%

**Known limitations:**
- Images and PDFs are not counted — `extract_user_message_text()` skips non-text `UserContent` variants. Conversations with many large images will be significantly under-estimated (Gemini in particular counts image tiles separately and can add hundreds of thousands of tokens).
- History is counted *before* the new user message is added to the conversation model, so the snapshot reflects the state at send time, not after the user message has been appended.
- Tool call results (e.g. web fetch responses) that appear in history *are* counted via `count_history()` (serialised to JSON).

## `CachedTokenCounts`

Preamble and tool tokens rarely change between turns. The cache stores the last BPE count and invalidates on content hash mismatch:

```
cache.preamble_tokens(&preamble_str, &counter)
    → hash(preamble) == stored_hash? return cached_count : recount + store
```

Tool tokens use a compact hint string encoding the tool configuration (`build_tool_hint()`), hashed the same way.

## `TokenTrackingSettings` Global

```rust
pub struct TokenTrackingSettings {
    pub enabled: bool,                          // show bar (default: true)
    pub response_reserve: usize,                // output headroom (default: 4096)
    pub high_threshold: f64,                    // amber at 70%
    pub critical_threshold: f64,                // red at 90%
    pub auto_summarize: bool,                   // auto-summarize at critical (default: false)
    pub summarization_model_id: Option<String>, // override model for summarization
}
```

Not yet persisted to disk — defaults applied at startup via `cx.set_global(TokenTrackingSettings::default())`.

**Read by:**
- `gather_snapshot_inputs()` — `response_reserve`
- `check_pressure()` — `high_threshold`, `critical_threshold`
- `TokenContextBarView::read_budget_snapshot()` — `should_show_bar()` (hides bar if `enabled = false`)

## Footer Bar UI

### Stacked bar segments

Ordered left-to-right: **Preamble** (blue `#60A5FA`) → **Tools** (violet `#A78BFA`) → **History** (emerald `#34D399`) → **Latest message** (cyan `#22D3EE`) → **Remaining** (grey).

Each segment width is `fraction × bar_width` where the fraction is the component's share of `effective_budget`.

### Bar border colour

| Colour | Condition |
|:-------|:----------|
| Theme border | Normal / Moderate (< 70%) |
| Amber `#F59E0B` | `utilization >= high_threshold` (70%) |
| Red `#EF4444` | `utilization >= critical_threshold` (90%) |

### Popover

- **Summary line** — `~estimated_total / model_context_limit tokens · utilization%`
- **Component breakdown** — one legend row per segment with absolute token count and percentage
- **Actual (from provider)** — shown after stream ends; includes signed estimation delta
- **Session totals** — cumulative `input_tokens`, `output_tokens`, cost across all exchanges (sourced from `ConversationTokenUsage`, separate from the snapshot)

### Stale snapshot guard

`read_budget_snapshot()` checks `snap.conversation_id == active_conversation_id`. On conversation switch, `load_conversation()` calls `GlobalTokenBudget::clear()` (publishes `None`), so the bar shows an empty state until a fresh snapshot arrives for the new conversation.
