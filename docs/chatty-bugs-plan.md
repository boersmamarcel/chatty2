# Chatty bugs — remediation plan

Plan for the Linear project
[**Chatty bugs**](https://linear.app/agents-research/project/chatty-bugs-ddd8cae74a17)
(team AGE). Covers the 10 open issues in the project as of 2026-09-03.

Every root cause below was re-verified against `main` at `a1d715e`; file/line
citations are from that commit. Where an issue's own diagnosis was confirmed the
plan says so and does not repeat it; where this pass found something the issue
does not mention, it is marked **New finding**.

---

## 1. Issue inventory

| Issue | Title (short) | Pri | Crate | Workstream |
|---|---|---|---|---|
| [AGE-151](https://linear.app/agents-research/issue/AGE-151) | `write_todos` nudge kills the stream, conversation stalls | High | gpui + core | **W1** |
| [AGE-188](https://linear.app/agents-research/issue/AGE-188) | Progress indicator keeps running after a tool failed | High | gpui | **W1** |
| [AGE-189](https://linear.app/agents-research/issue/AGE-189) | Progress footer keeps counting after the plan completed | High | gpui | **W1** |
| [AGE-187](https://linear.app/agents-research/issue/AGE-187) | Tool failure cards say only "the tool failed" | High | core + gpui | **W2** |
| [AGE-185](https://linear.app/agents-research/issue/AGE-185) | (duplicate of AGE-187) | Med | — | **W2** |
| [AGE-186](https://linear.app/agents-research/issue/AGE-186) | (duplicate of AGE-187) | High | — | **W2** |
| [AGE-179](https://linear.app/agents-research/issue/AGE-179) | Transcript turns overlap (height under-estimate) | High | gpui | **W3** |
| [AGE-183](https://linear.app/agents-research/issue/AGE-183) | Status header / to-do block overlap the tool card | Med | gpui | **W3** |
| [AGE-180](https://linear.app/agents-research/issue/AGE-180) | "Jump to latest" always visible, scrolls to the wrong place | High | gpui | **W4** |
| [AGE-184](https://linear.app/agents-research/issue/AGE-184) | Composer Send button clipped | Med | gpui | **W5** |
| [AGE-181](https://linear.app/agents-research/issue/AGE-181) | Artifact header: duplicate Code/Source tabs, dead Copy menu | Med | gpui | **W6** |

Ten open issues collapse into **six workstreams**, because several issues share a
root cause:

* AGE-188 and AGE-189 are the failure and success faces of one defect: the
  running indicator is derived from UI message flags rather than from the
  authoritative stream state. AGE-151's stall has the same origin.
* AGE-183 is not a z-order bug. It is the same virtual-list height
  under-estimate as AGE-179, hit through a different block type
  (`Block::Activity`).
* AGE-185, AGE-186 and AGE-187 are three filings of the same report.

---

## 2. W1 — One source of truth for "is this turn running"

**Closes AGE-151, AGE-188, AGE-189.** Highest value: it is the difference
between a UI that lies about whether the agent is working and one that does not.

### 2.1 What is actually wrong

**(a) The footer reads message flags, not stream state.**
`ChatView::is_thinking_indicator_visible()`
(`crates/chatty-gpui/src/chatty/views/chat_view/mod.rs:811`) is:

```rust
self.messages.iter().any(|msg| matches!(msg.role, MessageRole::Assistant) && msg.is_streaming)
```

but the only teardown paths — `finalize_assistant_message()` (`mod.rs:527`) and
`mark_message_cancelled()` (`mod.rs:642`) — both start from
`parent_streaming_assistant_index()` and clear exactly **one** message. Any other
message left with `is_streaming = true` (most easily a sub-agent progress row
whose `finalize_sub_agent_progress` never arrived, `sub_agent.rs:158`) keeps the
footer alive forever, with no "Working for Xs" header anywhere — which is
precisely the shape AGE-189 describes.

**(b) New finding — a deferred `StreamEnded` can tear down the *next* turn.**
`StreamManager::finalize_stream()` (`stream_manager.rs:446-479`) emits
`StreamEnded` through `cx.emit`, which GPUI delivers on the next effect flush,
not synchronously. But the caller
(`message_ops_internals.rs:592-646`) continues straight on and, at step 7,
injects the protocol follow-up via `send_protocol_follow_up` → `send_message_inner`
→ `register_stream`. So the ordering is:

1. `finalize_stream` — `StreamEnded` **queued**, entry removed.
2. Follow-up send — new assistant bubble started, new stream registered.
3. Effect flush — `StreamEnded` from turn *N* delivered, and
   `finalize_completed_stream` (`message_ops.rs:794`) runs against turn *N+1*:
   `input.set_streaming(false)` (`message_ops.rs:663`),
   `view.finalize_assistant_message(cx)`, and
   `conv.finalize_response(response_text, …)` with `streaming_message()` still
   empty.

That is both AGE-151 symptoms in one mechanism: an assistant message persisted
as `""` after tokens were billed, and a follow-up turn that runs but is
immediately declared finished, so the UI looks dead. Nothing in
`finalize_completed_stream` checks that the ended stream is still the current one.

**(c) The nudge cancels a turn it has no need to cancel.** Confirmed as filed:
`message_ops_internals.rs:443-452` sets `cancel_flag` for the `write_todos`
nudge, so the loop breaks at `message_ops_internals.rs:240` before
`StreamChunk::Done`, discarding the streamed text. The `verify_completion` path
(`stream_end_follow_up`, used at `message_ops_internals.rs:351`) queues instead
and is the correct model.

**(d) New finding — `cancel_flag` cannot interrupt a stalled provider.** The
flag is only read at the top of the loop (`message_ops_internals.rs:240`); the
`tokio::select!` below it awaits `stream.next()` with no timeout and no
cancellation branch. A provider or tool that stops yielding chunks — the
`ERR_NETWORK_CHANGED` case in AGE-188 — parks the loop forever, and the footer
counts up because, from its point of view, the stream genuinely never ended.

**(e) The "attention" verb is stale.** `render_message_list`
(`chat_view/mod.rs:1305-1320`) picks the last `TraceItem::ToolCall` in the trace
regardless of its state, so the footer says "Conjuring browser_navigate" long
after that call finished or errored.

### 2.2 Changes

1. **Stream epoch guard.** Give `StreamState` a monotonically increasing
   `epoch: u64`, carry it on `StreamManagerEvent::StreamEnded`, and have
   `handle_stream_manager_event` drop any `StreamEnded` whose epoch is not the
   one currently registered for that conversation (or that has already been
   superseded). Fixes (b) without changing the event-driven architecture.
2. **Footer visibility from `StreamManager`.** Replace
   `is_thinking_indicator_visible()`'s message scan with
   `StreamManager::is_streaming(&conv_id)` (`stream_manager.rs:573`) for the
   active conversation. Keep the message flags for per-bubble rendering only.
3. **Sweep on end.** In `finalize_assistant_message` / `mark_message_cancelled`,
   clear `is_streaming` on *every* assistant message, not just the parent, so no
   orphan row can outlive its stream.
4. **Queue the `write_todos` nudge, don't cancel.** Move it onto the same
   `stream_end_follow_up` path as `verify_completion`: drop the
   `cancel_flag.store(true, …)` at `message_ops_internals.rs:450` and let `Done`
   deliver it. (Leave the `AgentLoopGuard` cancel at `:456` alone — that one
   *is* trying to stop runaway work.)
5. **Never persist an empty assistant turn.** In `finalize_completed_stream`,
   if the accumulated response text is empty and the trace has no items, drop
   the turn instead of calling `conv.finalize_response("")`.
6. **Stall detection.** Add a `tokio::time::timeout`-style branch to the
   `select!` so the loop wakes periodically: re-check `cancel_flag`, and after a
   no-chunk threshold (propose 90 s, configurable) emit a
   `StreamChunk::Error("stream stalled…")` so the turn ends visibly instead of
   counting forever. This is what actually closes AGE-188's reported case.
7. **Attention only while running.** Filter the trace scan at
   `chat_view/mod.rs:1305` to tool calls in `ToolCallState::Running`; with none
   running, show the neutral ticker word and no tool name.
8. **Stop swallowing dropped follow-ups.** Per CLAUDE.md's no-silent-failures
   rule, upgrade `message_ops_internals.rs:645` and the `is_ready` early return
   at `message_ops.rs:82-85` from `.ok()` / `debug!` to `warn!` with the
   conversation id.

### 2.3 Verification

* Unit: a `StreamEnded` carrying a stale epoch leaves the newly-registered
  stream's UI state untouched (fails today).
* Unit: `finalize_*` clears `is_streaming` on a message list containing a
  sub-agent row plus a parent row.
* Unit: `observe_tool_result` returning the `write_todos` nudge does not set the
  cancel flag, and the prompt is still delivered after `Done`.
* Unit: an empty response with an empty trace produces no persisted message.
* Manual: drop the network mid `browser_navigate`; the footer must reach a
  terminal state within the stall threshold.

---

## 3. W2 — Real tool errors reach the user

**Closes AGE-187; AGE-185 and AGE-186 close as duplicates of it.**

### 3.1 What is actually wrong

The renderer is **not** discarding the message — this rules out one of the two
hypotheses in the issue. `ToolCallState::Error(err)` is threaded all the way to
`tool_row.rs:47-49` and painted as a danger `Tag` (`tool_row.rs:104-106`). The
string it paints really is `"the tool failed"`, because that is what arrives:
`llm_service.rs:53-60` already carries the comment *"Rig redacts many typed tool
errors to this generic feedback"*, and matches on the literal to classify a
result as an error at all (`llm_service.rs:137`, `:245`).

So the loss happens at the rig boundary: our tools return
`Err(ToolError::OperationFailed(msg))` (`tools/mod.rs:4-7`, e.g.
`browser_tools.rs:128-143`), and rig replaces the payload with the generic
string before it re-enters our stream. The model is fed the same redacted text
the UI shows — the issue asks whether the model sees more; it does not.

Two consequences the issue lists follow from the same place: two identical cards
are indistinguishable because the only distinguishing text was redacted, and
there is no expandable detail because there is no detail.

### 3.2 Changes

1. **Do not let a tool error reach rig as an error.** Every typed tool is
   registered at one site — `factories/agent_factory/tool_collector.rs:143-190`.
   Add a generic wrapper there, `FallibleTool<T>`, that delegates `NAME`,
   `definition()` and `Args` to `T`, sets `Error = Infallible` and
   `Output = serde_json::Value`, and maps `Err(e)` to
   `Ok(json!({ "error": { "tool": T::NAME, "message": e.to_string() } }))`.
   Wrap each `.tool(x)` as `.tool(FallibleTool(x))`. One site, no per-tool edits.
2. **Classify on the structured shape, not on prose.** Extend
   `tool_result_looks_like_error` (`llm_service.rs:53`) to recognise the wrapper's
   JSON envelope and pull the message out of it. Keep the `"the tool failed"`
   literal as a fallback for MCP and any tool the wrapper does not cover, and
   keep the existing test.
3. **Give the card something to show.** In `tool_row.rs`, render the error as
   tool name + short call summary + message, with the full payload behind the
   existing expand affordance instead of a single truncating `Tag`. Preserve the
   distinct-failure information the wrapper now carries so two cards are
   distinguishable.
4. **Verify the rig behaviour before building on it.** rig-core 0.42 sources are
   not vendored in this checkout, so the exact redaction site is inferred from
   our own code comment and the observed output. First step of this workstream is
   a 20-line harness that makes a tool return `Err` and prints what
   `StreamChunk::ToolCallError` carries — if rig 0.42 already forwards the real
   message for some providers, change (1) shrinks accordingly.

### 3.3 Verification

* Unit: `FallibleTool` over a tool whose `call` returns `Err` yields
  `Ok` with the message intact.
* Unit: `tool_result_looks_like_error` classifies the envelope and extracts the
  message.
* Manual: force `ERR_NETWORK_CHANGED` and confirm the card names
  `browser_navigate`, the URL, and the CDP error.

---

## 4. W3 — Transcript geometry

**Closes AGE-179 and AGE-183.**

### 4.1 What is actually wrong

AGE-179's analysis is confirmed against `adapter.rs:637` /
`chat_view/mod.rs:1336-1341`: caller-supplied estimates, no per-item
`ContentMask`, so an under-estimate paints over the next turn.

**New finding — AGE-183 is the same defect, one level down.** Its overlap is not
a stale layout size or an overlay: `block_estimated_height` (`adapter.rs:460-466`)
charges a **flat 40 px** for `Block::Activity` unless the group contains a
failure, no matter how many tool rows it has. And expansion state lives in
`ChatView::collapsed_tool_calls` (`chat_view/mod.rs:105`), which
`estimate_turn_height(turn, plan_steps, content_width)` never receives — so an
*expanded* card is always measured as if collapsed. An expanded "Explored 2
files" card is charged 40 px; the following blocks are placed 40 px down; they
land on top of it. That is exactly the reported picture, and it explains why the
work-fold header and the to-do panel are the two things that collide.

`Block::Plan` has the same shape of bug: it is sized from the *view-global*
`plan_steps` (`chat_view/mod.rs:1327-1331`), not from the block's own step count.

### 4.2 Changes

Ordered by value / risk, per the issue's own ordering:

1. **Sum per line instead of `max`** in `estimate_message_bubble_height`
   (`adapter.rs:~313-330`). Strictly ≥ the current value for every input, so it
   can only close overlaps.
2. **Size `Block::Activity` from its rows and its expansion state.** Thread the
   `collapsed_tool_calls` lookup (or a resolved `expanded: bool` per block) into
   `estimate_turn_height` / `block_estimated_height`, and charge
   `header + rows × row_height` when expanded. Do the same for `Block::Plan`
   using the block's own steps.
3. **Make the estimator markdown-aware**, walking the same `CachedParseResult`
   segments `render_message` renders from (`message_component.rs:633`) — headings,
   fenced blocks, tables, math and mermaid get their rendered footprint, the way
   `IMAGE_ATTACHMENT_ROW` (`adapter.rs:322`) already does for images.
4. **Scale by font size.** Derive `LINE_HEIGHT` / `AVG_CHAR_WIDTH` from
   `GeneralSettingsModel::font_size` instead of hardcoding the 14 px case.
5. **Deliberate asymmetric margin** (`× 1.05` or `+24 px`) with a comment saying
   why: an over-estimate is a gap, an under-estimate is text on text.
6. **Follow-up, not this workstream: measured-height feedback.** Cache real
   post-paint heights keyed by `(turn id, content_width, font_size)`. It needs a
   measurement hook `v_virtual_list` does not expose today — file it as its own
   issue rather than growing this one.

Rejected, as the issue says: clipping turns to their slot. Truncated text is
worse than overlapped text.

### 4.3 Verification

Extend the existing `attachment_height_tests` (`adapter.rs:347`):

* an expanded 2-row `Block::Activity` estimates taller than a collapsed one, and
  at least `header + 2 × row` (fails today — both are 40.0);
* a markdown fixture with headings, a nested list and a fence estimates at least
  the sum of its parts;
* `estimate_message_bubble_height` is monotone in content length and never below
  `Σ ceil(len(line) / chars_per_line) × LINE_HEIGHT`;
* the estimate grows when `font_size` grows.

Manual: set font size to 18, produce a long plan, send a follow-up — no overlap.

---

## 5. W4 — "Jump to latest" (AGE-180)

The issue's diagnosis is confirmed on `main` (line numbers have drifted:
`activate_sticky_scroll` is now `chat_view/mod.rs:753`, the latching block
`:1227-1241`, pin visibility `:1350`). Both defects are real: gpui-component's
`VirtualListScrollHandle::scroll_to_bottom()` aligns the **top** of the last
item, and `user_scrolled_away` latches on a 10 px threshold with no path back to
`false` except clicking the pin or switching conversations
(`chat_view/history.rs:49`).

Changes, as filed:

1. Scroll to the true content bottom — `scroll_to_item(last, ScrollStrategy::Bottom)`
   if it behaves for over-tall items, else drive `set_offset` from `max_offset`.
2. Replace the latching bool with distance-derived visibility and hysteresis:
   show above ~0.75 viewport (or ~300 px), hide below a smaller threshold.
   Sticky-scroll disengagement keeps its own tighter threshold — they need not
   be the same number.
3. Keep mid-stream user scroll suppressing auto-follow.

Also check `RunPinKind::JumpToLatest` in the artifact viewport
(`artifact_view.rs:~1563`), which is gated on separate state — not affected, but
it should not diverge in semantics.

Regression tests: one turn taller than the viewport → pin hidden after a render
pass; after `activate_sticky_scroll()`, `distance_from_bottom <= threshold`;
scroll up past the threshold and back → pin appears and disappears.

---

## 6. W5 — Composer overflow (AGE-184)

Confirmed. The composer's bottom row (`chat_input/render.rs:472-476`) is a plain
`div().flex().flex_row().items_center().gap_2()` with no `w_full`, no
`min_w_0` and no shrink control; children keep intrinsic width and the last one
— Send (`render.rs:603-645`) — is clipped by the parent's bounds. The model
`Popover` (`render.rs:207`) carries the widest, most variable label.

Changes:

1. `w_full()` + `overflow_hidden()` on the row.
2. `flex_shrink_0()` on the attach button and on Send/Stop so they always reserve
   their space.
3. `min_w_0()` + `truncate()` on the model button label, with the full name in a
   tooltip; workdir chip truncates second.
4. Check hit-testing, not just pixels, at the narrow end (artifact pane open,
   small window, long model name).

---

## 7. W6 — Artifact header (AGE-181) — needs a product call first

All three defects re-verified: `artifact_primary_body`'s first arm for
`is_code_artifact_path` is `artifact_source_input(editor)`
(`artifact_view.rs:876-891`), the identical call the `Source` tab makes on the
same editor entity, while the tab bar is built unconditionally as
`primary_label` + `"Source"` (`artifact_view.rs:1019-1023`); `copy_kind`
(`artifact_view.rs:547-559`) shares one arm between `"markdown"` and
`"rendered"` and falls through to `source` otherwise, so all three menu items
and the button itself are one action for an HTML file; and the copy control is
not gated on artifact kind, so PDF/image artifacts copy `""`.

This is the one workstream where the fix is a design decision, not a repair.
Recommendation (matching the issue's suggested direction):

* **Tabs:** render a second tab only where the two bodies differ —
  `Rendered | Source` for markdown, `Table | Source` for tabular, `Diff` when a
  snapshot exists, and **no tab bar** for a plain code file.
* **Copy:** one icon button, no caret, tooltip naming what it copies; a dropdown
  only where two genuinely distinct payloads exist; hidden entirely for
  PDF/image/browser artifacts.
* **Placement:** group Copy with Reveal as a file action.

Sequence this **after** the browser screencast work
([AGE-155](https://linear.app/agents-research/issue/AGE-155),
[AGE-156](https://linear.app/agents-research/issue/AGE-156)) lands, to avoid
conflicts in `artifact_view.rs` — the issue flags this and it still holds.

Regression tests at the header-model level, not on pixels: a `.html` path yields
one primary view with no separate `Source` tab absent a diff; `.md` still yields
`Rendered | Source`; every offered `copy_kind` variant produces a distinct
string; the copy control is absent for PDF/image/browser artifacts.

---

## 8. Sequencing

| Order | Workstream | Why here | Rough size |
|---|---|---|---|
| 1 | **W1** stream lifecycle | Three High issues; a UI that lies about running state makes every other bug harder to report | L |
| 2 | **W2** tool errors | Unblocks diagnosis of everything else, W1's stall work included | M |
| 3 | **W3** transcript geometry | Two issues, one estimator; independent of W1/W2 so it can run in parallel | M–L |
| 4 | **W4** jump-to-latest | Self-contained; touches `chat_view/mod.rs`, so land after W1's edits there | S–M |
| 5 | **W5** composer overflow | Isolated, low risk | S |
| 6 | **W6** artifact header | Blocked on a product call and on AGE-155/156 | M |

W3 and W5 touch files W1 does not, so they can run concurrently. W4 and W1 both
edit `chat_view/mod.rs`; keep them sequential.

## 9. Ground rules for each PR

Per CLAUDE.md:

* One workstream per PR, one issue per commit where they separate cleanly.
* Every bug closes with a test that **fails before the fix** — asserted, not
  assumed. Where a defect is only reproducible interactively (W5, parts of W1),
  say so in the PR and describe the manual check performed.
* `cargo test && cargo clippy --all-features -- -D warnings && cargo fmt --check`
  green before declaring done.
* Blast radius: all of W3, W4, W5, W6 and most of W1 are `chatty-gpui`-only.
  W2's wrapper and W1's stall detection touch `chatty-core` and therefore
  `chatty-tui` — check the TUI's stream loop for the same stall gap and note it
  in the PR (`ui-sync-check` will flag the crate asymmetry otherwise).

## 10. Linear housekeeping

* Close **AGE-185** and **AGE-186** as duplicates of **AGE-187**.
* Add to **AGE-183** that it shares AGE-179's root cause and is being fixed under
  the same workstream (`Block::Activity` flat 40 px + expansion state not
  reaching the estimator).
* Add to **AGE-187** that the renderer is exonerated: the error text is threaded
  to `tool_row.rs:47`, the payload itself is redacted upstream — and that the
  model receives the same redacted string the UI does.
* Add to **AGE-151** the deferred-`StreamEnded` race in §2.1(b) as the leading
  candidate for the "no agent turn at all" half the issue lists as unconfirmed.
* File the measured-height follow-up (§4.2 item 6) as a new issue.
