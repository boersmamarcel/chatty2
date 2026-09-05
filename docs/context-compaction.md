# Context compaction

How Chatty keeps a long agentic conversation inside the model's context
window without throwing the prompt cache away on every turn. This is the D2
decision recorded on AGE-233 and the plan carried by AGE-241 and its three
sub-issues: persist tool turns (AGE-247, done), the compaction engine
(AGE-248), and the compaction UX (AGE-249).

The diagrams are also checked in as editable Excalidraw scenes under
[`docs/diagrams/`](https://github.com/boersmamarcel/chatty2/tree/main/docs/diagrams);
open them at [excalidraw.com](https://excalidraw.com) to change them.

## The problem: a sliding window defeats the prompt cache

Until AGE-248 lands, both frontends run `shape_context`
(`crates/chatty-core/src/services/context_shaper.rs`) before every request.
Its thresholds are character counts, blind to the model's
`max_context_window`, and its only reachable stage keeps the first two and
the last N messages of *each request*. That tail is a sliding window: once
the history is long, the third message of every request is a different one
from the request before, so the prefix the previous request cached is never
hit again. This is exactly the regime, long tool-heavy sessions, where the
moving prompt-cache breakpoint (`prompt_cache_http.rs`, AGE-205) was meant
to pay off.

```mermaid
flowchart LR
    subgraph today["Today: context_shaper, a window per request"]
        direction TB
        subgraph n1["request N"]
            direction LR
            a1[m1] --- a2[m2] -.- a3[m3 dropped] -.- a4[m4 dropped] --- a5[m5] --- a6[m6] --- a7[m7] --- a8[m8]
        end
        subgraph n2["request N+1: the third message changed, prefix lost after m2"]
            direction LR
            b1[m1] --- b2[m2] -.- b3[m3 dropped] -.- b4[m4 dropped] -.- b5[m5 dropped] --- b6[m6] --- b7[m7] --- b8[m8] --- b9[m9 new]
        end
    end
    subgraph after["After AGE-248: one cut, kept until the trigger fires again"]
        direction TB
        subgraph g1["request N"]
            direction LR
            c0[summary] --- c5[m5] --- c6[m6] --- c7[m7] --- c8[m8]
        end
        subgraph g2["request N+1: identical prefix, only m9 is uncached"]
            direction LR
            d0[summary] --- d5[m5] --- d6[m6] --- d7[m7] --- d8[m8] --- d9[m9 new]
        end
    end
    style b6 fill:#ffc9c9,stroke:#ef4444
    style b9 fill:#b2f2bb,stroke:#22c55e
    style d9 fill:#b2f2bb,stroke:#22c55e
    style c0 fill:#d0bfff,stroke:#8b5cf6
    style d0 fill:#d0bfff,stroke:#8b5cf6
```

The fix has to be *generational*: decide a cut point when a token budget is
crossed, then keep that cut fixed until the next crossing, so consecutive
requests share a prefix. The character-based shaper goes away.

## Step 1: tool turns are persisted (AGE-247, done)

Compaction cuts at exchange boundaries, and an exchange only has boundaries
if the history holds what the model actually saw. Since AGE-247 a turn with
tool calls persists every message rig produced for it, in order: the
assistant tool-call message(s), the user tool-result message(s), then the
final assistant text. Payloads are kept whole at persist time (the D1
follow-up answer); bounding them is compaction's job, not persistence's.
`CLAUDE.md` > Architecture Notes > "Tool turns are persisted" has the code
map.

Two consequences matter here:

- An **exchange** is a user text message, every tool call/result pair after
  it, and the assistant text that answers. `services::exchange_count` counts
  them; readers that render or export skip the tool messages with
  `services::is_tool_message`.
- A cut that falls inside an exchange would split a tool call from its
  result, and OpenAI-compatible endpoints reject the orphan. So cuts fall
  on exchange boundaries only.

## Step 2: the engine (AGE-248)

Owner module: `crates/chatty-core/src/token_budget/`. It already owns
measurement (`TokenCounter`, `TokenBudgetSnapshot`); it gains the trigger
and the compaction itself. `services/context_shaper.rs` and its two call
sites (`message_ops_internals.rs`, `engine/mod.rs`) are deleted, together
with the `ContextShaperSettings` re-exports.

```mermaid
flowchart TD
    fin["Turn finishes<br/>finalize_completed_stream (desktop) / finalize_stream (terminal)"]
    trig{"prompt tokens ><br/>high_threshold × (max_context_window − response_reserve)?"}
    skip["next turn as is"]
    started["emit CompactionStarted { turns }"]
    cut["Cut point: keep the most recent 4 exchanges whole<br/>(user text → tool pairs → assistant text)"]
    sum["Summarise everything older<br/>utility model (AGE-227), honours summarization_model_id"]
    ratio{"result still > 40 % of the budget<br/>and fewer than 2 retries?"}
    fold["fold one more exchange into the summary"]
    persist["replace_history: summary first, then the kept tail<br/>next request shares every message but the new one"]
    finished["emit CompactionFinished { turns, tokens_before, tokens_after }"]
    ui["UI: status line while running,<br/>compaction card afterwards (AGE-249)"]

    fin --> trig
    trig -- "no, or no window configured" --> skip
    trig -- yes --> started --> cut --> sum --> ratio
    ratio -- yes --> fold --> sum
    ratio -- no --> persist --> finished --> ui
    style trig fill:#fff3bf,stroke:#f59e0b
    style ratio fill:#fff3bf,stroke:#f59e0b
    style started fill:#eebefa,stroke:#8b5cf6
    style finished fill:#eebefa,stroke:#8b5cf6
    style persist fill:#c3fae8,stroke:#06b6d4
```

### Trigger (Q1 a)

`compaction::should_compact(history_tokens, model_config, settings) -> bool`
fires when the estimated prompt tokens (preamble, tools, history and the
latest message: the existing snapshot on the desktop; the terminal counts
with the same `TokenCounter` since it has no snapshot) exceed
`high_threshold × (max_context_window − response_reserve)`. It never fires
for a model with no `max_context_window`.

### Shape (Q2 a)

Runs in the post-turn path of both frontends, once per trigger:

1. Keep the most recent 4 exchanges whole.
2. Summarise everything older with `AgentClient::prompt()`, which after
   AGE-227 is the tool-less utility model. When
   `summarization_model_id` is set, the utility model is built for that
   model instead (this implements the stub at
   `token_budget/summarizer.rs`).
3. If the result is still above 40 % of the effective budget, fold one
   more exchange into the summary and re-summarise, at most twice.
4. Persist through `Conversation::replace_history`, summary first, then
   the kept tail. Metadata (traces, attachments, timestamps, feedback) on
   the kept tail is carried over.

Prefix stability is the property to test: after a compaction, two
consecutive requests share every message except the new user message.

### Summary shape

A structured document, not prose, inserted as the first *user* message of
the retained history and prefixed with `[COMPACTED CONTEXT — N turns]` so
the renderer can show it as a compaction card:

```
[COMPACTED CONTEXT — 12 turns]
## Goal
## Decisions
## Files touched        (paths)
## Commands and outcomes
## Open items
## References           (URLs, PR and issue numbers, paths mentioned)
```

```mermaid
flowchart LR
    subgraph before["Before (tool turns persisted whole)"]
        direction TB
        subgraph old["older exchanges: summarised"]
            direction TB
            u1["user: set up the repo"] --> as1["assistant: Done, here is the layout"]
            as1 --> u2["user: add a CI workflow"] --> tc1["assistant: tool call write_file"] --> tr1["user: tool result written"] --> as2["assistant: CI added"]
        end
        subgraph keep["most recent 4 exchanges: kept whole"]
            direction TB
            u9["user: why does the test fail?"] --> tc9["assistant: tool call shell_execute"] --> tr9["user: tool result (test output)"] --> as9["assistant: an off-by-one in ..."]
            as9 --> more["... 3 more exchanges"]
        end
        old --> keep
    end
    subgraph afterc["After replace_history"]
        direction TB
        summ["user: [COMPACTED CONTEXT — N turns]<br/>## Goal · ## Decisions · ## Files touched<br/>## Commands and outcomes · ## Open items · ## References"]
        subgraph keep2["kept tail, byte-identical"]
            direction TB
            v9["user: why does the test fail?"] --> wc9["assistant: tool call shell_execute"] --> wr9["user: tool result (test output)"] --> vs9["assistant: an off-by-one in ..."]
            vs9 --> more2["... 3 more exchanges"]
        end
        summ --> keep2
    end
    before == compact ==> afterc
    style summ fill:#d0bfff,stroke:#8b5cf6
    style tc1 fill:#fff3bf,stroke:#f59e0b
    style tr1 fill:#fff3bf,stroke:#f59e0b
    style tc9 fill:#fff3bf,stroke:#f59e0b
    style tr9 fill:#fff3bf,stroke:#f59e0b
    style wc9 fill:#fff3bf,stroke:#f59e0b
    style wr9 fill:#fff3bf,stroke:#f59e0b
```

### Events

`CompactionStarted { turns }` and
`CompactionFinished { turns, tokens_before, tokens_after }` go through the
existing frontend event paths (`StreamManagerEvent` on the desktop,
`AppEvent` on the terminal) so step 3 can render progress. Until step 3
lands they are logged only.

### Acceptance for the engine

- Unit tests: trigger arithmetic; cut-point selection keeps call/result
  pairs intact; the ratio loop stops after two retries; prefix stability
  over two consecutive turns after a compaction.
- A scripted long conversation compacts once, then not again until the
  threshold is crossed again.
- With `summarization_model_id` set, the summariser request goes to that
  model (mock).
- Goldens: the turn callback sequences are unchanged; if the compaction
  events appear in the frontend goldens, the diff is explained.
- `docs/token-tracking.md` and `docs/message-path-debt.md` (A4, C5)
  updated.

## Step 3: the UX and the default (AGE-249)

- **Desktop.** While `CompactionStarted … CompactionFinished` is in flight,
  an animated status line in the transcript ("Compacting context, N turns
  …"), using the streaming status header the turn already has, so it reads
  as part of the turn rather than a modal. On finish, the
  `[COMPACTED CONTEXT — N turns]` message renders as a **compaction card**:
  collapsed by default ("N turns → summary, X k → Y k tokens"), expandable
  to the summary document. One new block kind in `views/transcript/`. The
  footer token bar shows the drop.
- **Terminal.** Status-bar spinner text during compaction; one system line
  on finish with the same numbers; the summary message renders as a dimmed
  block.
- **Default (Q4 a).** `TokenTrackingSettings.auto_summarize` defaults to
  `true`; the settings copy says what happens and at what threshold. The
  manual Summarize button and `/compact` stay.

## Guardrails

- Do not change the moving-breakpoint layer (`prompt_cache_http.rs`).
- Do not touch follow-up scheduling (D3, AGE-190).
- No compaction inside AGE-247; no engine behaviour change inside AGE-249.

## Until AGE-248 lands

The existing `/compact` command and the desktop's auto-summarise still call
`summarize_oldest_half`, which cuts at `history.len() / 2` regardless of
exchange boundaries. Now that tool turns are persisted, a manual compaction
of a tool-heavy chat can split a tool call from its result, and the provider
will reject the next request. The engine above replaces that path.

## Related

- [`token-tracking.md`](token-tracking.md): how the prompt-token estimate
  the trigger reads is computed.
- [`message-path-debt.md`](message-path-debt.md): findings A4 (the shaper)
  and C5 (two overlapping context systems) that this design resolves.
- `CLAUDE.md` > Architecture Notes: prompt caching (AGE-205) and persisted
  tool turns (AGE-247).
