# Token Tracking & Context Window Display

This document explains how Chatty tracks token usage and displays context window information in the footer bar.

## Data Flow

```
LLM API response
  → rig-core FinalResponse.usage()
    → StreamChunk::TokenUsage { input_tokens, output_tokens }
      → StreamManager.StreamState { token_usage, api_turn_count }
        → StreamManagerEvent::StreamEnded { token_usage, api_turn_count }
          → app_controller: TokenUsage::with_turn_count(input, output, turns)
            → Conversation.add_token_usage(usage)
              → TokenContextBarView reads it for display
```

### Key files

| File | Role |
|:-----|:-----|
| `src/chatty/models/token_usage.rs` | `TokenUsage` and `ConversationTokenUsage` structs, formatting helpers |
| `src/chatty/models/stream_manager.rs` | Tracks `api_turn_count` per stream, emits it in `StreamEnded` |
| `src/chatty/controllers/app_controller.rs` | Creates `TokenUsage`, calculates cost, stores in conversation |
| `src/chatty/views/footer/token_context_bar_view.rs` | Renders footer bar and popover |

## Token Usage Model

### TokenUsage (per exchange)

Each user→assistant exchange produces one `TokenUsage`:

```rust
pub struct TokenUsage {
    pub input_tokens: u32,       // Accumulated input tokens from rig-core
    pub output_tokens: u32,      // Output tokens generated
    pub estimated_cost_usd: Option<f64>,  // Cost if pricing is configured
    pub api_turn_count: u32,     // Number of LLM API calls in this exchange
}
```

### ConversationTokenUsage (per conversation)

Aggregates all exchanges in a conversation:

```rust
pub struct ConversationTokenUsage {
    pub message_usages: Vec<TokenUsage>,  // One per exchange
    pub total_input_tokens: u32,          // Sum across all exchanges
    pub total_output_tokens: u32,
    pub total_estimated_cost_usd: f64,
}
```

## rig-core Accumulation & Normalization

### The problem

rig-core accumulates `input_tokens` across all API turns within a single multi-turn exchange. When tool calls happen, the LLM is called multiple times:

- Turn 1: system + tools + user message → 5,000 input tokens
- Turn 2: same + assistant response + tool result → 6,000 input tokens
- Turn 3: same + more context → 7,000 input tokens

rig-core reports: **18,000** (the sum), not **7,000** (the actual context fill at the end).

A subsequent simple exchange (no tool calls) might report 8,000 — making the display appear to "reset" from 18K to 8K.

### The fix: api_turn_count

`StreamManager` tracks how many API turns occurred in each exchange by incrementing a counter on every `ToolCallResult` and `ToolCallError` chunk. This count is threaded through `StreamEnded` into `TokenUsage`.

`TokenUsage::estimated_context_tokens()` normalizes the value:

```rust
pub fn estimated_context_tokens(&self) -> u32 {
    self.input_tokens / self.api_turn_count.max(1)
}
```

This gives the average input tokens per turn — a better (though still approximate) measure of how full the context window actually is.

## Footer Bar Display

### Progress bar (always visible)

Shows `estimated_context_tokens / max_context_window` as a filled bar with percentage text.

Color thresholds:
- **Green** (`#22C55E`): < 60% full
- **Amber** (`#F59E0B`): 60–84% full
- **Red** (`#EF4444`): >= 85% full

`max_context_window` comes from `ModelConfig.max_context_window` in the settings for the active conversation's model.

### Popover (on click)

#### Summary line

`"16.3K / 111K tokens · 15%"` — human-readable current vs max with percentage.

#### System section (estimated)

These are **estimates** — the LLM API does not provide per-category breakdowns.

- **System Prompt** — Estimated tokens for the system prompt: `(preamble_chars + 1300) / 4`. The 1300 extra characters account for the tool summary (~500 chars) and formatting guide (~800 chars) appended by `agent_factory.rs`. The `/4` is a rough characters-to-tokens ratio.

- **Tool Definitions** — Estimated tokens for all tool JSON schemas: `tool_count × 300`. Tool count is computed from `ExecutionSettingsModel` by summing enabled tool groups:
  - 1 for `list_tools` (always present)
  - 11 if code execution enabled (shell: 4, git: 7)
  - 7 if workspace + filesystem read enabled (fs_read: 4, search: 3)
  - 5 if workspace + filesystem write enabled
  - 1 if fetch enabled
  - 1 for `add_attachment`
  - 4 for MCP management tools
  - 3 per enabled MCP server (rough estimate)

#### Conversation section (estimated)

- **Messages** — The remainder: `estimated_context_tokens - system_tokens - tool_tokens`, as a percentage of `max_context_window`. Represents conversation history (user messages + assistant responses). Floors at 0 via `saturating_sub`.

#### Session section (actual cumulative values)

These are **real values** summed across all exchanges in the conversation:

- **Input Tokens** — `ConversationTokenUsage.total_input_tokens`. Raw accumulated values from rig-core summed across all exchanges.

- **Output Tokens** — `ConversationTokenUsage.total_output_tokens`. Same for output.

- **Cost** (only shown if > $0) — Sum of per-exchange costs. Each exchange's cost is: `(input_tokens / 1M) × cost_per_million_input + (output_tokens / 1M) × cost_per_million_output`. Only calculated when the model has both `cost_per_million_input_tokens` and `cost_per_million_output_tokens` configured in settings. Displayed with adaptive precision (`$0.12`, `$0.003`, `$0.0001`, or `< $0.0001`).

## Token Recording

Token usage is always recorded regardless of whether pricing is configured. The flow in `finalize_completed_stream`:

1. Extract `(input_tokens, output_tokens)` and `api_turn_count` from the `StreamEnded` event
2. Create `TokenUsage::with_turn_count(input, output, turns)`
3. If model has pricing configured, call `usage.calculate_cost(...)`
4. Store via `conv.add_token_usage(usage)` — this updates both the per-exchange list and the cumulative totals

## Formatting Helpers

- `format_tokens(count)` — `500` → `"500"`, `16300` → `"16.3K"`, `1000000` → `"1M"`
- `format_cost(cost)` — `0.12` → `"$0.12"`, `0.003` → `"$0.003"`, `0.0` → `"$0.00"`
