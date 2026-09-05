# Message path in chatty-core: technical-debt analysis

**When to read this:** you are about to touch how a user message becomes an
LLM request, how the response stream comes back, or how the turn is
persisted, and you want to know where the path is slower, more fragile or
more duplicated than it needs to be.

Written 2026-09-05 against `main` at `df14649` (the moving prompt-cache
breakpoint, AGE-205). Every claim below was checked against the code at that
commit and, where rig's behaviour matters, against the rig-core / rig-agent
0.42.0 sources the workspace pins. Linear references: AGE-205, AGE-206,
AGE-207 (the prompt-caching review this audit widens), AGE-150, AGE-151,
AGE-188, AGE-191, AGE-192.

The path is the hot path of the product, so the bar here is stricter than
for the rest of the codebase: *no per-turn work that is not needed, nothing
decided twice, nothing decided by string matching when a type is available.*

---

## 1. What one turn actually does today

Both frontends drive the same chatty-core functions, in the same order. The
lines are the desktop's (`crates/chatty-gpui/src/chatty/controllers/app_controller/message_ops_internals.rs`)
and the terminal's (`crates/chatty-tui/src/engine/mod.rs`, `engine/streaming.rs`).

| # | Step | chatty-core code | Cost class |
|---|------|------------------|-----------|
| 1 | Snapshot history: `conversation.messages()` clones every `rig::Message` in the conversation, inline base64 image/PDF payloads included | `models/conversation.rs:363` | O(history bytes) |
| 2 | Shape history: `shape_context(history, &ContextShaperSettings::default(), None)` | `services/context_shaper.rs:134` | O(history) |
| 3 | Open the stream: `stream_prompt(&agent, &shaped, contents, channels…, max_turns)` — clones the history **again** (`history.to_vec()`), builds `Message::User`, picks one of two `async_stream` macros | `services/llm_service.rs:442-482` | O(history bytes) |
| 4 | rig-agent multi-turn loop. For OpenRouter each request body goes: rig serialisation → `finalize_openrouter_request_body` (system-message breakpoint) → `PromptCachingHttpClient::rewrite` (full JSON parse + re-serialise, last-message breakpoint) → `reqwest` | `factories/agent_factory/prompt_cache_http.rs:89-104` | O(body bytes) per request |
| 5 | Drive the loop: `run_stream_loop` multiplexes chunks, sub-agent progress and the 180 s stall watchdog, calling the frontend's `StreamChunkHandler` | `services/stream_processor.rs:86-159` | per chunk |
| 6 | Persist: `finalize_response(text, artifacts, trace_json)` stores **only the final assistant text** as a rig message; tool calls and results live only in the trace JSON | `models/conversation.rs:312` | — |
| 7 | Post-turn utility calls: `generate_title` (first exchange) and, on the desktop, `summarize_oldest_half` under budget pressure — both through `agent.prompt()` on the **full tool-laden conversation agent** | `services/title_generator.rs:112`, `token_budget/summarizer.rs:228` | one extra model call each |

Things that are *right* and should stay that way:

- The agent is built once per conversation (`Conversation::new` / `from_data`),
  not per turn; MCP tool lists are fetched per agent build and cached per
  connection (`McpConnection::list_tools`, `mcp_service.rs:617`).
- The augmented preamble (`preamble_builder.rs`) contains nothing
  time-varying: no date, no counters, no random ids. MCP tools are iterated
  in `BTreeMap` order (AGE-206) and secret *names* come from an ordered
  `Vec`. The system prefix is byte-stable across restarts as long as the
  settings are.
- Streamed OpenRouter requests really do pass through the cache-rewrite
  layer: rig's generic client sends every streaming request via the
  configured `HttpClientExt` (`rig-core/src/client/mod.rs:592`,
  `self.http_client.send_streaming(req)`). AGE-205 step 2 applies to real
  turns, not only to unit tests.

---

## 2. Findings

Severity is about product impact on the hot path, not code size. Effort is
a rough PR size: **S** under a day, **M** a few days, **L** a design change.

### A. Correctness and silent-failure debt

**A1 — The Azure Entra token is baked into the agent; the mid-stream 401 refresh never reaches it.** Severity high (Azure users), effort M.
`provider_builder.rs:173-185` fetches a token string once and hands rig
`AzureOpenAIAuth::Token(token)`. On a 401 the desktop awaits
`AzureTokenCache::refresh_token()` (`message_ops_internals.rs:208-226`) —
which is the stated reason `StreamChunkHandler::on_chunk` is `async` — but
nothing rebuilds the agent afterwards. Agent rebuilds are triggered only by
MCP changes, a workspace-dir change, or a model switch
(`conversation_ops_modify.rs:103`, `message_ops.rs:220-239`,
`app_controller/mod.rs:583`). A conversation that outlives the token
(≈1 h) sends the expired token on every subsequent turn. Fix: inject
`Authorization` per request from the cache with a small `HttpClientExt`
wrapper (the same pattern `PromptCachingHttpClient` already uses), or
rebuild the agent after a successful refresh. Either way `on_chunk` no
longer needs to be async, which simplifies the shared trait.

**A2 — The terminal frontend sends the user message twice.** Severity high (TUI cost and behaviour), effort S.
`engine/mod.rs:672` pushes the user message into the conversation, then
`engine/mod.rs:699` snapshots `conversation.messages()` (which now contains
it) and passes that as `history` **and** the same text as `contents` to
`stream_prompt`. rig appends the prompt after the caller-supplied history
(`rig-agent/src/agent/run/mod.rs:728-745` splits the prompt off `new_messages`;
`prompt_request/mod.rs:614-620` builds the request as `chat_history ++ new_messages`,
no de-duplication), so every terminal request
carries two consecutive identical user turns. The desktop avoids this by
adding the message *after* `stream_prompt` returns
(`message_ops_internals.rs:584-594`). Fix in the TUI: snapshot before the
push, exactly like the desktop; better, make the core turn API (C1) own the
ordering so it cannot diverge again.

**A3 — Title generation and summarisation run through the full agent.** Severity medium, effort S.
`generate_title` and `summarize_oldest_half` call `AgentClient::prompt()`,
which is rig's `agent.prompt()` on the conversation agent: the whole
augmented preamble plus every registered tool schema (up to ~60 native
tools plus MCP) ride along on a request whose answer is 3–7 words. rig's
implicit budget for a bare `prompt()` is **one** model call
(`rig-agent/src/agent/prompt_request/mod.rs:230-233`); if the model answers
with a tool call the call fails with `MaxTurnsError`
(`rig-agent/src/agent/run/mod.rs:735-741`) and the title silently stays
"New Chat". With OpenRouter caching the system prefix is a cache read, but
the moving breakpoint still writes a fresh cache entry for the utility
prompt. Fix: build a second, tool-less completion model next to the agent
(`client.completion_model(..)` with a one-line system prompt) and route
utility calls through it. `AgentClient::prompt`'s doc comment already
calls itself "the central hook point for … title generation,
summarization, and other non-streaming calls" — it is the right seam, it
just needs the cheap model behind it.

**A4 — The context shaper defeats history caching and mostly targets content that never exists.** Severity high (long conversations), effort M.
Both frontends run `shape_context(history, default, None)` before every
call (`message_ops_internals.rs:551-560`, `engine/mod.rs:704-716`), agent
`None` so only stages 1–3 can run. Three problems:

1. Stages 1 and 3 trim `UserContent::ToolResult` payloads. The persisted
   history never contains any: `finalize_response` stores only the final
   assistant text and no code path in either frontend pushes a tool result
   into `entries`. Those stages are dead in production.
2. Stage 2 (snip) is the only reachable free stage. It keeps the first 2
   and the last 8 messages and drops the middle. The tail is a **sliding
   window**: once the history exceeds 80 000 chars, the third message of
   every request changes on every turn, so the prefix the previous request
   cached is never re-hit. This is exactly the regime (long agentic
   sessions) where the AGE-205 moving breakpoint was supposed to pay off.
3. Thresholds are character counts, blind to the model's
   `max_context_window` (80 000 chars is ~20 k tokens, a tenth of a 200 k
   window), and images count as 256 chars each.

Fix: compaction has to be *generational* to be cache-friendly — decide a
cut point when a token budget (derived from `ModelConfig.max_context_window`)
is crossed, then keep that cut fixed (ideally persisted through
`replace_history`, which already exists) until the next crossing, so
consecutive requests share a prefix. Delete stages 1 and 3 or, if tool
history should be visible to the model across turns (see C4), persist tool
turns first and then make those stages real. See also C5: this pipeline
and `token_budget` are two overlapping context-management systems.

**A5 — A handler error skips `on_stream_ended`.** Severity low today, effort S.
`run_stream_loop` propagates `handler.on_chunk(...).await?`
(`stream_processor.rs:130`, `:139`), so a handler that returns `Err` exits
the loop without `on_stream_ended`. The characterization goldens never
script a handler error, so the contract is untested. Either make the
contract explicit (call `on_stream_ended` on every exit path) or make
`on_chunk` infallible and let handlers log.

**A6 — Usage semantics are hard-coded where the provider is known.** Severity low, effort S.
`stream_prompt` sets `UsageSemantics::InputIncludesCache` unconditionally
(`llm_service.rs:453`). `AgentClient` carries `provider: ProviderType` but
its only reader is a `#[allow(dead_code)]` accessor
(`agent_factory/mod.rs:1069-1072`). The enum's own doc says the other
variant exists so a native Anthropic route cannot double-count; wire it
from `agent.provider` now so the trap is closed before that route is
added.

**A7 — Errors are stringified in core and re-classified by text in each frontend.** Severity medium, effort M.
`StreamChunk::Error(e.to_string())` (`llm_service.rs:256`, `:369`) flattens
rig's typed `PromptError` / `CompletionError` (which carry the HTTP status)
into prose. Downstream, the desktop sniffs `"401"` / `"Unauthorized"`
(`message_ops_internals.rs:723`) and `"malformed json input"`; the headless
TUI has its own list (`"eof while parsing"`, `502`/`503`/`504`, "server
overloaded") in `headless/recovery.rs:10-39`; the interactive TUI classifies
nothing. Tool-result failures are likewise sniffed by
`tool_result_looks_like_error` (documented in CLAUDE.md; the stream carries
no error flag). Fix: a typed `StreamError { kind: Auth | RateLimit |
Provider(status) | Transport | MalformedToolCall | Stalled, message }`
produced once in core; frontends match on the kind.

### B. Efficiency debt on the per-turn path

**B1 — The history is copied two to three times per turn, payloads included.** Effort S.
Desktop: `conversation.messages()` (copy 1), `history.clone()` for the
token-budget task (copy 2, `message_ops_internals.rs:494`),
`stream_prompt`'s `history.to_vec()` (copy 3, `llm_service.rs:455`).
Terminal: copies 1 and 3. Each copy includes every inline base64 image and
PDF (`UserContent::image_base64`, `DocumentSourceKind::Base64`), so a
conversation with a few screenshots copies megabytes per turn. On the desktop a fifth copy, `conv.entries().to_vec()`
(`message_ops.rs:271`), duplicates every message *and* its trace JSON only
to look up one entry's attachment paths, and `conv.agent().clone()`
(`message_ops.rs:265`) deep-copies rig's `AgentConfig`, preamble string
included, because `rig_agent::Agent` is not `Arc`-backed. Fix:
`stream_prompt` takes `Vec<Message>` by value (both callers already own
the vector), or the conversation hands out `Arc<[Message]>`; wrap the
agent in an `Arc` on `Conversation`.

**B2 — Token counting BPE-tokenises base64 every turn.** Effort S–M.
`TokenCounter::count_history` serialises each message to JSON and runs
tiktoken over the string (`token_budget/counter.rs:112-126`). That includes
base64 image data, which tokenises slowly and produces a number with no
relation to what providers bill (images are priced per pixel). It runs on
a background thread, but it is O(whole history) per turn and its answer
for image-heavy chats is wrong. Fix: skip non-text content (fixed
per-image estimate) and cache per-entry counts so a turn costs O(delta).

**B3 — The cache rewrite double-serialises every request.** Effort S (upstream).
`PromptCachingHttpClient::rewrite` parses the complete body rig just
serialised, mutates one field and serialises it again
(`prompt_cache_http.rs:89-104`); it also reorders object keys, harmlessly.
For a history with attachments that is a second full pass over megabytes
per request. rig already holds the body as `serde_json::Value` in
`finalize_openrouter_request_body` (`rig-core/src/providers/openrouter/completion.rs:1398`),
so a `prompt_caching` mode that also marks the last message is a small
upstream change. Keep the wrapper as the fallback and pin it with the
existing tests; open the upstream PR.

**B4 — A fresh `reqwest::Client` per OpenRouter agent build.** Effort S.
`provider_builder.rs:60` does `reqwest::Client::new()` inside the agent
build, so every conversation (and every rebuild) gets its own connection
pool and pays its own TLS handshake; it also bypasses
`services::http_client`, whose module doc says all clients should go
through it. Ollama and Azure use rig's default client, also per build.
Fix: one process-wide `LazyLock<reqwest::Client>` for LLM traffic, shared
by all three arms (no total timeout — streams are long).

**B5 — `from_model_config_with_tools` is a 950-line function that rebuilds the world.** Effort M.
`agent_factory/mod.rs:114-1066` constructs `FileSystemService`,
`CodeSearchService`, `GitService` (spawns subprocesses), `SandboxManager`,
optionally a `BrowserManager`, ~60 tool structs and the preamble on every
conversation open, conversation switch and agent rebuild. Not per turn,
but on the path the user feels when switching chats. The workspace-scoped
services could be cached by workspace directory across conversations; the
tool-construction body could be split by capability group so the function
stops being the single largest thing in core.

**B6 — Two 160-line macros duplicate the stream mapping and are untested.** Effort S–M.
`process_agent_stream!` and `process_agent_stream_with_approvals!`
(`llm_service.rs:163-428`) contain the same `MultiTurnStreamItem → StreamChunk`
arms verbatim; only the extra `select!` arms for approvals differ. The
stream fixtures script `StreamChunk`s directly, so the mapping (tool-id
resolution, error sniffing, usage normalisation per item) has no test.
Fix: one `fn map_item(item, semantics) -> impl Iterator<StreamChunk>` and
one stream; fold the approval / resolution / clarification receivers into
`run_stream_loop`'s existing `select!`, which already multiplexes sub-agent
progress. That removes a whole layer of `async_stream` + `select!`.

**B7 — `StreamChunk::TokenUsage` duplicates `ApiCallUsage`.** Effort S.
Same four fields, `turn = 0`. Collapse to `StreamChunk::TurnUsage(ApiCallUsage)`
and drop one conversion in each frontend.

**B8 — MCP tool gathering holds a write lock across awaited calls.** Effort S.
`get_all_tools_with_sinks` takes `connections.write()` and awaits each
server's `list_tools` under it (`mcp_service.rs:799-807`); it only needs the
lock to reach the per-connection cache. Cheap after the first call, but it
serialises concurrent conversation opens behind a network round trip.

### C. Structural debt

**C1 — Per-turn orchestration lives in both frontends, not in core.** Effort L (incremental).
The following are duplicated, in the same order, by
`message_ops_internals.rs::run_llm_stream` and `engine/streaming.rs::run_stream`
plus `engine/mod.rs::send_message`: approval/resolution/clarification channel
setup and global-notifier registration; `ContextShaperSettings::default()` +
`shape_context`; `stream_prompt`; `agent_task_controller.reset()` gated on
`reset_agent_task` (with the same AGE-150 comment); `install_progress_channel`;
`run_stream_loop`; reading `pending_follow_up` back off the handler; the
title-generation trigger; `summarize_oldest_half` + `replace_history`. Where
the copies differ they differ by accident (A2; the TUI cancels the stream
on a todo-protocol nudge, which the desktop deliberately stopped doing for
AGE-151; the TUI drops a follow-up if a stream is running; the TUI decides
`reset_agent_task` by sniffing the message text). Core should expose the
turn as an API — roughly `prepare_turn(conversation, contents, settings) →
TurnPlan` and `run_turn(plan, handler)` — so the frontends supply only a
`StreamChunkHandler`, the way AGE-191/192 already did for the loop.

**C2 — Dead and vestigial surface.** Effort S.
`stream_prompt` returns the `Message` it built and both callers bind it to
`_user_message`; `AgentClient.provider` is unread (A6);
`AgentBuildContext.theme_colors`, `.conversation_id` and
`.pending_artifacts` are permanently `None` from the TUI;
`from_model_config_with_tools` returns a 3-tuple whose second and third
elements exist only to be stored back on the conversation; the Ollama arm
ignores `supports_temperature` and `max_tokens` while the other two arms
honour them (`provider_builder.rs:96-99`).

**C3 — Documentation describes providers that no longer exist.** Effort S.
CLAUDE.md's "Model Capability Architecture" and
`docs/architecture-overview.md` still list Anthropic / Gemini / OpenAI /
Mistral variants; `ProviderType` has three variants (OpenRouter, Ollama,
AzureOpenAI) with serde aliases for the removed ones. Anyone reading the
capability table will look for match arms that are not there.

**C4 — The model has no memory of its own tool activity across turns.** Effort L (product decision first).
Because only final text is persisted (step 6), turn N+1 sees the user
question and the assistant's prose answer, never the commands run or files
read in turn N. That is a defensible choice (small prompts, cache-friendly,
no tool-result compaction needed) but it is currently implicit, and the
context shaper was written as if the opposite were true (A4). Decide it
explicitly. If tool turns should be kept, rig already returns them
(`PromptResponse.messages` when history is supplied), and the shaper's
stages 1 and 3 become real — with the generational compaction from A4.

**C5 — Two context-management systems.** Effort M (merges into A4).
`token_budget/` (snapshot, `TokenTrackingSettings.auto_summarize`,
`summarize_oldest_half`, persisted via `replace_history`) and
`services/context_shaper.rs` (per-call, non-persisted, char-based) overlap
in purpose and disagree in units. One of them should own "what the model
sees" and the other should only measure.

**C6 — Synthetic follow-up turns are scheduled from the frontends.** Effort M (part of C1).
The todo-protocol nudge (`AgentTaskController`), the loop-guard pivot and
deadline (`AgentLoopGuard`), the malformed-tool-call retry and the
verbosity stop each inject an extra model call as a synthetic user turn.
The decision logic is in core; the wiring, cancellation policy and prompt
text live in each frontend and have already diverged (two different
verbosity-overflow prompts, cancel-vs-continue on nudge, dropped
follow-ups). These are billed turns; they belong in the core turn runner
with one policy and golden coverage.

### D. Prompt-cache hygiene after AGE-205

- **Verified stable:** preamble content, MCP tool order, secret-name order,
  chart-tool schema (theme colours are used at render time, not in the
  tool description).
- **Verified applied:** the moving breakpoint is added on streamed requests
  (rig sends streams through the configured `HttpClientExt`).
- **Still open from AGE-205:** the real-run check with the per-call
  `LLM completion call usage` log line.
- **Will break history caching today:** the sliding snip window (A4) once
  a conversation passes 80 000 chars.
- **Wastes a cache write per conversation:** the tool-laden title call
  (A3).
- **Upstream candidate:** marking the last message inside rig's
  `finalize_openrouter_request_body` instead of re-serialising the body
  (B3).

---

## 3. Frontend seams on the same path

The audit is about chatty-core, but the turn starts and ends in the
frontends, and several of the most expensive things on the path are decided
there. These were verified the same way; they are listed so the core fixes
above are not undone one layer up.

### Desktop (`chatty-gpui`)

**F1 — The agent can be rebuilt on every send.** Severity high when it triggers, effort S.
Before each send the controller compares the workspace directory the agent
was built with against the effective one and rebuilds on mismatch
(`message_ops.rs:220-239`). The two sides are normalised differently:
`rebuild_conversation_agent` stores the **canonicalised** path
(`app_controller/mod.rs:182-184`, `:253-256`, `:361-366`), while
`Conversation::new` stores the raw settings string (`conversation.rs:115-119`)
and `create_new_conversation` normalises the directory it hands the factory
but stores the raw selection on the conversation
(`conversation_ops.rs:326-331`, `:420`). Whenever the configured path is not
already canonical (a symlinked directory, `/tmp` on macOS) and either a
per-conversation directory is set or any rebuild has happened, the
comparison is true on every send and the full 950-line agent build (B5),
including a new `reqwest::Client` (B4) and a git subprocess, runs per turn.
Fix: normalise once, at the boundary where the setting is read, and compare
canonical to canonical.

**F2 — Assistant-generated artifacts are re-sent and re-persisted on every later turn.** Severity high, effort S.
`select_recent_assistant_attachments` (`message_ops_internals.rs:749-783`)
walks back to the most recent assistant entry that *has* attachments, not
the most recent assistant message, and the send path re-reads and
base64-encodes those files into the new user message
(`message_ops.rs:305-321`, `message_ops_internals.rs:786-832`). That user
message is then persisted with the base64 inline
(`message_ops_internals.rs:584-591`). Once a turn produces a chart or PDF,
every subsequent user turn carries another copy, so history, every later
prompt, the token count (B2), the cache rewrite (B3) and the on-disk row
all grow by the artifact size per turn until a newer assistant message has
attachments. Fix: attach only when the previous assistant message produced
the artifact, and keep the attachment out of the persisted message (store
the path, encode at send).

**F3 — Save amplification.** Severity medium, effort S–M.
`persist_conversation` (`conversation_ops_modify.rs:486-528`) serialises the
entire conversation — history, traces, token usage, attachment paths,
timestamps, feedback, regeneration records — synchronously on the UI thread
(`app_controller/mod.rs:775-825`) and upserts one SQLite row. It runs at
stream end, after every todo-tool result mid-stream
(`message_ops_internals.rs:116-149`), and the ATIF / JSONL auto-exports
rebuild the same data up to twice more per turn (`message_ops.rs:1143`,
`:1152`). With F2 in play each save also rewrites the accumulated base64.

**F4 — The approval channel is a process-wide singleton replaced on every send.** Severity medium, effort M.
`set_global_approval_notifier` (`message_ops_internals.rs:443`, same for
clarifications at `:455`) overwrites a `OnceLock<Mutex<Option<Sender>>>`
(`models/execution_approval_store.rs:9-19`) that the shell and git tools use
to raise approval prompts. The desktop supports background streams, so with
two conversations streaming the approval for either conversation's tool is
delivered to whichever stream registered last. The notifier should be
per-agent (it is already threaded into the tools via `PendingApprovals`),
and the global should go.

**F5 — Dead per-turn work.** Severity low, effort S.
`_workspace_dir` is computed on the GPUI thread with `std::fs::canonicalize`
and discarded (`message_ops_internals.rs:462-482`); `_model_id` is destructured
and unused (`message_ops.rs:243`); the user message `stream_prompt` builds
is discarded and rebuilt (`message_ops_internals.rs:572`, `:585-591`), so
the user content — attachments included — exists three times per send;
`ToolCallStarted` clones the whole response-so-far into `text_before`
(`message_ops.rs:426`) and the view clones it again
(`chat_view/handlers.rs:153`).

**F6 — Follow-up turns re-enter the whole send prologue.** Severity medium, effort M (part of C6).
A todo-protocol nudge, loop-guard pivot or malformed-tool-call retry goes
through `send_message_inner` again (`message_ops_internals.rs:701-718`): the
rebuild check (F1), the history copies (B1), the token count (B2) and the
shaper (A4) all run for a turn the user never sees. Because the handler is
rebuilt per follow-up, the one-shot retry guard has to scan history for a
literal prompt prefix (`already_asked_to_retry`,
`message_ops_internals.rs:1081-1098`).

**F7 — Small inconsistencies.** Severity low, effort S.
The generated title is written to the in-memory conversation and metadata
but not saved (`message_ops.rs:1090-1105`), so it reaches disk only on a
later save. The PDF test on user attachments is case-sensitive
(`message_ops.rs:290`) while the assistant-attachment filter is not
(`message_ops_internals.rs:767-771`). `finalize_completed_stream` and
`finalize_stopped_stream` apply different empty-turn rules
(`message_ops.rs:896`, `:1185-1196`).

### Terminal (`chatty-tui`)

**T1 — User message sent twice.** See A2.

**T2 — The todo-protocol nudge still cancels the stream.** Severity high, effort S.
`TuiStreamHandler` returns `ChunkAction::Break` when the task controller
asks for a follow-up (`engine/streaming.rs:66-85`), discarding the
`ToolCallResult` chunk (the tool stays rendered as running) and cancelling a
live turn — the AGE-151 regression the desktop fixed with
`follow_up_requires_cancel(TodoProtocol) == false`
(`message_ops_internals.rs:1058-1063`).

**T3 — Follow-ups are dropped silently.** Severity high for headless runs, effort S.
`engine/mod.rs:879-881` sends a protocol follow-up only `if !self.is_streaming`,
with no log when it is skipped. Headless goes further: `headless/mod.rs:190-191`
and `:199-200` call `stop_stream()` (which only sets the cancel flag) and then
`send_message()` in the same breath, which `send_message` refuses because
`is_streaming` is still true. The loop-detection pivot and the compact-file
finalisation prompt never reach the model.

**T4 — Empty turns are committed and never rolled back.** Severity medium, effort S.
`finalize_partial_response` (`engine/mod.rs:1168-1174`) always calls
`finalize_response`, so an errored or cancelled empty turn writes an empty
assistant message that is sent back to the provider next turn; the
desktop's guard (`message_ops.rs:896-902`) and
`remove_last_user_message` rollback (`message_ops.rs:1190`) have no
terminal counterpart. It also always passes `None` for the trace.

**T5 — Usage and guards are weaker copies.** Severity low–medium, effort S.
`ApiCallUsage` chunks are discarded (`engine/streaming.rs:110-112`), the
four running counters use non-saturating `+=` (`engine/mod.rs:856-859`),
the title trigger counts *display* messages so any system line defeats it
(`engine/mod.rs:1149`), the interactive TUI has no loop guard and no error
classification at all (only headless does, with its own list), and the
progress slot is never cleared after a turn.

**T6 — Duplicated construction.** Severity low, effort S.
`init_conversation` and `spawn_init_conversation` are a 70-line copy of
each other including the 20-field `AgentBuildContext` literal
(`engine/mod.rs:464-529`, `:538-638`).

---

## 4. Suggested order of work

Each step is independently shippable and keeps the goldens green.

0. **Two desktop quick wins first (S each):** canonical-to-canonical
   workspace comparison so the agent stops rebuilding per send (F1), and
   attach an artifact only on the turn after it was produced, without
   persisting the base64 (F2). Both shrink every later measurement.
1. **Core-only mechanical clean-up (S):** one stream mapper instead of two
   macros, `stream_prompt` by value, `TurnUsage(ApiCallUsage)`, usage
   semantics from `agent.provider`, drop the unused return value. Adds the
   missing mapping tests. (B1, B6, B7, A6, C2)
2. **Typed stream errors (M):** `StreamError` produced in core; frontends
   drop their string matching. Unblocks 3 and 4. (A7)
3. **Per-request Azure auth (M):** auth header from the cache on every
   request; `on_chunk` becomes sync. (A1)
4. **Utility model (S):** tool-less completion model for titles and
   summaries. (A3)
5. **TUI parity (S):** snapshot before push (A2), stop cancelling on the
   todo nudge (T2), never drop a follow-up silently (T3), guard empty turns
   (T4) — or fold all four into 7.
6. **Cache-aware compaction (M):** token-budgeted, generational, persisted;
   delete the dead shaper stages; decide C4 first. (A4, C5, C4)
7. **Core turn runner (L, incremental):** move the duplicated orchestration
   and follow-up scheduling into core one piece at a time, TUI first since
   it has the most divergence. (C1, C6)
8. **Shared LLM HTTP client and O(delta) token counting (S each).** (B4, B2)

| Finding | Severity | Effort | Primary file |
|---|---|---|---|
| A1 Azure token baked into agent | High | M | `factories/agent_factory/provider_builder.rs` |
| A2 TUI double user message | High | S | `chatty-tui/src/engine/mod.rs` |
| A3 Title/summary through full agent | Medium | S | `services/title_generator.rs`, `token_budget/summarizer.rs` |
| A4 Shaper defeats caching, dead stages | High | M | `services/context_shaper.rs` |
| A5 `on_stream_ended` skipped on handler error | Low | S | `services/stream_processor.rs` |
| A6 Usage semantics hard-coded | Low | S | `services/llm_service.rs` |
| A7 Stringified errors, per-frontend sniffing | Medium | M | `services/llm_service.rs` |
| B1 History copied 2–3× per turn | Medium | S | `services/llm_service.rs`, both frontends |
| B2 BPE over base64 every turn | Medium | S–M | `token_budget/counter.rs` |
| B3 Double body serialisation | Low | S (upstream) | `factories/agent_factory/prompt_cache_http.rs` |
| B4 `reqwest::Client` per agent | Low | S | `factories/agent_factory/provider_builder.rs` |
| B5 950-line agent build on every open | Medium | M | `factories/agent_factory/mod.rs` |
| B6 Duplicated, untested stream macros | Medium | S–M | `services/llm_service.rs` |
| B7 `TokenUsage` vs `ApiCallUsage` | Low | S | `services/llm_service.rs` |
| B8 Write lock across awaits | Low | S | `services/mcp_service.rs` |
| C1 Orchestration duplicated in frontends | High | L | both frontends → core |
| C2 Dead surface | Low | S | `factories/agent_factory/` |
| C3 Stale provider docs | Low | S | `CLAUDE.md`, `docs/architecture-overview.md` |
| C4 No tool memory across turns (implicit) | Decision | L | `models/conversation.rs` |
| C5 Two context systems | Medium | M | `token_budget/`, `services/context_shaper.rs` |
| C6 Follow-up turns scheduled per frontend | Medium | M | both frontends → core |
| F1 Agent rebuilt per send on path mismatch | High | S | `chatty-gpui/.../message_ops.rs` |
| F2 Artifacts re-sent and re-persisted every turn | High | S | `chatty-gpui/.../message_ops_internals.rs` |
| F3 Full-conversation serialise per save, 3× per turn | Medium | S–M | `chatty-gpui/.../conversation_ops_modify.rs` |
| F4 Process-wide approval notifier | Medium | M | `models/execution_approval_store.rs` |
| F5 Dead per-turn work (canonicalize, rebuilt message) | Low | S | `chatty-gpui/.../message_ops_internals.rs` |
| F6 Follow-ups re-run the whole prologue | Medium | M | `chatty-gpui/.../message_ops_internals.rs` |
| F7 Title not saved, PDF case, empty-turn rules | Low | S | `chatty-gpui/.../message_ops.rs` |
| T2 TUI cancels on todo nudge (AGE-151 regression) | High | S | `chatty-tui/src/engine/streaming.rs` |
| T3 TUI drops follow-ups silently | High | S | `chatty-tui/src/engine/mod.rs`, `headless/mod.rs` |
| T4 TUI commits empty turns | Medium | S | `chatty-tui/src/engine/mod.rs` |
| T5 TUI usage/guard gaps | Low | S | `chatty-tui/src/engine/` |
| T6 TUI duplicated conversation init | Low | S | `chatty-tui/src/engine/mod.rs` |
