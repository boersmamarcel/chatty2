# Chatty

A desktop chat application built with Rust and GPUI.

## Tech Stack

- **UI Framework**: [GPUI](https://crates.io/crates/gpui) - Zed's GPU-accelerated UI framework
- **Components**: gpui-component for UI components
- **LLM Integration**: [rig-core](https://crates.io/crates/rig-core) for LLM operations
- **Async Runtime**: Tokio
- **Serialization**: serde/serde_json for persistence

## Behavioral Guidelines

Guidelines to reduce common AI coding mistakes. **Bias toward caution over speed; for truly trivial changes, use judgment.**

### 1. Think Before Coding

**Don't assume. Surface tradeoffs. Ask when confused.**

Before writing any code:
- State your assumptions explicitly. If uncertain, ask.
- If multiple valid interpretations exist, list them — don't pick silently.
- If a simpler approach exists, say so and push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask before proceeding.

This is especially important in this codebase because GPUI's entity/context model has subtle ownership rules that are easy to misread. When in doubt about `Context<T>` vs `AsyncApp` vs `App`, confirm before writing code that may not compile.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: *"Would a senior Rust engineer say this is overcomplicated?"* If yes, simplify.

In this codebase specifically:
- Prefer existing patterns (globals, events, optimistic updates) over inventing new ones.
- Don't introduce new async primitives when `cx.spawn()` + `.detach()` already covers the case.
- Don't add new crate dependencies unless absolutely necessary.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code or issues, mention them — don't silently fix them.

When your changes create orphans:
- Remove imports/variables/functions that *your* changes made unused.
- Don't remove pre-existing dead code unless asked.

**The test:** Every changed line should trace directly to the user's request.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals before coding:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Reproduce it with a test, then make that test pass"
- "Refactor X" → "Ensure `cargo test` passes before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Always run `cargo test && cargo clippy --all-features --all-targets -- -D warnings && cargo fmt --check` after changes to verify nothing is broken before declaring the task done.

---

**These guidelines are working if:** diffs are clean and minimal, rewrites due to overcomplication are rare, and clarifying questions come *before* implementation rather than after mistakes.

## Project Structure

Chatty is a Cargo workspace of `crates/chatty-*` crates, not a single
`src/` tree. The three crates most of this document talks about:

```
crates/
├── chatty-core/src/          # UI-agnostic domain logic (shared by gpui + tui)
│   ├── auth/                 # Provider auth (API keys, Azure Entra ID)
│   ├── exporters/            # JSONL / ATIF conversation export
│   ├── factories/            # Agent construction (agent_factory)
│   ├── models/                # Data models (conversations, approvals, tokens)
│   ├── repositories/          # Persistence layer (JSON / SQLite)
│   ├── services/               # Domain services (LLM streaming, MCP, memory, …)
│   ├── settings/                # Settings models (providers, models, general)
│   ├── token_budget/             # Token counting / summarization
│   └── tools/                     # LLM-callable tools (shell, filesystem, MCP, …)
├── chatty-gpui/src/          # Desktop app (GPUI)
│   ├── main.rs                # Entry point, initialization, theme handling
│   ├── chatty/                 # Main chat application module
│   │   ├── controllers/         # App controllers (ChattyApp, StreamManager glue)
│   │   ├── models/               # GPUI-specific models
│   │   ├── services/              # GPUI-specific services
│   │   └── views/                  # UI views (chat_view, chat_input, sidebar, …)
│   └── settings/               # Settings window UI
│       ├── controllers/          # Settings window controllers
│       ├── models/                # Settings-page-specific models
│       ├── providers/              # Provider implementations (e.g., Ollama)
│       └── views/                   # Settings UI views
└── chatty-tui/src/           # Terminal app (Ratatui)
```

For the full crate-by-crate split (including the smaller research/support
crates) see `docs/workspace-crate-split.md`.

## Build Commands

```bash
# Build debug
cargo build

# Build release
cargo build --release

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run clippy lints (CI also lints test/bench/example targets via --all-targets)
cargo clippy --all-features --all-targets -- -D warnings
```

Dependencies are built with `debug = "line-tables-only"` and DuckDB with no
debuginfo (root `Cargo.toml`, `[profile.dev.package.*]`); a full test build
still needs about 16 GiB of `target/`. See `docs/build-disk-usage.md`.

## Packaging

Scripts are in `scripts/`:

```bash
# Package for macOS (creates .app bundle and .dmg)
./scripts/package-macos.sh

# Package for Linux (creates .tar.gz)
./scripts/package-linux.sh
```

## Linux Dependencies

For building on Linux, install these system packages:

```bash
sudo apt-get install -y \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libwayland-dev \
  libvulkan-dev \
  libx11-dev \
  libxcb1-dev \
  libxcb-render0-dev \
  libxcb-shape0-dev \
  libxcb-xfixes0-dev \
  libxcursor-dev \
  libxrandr-dev \
  libxi-dev \
  libgl1-mesa-dev \
  libfontconfig1-dev \
  libasound2-dev \
  libssl-dev \
  pkg-config
```

## Architecture Notes

- **Tokio Runtime**: The app uses Tokio for all async operations. The runtime is entered at startup and maintained throughout the application lifecycle.
- **Global State**: Uses GPUI's global state system (`cx.set_global`, `cx.global`) for app-wide state like providers, models, and settings.
- **Async Loading**: Providers, models, and settings are loaded asynchronously to avoid blocking the UI during startup.
- **Stream Lifecycle**: LLM response streams are managed by `StreamManager` (`src/chatty/models/stream_manager.rs`), a centralized GPUI entity that owns all stream state, emits events for decoupled UI updates, and uses cancellation tokens for graceful shutdown. Each registered stream carries a monotonic epoch so a `StreamEnded` event from a superseded stream (e.g. a synchronously re-registered follow-up turn) is ignored rather than tearing down the new one. Both frontends drive chatty-core's shared `run_stream_loop` (`crates/chatty-core/src/services/stream_processor.rs`): the loop, its cancellation checks and its stall watchdog (`STALL_TIMEOUT` = 180s, ends a turn that yields nothing for too long; `--headless` resumes it up to `HEADLESS_STALL_RESUME_ATTEMPTS` times via `decide_recovery`) live there, and the one `StreamChunkHandler` that drives it is chatty-core's own `SessionStreamHandler` (see AgentSession below); neither frontend has a handler of its own any more (AGE-192, AGE-195). Turn-lifecycle behaviour is changed in core, once. `on_chunk` is synchronous: the Azure Entra token is attached to every request by `AzureAuthHttpClient` (`factories/agent_factory/azure_auth_http.rs`, an `HttpClientExt` wrapper over the token cache, AGE-245), so a handler never has to await a token refresh; the trait carries no `Send` bound, since the desktop's event sink holds an `AsyncApp`. Scripted-stream fixtures for both frontends' characterization tests live in `chatty_core::services::stream_fixtures` behind the `test-support` feature, with golden event sequences under `crates/chatty-core/src/services/goldens/`, `crates/chatty-core/src/session/goldens/`, `crates/chatty-tui/src/engine/goldens/` and `crates/chatty-gpui/src/chatty/controllers/app_controller/goldens/` (`UPDATE_GOLDENS=1` rewrites them). See Idiomatic Patterns > StreamManager Pattern for details.
- **AgentSession (AGE-194)**: `chatty_core::session::AgentSession` (`crates/chatty-core/src/session/`) is the turn written once: it owns one `Conversation` plus the three per-agent stores (execution approvals, write approvals, clarifications) whose handles the agent's tools were built with — the owner assembles only the services part of an `AgentBuildContext` and the session completes it (`build_context`), builds and owns the conversation (`create_conversation` / `restore_conversation`) or installs a rebuilt agent on it (`install_agent`; AGE-272) — and `begin_turn` does approval channels → `stream_prompt` → `run_stream_loop` and returns the turn as a future for the frontend to spawn. Every outcome arrives as a `SessionEvent` (`session/event.rs`, the contract every frontend binds to): `TurnStarted` first, `TurnEnded` exactly once and last, `Error`/`Cancelled` before it saying why, and `FollowUp` after it carrying the next turn's prompt. The per-chunk protocol logic that used to live in each frontend's handler — loop guard, malformed-tool-call retry (`MALFORMED_TOOL_CALL_FOLLOW_UP` lives here), usage folding — is `SessionStreamHandler` (`session/handler.rs`), driven by a `TurnPolicy` the session builds from `AgentSessionConfig` (settings by value; the session never calls a repository and holds no `'static` state, so two sessions in one process share nothing). There is no per-chunk todo-protocol nudge any more (AGE-479 deleted `AgentTaskController::observe_tool_result`, the "this has become a multi-step task" prompt injected after a turn's second non-todo tool result with no plan): whether a task needs a plan is gated by the `write_todos` tool description and by tool profile (`coder`/`reviewer` don't offer `write_todos`/`update_todo`/`verify_completion` at all) rather than by a mid-stream nudge or the preamble's old "multi-step todo protocol" paragraph. `AgentTaskController::stream_end_follow_up`, still called from `SessionStreamHandler`, only steers a plan that already exists (an in-progress todo, or a call to `verify_completion` before the final reply) and returns `None` for a turn that never called `write_todos`. The owner feeds events back through `apply()`, which records the turn on the conversation: streaming text, the trace (tool calls, approvals, clarifications, the delegation row; AGE-274), turn messages, the todo snapshot, usage. On `TurnEnded` it calls `finish_turn(trace, artifacts)`, which commits under the shared empty-turn rule; `None` persists the session's own trace, and the desktop passes the ChatView's instead because it carries the user's clarification answers. `finish_turn` is also the turn barrier for `Conversation`'s lifetime totals (AGE-351): it counts that turn's `TraceItem::ToolCall`s into `tool_call_count`, sets `context_tokens` to the last completed API call's prompt size (`input + cache_read + cache_write`, output excluded), and — since it is the only place a turn's usage is priced — calls `usage.calculate_cost` against the `TokenPricing` (`ModelConfig::token_pricing()`) bound to the conversation wherever its model is bound (`create_conversation` / `restore_conversation` / `install_agent`, which takes `&ModelConfig` rather than a bare `model_id` for this reason); an unpriced model leaves the turn's cost `None`. What a delegated worker spent is folded in the same way (AGE-415): `note_delegation` collects the `TokenUsage` off each `InvokeAgentProgress::Finished` the turn saw (a failed task's usage counts too — it still spent the tokens), and `finish_turn` prices each at the conversation's own bound pricing and adds it via `Conversation::add_delegated_usage` *ahead of* the turn's own usage, as its own `TokenUsage` line with `delegated_to` naming the worker; `total_cost` and the totals carry the whole delegated tree while `last_usage()` and `context_tokens` stay this agent's own (a worker's prompt is not this agent's context fill). The wire carries this as `metadata.usage` on the terminal status (`services::a2a_client::usage_from_status_metadata`, read by `invoke_agent_tool`), and a sub-leader's own `TaskMapper` folds its workers' usage into that number before reporting it up, so a root sees one delegated line per direct child regardless of nesting depth. A leader that wants to judge a worker's derivation rather than just its answer can ask for it explicitly (AGE-467): `invoke_agent`'s `include_trace` arg (default `false`) is off unless asked, since a plain delegation must cost no extra context. When set, the worker's `TaskMapper` renders its own tool calls into a compacted trace (per-field char caps, a head/tail step cap, a whole-trace char cap) and attaches it under `trace` on the same terminal-status metadata, next to `usage`; `services::a2a_client::trace_from_status_metadata` reads it back and `InvokeAgentOutput.trace` (`#[serde(skip_serializing_if = "Option::is_none")]`) surfaces it to the model only when it is `Some`. Because the session holds no `RepositoryRegistry`, prices are fixed at bind time, not looked up live per turn, so a price edited in Settings mid-conversation applies from the next model switch or reload. `recovery_action(&error)` is the owner's stream-error policy (`decide_recovery` for the session's surface) with the attempt bookkeeping inside the session, per error kind and reset by a human turn (AGE-273); headless is the one surface that retries, and sends its recovery prompt as a `ProtocolFollowUp` turn so the budget holds. A turn's context is guarded inside the tool loop, not before it (AGE-504): every chat agent carries the rig hook `ContextShaper` (`services/context_shaper.rs`), which before each model call measures the history in tokens against the model's window (`max_context_window`, else an assumed 32k) minus the reply reserve, the measured preamble-plus-tool-schema base and the prompt, and only when over budget hands rig a shorter history for that one request (`RequestPatch::history`: cap oversized tool results outside the last 8 messages, one-line them, snip the middle, then the tail oldest-first, never the last message) — nothing is persisted and a request that fits goes out as recorded; the one exception is a single tool result larger than two fifths of the budget, which the same hook's `on_tool_result` cuts once, at recording, since the prompt is never shaped and such a result could never be sent. Because the guard runs before every model call rather than once between turns, the snip usually lands inside a live tool loop, so its cut steps off a tool round-trip before firing (AGE-512): the kept tail starts no later than the assistant message issuing the calls it answers and the kept head ends no later than the last call it still answers, since splitting the two is a 400 from every OpenAI-compatible provider on the request the guard itself built. `docs/context-compaction.md` has the numbers. Silence is not an answer (AGE-401): every chat agent carries the rig hook `EmptyTurnRetry` (`factories/agent_factory/empty_turn_retry.rs`), which rejects the first tool-free model turn with no text and no tool call — reasoning alone does not count, which is how a qwen3 tool call written into the thinking channel looks — and retries it once inside the same turn with `EMPTY_COMPLETION_FOLLOW_UP`; a still-empty final completion is reported by the handler as `StreamErrorKind::EmptyCompletion` (`Stop` on every surface), so headless exits non-zero with the error on stderr and a worker's task ends `failed` instead of reaching its leader as done. Adapters: `From<SessionEvent> for AppEvent` (chatty-tui `events.rs`) and `StreamManager::handle_session_event` (chatty-gpui). Both frontends run on it (AGE-195): chatty-tui's `ChatEngine` owns one `AgentSession` and keeps only display state; on the desktop `ConversationsStore` holds an `AgentSession` per loaded conversation (`get_session`/`get_session_mut`, with `get_conversation` delegating), so each conversation's agent raises approvals on its own stores — there are no app-wide approval-store globals any more, and the approve/deny/clarification UI resolves a request through `ConversationsStore::resolve_execution_approval` / `resolve_write_approval` / `resolve_clarification`, which find the session that raised it. `run_llm_stream` (`message_ops_internals.rs`) is the desktop's event sink (`DesktopSink`): it calls `session.apply` for the conversation, forwards to `StreamManager`, and keeps the desktop-only work — ChatView trace capture before `Error`/`TurnEnded`, the delegation row and plan strip in the view, follow-up injection. Raw `SessionEvent::Text` chunks are coalesced upstream of both calls by a `TextBatch` (`stream_manager.rs`) the sink fills, sharing `StreamManager`'s `should_flush_text` policy (`FLUSH_INTERVAL` = 20ms; see Idiomatic Patterns > StreamManager Pattern), and any non-text event flushes it first (AGE-166). That batch is shared with `StreamManager` (`attach_text_batch`), since the sink dies with the turn's future: the manager's flush timer drains it, and so does every path that drops the task — `stop_stream`, `cancel_pending`, both supersede paths — or Stop would truncate the reply in the UI and in the persisted message (AGE-372). `finalize_completed_stream` and `finalize_stopped_stream` call `finish_turn`; an errored desktop turn is finalized like a stopped one, so no user message is left dangling. `begin_turn_with_flag` lets `StreamManager` register the turn with the cancel token before the turn starts. Each frontend's characterization replays the scenarios through the session and its adapter (`engine/characterization.rs`, `session_characterization.rs`). Session `TurnKind::Regenerate` takes the tail user message off the history snapshot and resends it; nothing is added. Headless and `--pipe` ride the session directly (AGE-196): `chatty-tui/src/headless/runner.rs`'s `HeadlessRunner` owns an `AgentSession` and a `Transcript` (`engine/transcript.rs`, the terminal transcript bookkeeping shared with `ChatEngine`) and nothing of the terminal; both assemble an `AgentServices` and hand it to `AgentBuildContext::from_services` (`chatty-core/src/factories/agent_factory/build_context.rs`), the single place the services half of a build context is written — chatty-gpui and `hive`'s `chatty-server` use the same seam, and `gated_exec_settings` is the shared gate deciding whether an agent gets execution tools at all (AGE-293). A delegated child reports its turn to the parent through the runner's event observer — the broker participant's socket (`chatty-tui/src/participant/`, AGE-301) — and `SessionEvent` is serde-serializable so it can cross that boundary. Stderr is the child's human-readable log and nothing else, which `/agent` shows. There is no other progress protocol. A child's `ask_user` is not cancelled when a parent is listening: it parks the child's task in A2A `input-required` with the question attached, the parent's `invoke_agent` re-asks it on the parent's own clarification store (the same popover, or one more hop up if the parent is a worker too), and the answer comes back down as a broker `input` frame that `chatty_protocol_gateway::worker::answer_clarifications` resolves on the child's store (ADR-0011 C7, AGE-306). A lone `--headless` agent still cancels, since nobody is listening; a `--team` leader has no `event_observer` either, but `HeadlessRunner::handle_event` special-cases `is_team_leader()` to answer with a canned "no human is available, use your best judgment and proceed" default instead of cancelling, so a worker's relayed question (or the leader's own `ask_user`) doesn't hard-fail the turn (AGE-452).
- **Conversation mailbox (AGE-482)**: A message sent while a conversation's turn is streaming used to have nowhere to go — the session refused it, both composers hid Send, the TUI ignored Enter — except for one single-slot `pending_agent_follow_up` field duplicated across chatty-tui's `ChatEngine` and `HeadlessRunner`. `chatty_core::session::mailbox` (re-exported as `Arrival`, `Decision`, `Mailbox`, `Queued`, `QueuedId`, `Refusal`, `TurnEnd`) is a pure state machine — no I/O, generic over the owner's message type (`String` in chatty-tui, a `QueuedSend { message, attachments }` on the desktop) — replacing that field and its two duplicates with one per-conversation queue (`DEFAULT_MAILBOX_CAP` = 5). Its owner feeds it an `Arrival` (`Message`, `Interrupt`, `FollowUp`, `Stop`, `Withdraw`) and gets back a `Decision` (`Dispatch`, `Queued`, `Cancel`, `Withdrawn`, `Refused`, `Nothing`) to execute with verbs it already has (start a turn, cancel a turn); `turn_ended(TurnEnd)` says what runs next. Rules: idle dispatches immediately; a `Message` while running is appended (a sixth is `Refused` with `MailboxFull`); `Interrupt` pushes to the front and cancels the running turn; `Stop` cancels and *holds* the queue — nothing fires until the user sends again, which dispatches the held front and appends the new message; a `FollowUp` (the loop's own continuation, AGE-242/D3) takes the one front slot a second `FollowUp` is refused for. On turn end, `Completed` drains the front unless held, `Cancelled` drains only when an `Interrupt` caused it (otherwise it holds, same as `Stop`), and `Error` always holds. chatty-tui's `ChatEngine`/`HeadlessRunner` each own a `Mailbox<String>`; `/now <text>` sends an `Interrupt`, `/unqueue` withdraws the most recently queued message, and the status bar shows `N queued`. The desktop's `ChattyApp` owns a `mailboxes: HashMap<conv_id, Mailbox<QueuedSend>>`; `send_message`/`send_protocol_follow_up` route through it, Send stays visible (not swapped for hidden) while a reply streams, and queued messages render as bubbles below the transcript (`ChatView::render_queued_strip`) with × (`WithdrawQueued`) and ↑ (`SendQueuedNow`, an `Interrupt`) in front. `TurnKind`, `SessionEvent`, `chatty-server` and the participant wire are unchanged; a conversation switched away from keeps its queue and only runs it once the user sends there again.
- **Conversation mode: Local vs. Hosted (AGE-298)**: A conversation carries a `ConversationMode` (`Local | Hosted { server_url, remote_id }`, `chatty_core::models::conversation`) recording whether its turns run in-process or on a `chatty-server`. `Local` is the absent case: it lives on `Conversation`/`ConversationSnapshot`/`ConversationData` as `Option<String>` JSON, so a conversation that never moves persists exactly the bytes it did before the mode existed, and a pre-AGE-298 row loads as `Local`. SQLite migration 4 adds a nullable `mode` column; `ConversationMetadata` carries the same serialized mode so the sidebar can badge a hosted conversation (`ConversationsStore::is_hosted`) without loading it. The frontend keeps its `AgentSession` either way — it owns the local `Conversation`, applies every event, and finishes the turn — so a hosted conversation only adds a `HostedSession` (`session/hosted.rs`) alongside it in `ConversationsStore`'s `hosted: HashMap<String, HostedSession>` (kept in sync with each conversation's own mode, never set independently). `HostedSession` turns one SSE response body back into the same `SessionEvent` sequence a local turn would emit — the wire is a serialization of `SessionEvent` (AGE-281), so nothing here invents a vocabulary or interprets an event — and guarantees the same `TurnStarted`…`TurnEnded` pairing even when the server never accepts the turn. `session::transport` (re-exported as `turn_transport`) is four free functions — `begin_turn`, `is_turn_active`, `cancel`, `resolve_approval`/`resolve_clarification` — that dispatch to the local session or the `HostedSession` depending on which is present; it is functions rather than a trait because the turn future must stay `Send` for chatty-tui's `tokio::spawn` while `emit` must not be `Send` for chatty-gpui (`AsyncApp`-holding sink), and `futures::future::Either` is `Send` exactly when both arms are. `session::move_conversation` holds the move itself: `take_online`/`fetch_hosted` transfer history over HTTP, and the mode flips only after that succeeds — a client killed mid-move leaves the local conversation untouched. `MoveSummary` (`TAKE_ONLINE_SUMMARY`/`BRING_BACK_SUMMARY`) is the single source both the TUI's `/online` command and the desktop's move-confirmation dialog (`MoveConversationDialog`, sidebar globe badge) read to tell the user what does and does not move — workspace files, attachments, MCP servers, memory, skills and provider keys never move. `refuse_reason` blocks a move mid-turn (a pending approval/clarification lives in the stores of the session that raised it) or onto/off the mode a conversation is already in. Bringing a conversation back reconciles rather than overwrites: the server's history is adopted via `Conversation::import_history` only when it is longer than what this client already recorded.
- **Theme System**: Themes are loaded from `./themes` directory. User preferences (theme name + dark mode) are persisted to JSON.
- **Math Cache**: LaTeX math expressions are compiled to SVG using Typst and cached in platform-specific directories:
  - **macOS**: `~/Library/Application Support/chatty/math_cache/`
  - **Linux**: `~/.local/share/chatty/math_cache/` or `$XDG_DATA_HOME/chatty/math_cache/`
  - **Windows**: `%APPDATA%\chatty\math_cache\`
  - Base SVGs (`{hash}.svg`) are cached indefinitely for reuse
  - Styled SVGs (`{hash}.styled.{color_hash}.svg`) are theme-specific variants that are cleaned up on app restart
  - The generated Typst sets `fill: none` on the page. Typst's default page fill is white for SVG export, which would put an opaque white card behind every equation in a dark theme
  - The `inject_svg_color()` method rewrites six-digit black `fill`/`stroke` attributes to the theme colour (it injects no CSS and does not touch backgrounds)
  - The lookup runs at parse time only: `resolve_math_segments` stores each equation's styled path in `CachedMathSegment` (the parse cache, keyed by content + theme mode + foreground colour), and the render pass builds `img(path)` from it. `render_to_svg_file` checks the disk cache before the 200-entry in-memory one, so a message with more distinct equations than that never recompiles Typst; the embedded Typst fonts are parsed once per process (AGE-394)
  - `MathRendererService::CACHE_VERSION` is hashed into the cache key: base SVGs are written once and never cleaned, so a rendering change needs a bump to reach existing installs
- **Pdfium Library Cache**: The pdfium native library is cached in a persistent user-data directory so it survives bundle corruption, app translocation, and partial auto-update rsyncs. Lookup order in `pdfium_utils::create_pdfium()`: user-data cache → exe-relative → `CHATTY_PDFIUM_LIB_DIR` env → compile-time `PDFIUM_LIB_DIR` → system. On every successful bind, the library is opportunistically copied to the cache (`self_heal_copy()`). Platform paths:
  - **macOS**: `~/Library/Application Support/chatty/lib/`
  - **Linux**: `~/.local/share/chatty/lib/` or `$XDG_DATA_HOME/chatty/lib/`
  - **Windows**: `%APPDATA%\chatty\lib\`
- **Pdfium Thread Safety (AGE-176)**: pdfium is not thread-safe — concurrent binds hit a Chromium `CHECK` (SIGTRAP) and concurrent renders corrupt its global font mapper (SIGSEGV/SIGABRT). `create_pdfium()` therefore returns a `PdfiumHandle` (`crates/chatty-core/src/services/pdfium_utils.rs`) that holds a process-wide `Mutex` for its whole lifetime (`Deref<Target = Pdfium>`; documents/pages borrow from it and can't outlive the lock), serializing every caller — including the three PDF tools, which run pdfium on `spawn_blocking`. Never call `create_pdfium()` while already holding a handle on the same thread; the lock isn't re-entrant. This is why `cargo test` no longer needs `--test-threads=1` (the CI/Makefile workaround was removed once this landed).
- **PPTX Slide Rendering (AGE-343)**: The artifact panel's Rendered tab for a `.pptx` shows real slide pixels, not the AGE-138 text cards built from a hand-rolled `zip`+`roxmltree` walk. `chatty_core::services::pptx_render` opens the deck with `rpptx::Presentation` and renders it once to a PDF via `to_pdf_deterministic()` (slide N == page N), cached per (path, mtime) in the session temp directory so `write_pptx` rewriting a deck in place doesn't serve a stale slide; each slide turn is then one `pdf_thumbnail::render_pdf_page` raster, reusing the PDF viewer's rasteriser and page cache. `PptxPreview` (`artifact_view.rs`) mirrors `PdfPreview` — one image at a time, fit to the panel width via the shared `fit_to_width_page` helper (AGE-472) — instead of holding the whole parsed deck. `pptx_tool::read` and the panel's Source tab now share the same `rpptx`-based reader (`read_pptx`), so the tool's wire format is unchanged (pinned by a golden-text test) even though the parser underneath is not. The `pptx` Cargo feature now implies `pdf`, since rendering a slide means rasterising a PDF page. **Pinned fork dependency**: `rpptx` is pinned to an exact version (`=0.12.1`) and, via a root-`Cargo.toml` `[patch.crates-io]` block, three of its `rdocx` dependencies (`rpptx`, `oxml-layout`, `oxml-pdf`) are sourced from `boersmamarcel/rdocx` at a fixed git rev rather than crates.io — the published 0.12.1 doesn't compile alongside `usvg` (a non-exhaustive `fontdb::Source` match under feature unification) and mishandles real decks (slow media probing, oversized output PDFs, dropped low-bit-depth PNGs). Each fix is a separate upstream-facing branch with its own regression test. Drop the `[patch.crates-io]` block and bump the `rpptx` pin once a crates.io release carries these fixes; until then, a plain `cargo update` will not move `rpptx` off the fork.
- **Sandbox Container Lifecycle**: `SandboxManager` (`crates/chatty-core/src/sandbox/manager.rs`) lazily starts one long-lived Docker container per language (`sleep infinity`, no `--rm`), so a leaked container outlives the process. There is no explicit call site that tears these down when a conversation ends, is deleted, or gets its agent rebuilt — ordinary `Arc` drop is the only signal — so cleanup is a `Drop` impl on `SandboxManager` rather than a call any of those paths must remember to make. `Drop::drop` can't await the async Docker removal, so it only spawns a detached task on the already-entered Tokio runtime (`Handle::try_current()`); detached tasks are cancelled when the runtime is dropped, which happens immediately after `app.run` returns, so a manager dropped that late would still leak. Every `SandboxManager`'s container map is therefore also held by a process-wide registry (`LIVE_SANDBOXES`) from construction until its cleanup actually completes, and both frontends drain it synchronously on the way out: `chatty-gpui/src/main.rs` blocks on `sandbox::shutdown_all()` after the window closes but before `_tokio_runtime` drops, and `chatty-tui/src/main.rs` awaits it after `app::run`/headless finishes. `destroy()` remains a real `pub async fn` for callers that want a graceful, awaited shutdown instead of the detached best-effort one. A startup sweep for containers orphaned by a crash or `SIGKILL` is deliberately not implemented.
- **Built-in Browser** (AGE-142, `browser` feature): A CDP control layer over a real Chrome, in `crates/chatty-core/src/services/browser/`. Headless. By default the agent's tools reach only `localhost` and workspace-local `file://` URLs (Lane A) — the self-review loop (render → screenshot → critique → fix) carries no credentials and needs no approval gate. When the app's internet-access setting (`ExecutionSettings::fetch_enabled`, the same toggle that gates `fetch_tool`/`search_web`) is on, the agent factory builds an open-web manager (`BrowserManager::open_web`) instead: `NavigationPolicy::Open` allows any public http(s) host too, still refusing private/internal network targets via the same SSRF denylist as `fetch_tool` (`services::ssrf_guard`, shared so the two can't drift) — unless `ExecutionSettings::allow_private_network_access` (AGE-459, off by default, its own settings-page toggle) opts the workspace into reaching its own LAN, in which case only the link-local/cloud-metadata range (`169.254.0.0/16`) stays refused; `fetch_tool` never sees this flag. The profile stays ephemeral either way — no stored credentials, so still no approval gate. The navigation policy lives on the profile (`profile.rs`), not in the tools, so Lane B's per-task origin allowlist (`AGE-158`, not implemented yet) extends it rather than replacing it. Element refs from `browser_snapshot` carry a generation that navigation invalidates; a stale ref is refused, never mis-resolved. `browser_click` (AGE-489, `services/browser/click.rs`) is the one way the agent interacts with a page — a ref in, a single left click out, no coordinates, no selectors, no JavaScript — and refuses before clicking: the live role+name (`Accessibility.getPartialAXTree`) must still match what the snapshot showed, the control lock and navigation policy must allow it, the element must be on screen after `DOM.scrollIntoViewIfNeeded`, and the click point's hit test (`DOM.getNodeForLocation`) must land on the target or a descendant rather than a covering overlay. A click on a page outside Lane A (loopback http(s) or workspace `file://`) always raises the existing execution-approval card, decided by the page's URL rather than the manager's policy, and always asks regardless of the shell's auto-approve mode — approval gating stays per-action rather than generalized (AGE-158's decision) — with the snapshot generation re-checked after the user answers. `browser_type` (AGE-492, `services/browser/typing.rs`) fills a text field on the same footing as `browser_click` — ref only, no selectors or coordinates — sharing that tool's resolve → gate → re-check-generation sequence as one `prepare_action`: it additionally refuses before typing into a `<input type="password">` or any `autocomplete="cc-*"` field (checked via `DOM.describeNode`, so the agent can't type credentials — the user does that under take-control), focuses with the same verified pointer events as a click, replaces the field's contents (a `selectAll` Blink editing command then `Input.insertText`, never appends), and reads the AX `value` back to confirm the text landed rather than silently "succeeding" on a read-only field. Every approval card raised for a click or a type trims the page URL for display (`display_url`, AGE-492): the host is always kept, the path may be cut with `…`, and the query/fragment are dropped outright rather than truncated, since they can carry session tokens the transcript would otherwise persist. Console and network output is drained to files under `<workspace>/.chatty/browser/` and summarized, rather than dumped into context. `browser_screenshot` queues the PNG through `PendingArtifacts` (the `add_attachment` path) rather than returning `ToolResultContent::Image`: rig rejects tool-result images for OpenRouter, Ollama and OpenAI Chat Completions, and the conversion error kills the whole stream. The consequence is that the model sees a screenshot on the turn *after* it captures one. When a Lane A browser tool call appears in the transcript, `ChatView::maybe_open_browser_artifact` (chat_view/mod.rs) auto-docks a live artifact panel: it looks up the running conversation's `BrowserManager` via `services::browser::registry` (a `conversation_id → Arc<BrowserManager>` map the agent factory populates, since chatty-core has no other path to chatty-gpui's UI layer) and streams `Page.startScreencast` frames (`screencast.rs`) into it — watching the agent drive the page live, not a one-shot screenshot. The panel forwards mouse/keyboard input over CDP (`input.rs`) so the user can type/click directly. A `ControlLock` (`control.rs`) arbitrates: the agent holds control by default and needs no permission to act, but the user can *take* control at any moment (no negotiation), which refuses mutating session actions (navigate, resize) with `BrowserError::ControlHeldByUser` until released — read-only tools (snapshot, screenshot, console, network) are never gated, since watching never collides. Each real take/release transition (AGE-156) is recorded in the activity trail as a synthetic, already-finished tool row (`ToolCallBlock::browser_control_handoff`, classified as its own `ToolKind::Handoff` so it tallies as "N browser handoffs" instead of files explored) — `ChatView::record_browser_control_change` decides whether the row joins the live streaming trace (mid-turn; the stream's own finalization persists it) or is appended straight to the last assistant message via `Conversation::append_trace_item_to_last_assistant` (post-turn; persisted immediately), so the view and the store never disagree about which path was taken. A release with no turn running also resumes the agent (AGE-379): `ChattyApp::handle_browser_control_changed` sends a user message — "I've handed the browser back to you at `<url>`. Take a fresh look at the page and continue from where I left it." — since every element ref the agent held is stale after a takeover; a release mid-stream leaves it to the running turn's next tool call, which simply succeeds. A session drives one page at a time but tracks every page target it knows about (AGE-458, AGE-473): a link with `target="_blank"`, a `window.open` call or an OAuth sign-in popup creates a second CDP target, and `targets.rs` watches `Target.targetCreated`/`targetDestroyed` (chromiumoxide already turns on `Target.setDiscoverTargets` and attaches to what it finds, so nothing here re-implements attachment) and hands every new *page* target to `BrowserSession::track_target`. `BrowserSession` holds an ordered `Tabs` list — one `Tab` per open target with its `Page`, title, URL and a per-tab `blocked` flag — plus the one `active` page every consumer drives; the page the session launched with is simply the first entry, not a special case. A new tab the policy allows becomes active: the screencast moves to it keeping the same `watch` channel (so the panel just shows the new tab), forwarded input and the agent's tools follow it, element refs are invalidated as a navigation would, the address bar gets its URL, and its console/network output takes over the same buffers (those pumps are each `Tab`'s own `listeners`, running only while it is the active tab and aborted the moment it stops being one, so a tab nobody is driving cannot keep writing into what the tools read). The tab list is broadcast on `BrowserSession::watch_tabs` (a `watch::Sender<Vec<BrowserTab>>` next to `watch_url`, published with `send_replace` so a late subscriber sees the current list), and the artifact panel mirrors it into `ArtifactView::browser_tabs` with a tab-watcher task (`start_tab_watcher`), the same shape as the address bar's — the panel is woken by the session, never polls it per frame. The browser's tabs share the artifact panel's existing header tab bar (`artifact-files`) with the open files rather than getting a strip of their own: `artifact_tab_model` lays out one entry per open file (`ArtifactTabTarget::File(ix)`) followed by one per browser tab (`ArtifactTabTarget::Browser(id)`, label from `browser_tab_label`: title, else URL, else "New tab"), and returns the selected index — the on-screen file while a file/table/chart shows, else `files.len() + active-tab index` while the browser shows. The bar renders once that list has two or more entries. A browser entry carries a `Globe` prefix (a `CircleX` when blocked) and its own × close button; a file entry has neither. Clicking routes through `ArtifactView::select_artifact_tab`: a file entry opens the file (which pauses the browser, below); a browser entry brings the browser back to the screen if a file was in front of it and switches to that tab (`BrowserSession::select_tab`) unless it is already active; the × calls `BrowserSession::close_tab` with `cx.stop_propagation()` so closing never also selects. Selecting a tab and closing one both take control first, like the address bar, since they change what the agent's tools address. Because a browser can be *paused* — a file shown in front of it while its session and tab list stay alive so its entries stay in the bar (`pause_browser`, used by `open`/`open_table`/`open_chart`) — rather than only fully torn down (`drop_browser`, used by closing the panel and by session review), an explicit `browser_shown` flag, not `browser_manager.is_some()`, is what tells `render` the browser is the artifact on screen; the tab watcher is keyed on its own generation so it outlives a pause while the single screencast (stopped on pause, per AGE-155) does not. `select_tab` is the same `activate` path a new tab takes; `close_tab` removes the tab from the list *before* closing the CDP target, and a tab closed by the page itself (`window.close()`, `targetDestroyed`) goes through the same `remove_tab`: when the active tab goes, its left-hand neighbour (or the new first tab) takes over, and when the last one goes, `reopen_blank_page` opens `about:blank` and tracks it like any other tab, so the panel is never left on a dead reference. `track` dedupes by target id under the lock, since the watcher's `targetCreated` and `reopen_blank_page` can both learn about one page. Inactive tabs are not screencast — there is exactly one live `Page.startScreencast`, on the active tab — and the strip shows only their title. Nothing about agent-driven navigation changed: `browser_navigate` and every other tool act on whichever tab is active, and no tool call ever opens a tab. **What the policy actually covers, since tracking widens what the agent can see:** `NavigationPolicy` is checked when a tab is tracked *and again on every main-frame navigation the page makes on its own* — `targets::spawn_navigation_guard` runs one guard per tracked tab for the tab's whole lifetime, active or not (the task ends by itself when the target is destroyed), listening for `Page.frameNavigated` and calling `BrowserSession::page_navigated`, and for `Page.loadEventFired` to read `document.title` for the strip (never off a blocked tab). One check is not enough: an opener can `window.open()` a blank window (nothing to refuse — a target that has not navigated, and Chrome's own `chrome-error://` page, are exempt via `is_exempt_url`, or a failed load would look like a violation), let it be shown, then assign its `location`. A tab that lands somewhere refused — the session's own page, a popup on screen, or a *background* tab — is blocked at once, per tab: the tab stays in the strip marked blocked with the URL it was refused at, and a new tab that opens straight onto a refused URL is tracked blocked and not activated. While the *active* tab is blocked, `page()`/`events()` (so every tool), forwarded input and `reload_as_user` return `NavigationRefused`, the event buffers are cleared and the cast is suspended with an explanatory `ScreencastUpdate::Error` rather than showing the page; `start_screencast` on a blocked tab creates the cast suspended (`screencast::hold`) so a switch to another tab has a channel to revive. A blocked background tab affects nothing on the active one. Navigating somewhere allowed — by tool, by address bar, or by the page itself — lifts that tab's block and revives the cast if it is the active one; selecting another tab shows that tab instead. What this does *not* police: subresources and iframes a page loads (only main-frame document navigations are checked, as `browser_navigate` only ever checked documents), and a blocked tab is left loading invisibly rather than closed — the user closes it from the strip. The screencast channel belongs to the viewer, not to a page (`screencast::Screencast`): the cast underneath moves, pauses and restarts without the panel's receiver closing, so a CDP failure while moving it leaves a live channel carrying the reason instead of reporting a session that ended. Click/input mapping (`browser_viewport_position` in `artifact_view.rs`) goes through the frame actually on screen — a `FrameGeometry` built from the raster's pixel size plus the CSS viewport Chrome reports alongside it (`ScreencastFrame::css_width`/`css_height`) — never through `browser_requested_size`, which is only what the viewport was last *asked* to be and disagrees with the frame on screen during a resize debounce, after a refused CDP retarget, or once Chrome downscales the raster to `maxWidth`/`maxHeight`. The frame is painted by hand with `window.paint_image` off the same canvas element that records its bounds, not `img().object_fit(Contain)` — that painted nothing in `ArtifactMode::Full` (not root-caused inside gpui) while the same frame painted fine once the panel was docked again. Chrome is not bundled: `provisioning.rs` prefers a cached pinned Chrome for Testing build, then an installed Chrome/Chromium/Edge at or above `MIN_CHROME_MAJOR`, then downloads the pinned build and verifies it against a SHA-256 committed in that file. Regenerate the pin with `scripts/pin-chrome.sh`. Platform cache paths:
  - **macOS**: `~/Library/Application Support/chatty/browsers/<version>/`
  - **Linux**: `~/.local/share/chatty/browsers/<version>/` or `$XDG_DATA_HOME/chatty/browsers/<version>/`
  - **Windows**: `%APPDATA%\chatty\browsers\<version>\`
- **Sidebar file explorer + Cmd/Ctrl+P quick-open (AGE-480, moved from the artifact panel's AGE-476/478)**: The left sidebar doubles as a light IDE navigator, toggled by a Chats | Files control in the status footer bar, next to the warning/error indicators (`StatusFooterView`, `SidebarView::set_mode`, two ghost icon buttons — Chats uses `CustomIcon::MessageSquare`, Files uses `CustomIcon::FolderTree`) — Files mode replaces the conversation list with the tree, collapses with the sidebar, and persists across restarts in `GeneralSettingsModel.sidebar_mode` (`SidebarMode::Chats | Files`, `#[serde(default)]`), applied on boot from the same callback that restores the theme (`SidebarView::apply_persisted_mode`, `general_settings_controller::update_sidebar_mode` to save). `SidebarView.explorer: Option<FileTree>` (`views/transcript/file_tree.rs`, pure model; `sidebar_file_tree.rs`, the pixels, `render_sidebar_file_tree`) is rooted at `active_workspace_root` — the active conversation's own working directory, else `ExecutionSettingsModel::workspace_dir`, else the process cwd, the same fallback the tools use — and re-roots when the active conversation changes (`SidebarView::reroot_for_conversation`, hooked into `display_loaded_conversation`), not just when Files mode is first shown. Opened from the titlebar/floating artifact picker ("Files" → `SidebarView::show_files_mode`, which also expands a collapsed sidebar) or the status footer toggle; the artifact panel is a pure viewer again — no explorer column, no `PanelLeft` toggle, no `ARTIFACT_PANEL_WIDTH_WITH_EXPLORER` widening, just the fixed `ARTIFACT_PANEL_WIDTH` (380 px). The tree lists directories lazily on expand (folders first, case-insensitive, `.git` hidden, an empty folder is still a folder) and re-lists an expanded directory when its mtime moves, polled from `render` at most every `SYNC_INTERVAL` (2 s) — a directory's mtime changes on add/remove/rename in it, so agent-written files appear without a watcher crate. A file click emits `SidebarEvent::OpenFile(path, root)`, handled in `app_controller` by calling `ArtifactView::open_from_sidebar` (the tree itself holds no reference to the panel); a directory click toggles it in place. Root-level create ("New file…"/"New folder…") lives only in the header's and empty-folder placeholder's context menu (`root_new_menu`) — there are no header buttons, per the issue's v1 scope. Rows otherwise carry the same `ContextMenuExt` menu as before (Open on a file, Rename, Delete, Reveal, Copy path); create/rename use one inline `explorer_input` rendered in the row's slot (`PendingEdit` → `RowKind::Editor`), focused from the *next* render via `explorer_focus_pending` because the menu's dismissal would otherwise steal it — Enter commits, Escape (handled on the row) cancels, blur commits a typed name and drops an empty or refused one; a refused name keeps the row with `FileTree::error` under the tree (a failed save no longer surfaces there too — AGE-476's `tree.set_error` on a write failure was dropped in the move since the tree no longer lives next to the editor; it still logs via `warn!`). Delete asks first (`window.open_dialog(...).confirm()`), then `remove_dir_all`/`remove_file`; there is no trash. `FileOp` (emitted as `SidebarEvent::FileOp(op, root)`, handled by calling `ArtifactView::apply_file_op`) tells the panel what to do after: a created file opens straight into the Source tab, a rename re-points `files`, `unsaved` and `path`, a delete closes the tabs under it; `rebase_path` does the re-pointing (never `to.join("")`, which leaves a trailing separator that `std::fs` treats as a directory). **Multi-select and drag-to-move:** `FileTree` holds an ordered `selection: Vec<PathBuf>` (last = primary, what `selected()` reports) plus a Shift `anchor`; `click_select` takes a `SelectGesture` from the click's modifiers (`sidebar_file_tree::gesture_for`: Shift → `Range` over the *visible* rows between anchor and click, Ctrl/⌘ (`Modifiers::secondary`) → `Toggle`, else `Single`), and only `Single` activates (opens a file / toggles a folder). Rows are gpui drag sources (`on_drag` with a `DraggedEntries { paths }` payload — the whole selection when a selected row is picked up — and a `DragPreview` entity as the ghost) and drop targets (`drag_over` highlights with `theme.drop_target` unless the drop would be a no-op; `on_drop` moves into the folder row, or into a file row's folder); the list body is the drop target for the root — gpui stops propagation after a row's drop, so it never double-fires. `FileTree::move_entries` renames with `std::fs::rename` (same filesystem only), skips entries already there and a folder dropped into itself or a descendant, refuses a name clash per entry, then selects the moved entries at their new paths; `delete_many` skips children of a folder in the same batch. The row's context menu acts on the whole selection when the row is part of one (`Delete N items…`, `Copy paths`; Rename is hidden), and `delete_prompt` names up to five of them. Editing in the panel is unchanged: `dirty` is "editor text ≠ `source`", kept from the editor's own `InputEvent::Change` (which `set_value` also emits, so a programmatic sync compares equal and clears it). Save is Ctrl/Cmd+S on the panel (`is_save_keystroke` in the same `on_key_down` as Escape) or the header's Save button; `save_current` writes the file, refreshes `source`/`rendered`/`headings`/`loaded_version` and forces an outline re-sync without touching the editor. `leave_current_file` stashes a dirty buffer into `unsaved: HashMap<PathBuf, String>` before any other artifact takes over (`open`/`open_table`/`open_chart`/`open_browser`/`open_review`) and `sync_editor` loads it back over the disk text when that file returns; `reload_from_disk` drops it on purpose. File tabs have a dirty dot and their own × (`close_file_tab`, a dirty tab asks to discard; `remove_file_tab` shows the neighbour or, with nothing left, `clear_document`, which now closes the panel unless a browser is still up — there is no explorer-up placeholder case any more since the explorer isn't in the panel). **Cmd/Ctrl+P quick-open** (`quick_open_dialog.rs`, new in AGE-480) is a separate fuzzy "Go to File" picker over the same root (`active_workspace_root`), reusing the tree's `.git`-hidden rule via `file_tree::walk_files` (eager, capped at `MAX_QUICK_OPEN_FILES` = 20,000, so a huge workspace or a symlink cycle can't hang it) rather than the tree's lazy per-directory listing; it ranks a plain case-insensitive subsequence match (`fuzzy_score`: rewards long contiguous runs and an earlier first match) and opens the top/selected match the same way the tree does (`open_from_sidebar`), working with the sidebar collapsed. Its keybinding (`cmd-p`/`ctrl-p` → `QuickOpenFiles`) is handled by an element-level `.on_action` on the app's root render tree (`app_view.rs`) rather than a global `cx.on_action` in `actions.rs`, because opening the dialog needs a `Window` and `cx.active_window()` is unreliable off a real window manager (e.g. the screenshot harness's Xvfb host) even though keystrokes still reach the window.
- **Transcript Rendering**: The desktop transcript renders conversation history as typed blocks (`crates/chatty-gpui/src/chatty/views/transcript/`) — turns, tool rows, diffs, plans, artifact cards, approvals, etc. — built from `MessageEntry` + `system_trace` JSON via `adapt_message()`/`adapt_messages()`. Persistence stays untyped in chatty-core; these typed block types live only in chatty-gpui. The list itself renders on gpui's `list`/`ListState`, not gpui-component's `v_virtual_list`: `List` measures each item as it lays it out and caches the result, so turn heights are an output, not a hand-estimated input (the old `TranscriptLayout` estimator was deleted along with every height constant it needed). Use `ListAlignment::Top`, not `Bottom` — `Bottom` re-nulls the scroll anchor every frame while pinned, so `bounds_for_item` returns `None` and the plan strip's measured geometry disappears; sticky-to-bottom is instead one line (anchor past the last item, let `layout_items` backfill). `set_scroll_handler`'s callback runs while `ListState`'s internal `RefCell` is mutably borrowed, so it may only set a flag — calling back into the list from inside it panics. `adapt_message()` emits a `Block::Plan` per message that called `write_todos`, but every plan block renders the same live conversation-level snapshot, so a follow-up turn that re-plans would otherwise paint the identical panel twice; `retain_last_plan_block()` (`transcript/adapter.rs`) keeps only the newest plan block per adapted turn list. `turn_fingerprint()` (`chat_view/mod.rs`) is what tells `ListState` a turn's rendered height may have changed, so its contract is that a turn whose height *can* change must hash differently — that includes out-of-band state a block itself doesn't carry: `Block::Plan` renders the live `AgentTaskSnapshot`, not its own fields, so the fingerprint takes the snapshot as a separate argument and hashes `write_todos_called`, todo count/status/text-length per todo; `Block::Approval` hashes `approval.command.len()` since the alert card grows with the command (a heredoc runs to many lines) (AGE-338). `ChatView` does not rebuild the typed transcript from scratch on every `cx.notify()` (a streaming turn notifies once per 20ms text batch): `chat_view/turn_cache.rs`'s `TurnCache` keeps last frame's adapted `Turn`s and re-adapts only the messages whose `adapt_key()` moved (index, role, content length, collapse state, shape of the live/persisted trace), so a frame costs O(changed turns) rather than O(history) (AGE-165). Plan-block placement is expressed as ownership rather than the old two destructive passes over a fresh vec: `plan_owner()` picks which turn draws the panel (the newest turn that planned for itself, else the last assistant turn while a snapshot is live) and `apply_plan_ownership()` re-asserts that against whatever the cache holds every frame, so a cached turn converges on what a full rebuild would produce; `retain_last_plan_block()`/`attach_plan_block()` (`transcript/adapter.rs`) still exist and back that logic but are no longer called directly from `ChatView`. `reset_transcript_list()` clears the cache on conversation switch, since cache entries are keyed by message index. While a turn streams, the open line (everything after the last `\n` of the growing text) renders as a `CachedMarkdownSegment::PlainTail` — plain text, never reaching the math parser, markdown parser or Typst — while the settled prefix through the last newline keeps its full parse, reused by `Arc` (`message_parsing.rs`'s `LiveTailCache`) while byte-identical; a newline promotes the line, and stream end promotes it through the unchanged finalized path (AGE-167). Sticky scroll is re-asserted only once per drawn frame, from `prepare_render` (and on activation, from `activate_sticky_scroll`) — never from a stream-event handler like `append_assistant_text`, since those fire once per 20ms text batch regardless of whether a frame is drawn; `chat_view::sticky_scroll_tests` pins the call count at exactly those two sites. `consolidate_receipt_artifacts()` (`transcript/adapter.rs`) also dedups standalone artifact cards (PDF/image/pptx/tabular) that share the same tool-reported path (AGE-488): `dedup_standalone_artifacts` keeps only the *last* occurrence of a path, left at its own trace position rather than moved to the first occurrence's slot (last-write-wins). The dedup key is `Block::Artifact.path` exactly as the tool reported it, not resolved against the workspace, so two tool calls naming the same file with different spellings (an absolute `saved_path` vs. a later relative name) still render as two cards — closing that needs `resolve_artifact_path`'s workspace-aware resolution (AGE-487), deliberately not done here.
- **TUI plan card (AGE-342)**: chatty-tui renders the same todo-plan progress chatty-gpui's `Block::Plan` does, but as one card in `crates/chatty-tui/src/ui/plan.rs` — pure, ratatui-free row/status-line layout, testable without a frame — rewritten in place as `write_todos`/`update_todo`/`verify_completion` calls arrive, instead of a tool-call row per call. `is_agent_todo_tool()` and `snapshot_from_tool_output()` (`chatty-core/src/services/agent_task_controller.rs`, re-exported from `services::mod`) are the shared definitions both frontends read a todo tool's output through. `Transcript::plan` (`chatty-tui/src/engine/transcript.rs`) holds the latest snapshot for the status bar's live position (`ui/status_bar.rs`, `plan::status_line`) and is cleared on the next user turn, mirroring `AgentTaskController::reset()` — that reset is what makes collapsing a verified plan to its header affordable. `plan::plan_counts()` mirrors the desktop client's `plan_counts` so the two frontends never disagree about progress. A failed todo call still renders as an ordinary error row (the AGE-340 invariant that errors are never folded or hidden); `/verbose` still shows the raw tool payloads, plan tools included.
- **TUI transcript typed blocks (AGE-136)**: chatty-tui's `MessageBlock` (`crates/chatty-tui/src/engine/mod.rs`) matches the desktop's typed transcript in behaviour, not widgets. Alongside `Text`/`ToolCall` it carries `Activity(Vec<ToolCallInfo>)` — a run of two or more consecutive, successfully settled tool calls folded into one counted sentence once the turn moves on (`DisplayMessage::fold_settled_tools`, called from `push_text`/`finish_streaming`/`mark_error`/`mark_cancelled`); a `Running`/failed call, an approval, a plan or a diff always ends a run, so nothing is hidden. `Approval(ApprovalInfo)` shows the approval inline where the stream raised it, with `decision` filled in by `ApprovalResolved` (`Transcript::approval_requested`/`approval_resolved`) — the y/n input slot in `ui/approval.rs` is untouched and still resolves by id; an undecided approval on a row that stopped streaming renders as cancelled. `Plan(AgentTaskSnapshot)` is rewritten in place by `DisplayMessage::set_plan` as todo results arrive (the block form of the AGE-342 card above). `Error(String)` is a stream-ending error as its own block, and `Diff(DiffStat)` is an edit's `path +a −r` stat row. `crates/chatty-tui/src/ui/verb.rs` is a hand-copied table of the desktop's tense verbs and tally order (`chatty-gpui/.../transcript/{verb,activity}.rs`, edited → explored → searches → tools → ran commands → handoffs) — no chatty-gpui dependency, nothing shared through chatty-core — so a known tool's header reads `✓ Read README.md` / `⟳ Running cargo test` / `✗ Failed Ran pwd` instead of raw `name(args)` (an unknown tool keeps the raw form), and the status bar gets a `verb::change_tray_label` footer segment (`N files +a −b`). `chatty-tui --headless` prints the approval prompt/verdict and the plan card too (`headless/tool_format.rs`'s `format_approval_requested`/`format_plan_lines`, the latter reusing `ui::plan::plan_rows`).
- **Tool turns are persisted as rig produced them (AGE-247, D1 = b)**: a turn with tool calls persists every message rig recorded for it, in order — assistant tool-call message(s), user tool-result message(s), then the final assistant text — so the model sees its own tool activity on later turns. rig hands the list over on the final response (`PromptResponse.messages`); `llm_service` yields it as `StreamChunk::TurnMessages` before `Done`, each frontend parks it on the `Conversation` (`set_streaming_turn_messages`), and the turn's finalizer persists the tool round-trips ahead of the final text entry. On the streamed path that finalizer is `Conversation::finalize_turn` (AGE-243, both frontends), which delegates to the same `finalize_response_state` helper that `Conversation::finalize_response` uses for slash-command results — one ordering, not two that can drift. Both take the parked record, so a dropped turn discards it instead of leaking it into the next. Only the final text entry carries the trace JSON and attachments; tool entries carry neither, and `turn_tool_messages` cuts after the last tool result so no tool-call message is ever persisted without its result (OpenAI-compatible endpoints reject orphans). That cut isn't quite enough on its own: a parallel tool-call batch cut short mid-way — a loop-guard pivot firing on one sibling, or a user cancel — can leave an *earlier* assistant `tool_calls` entry with some ids still unanswered even after the trailing cut. `services::enforce_tool_round_trips` (AGE-485, generalised in AGE-513), run inside `finalize_response_state` on every turn regardless of cause, synthesizes a placeholder tool result for each such id before persistence — and drops any tool result that answers no earlier call — so the invariant holds unconditionally rather than only on the common path. It is the one place that knows the rule: `ContextShaper::request_history` runs it again on the history of every outgoing request, passing the ids the request's separate `prompt` answers so a live tool loop's last call isn't mistaken for a dangling one; `SessionStreamHandler::on_tool_completed` (`session/handler.rs`) also defers acting on a loop-guard pivot until every call in the same batch has completed, so the common case doesn't need the repair at all. Payloads are kept whole; compaction bounds them (AGE-248). Readers that render, export or count turns skip tool messages via `services::is_tool_message` (transcript `load_history`, markdown/ATIF/JSONL exporters) and count *exchanges* with `services::exchange_count` (title trigger, `generate_title`), while regeneration (`remove_last_assistant_message`) pops the whole turn back to the user text message that started it.
- **Token usage is per provider request, normalised once**: rig-agent's multi-turn stream emits a `CompletionCall` item per provider request and a `FinalResponse` with the turn's aggregate. `llm_service::normalize_usage` turns each into an `ApiCallUsage` (`models/token_usage.rs`) with `input_tokens` meaning the *uncached* prompt share, so `input + cache_read + cache_write` is the whole prompt whichever provider convention the numbers arrived in (OpenAI-compatible reports cached tokens inside `prompt_tokens`; Anthropic native reports them separately). The stream yields `StreamChunk::ApiCallUsage` per request and a final `StreamChunk::TokenUsage` aggregate; `StreamManager` builds the persisted `TokenUsage` from the per-call records (`TokenUsage::from_calls`, which is where `api_turn_count` comes from) and only falls back to the aggregate when no per-call record arrived. Cache hit rate is a per-request property, so never sum first and divide later. Each call is logged as `LLM completion call usage` with `hit_rate`; that log line is the prompt-caching diagnostic (AGE-207). Cost uses `TokenPricing`, with cache read/write rates from `ModelConfig` when set (OpenRouter sync fills them from `pricing.input_cache_read` / `input_cache_write`) and the input rate otherwise. OpenRouter agents are built from `completion_model(..).with_prompt_caching()` rather than `client.agent(..)`, since that is the only place rig's `cache_control` opt-in lives; that marks the system message (preamble + tools). The conversation history caches only if the *latest* message carries a breakpoint too, and rig's hooks cannot add one (`RequestPatch` merges `additional_params` at the top level, so `messages` cannot be patched), so the OpenRouter client is built on `PromptCachingHttpClient` (`factories/agent_factory/prompt_cache_http.rs`), a `reqwest` wrapper implementing rig's `HttpClientExt` that rewrites every `POST …/chat/completions` body to mark the last user/assistant message (AGE-205). Two breakpoints total, within Anthropic's limit of four. The moving one rewrites the previously-last message every turn, so on the OpenRouter path the request history is append-only only modulo `cache_control` markers: `session/append_only_prefix.rs` records both provider paths against a fake daemon and pins exactly that, and `agent_factory/cache_breakpoint_probe.rs` (ignored, needs `OPENROUTER_API_KEY`) measures against the real provider whether the marker counts as cached content (AGE-291). MCP connections are a `BTreeMap` so the tool block is byte-stable across restarts (AGE-206).
- **Tool failure detection is text-based, not a flag**: the streamed `ToolResult` carries no error flag (rig's `is_error()` lives on `ToolExecutionResult`, which never reaches the stream), so `llm_service::tool_result_looks_like_error` recognizes a failure by sniffing the message text (`Error:` prefix, `"the tool failed"`, `"malformed JSON"`). `tools::mod::map_tool_error()` must keep writing that `Error: {tool_name}: {message}` prefix — dropping it once made every typed tool failure get filed as a success, with the error prose landing in `output` (a failed `compile_typst` minted an artifact card for a PDF that was never written). A test (`failures_are_recognisable_as_errors_downstream`) pins the two together; keep it green when touching either side.
- **Unknown tool names are repaired, not fatal (AGE-497)**: a local model often reaches for a name it was trained on rather than this agent's own (`grep`, `cat`, `bash`, `search_codebase`), which used to end the turn on rig's `UnknownToolCall`. Every chat agent carries the rig hook `ToolNameRepair` (`factories/agent_factory/tool_name_repair.rs`, wired into `chat_agent_builder` ahead of `ContextShaper`), which resolves the call in `on_invalid_tool_call` instead: a known alias (`ALIASES`) whose target tool is allowed this turn and whose emitted arguments fit that tool's parameters (`TARGET_PARAMS`) is renamed and run; an alias called with arguments its target doesn't take is answered, not run, with the target's name and parameters so the caller retries correctly; anything else is answered with the nearest allowed tool names by edit distance (3, `SUGGESTIONS`). Either way the turn continues rather than erroring out.
- **Unattended-run robustness (headless / `--pipe` / delegated workers)**: `execution_settings.max_agent_turns` defaults to `0`, meaning no cap — `TurnBudget` (`services/turn_budget.rs`) treats `0` as `UNCAPPED_RIG_TURNS` (rig's own cap set to 1,000,000, no turn-countdown notes, no wrap-up call); the old default was 10. A run with nobody watching (`--headless`, `--pipe`, a delegated worker with no `--max-agent-turns`) instead runs uncapped under a wall-clock `Deadline`: `chatty-tui`'s `--max-duration <secs|30m|2h|1h30m>` (default 30m) — from 85% of the budget, tool results say how many minutes are left, and the first model call past the deadline goes out tool-free as a forced wrap-up, mirroring what a spent turn cap does. The persistent shell (`services/shell_service/mod.rs`) now sources the login profile (`LOGIN_PROFILE_INIT`: `/etc/profile`, then the first of `~/.bash_profile`/`~/.bash_login`/`~/.profile`) into its first command before the sentinel protocol starts — the shell itself still starts `bash --norc --noprofile`, so a project's conda env or `PATH` additions reach the model's commands where they didn't before; inherited `PATH` entries Debian's `/etc/profile` drops are appended back, and `set -e`/`-u`/`-x` a profile leaves behind are undone. `ExecutionSettingsModel::tool_loading` (`ToolLoading::All | Dynamic`, `--tool-loading` on chatty-tui) can shrink the first request's tool schemas: under `Dynamic`, `ToolLoader` (`factories/agent_factory/tool_loading.rs`) advertises only `CORE_TOOLS` plus a `load_tools {group}` catalog, and expands per-call via rig's `active_tools` — loading is monotonic and by whole group so the prefix cache never churns from an unload. `ConnectRetryHttpClient` (`factories/agent_factory/connect_retry_http.rs`) sits under every provider client and retries a request that failed before any response byte (connect/send errors only, 4 retries at 1/2/4/8s) so a dropped connection doesn't take rig's whole multi-turn run with it; `RequestRecorder` keeps the last model call's messages so a run that still fails with a provider error persists that round-trip via `TurnMessages` before the `Error`, instead of silently losing it. Unattended runs also compact instead of only truncating once history passes `COMPACTION_TRIGGER_FRACTION` (0.85) of the shaper's budget (`services/context_compaction.rs`, wired by `ContextShaper::enable_compaction`, only when `AgentBuildContext.unattended`): older messages become one summary (task, files the tool history wrote, `git status --short`, last command's tail, plus a short progress note from a tool-free utility-agent call) and the newest `COMPACTION_KEEP_RECENT_FRACTION` (0.40) stay verbatim. `AgentLoopGuard::with_progress_check` (headless only, not the desktop or interactive TUI) nudges after `PROGRESS_WINDOW_TOOL_CALLS` (25) tool calls in a row that write no new file, run no new test command and read no new file, and finalizes the run after a second such window with no progress. Headless also answers a turn that ends on a text-only message announcing its next step ("Let me apply the fix.") with one continue nudge, at most `MAX_ANNOUNCED_STEP_NUDGES` (2) per run (`chatty-tui/src/headless/announced_step.rs`). `ExecutionSettingsModel::ask_user_enabled` (default `true`, skipped on serialization at its default like `tool_loading`) gates the `ask_user` tool itself, independent of whether a clarification store exists: `chatty-tui --disable ask-user` turns it off so an unattended run (e.g. a benchmark harness with nobody to answer) fails a stray call fast instead of parking it until its timeout. It joins `shell`, `fs-read`, `fs-write`, `fetch`, `git`, `code-exec`, `docker-exec` as an `--enable`/`--disable` tool group name; group names are case-insensitive, `_` ≡ `-`, and accept the tool's own name (`ask_user`, `shell_execute`), and an unknown name is now a hard error rather than a logged warning. `--only <groups>` (chatty-tui, AGE) is a strict allow-list applied after `--enable`/`--disable`: every group in that list (not `disable_tools`/MCP/web-search/etc.) is turned off first, then only the named ones turned on — the Harbor benchmark adapter and `disable_tools` on a broker worker rely on the same `set_tool_group`/`canonical_tool_group` (`chatty-tui/src/main.rs`) both paths and the docs share.
- **Image input is stripped for a text-only model, not sent and left to 400**: `AgentClient` carries `supports_images` (`ModelConfig::supports_images` at build time, exposed via `AgentClient::supports_images()`), and `llm_service::stream_prompt` checks it before every request: when `false`, `strip_unsupported_images` replaces every `UserContent::Image`/`ToolResultContent::Image` — in the new message, in history (a model switch mid-conversation to a text-only one), and inside a tool result (`pdf_to_image`, a later-turn screenshot) — with a text note (`IMAGE_UNSUPPORTED_NOTE`) instead of letting the provider reject the whole request (e.g. OpenAI-compatible's "At most 0 image(s) may be provided"). This is a request-time safety net under the Model Capability Architecture section's composer-level attachment filtering below, which only stops a *new* attachment from being sent and doesn't see history or tool-produced images.
- **Desktop boot order (AGE-161)**: `chatty-gpui/src/main.rs` opens the main window *first* inside the `Application::run` callback and registers every task, thread and settings load below it. `cx.open_window` draws its first frame synchronously, so anything above it — a `cx.spawn`, a `std::thread::spawn`, a repository open — runs before the user sees a window. Only constructors with no I/O in them belong above `open_window`. Two kinds of global have to be up there, and they fail differently. The first frame's render tree does exactly six hard `cx.global::<T>()` reads — `GeneralSettingsModel`, `ExecutionSettingsModel`, `ExtensionsModel`, `ErrorStore`, `AutoUpdater`, and `ConversationsStore` — and a missing one *panics on first paint*; the first five are set in `main.rs`, while `ConversationsStore` is set by `ChattyApp::new`, which still runs before the frame is drawn. The rest are `try_global` reads that fail *silently*, which is the more dangerous case: `GlobalStreamManager` and `GlobalModelsNotifier` would skip their `ChattyApp::new` subscriptions (dead streams, dead model picker) and `MemoryInitSignal` would skip the memory-ready wait and build an agent with no memory tools. Everything else the views touch (`TokenTrackingSettings`, `GlobalTokenBudget`, `DiscoveredModulesModel`, …) is a `try_global` read that degrades harmlessly, and is set early only because it costs nothing. `main()` itself must not `block_on` before `Application::run`: the conversation repository is handed to the window as `ConversationSqliteRepository::deferred()` and opens its pool on the first query. `boot_order_tests` in `main.rs` pins all of this against the file's own source.
- **Repository conformance suite (AGE-280)**: `chatty_core::repositories::store_conformance` (behind the `test-support` feature, the same seam as `services::stream_fixtures`) exercises create/read/update/delete against any implementation of the settings repository traits (`define_single_json_repository!`/`define_list_json_repository!` in `settings/repositories/mod.rs`) or `ConversationRepository`, via a `single_settings_conformance!`/`list_settings_conformance!`-generated fn per trait plus `conformance_conversation`. A repository keyed per name rather than holding one singleton value — `OAuthCredentialRepository` (MCP OAuth access/refresh tokens, AGE-317) — fits neither macro, so it gets a hand-written `conformance_oauth_credentials` instead, covering an absent server, a save/load round trip unchanged to the last field, replace-not-merge on a second save, and `clear` touching only the named server. Comparisons go through `serde_json::Value`, not `PartialEq`, so the suite needs no changes to the settings model structs. It's exported so an out-of-tree store-backed implementation (e.g. hive's) can run the identical suite from a dev-dependency and be proven equivalent to the JSON/SQLite backends shipped here; every JSON repository's `with_path` constructor and `ConversationSqliteRepository::deferred_with_path` exist only for this (`#[cfg(any(test, feature = "test-support"))]`). Replaces the deleted `InMemoryConversationRepository`.
- **SettingsSnapshot / SettingsDelta (AGE-283)**: `chatty_core::settings_snapshot` (re-exported as `chatty_core::{SettingsSnapshot, SettingsDelta}`) is a byte-stable view over the 13 `RepositoryRegistry` settings families (`ConversationRepository` is intentionally excluded — it isn't a registry field). A hosted guest boots from `RepositoryRegistry::from_snapshot(snapshot)`, an in-memory-only registry backed by `settings/repositories/in_memory_repository.rs` (production code, not `test-support`-gated), and on release reports back `registry.delta_since(&snapshot)` — a `SettingsDelta` with `Option<T>` per family, `None` meaning unchanged — which the real disk/DB-backed registry writes with `registry.apply(delta)`. `SettingsSnapshot::canonical_bytes()` recursively re-sorts every JSON object's keys (arrays are left in original order) rather than relying on the workspace's ambient serde_json `preserve_order` feature (pulled in transitively by GPUI), so cross-process snapshot bytes are byte-identical regardless of `HashMap`/`IndexMap` iteration order.
- **Broker / local participants (ADR-0011, AGE-301/AGE-314)**: `crates/chatty-protocol-gateway` fronts loaded WASM modules over OpenAI-completions, MCP and A2A, and now also **local participants** — processes that register over a Unix socket (`participant/`) and are served at the same `/a2a/{name}` routes, looked up ahead of modules. This half is Unix-only (`#[cfg(unix)]` on the gateway's `worker`/`participant` modules and on both consumers — `chatty-tui/src/participant/`, `chatty-gpui/src/chatty/services/broker_runner.rs`, and the wiring block in `module_settings_controller.rs`, AGE-339): on Windows the protocol gateway starts as just the gateway, and `--participant-socket` returns an `Err` instead of failing to compile. Keep any new code that touches this path behind the same `#[cfg(unix)]` gate. `ProtocolGateway::with_virtual_agent` is additive — each call inserts one more named worker into a `BTreeMap<String, Arc<dyn VirtualAgent>>` (`GatewayState.runners`), so `/a2a/{name}` and `list_agents` enumerate a whole declared team rather than one fixed participant (ADR-0011 C10 / AGE-377). Roles are declared in settings, not passed on `invoke_agent`, so the leader's tool schema and prompt stay identical whatever the team: `ModuleSettingsModel.virtual_agents: Vec<VirtualAgentConfig { name, model, tools, preamble, disable_tools, max_agent_turns, extra_args }>` (`settings/models/module_settings.rs`), empty meaning the one default worker, `local-agent` (`chatty_core::tools::LOCAL_AGENT_NAME`). A role is name, model, profile and preamble (ADR-0011 C11, AGE-405): `tools` names a profile in `factories::agent_factory::tool_profile` (`coordinator`/`coder`/`reviewer`) — an allowlist of tool *names*, composed with `disable_tools`' whole groups (AGE-452): both apply, so `disable_tools` can only ever narrow the profile further, never re-enable a tool the profile already excludes. The profile and the preamble travel to the worker as `chatty-tui --tools <profile>` / `--preamble <text>`, arrive as `AgentBuildContext.role` (`AgentRole { preamble, profile }`, on the context rather than in `AgentServices` because only a worker has one), and are applied in `agent_factory`: the profile filters registration by `Tool::NAME` in `tool_collector`, narrows the `ToolAvailability` the preamble describes, and drops every MCP tool, while the preamble lands in the system prompt ahead of the tool summary. A worker's own `max_agent_turns` (AGE-440) overrides the no-cap default (`max_agent_turns` 0 = unlimited) its `chatty-tui` child would otherwise run with — since that child runs unattended, an unset worker instead runs uncapped under the headless `--max-duration` wall-clock budget (default 30m) — independent of `TeamFile::max_agent_turns` (the leader's own budget): `resolve_virtual_agents` forwards it as a `--max-agent-turns <n>` flag on that worker's argv, ahead of `extra_args`, and unset agents get no such flag and keep the default. The card carries the profile name and the preamble's first sentence. `chatty_core::services::virtual_agents::resolve_virtual_agents` is the one function — used by both `chatty-gpui/src/chatty/services/broker_runner.rs`'s `local_runners` and chatty-tui's `--broker` wiring — that turns that config into a `VirtualAgentSpec` per agent (name, a card description carrying its model and disabled tool groups, its children's argv, and the model endpoint to meter it on); each spawns a one-shot `chatty-tui` child per delegated task, waits for it to register, routes the task to it, and reaps it. The desktop wires this up with the gateway started by the module-settings controller, socket at `dirs::runtime_dir()/chatty/participants.sock`. Since AGE-376, chatty-tui gets the same wiring via `--broker` (valid with `--headless`, `--pipe`, and the interactive TUI alike): `chatty-tui/src/participant/broker.rs`'s `Broker::start` runs its own gateway on an ephemeral port and a pid-suffixed participant socket (so multiple headless leaders on one host, e.g. under the Harbor adapter, AGE-285, never collide), with no WASM module registry of its own — it exists only to make the declared virtual agents reachable. The published names reach the agent build as `AgentBuildContext`/`AgentServices.local_agents: Vec<String>` (`factories/agent_factory/build_context.rs`); `invoke_agent` (`tools/invoke_agent_tool.rs`, `with_local_agents`) resolves any of them when the gateway is running, ahead of WASM modules; `list_agents` (`with_local_workers`) advertises them the same way, and lets the broker's live card — carrying each worker's actual model and disabled tool groups — beat the static stand-in text once the gateway answers. This is the only delegation path: ADR-0011 measured the broker hop against the old in-process `sub_agent` tool (`docs/research/adr-0011-broker-ab-2026-09-08.md`, ≤ ~20ms, progress events a strict superset) and AGE-303 then deleted `sub_agent`, so `invoke_agent` is the one tool and there is no direct path to keep in step. `chatty_core::services::worker_tree` (ADR-0012) is the one `git worktree`-per-worker implementation behind the broker's `WorkspaceFactory` (`worker_tree::create_with_commit_hook` bundles the commit-on-exit hook both frontends' broker wiring need). The worker name is a wish, not a guarantee: branches and worktree paths live in one repository shared by every broker on it, so `create` takes `<name>` when `sub-agent/<name>` and `.chatty/worktrees/<name>` are both free and otherwise the first free `<name>-N` (from 2) — a sub-leader started with `--broker` names its workers from a counter of its own, so its `local-coder-0` would otherwise collide with the top leader's, and a tree left from an earlier run keeps its branch (AGE-402). The evidence block names the branch actually created, never the one asked for. In a git repository, a tree that cannot be made now fails the delegation with the reason rather than falling back to the shared tree (`GitService::branch_exists`/`worktree_path` back the free-name search); only a workspace that is not a repository still runs its workers unisolated. `chatty_core::services::worker_endpoint::resolve_worker_endpoint` (resolving each named agent's own `--model`, so a reviewer on another server is metered on that server rather than the leader's) likewise backs `resolve_virtual_agents`' per-agent endpoint budget for both frontends' broker wiring; `tools::worker_executable()` resolves the same `chatty-tui` binary for both; and `tools::progress_text_for_event` renders the same progress lines for both, so the desktop and chatty-tui leaders never drift in what the parent's transcript shows. A task frame carries the caller's bearer when the A2A request had one (`BrokerFrame::Task.bearer`, `TaskBearer`, AGE-371): the gateway copies `Authorization: Bearer` off `message/send`/`message/stream` into the `DelegatedTask` it hands `submit_task` / `VirtualAgent::run_task` / `serve_one_task`'s closure, so a hosted worker (hive's `chatty-server` in participant mode) can validate it as the tenant boundary; a local worker ignores it, and a task without one is the pre-AGE-371 frame byte for byte. `TaskBearer`'s `Debug` is redacted; never log a frame's bearer. What the leader is told about a worker's output is the *runner's* account, not the model's (ADR-0011 C12, AGE-406): `create_with_commit_hook(root, worker, verification)` returns an `EvidenceHook` beside the exit hook, and the gateway awaits it through `WorkerHandle::evidence()` (default `None`) — a `WorkerWorkspace.evidence: Option<EvidenceFactory>` — at the task's terminal status. The hook commits the worktree and *then* reads it back, since the commit count and `git diff --stat <default>..<branch>` are one edit stale otherwise; both hooks commit and only the first to run does, so a caller that hangs up mid-task still gets its worker's edits committed from `Drop`. The result is a `worker_tree::Evidence` (branch, base, commits, diff stat, and the `team.verification` command's exit code and last 20 lines, run in its own process group in the tree under a five-minute timeout that kills the group, not just the shell) rendered as a fenced `evidence` block appended to the answer and carried structured on the terminal status's `metadata.evidence`. It replaces AGE-399's `merge_hint` sentence at the same seam. An empty branch produces **no** envelope, so a read-only reviewer is never handed something to merge; nothing is merged automatically. `team.verification` is `ModuleSettingsModel.team` (`TeamConfig`, serde-defaulted so old files round-trip) and reaches each runner as `VirtualAgentSpec.verification`, `None` for an agent whose profile has no shell — its `tools` profile does not allow `shell_execute`, or its `disable_tools` names `shell` (the two compose, AGE-452). On `stream_task` the block is sent as an artifact chunk *ahead of* the terminal status event: `A2aClient::send_message_stream` returns as soon as it sees a `final` status, so anything sent after would never be seen. A hosted leader's delegation budget is also a monthly dollar cap, not just a turn count (ADR-0010, AGE-416): `AgentBuildContext.spend_gate: Option<Arc<dyn SpendGate>>` (`services/spend_gate.rs`), asked by `InvokeAgentTool::call` before agent resolution and before any `Started` progress event, so a refusal spawns nothing. A refusal is the typed `InvokeAgentError::CapExceeded`, model-visible as `Error: invoke_agent: cap_exceeded: month-to-date $X.XX ≥ cap $Y.YY; no delegation started`, and `CapExceeded` serializes to the same three fields (`error`, `month_to_date`, `cap_usd`) as hive's `POST …/turns` 402 body. chatty2 ships only the trait, the hook and `TokenTrackingSettings.cap_usd: Option<f64>` (byte-stable, absent-key-safe); hive's `chatty-server` implements `check()` against its usage ledger and sets the gate on the leader's context — every other host (the desktop, chatty-tui, any leader built with no gate) leaves it `None`, so no check runs and behaviour is unchanged. A *team directory* is the unit all of this ships as (ADR-0011 C13, AGE-407): `teams/<id>/team.json` — `leader { model, profile, preamble }`, `agents: [VirtualAgentConfig…]`, `verification`, `skill`, `max_agent_turns` — with `SKILL.md` beside it, loaded by `chatty_core::services::team::load_team` from `<workspace>/.chatty/teams/`, then `<data_dir>/chatty/teams/`, then the presets compiled into chatty-core (`crates/chatty-core/teams/`, one so far: `coder-reviewer`). `chatty-tui --team <id>` implies `--broker`, applies the file to *this run* only (`Team::run_module_settings` gives the broker a copy of the on-disk module settings with `virtual_agents` and `team.verification` replaced, so `/modules` still saves what was on disk; `Team::apply_turn_budget` sets `max_agent_turns` on the run's execution settings; the engine's `local_agents()` reads the roster from the team, never from `module_settings`), takes the leader's model/profile/preamble unless the flag was given, hands the `SKILL.md` to `read_skill` through `AgentBuildContext.team_skill` (served ahead of the skill directories, so a compiled-in preset needs no file), and opens the first human turn with `read_skill <skill> and follow it` plus the verification command, since a `coordinator` leader has no shell and can only delegate the check.
- **GitHub PR Status Bar**: `chatty_core::services::github_pr_service::resolve_pull_request` resolves the pull request whose head is the workspace's current branch — `gh` CLI first (already authenticated, works for private repos), REST API fallback (`GITHUB_TOKEN`/`GH_TOKEN` from env only, never persisted or logged). Every failure path (not a git repo, no GitHub remote, no PR for the branch, request error) resolves to `None`, never an error — the bar simply doesn't render. `PrStatusBarView` (`chatty-gpui/src/chatty/views/chat_input/pr_status_bar_view.rs`) owns its own poller (20s while CI is pending, 60s once settled) and is gated on `ExecutionSettingsModel::git_enabled`; `ChatView::sync_pr_status` only feeds it the current conversation ID and workspace path each frame. A `generation` counter bumped on every context change lets an in-flight poll's stale result be discarded rather than overwriting a newer context's state. Dismissal is keyed on `(conversation_id, pr_number)`, so it reappears if the branch/PR changes. chatty-tui's status bar (`ui/status_bar.rs`) surfaces the same PR number and CI state alongside the git branch.

## CI/CD

### Workflows

| Workflow | Trigger | Purpose |
|:---------|:--------|:--------|
| **CI** (`ci.yml`) | PR / push to `main` | Tests, formatting, clippy. A `windows` job runs `cargo check -p chatty-gpui -p chatty-tui` on `windows-latest` at PR time — the crates `release.yml`'s `build-windows` builds — so a Windows-only compile break (e.g. a `#[cfg(unix)]`-gated dependency imported unconditionally) fails the PR instead of surfacing only after merge, in `release.yml`, too late for `upload-assets` to publish any platform. Docs-only diffs skip compile on both `test` and `windows` (required checks still report success, not skipped, via step-level `if`). Stale PR runs are cancelled. `Swatinem/rust-cache` is warmed on `main` (never with `cache-on-failure`: a cancelled run would save a partial `target/` that is then never re-saved). On pushes to `main`, `warm-release-cache` also builds `--release` on Linux, macOS and Windows once per Cargo.lock + rustc and saves each under the key from `scripts/release-cache-key.sh`. |
| **Auto-arm** (`auto-arm.yml`) | PR opened/reopened/marked ready for review | Arms GitHub auto-merge (squash) on same-repo PRs from the repository owner, `github-actions[bot]`, or a docs bot's `docs/*` branch, so nobody has to manually arm a PR — it lands once required checks are green and the branch is current (falls back to an immediate squash merge if the PR is already `CLEAN`). Uses `AUTO_UPDATE_TOKEN`, not `GITHUB_TOKEN`, so the merge doesn't suppress workflows that listen for pushes to `main`. Hold a PR back by opening it as a draft. No labels involved — `ship:auto` means "cut a patch release" and is guarded separately by `ship-auto-guard.yml`. |
| **Prepare Release** (`prepare-release.yml`) | PR merged with `release:patch`/`release:minor`/`release:major` label, or manual `workflow_dispatch` | Bumps version via a `cut-release` PR (main is protected), generates changelog, tags, creates the GitHub Release, then calls Release via `workflow_call`. |
| **Release** (`release.yml`) | Called by Prepare Release via `workflow_call`, or manual GitHub Release publish | Builds cross-platform artifacts (Linux AppImage, macOS DMG, Windows EXE), generates checksums, uploads to release. Restore-only cache: each platform restores the release cache that CI's `warm-release-cache` matrix built on `main`. |
| **Claude Code Review** (`claude-code-review.yml`) | PR opened/updated (Rust/CI paths only) | Automated AI code review via Claude. Skipped for docs-only PRs. |
| **Rig canary** (`rig-canary.yml`) | Weekly, or PR that touches Cargo manifests | Informational `cargo update` + `cargo check` against latest rig (AGE-26). |
| **Claude** (`claude.yml`) | `@claude` mention on issues/PRs | Interactive AI assistance. |
| **Update user docs** (`update-readme.yml`) | PR merged to `main` | Claude analyzes the diff; if user-facing features changed, opens a follow-up PR updating `docs-site/src/user/*.md` (README stays a short landing page, edited only for download/install/positioning/links). Add `skip-readme` label to opt out. |
| **Update Agent Docs** (`update-agent-docs.yml`) | PR merged to `main` | Claude analyzes the merged PR for guidance drift and opens a follow-up PR to sync `CLAUDE.md` / `AGENTS.md` when needed. Add `skip-agent-docs` label to opt out. |
| **UI Sync Check** (`ui-sync-check.yml`) | PR merged to `main` | Claude checks if one UI crate (chatty-gpui/chatty-tui) changed without the other; creates `ui-sync` labeled issue if sync needed. Add `skip-sync-check` label to opt out. |
| **Dependency Check** (`dependency-check.yml`) | Weekly (Monday 9:00 UTC) or manual | Checks crates.io for dependency updates, files grouped work in Linear only (auto-ship, agent tech debt, or human tech debt track) — no GitHub issues; requires `LINEAR_API_KEY`. |

### Release Flow

The recommended release flow from Claude Code:

```
/create-release patch   # Adds release:patch label to current PR
```

Then merge the PR. The full pipeline runs as a single workflow:

```
PR merge → Prepare Release ──────────────────────────────────────────►
           (bump PR on main,    calls release.yml    (build 3 platforms,
            tag, GH release) ── via workflow_call ──► checksums, upload)
```

Key design: Prepare Release calls Release directly via `workflow_call` — no event-based handoff, no PAT needed, build status appears inline. Version bumps go through a `cut-release` PR because `main` requires status checks (direct `git push origin main` fails with GH006).

Alternative triggers:
- **Manual**: Actions UI → Prepare Release → Run workflow (with bump type selector and dry run option)
- **Manual release**: GitHub UI → Create Release → Release workflow runs standalone
- **On `main`**: `/create-release patch` triggers `workflow_dispatch` directly

**A merge only releases if the PR carried a release label.** The label is read
from the merged pull request, so it must be on the PR *before* the merge —
adding it afterwards does nothing, and neither does a plain merge of unlabeled
work. When the gate rejects a merge, Prepare Release still shows a run: its
conclusion is `skipped`, not `failure`. The gate also declines a PR whose head
branch starts with `docs/` or that carries the `documentation` label, so
docs-only work never cuts a version. To ship work that already landed
unlabeled, merge any labeled follow-up PR — the changelog spans every commit
since the last tag, so the earlier merge is picked up.

### Changelog Generation

The Prepare Release workflow auto-generates release notes by parsing commits since the last tag:
- **Features & Improvements**: commits starting with Add, Feat, New, Implement, Wire
- **Bug Fixes**: commits starting with Fix, Bug, Resolve, Correct
- **Other Changes**: everything else (version bumps and merge commits are excluded)

The changelog becomes the GitHub Release body automatically.

## Idiomatic Patterns

A curated human-readable version of these patterns is on the docs site: docs-site/src/dev/contributing-patterns.md.

This section documents the common Rust and GPUI patterns used throughout the Chatty codebase.

### 1. Global Entity Patterns

**When to use**: For application-wide state that needs to be accessed from multiple components (settings, stores, notifiers).

**Pattern**: Types implement the `Global` trait and are stored/accessed via `cx.set_global()` and `cx.global()`.

```rust
// Define a global type
use gpui::Global;

pub struct ConversationsStore {
    conversations: HashMap<String, Conversation>,
    active_conversation_id: Option<String>,
}

impl Global for ConversationsStore {}

// Initialize at startup (main.rs)
cx.set_global(settings::models::GeneralSettingsModel::default());
cx.set_global(settings::models::ProviderModel::new());

// Access from anywhere
let models = cx.global::<ModelsModel>();

// Mutate with update_global
cx.update_global::<ModelsModel, _>(|model, _cx| {
    model.replace_all(models);
});

// Check existence
if !cx.has_global::<ConversationsStore>() {
    cx.set_global(ConversationsStore::new());
}
```

**Entity references in globals**: Use the generic wrappers in `global_entity.rs` — `GlobalWeakEntity<T>` (default; entity's lifetime is owned elsewhere) or `GlobalStrongEntity<T>` (the global itself keeps the entity alive, e.g. `StreamManager`, `ModelsNotifier`):

```rust
// Weak — entity owned elsewhere, caller must upgrade before use
pub type GlobalMyNotifier = GlobalWeakEntity<MyNotifier>;
cx.set_global(GlobalMyNotifier::new(entity.downgrade()));

if let Some(notifier) = cx.try_global::<GlobalMyNotifier>().and_then(|g| g.try_upgrade()) {
    notifier.update(cx, |_notifier, cx| {
        // Use notifier
    });
}

// Strong — global owns the entity for the app's lifetime (no downgrade)
pub type GlobalModelsNotifier = GlobalStrongEntity<ModelsNotifier>;
cx.set_global(GlobalModelsNotifier::new(models_notifier));

if let Some(notifier) = cx.try_global::<GlobalModelsNotifier>().and_then(|g| g.get()) {
    notifier.update(cx, |_notifier, cx| {
        // Use notifier
    });
}
```

**Gotcha**: Default to `GlobalWeakEntity` to avoid circular references. Only reach for `GlobalStrongEntity` when the global must be the sole thing keeping the entity alive (e.g. a notifier with no other owner that must outlive individual windows).

### 2. Event-Subscribe Patterns

**When to use**: For decoupled communication between components (e.g., notifying UI when data loads, responding to user input).

**Pattern**: Define events as enums implementing `EventEmitter`, emit with `cx.emit()`, subscribe with `cx.subscribe()`.

```rust
// Define event type (models_notifier.rs)
use gpui::EventEmitter;

#[derive(Clone, Debug)]
pub enum ModelsNotifierEvent {
    ModelsReady,
}

pub struct ModelsNotifier;

impl EventEmitter<ModelsNotifierEvent> for ModelsNotifier {}

// Emit events (models_controller.rs)
if let Some(notifier) = cx.try_global::<GlobalModelsNotifier>().and_then(|g| g.get()) {
    notifier.update(cx, |_notifier, cx| {
        cx.emit(ModelsNotifierEvent::ModelsReady);
    });
}

// Subscribe to events (app_controller.rs)
cx.subscribe(
    &notifier,
    |app, _notifier, event: &ModelsNotifierEvent, cx| {
        if matches!(event, ModelsNotifierEvent::ModelsReady) {
            // Handle event
            info!("Models ready!");
        }
    },
)
.detach();  // Important: detach to prevent blocking
```

**Subscribing to UI events**:

```rust
// Subscribe to input events (chat_view.rs)
cx.subscribe(&input, move |_input_state, event: &InputEvent, cx| {
    if let InputEvent::PressEnter { secondary } = event {
        if !secondary {  // Plain Enter, not Shift+Enter
            state.update(cx, |state, cx| {
                state.send_message(cx);
            });
        }
    }
})
.detach();
```

**Gotcha**: Always call `.detach()` on subscriptions, or store the subscription handle to keep it alive.

**Design rule:** All entity-to-entity communication uses `EventEmitter`/`cx.subscribe()` — no `Arc<dyn Fn>` callbacks between entities. `IntoElement` components (e.g., `ConversationItem`) keep callbacks but route them through the parent entity's `cx.emit()`. See `docs/entity-communication.md` for full rationale.

### 3. Async Patterns with Tokio Integration

**When to use**: For long-running operations (file I/O, network requests, LLM calls) that shouldn't block the UI.

**Pattern**: Use `cx.spawn()` with async blocks. The Tokio runtime is initialized at app startup in `main.rs`.

**Tokio runtime setup** (main.rs):

```rust
let _tokio_runtime = tokio::runtime::Runtime::new()
    .expect("Failed to create Tokio runtime");
let _guard = _tokio_runtime.enter();  // Enter for entire app lifecycle

let app = Application::new().run(move |cx| {
    // App code runs within Tokio context
});
```

**Spawning async tasks**:

```rust
// Simple async operation (main.rs)
cx.spawn(async move |cx: &mut AsyncApp| {
    let repo = GENERAL_SETTINGS_REPOSITORY.clone();
    match repo.load().await {
        Ok(settings) => {
            cx.update(|cx| {
                cx.set_global(settings);
            })
            .ok();
        }
        Err(e) => {
            warn!(error = ?e, "Failed to load settings");
        }
    }
})
.detach();

// Complex async with entity updates (app_controller.rs)
cx.spawn(async move |_, cx| {
    let task_result = app_entity.update(cx, |app, cx| {
        app.create_new_conversation(cx)
    });
    
    if let Ok(task) = task_result {
        let _ = task.await;
    }
    
    app_entity.update(cx, |app, cx| {
        app.is_ready = true;
        cx.notify();
    }).ok();
})
.detach();
```

**Returning Tasks**:

```rust
// Method that returns async Task (app_controller.rs)
pub fn create_new_conversation(
    &mut self,
    cx: &mut Context<Self>,
) -> Task<anyhow::Result<String>> {
    cx.spawn(async move |_weak, cx| {
        let conv_id = uuid::Uuid::new_v4().to_string();
        let conversation = Conversation::new(/* ... */).await?;
        
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.add_conversation(conversation);
        })?;
        
        Ok(conv_id)
    })
}

// Caller
let task = app.create_new_conversation(cx);
let conv_id = task.await?;
```

**Gotcha**: In `AsyncApp` context, you must use `cx.update()` to access UI state. Direct access is not allowed.

### 4. Entity/Model Patterns

**Models**: Simple data containers implementing `Global` for shared state.

```rust
#[derive(Clone)]
pub struct ModelsModel {
    models: Vec<ModelConfig>,
}

impl Global for ModelsModel {}

impl ModelsModel {
    pub fn new() -> Self {
        Self { models: Vec::new() }
    }
    
    pub fn get_model(&self, id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.id == id)
    }
}
```

**Entities**: Components with behavior, lifecycle, and event handling.

```rust
pub struct SidebarView {
    conversations: Vec<(String, String, Option<f64>)>,
    active_conversation_id: Option<String>,
}

// Entity communication via events (not callbacks)
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    NewChat,
    SelectConversation(String),
    DeleteConversation(String),
    // ...
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    pub fn set_conversations(&mut self, conversations: Vec<(String, String, Option<f64>)>, cx: &mut Context<Self>) {
        self.conversations = conversations;
        cx.notify();  // Trigger re-render
    }
}
```

**Entity initialization with globals** (app_controller.rs):

```rust
impl ChattyApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Store weak reference in global for later access
        let app_weak = cx.entity().downgrade();
        cx.set_global(GlobalChattyApp {
            entity: Some(app_weak),
        });
        
        let app = Self { /* ... */ };
        app.setup_callbacks(cx);
        app
    }
}
```

**Gotcha**: Use `cx.notify()` after mutating entity state to trigger re-renders.

### 5. View Rendering Patterns

**Pattern**: Implement `Render` trait returning nested element builders using fluent API.

```rust
impl Render for ChattyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .child(AppTitleBar::new(self.sidebar_view.clone()))
            .child(
                div()
                    .flex_1()
                    .flex_row()
                    .child(self.sidebar_view.clone())
                    .child(self.chat_view.clone())
            )
            .when(condition, |this| {
                // Conditional rendering
                this.child(some_element)
            })
    }
}
```

**Rendering collections** (sidebar_view.rs):

```rust
.children(
    self.conversations
        .iter()
        .enumerate()
        .map(|(ix, (id, title, cost))| {
            let is_active = active_id.as_ref() == Some(id);
            
            div()
                .id(ix)  // Unique ID for each item
                .child(
                    ConversationItem::new(id.clone(), title.clone())
                        .active(is_active)
                        .cost(*cost)
                )
                .when(ix == 0, |this| this.mt_3())
        })
        .collect::<Vec<_>>()
)
```

**Event handlers**:

```rust
Button::new("toggle-sidebar")
    .icon(Icon::new(IconName::PanelLeftOpen))
    .on_click({
        let sidebar = sidebar.clone();
        move |_event, _window, cx| {
            sidebar.update(cx, |sidebar, cx| {
                sidebar.toggle_collapsed(cx);
            });
        }
    })
```

### 6. Context Type Patterns

GPUI provides different context types for different scopes:

- **`App`**: Application-level operations, full access to globals and entities
- **`AsyncApp`**: Limited async context, requires `cx.update()` for UI access
- **`Context<T>`**: Entity-specific context for a particular component
- **`Window`**: Window-specific operations (sizing, positioning, etc.)

```rust
// App context - full access
pub fn new(window: &mut Window, cx: &mut App) -> Self {
    let input = cx.new(|cx| InputState::new(window, cx));
    Self { input }
}

// Context<Self> - entity-specific
fn setup_callbacks(&self, cx: &mut Context<Self>) {
    let app_entity = cx.entity();
    // ...
}

// AsyncApp - limited, must use update()
cx.spawn(async move |cx: &mut AsyncApp| {
    let result = load_data().await;
    cx.update(|cx| {
        cx.set_global(result);
    }).ok();
})
.detach();
```

### 7. Optimistic Update Pattern

**When to use**: For operations that persist data but need instant UI feedback.

**Pattern**: Update global state immediately, refresh UI, then save asynchronously with error handling.

```rust
// models_controller.rs
pub fn create_model(mut config: ModelConfig, cx: &mut App) {
    // 1. Update state immediately (optimistic)
    let model = cx.global_mut::<ModelsModel>();
    model.add_model(config);
    
    // 2. Get data for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();
    
    // 3. Refresh UI immediately
    cx.refresh_windows();
    
    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = MODELS_REPOSITORY.clone();
        if let Err(e) = repo.save_all(models_to_save).await {
            error!(error = ?e, "Failed to save models");
        }
    })
    .detach();
}
```

### 8. Closure Capture Pattern

**When to use**: Passing entities or data into event handlers and closures.

**Pattern**: With `cx.subscribe()`, the subscriber closure receives `&mut Self` directly — minimal cloning needed. For `IntoElement` component callbacks that must capture entity references, clone before the closure.

```rust
// EventEmitter subscription — direct &mut Self, no clone gymnastics
cx.subscribe(&self.sidebar_view, |app, _sidebar, event: &SidebarEvent, cx| {
    match event {
        SidebarEvent::SelectConversation(id) => {
            app.load_conversation(id, cx);  // Direct access to app
        }
        // ...
    }
}).detach();

// IntoElement callback — must clone entity reference
ConversationItem::new(id, title)
    .on_click({
        let entity = sidebar_entity.clone();  // Clone before closure
        let id = id.clone();
        move |_conv_id, cx| {
            entity.update(cx, |_, cx| {
                cx.emit(SidebarEvent::SelectConversation(id.clone()));
            });
        }
    })
```

**Gotcha**: Clone before the closure moves the data, then clone again inside if needed for multiple uses.

### 9. Deferred Updates Pattern

**When to use**: To avoid re-entering the same entity during an update (prevents borrow conflicts).

**Pattern**: Use `cx.defer()` to schedule work for the next frame.

```rust
// app_controller.rs
let chat_view = app.chat_view.clone();
let mid_for_defer = mid.clone();

cx.defer(move |cx| {
    let capabilities = cx
        .global::<ModelsModel>()
        .get_model(&mid_for_defer)
        .map(|m| (m.supports_images, m.supports_pdf))
        .unwrap_or((false, false));
    
    chat_view.update(cx, |view, cx| {
        view.chat_input_state().update(cx, |state, _cx| {
            state.set_capabilities(capabilities.0, capabilities.1);
        });
    });
});
```

**Gotcha**: Use `cx.defer()` when you need to update an entity that's currently being updated.

### 10. Stream Processing Pattern

**When to use**: Processing LLM response streams or other async iterators.

**Pattern**: Use `async_stream::stream!` with a single mapping function from rig's per-item stream to this app's `StreamChunk`s, rather than a macro duplicated per provider (AGE-210 replaced the old `process_agent_stream!` macro, which had two near-identical expansions, with `map_item`).

```rust
// llm_service.rs
fn map_item(item: MultiTurnStreamItem, semantics: UsageSemantics) -> Vec<StreamChunk> {
    match item {
        MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text)) => {
            vec![StreamChunk::Text(text.text)]
        }
        MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall { tool_call, .. }) => {
            vec![
                StreamChunk::ToolCallStarted { id: /* resolve_call_id(..) */, name: tool_call.function.name.clone() },
                StreamChunk::ToolCallInput { id: /* .. */, arguments: /* serialized args */ },
            ]
        }
        MultiTurnStreamItem::FinalResponse(final_response) => {
            // turn usage aggregate, plus TurnMessages when rig recorded the turn
            vec![/* .. */]
        }
        // CompletionCall, StreamUserItem (tool result), etc.
        _ => Vec::new(),
    }
}

pub async fn stream_prompt(agent: &AgentClient, history: Vec<Message>, /* .. */) -> Result<ResponseStream> {
    // TurnBudget (services/turn_budget.rs) sets rig's cap to max_agent_turns + 1:
    // late tool results say how many tool turns are left, and the extra call has
    // no tools, so an exhausted budget ends in a text answer, not MaxTurnsError.
    let mut agent_stream = TurnBudget::new(max_agent_turns)
        .apply(agent.agent.stream_prompt(user_message).history(history))
        .await;

    Ok(Box::pin(async_stream::stream! {
        loop {
            tokio::select! {
                item = agent_stream.next() => match item {
                    Some(result) => {
                        // map_stream_result: Ok(item) -> map_item(item, semantics);
                        // Err(e) -> classify_streaming_error(e) into a typed StreamErrorKind (AGE-244)
                        let (chunks, stop) = map_stream_result(result, semantics);
                        for chunk in chunks { yield Ok(chunk); }
                        if stop { return; }
                    }
                    None => { yield Ok(StreamChunk::Done); return; }
                },
                // .. approval / resolution / clarification channels
            }
        }
    }))
}
```

### 11. StreamManager Pattern (Centralized Stream Lifecycle)

**When to use**: For managing the lifecycle of long-running async operations (like LLM response streams) that need coordinated state, cancellation, and event-driven UI updates.

**Pattern**: A GPUI entity (`StreamManager`) owns all stream state in a `HashMap<String, StreamState>`, emits typed events via `EventEmitter`, and uses `Arc<AtomicBool>` cancellation tokens for graceful shutdown.

**Architecture**:

```
send_message() ──► StreamManager ──► StreamManagerEvent ──► handle_stream_manager_event()
     │                  │                                            │
     │              owns task,                                  routes to
     │           cancel_flag,                               ChatView methods
     │           response_text                             (append_text, etc.)
     │                  │
     └── stream loop ───┘
         only updates:
         1. Conversation model
         2. StreamManager.handle_chunk()
```

**Key types** (`src/chatty/models/stream_manager.rs`):

```rust
pub enum StreamStatus { Active, Completed, Cancelled, Error(String) }

pub struct StreamState {
    epoch: u64,                            // registration epoch; a stale StreamEnded is ignored
    pub status: StreamStatus,
    pub token_usage: Option<TokenUsage>,   // built from `calls` once the aggregate arrives
    calls: Vec<ApiCallUsage>,              // one record per provider request, in order
    pub trace_json: Option<serde_json::Value>,
    task: Option<Task<anyhow::Result<()>>>,
    cancel_flag: Arc<AtomicBool>,
    pending_artifacts: Option<PendingArtifacts>, // queued by AddAttachmentTool, drained on finalize
    has_emitted_first_chunk: bool,         // first text chunk goes out immediately, later ones batched
    // ... plus the per-stream text-batching buffer (pending_text, last_flush)
    text_batch: Option<SharedTextBatch>,   // the DesktopSink buffer one layer upstream
}
// There are two text-buffering layers, and one timer drains both.
// Layer 1 is `TextBatch` (`stream_manager.rs`), created per turn by
// `run_llm_stream`: `DesktopSink` (`message_ops_internals.rs`) fills it
// with raw `SessionEvent::Text` chunks under `should_flush_text` and
// `TextBatch::drain` is the single path that applies them to
// `Conversation.streaming_message`, before forwarding one coalesced
// chunk on; any non-text event flushes it first so ordering (e.g. a tool
// call's `text_before`) is never stranded (AGE-166). Layer 2 is
// `StreamState.pending_text`, which governs the emitted `TextChunk`
// rate. The batch is *shared*: `run_llm_stream` hands it to the manager
// with `attach_text_batch` and `StreamState.text_batch` holds it, because
// the sink lives inside the turn's future. `StreamManager` therefore owns
// the only timer — `flush_timer: Option<Task<()>>`, period
// `FLUSH_INTERVAL` = 20ms, spawned lazily on the first stream and
// stopping itself once none remain — and every flush inside the manager
// goes through `flush_text`, which drains layer 1 into layer 2 and then
// emits. That is what makes a pause after a burst paint (the timer
// reaches the layer that actually buffers) and what keeps Stop from
// truncating the reply: `stop_stream`, `cancel_pending` and both
// `register_*_stream` supersede paths `drop` the turn's task — and the
// sink with it — so each calls `flush_text` first (AGE-372).

pub enum StreamManagerEvent {
    StreamStarted { conversation_id },
    TextChunk { conversation_id, text },
    ToolCallStarted { conversation_id, id, name },
    TokenUsage { conversation_id, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens },
    // ... 7 more variants
    StreamEnded { conversation_id, status, response_text, token_usage, trace_json },
}
```

**Cancellation token pattern** (replaces task drop):

```rust
// Create cancel flag before spawning
let cancel_flag = Arc::new(AtomicBool::new(false));
let cancel_flag_for_loop = cancel_flag.clone();

// In stream loop: check at top of each iteration
while let Some(chunk) = stream.next().await {
    if cancel_flag_for_loop.load(Ordering::Relaxed) {
        break;  // Clean exit
    }
    // process chunk...
}

// To stop: set flag (stream exits cleanly on next iteration)
cancel_flag.store(true, Ordering::Relaxed);
```

**Event-driven finalization** (replaces inline finalization in async block):

```rust
// Stream loop only does two things:
// 1. Updates Conversation model (source of truth for background streams)
// 2. Forwards chunks to StreamManager via handle_chunk()

// All finalization happens in the event handler:
fn handle_stream_manager_event(&mut self, event: &StreamManagerEvent, cx: ...) {
    match event {
        StreamEnded { status: Completed, .. } => self.finalize_completed_stream(...),
        StreamEnded { status: Cancelled, .. } => self.finalize_stopped_stream(...),
        TextChunk { .. } => chat_view.append_assistant_text(...),
        // ...
    }
}
```

**Pending stream promotion** (for streams started before conversation creation):

```rust
// Register under "__pending__" key
mgr.register_pending_stream(task, resolved_id, cancel_flag, cx);

// Once conversation ID is known, promote to real key
mgr.promote_pending(&conv_id);
```

**Design principles**:
- **Single source of truth**: StreamManager owns all stream state; ChattyApp has no stream-related fields
- **Decoupled UI**: Stream loop never calls `chat_view.update()` directly; events decouple the stream from the UI
- **Graceful cancellation**: Cancel flag checked at loop top; no task drops mid-execution
- **Conversation-scoped**: Events tagged with `conversation_id` so handlers can filter for the active conversation

### Key Takeaways

1. **Globals** are initialized at startup and accessed throughout the app via `cx.global()`
2. **Events** enable decoupled communication; always `.detach()` subscriptions
3. **Async operations** use `cx.spawn()` with `AsyncApp` context
4. **Entities** are cloned and stored as `WeakEntity` in globals
5. **Optimistic updates** provide instant UI feedback before async persistence
6. **Tasks** allow async operations that complete later
7. **Closures** must clone captured entities to avoid borrow issues
8. **Deferred updates** prevent re-entrancy problems with `cx.defer()`
9. **Render** uses fluent API for composing UI elements
10. **Context types** determine available operations and access levels
11. **StreamManager** centralizes stream lifecycle with events and cancellation tokens



## Model Capability Architecture

Model capabilities (image/PDF/temperature support) are stored in two complementary layers:

### Layer 1: ProviderType::default_capabilities() - Initialization Defaults

**Location**: `src/settings/models/providers_store.rs`

**Purpose**: Provides default capability values when creating new models.

`ProviderType` has three variants today — `OpenRouter`, `Ollama`, and
`AzureOpenAI`. The former per-provider variants (`OpenAI`, `Anthropic`,
`Gemini`, `Mistral`) were removed because OpenRouter now fronts all of those
providers as a single gateway client; each removed variant carries a serde
`alias` onto `OpenRouter` so existing stored JSON with the old variant names
keeps deserializing without a migration step.

**Implementation**:
```rust
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// OpenRouter — gateway to 200+ models (Anthropic, Google, Mistral, Meta, etc.)
    /// Accepts legacy JSON values from removed provider variants for backward compatibility.
    #[serde(
        alias = "open_ai",
        alias = "open_a_i",
        alias = "anthropic",
        alias = "gemini",
        alias = "mistral"
    )]
    OpenRouter,
    Ollama,
    #[serde(rename = "azure_openai")]
    AzureOpenAI,
}

impl ProviderType {
    pub fn default_capabilities(&self) -> (bool, bool) {
        match self {
            // OpenRouter is a gateway to multimodal models (Anthropic, Google, etc.)
            ProviderType::OpenRouter => (true, true),   // Images + PDFs
            ProviderType::AzureOpenAI => (true, false), // Images only (PDF lossy)
            ProviderType::Ollama => (false, false),     // Per-model detection
        }
    }
}
```

`ProviderType` has exactly these three variants (`crates/chatty-core/src/settings/models/providers_store.rs`). The removed direct providers (`anthropic`, `gemini`, `mistral`, `open_ai`) survive only as serde aliases that deserialize to `OpenRouter`, so old settings files still load.

**Used in**:
- `models_controller.rs`: When creating new models
- `main.rs`: Applying defaults at startup for models with unset capabilities

### Layer 2: ModelConfig - Persisted Per-Model State

**Location**: `src/settings/models/models_store.rs` → JSON storage

**Purpose**: 
- Stores actual per-model capabilities (critical for Ollama)
- Drives UI decisions (show/hide attachment buttons)
- Used for runtime validation before sending to LLM

**Fields**:
```rust
pub struct ModelConfig {
    pub supports_images: bool,
    pub supports_pdf: bool,
    pub supports_temperature: bool,
    // ... other fields
}
```

**Special Case: Ollama Models**

Ollama models have **per-model** capabilities that are dynamically detected:
- Vision capability varies by model (e.g., `llama3.2-vision:latest` vs `llama3.2:latest`)
- Detected via `/api/show` endpoint in `sync_service.rs`
- Stored in ModelConfig for persistence across app restarts

**Usage Locations**:

1. **UI Layer** (app_controller.rs, chat_input.rs):
   ```rust
   // Read from ModelConfig to show/hide buttons
   let model_config = cx.global::<ModelsModel>().get_model(&model_id)?;
   input_state.set_capabilities(model_config.supports_images, model_config.supports_pdf);
   ```

2. **Message Send** (app_controller.rs):
   ```rust
   // Filter attachments before sending to LLM
   let model_config = cx.global::<ModelsModel>().get_model(&model_id)?;
   if is_pdf && !model_config.supports_pdf {
       warn!("Skipping PDF: model doesn't support PDFs");
       continue;
   }
   ```

3. **Agent Creation** (agent_factory.rs):
   ```rust
   // Conditionally set temperature for OpenAI reasoning models
   if model_config.supports_temperature {
       builder = builder.temperature(model_config.temperature as f64);
   }
   ```

### Why Two Layers?

- **Layer 1 (ProviderType defaults)**: Quick initialization for new models
- **Layer 2 (ModelConfig persistence)**: Handles per-model overrides (Ollama) and user preferences

### Adding New Providers

Most new models need no new provider: OpenRouter already fronts Anthropic, Google, Mistral, Meta and others, so prefer adding the model under `ProviderType::OpenRouter`. A genuinely new backend (its own API shape, like Azure) needs:

1. Add variant to `ProviderType` enum (with `display_name()` and a stable serde name)
2. Update `ProviderType::default_capabilities()` with provider defaults
3. ModelConfig automatically inherits these defaults via `create_model()` controller

**That's it!** No need to update multiple capability checks scattered throughout the codebase.

### Architectural Benefits

✅ Single source of truth for runtime decisions (ModelConfig)
✅ Supports per-model capabilities (Ollama vision detection)
✅ UI shows correct attachment options based on selected model
✅ Prevents sending unsupported attachments to LLM APIs
✅ Simple to extend with new providers


## Error Handling Pattern: Avoid Silent Failures

Never use `.ok()` to silently discard errors from UI updates or other operations. Always log failures for debugging.

**Bad:**

```rust
cx.update(|_, cx| {
    cx.refresh_windows();
}).ok(); // Error silently discarded!
```

**Good:**

```rust
cx.update(|_, cx| {
    cx.refresh_windows();
}).map_err(|e| warn!(error = ?e, "Failed to refresh windows"))
.ok();
```

**When to propagate vs log:**
- **Log as `warn!()`**: UI refresh failures, non-critical updates
- **Propagate with `?`**: File I/O failures, download failures, critical operations

## Tool Error Reporting Pattern

Every `impl Tool` block's error path must route through `map_tool_error(tool_name, error)` (`crates/chatty-core/src/tools/mod.rs`) instead of relying on rig's default `Tool::map_error`. rig's default (`ToolExecutionError::from_error`) redacts arbitrary source errors down to a generic per-kind message — for `ToolErrorKind::Other` that message is the literal string `"the tool failed"`, which is all the model and the transcript ever see, regardless of what actually went wrong.

```rust
// WRONG — real failure reason is redacted to "the tool failed"
.map_err(|e| ToolExecutionError::from_error(e))

// CORRECT — tool name + real message stay visible to the model and the UI
.map_err(|e| map_tool_error("my_tool_name", e))
```

`map_tool_error` prefixes the tool name onto the message and best-effort classifies the failure (timeout/permission/not-found/network/other) for rig's retryability hint — the classification only affects telemetry, the full message reaches the model either way. Only do this for errors the tool authored itself; don't route raw provider/third-party error text through it without checking it first, since that's the case rig's redaction exists for.

## Complex Function Documentation Pattern

For functions exceeding ~100 lines with multiple responsibilities, add comprehensive documentation with phase markers:

```rust
/// Brief description of what the function does
///
/// Detailed breakdown of phases:
/// 1. Phase one description
/// 2. Phase two description
/// 3. Phase three description
///
/// # Note
/// Acknowledge complexity and future refactoring opportunities
fn complex_function(&mut self, ...) {
    // PHASE 1: Clear description
    // ... code ...
    
    // PHASE 2: Another clear description
    // ... code ...
    
    // PHASE 3: Yet another clear description
    // ... code ...
}
```

This pattern:
- Helps navigate large functions
- Documents intent without changing behavior
- Flags technical debt for future refactoring
- Makes code reviews easier

## Filesystem Tools Configuration

To enable filesystem tools:
1. Open Settings → Execution
2. Set workspace directory (absolute path required)
3. Enable code execution
4. Configure approval mode

## Security Practices

### MCP API Key Masking

`McpServerConfig` (`crates/chatty-core/src/settings/models/mcp_store.rs`) describes an already-running MCP endpoint: `name`, `url`, `api_key: Option<String>` (sent as `Authorization: Bearer …`), `enabled`, `is_module`. There is no env-var map; the API key is the only secret. The LLM must never see its value.

**Rule**: Any path that sends `McpServerConfig` data to the LLM reports `has_api_key()` (a `bool`), never the `api_key` field.

```rust
// WRONG — sends the real secret to the LLM
let key = server.api_key.clone();

// CORRECT — only whether a key is configured
let has_api_key = server.has_api_key();
```

**Where masking is applied today:**
- `list_mcp_services` tool output (`McpServerSummary.has_api_key`, `tools/list_mcp_tool.rs`; `test_list_masks_api_key` pins it)

**Where masking must be added if new LLM-facing surfaces are added:**
- Any future tool that returns `McpServerConfig` data
- Any future "show config" or "status" tool output
- Log statements that could capture tool args/results in a trace visible to users

### Masked Sentinel Preservation

`MASKED_API_KEY_SENTINEL = "****"` (`mcp_store.rs`) means "preserve the existing stored value". Any write path that accepts an API key from the LLM must resolve it:

```rust
// If LLM sends back "****", keep the real stored value — don't overwrite
let api_key = if incoming.as_deref() == Some(MASKED_API_KEY_SENTINEL) {
    existing.api_key.clone()
} else {
    incoming // LLM sent a new real value — store it
};
```

**If a new tool accepts an API key as input**, apply the same sentinel resolution pattern, and hold `MCP_WRITE_LOCK` (same file) around load → modify → save so concurrent tool calls cannot race.

**If adding a new server**: reject `****` with a clear error — there is no existing value to preserve.

### Logging Rules

Never log sensitive values. Log presence, not the key.

```rust
// WRONG
tracing::info!(api_key = ?server.api_key, "Server configured");

// CORRECT
tracing::info!(has_api_key = server.has_api_key(), "Server configured");
```

### New LLM-Facing Output Structs

When adding a new `#[derive(Serialize)]` struct that will be returned as tool output:

1. If it wraps `McpServerConfig`, expose `has_api_key` — never `api_key`
2. If it includes any `ProviderConfig` fields, exclude `api_key` (it is never exposed to the LLM)
3. Add a test that the output for a server with a real API key does not contain the key

### Where Real Values Are Safe

These paths use raw (unmasked) values intentionally:

| Location | Why raw values are safe |
|:---------|:------------------------|
| `McpService::connect` (`services/mcp_service.rs`) | Sets the bearer auth header on the HTTP transport, never sent to LLM |
| `McpRepository` (`settings/repositories/`) | Disk persistence, private config directory |
| `providers_store.rs` → disk | API keys for LLM API auth, private storage only |
