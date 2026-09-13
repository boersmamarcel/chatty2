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
- **Stream Lifecycle**: LLM response streams are managed by `StreamManager` (`src/chatty/models/stream_manager.rs`), a centralized GPUI entity that owns all stream state, emits events for decoupled UI updates, and uses cancellation tokens for graceful shutdown. Each registered stream carries a monotonic epoch so a `StreamEnded` event from a superseded stream (e.g. a synchronously re-registered follow-up turn) is ignored rather than tearing down the new one. Both frontends drive chatty-core's shared `run_stream_loop` (`crates/chatty-core/src/services/stream_processor.rs`): the loop, its cancellation checks and its stall watchdog (`STALL_TIMEOUT` = 180s, ends a turn that yields nothing for too long) live there, and the one `StreamChunkHandler` that drives it is chatty-core's own `SessionStreamHandler` (see AgentSession below); neither frontend has a handler of its own any more (AGE-192, AGE-195). Turn-lifecycle behaviour is changed in core, once. `on_chunk` is synchronous: the Azure Entra token is attached to every request by `AzureAuthHttpClient` (`factories/agent_factory/azure_auth_http.rs`, an `HttpClientExt` wrapper over the token cache, AGE-245), so a handler never has to await a token refresh; the trait carries no `Send` bound, since the desktop's event sink holds an `AsyncApp`. Scripted-stream fixtures for both frontends' characterization tests live in `chatty_core::services::stream_fixtures` behind the `test-support` feature, with golden event sequences under `crates/chatty-core/src/services/goldens/`, `crates/chatty-core/src/session/goldens/`, `crates/chatty-tui/src/engine/goldens/` and `crates/chatty-gpui/src/chatty/controllers/app_controller/goldens/` (`UPDATE_GOLDENS=1` rewrites them). See Idiomatic Patterns > StreamManager Pattern for details.
- **AgentSession (AGE-194)**: `chatty_core::session::AgentSession` (`crates/chatty-core/src/session/`) is the turn written once: it owns one `Conversation` plus the three per-agent stores (execution approvals, write approvals, clarifications) whose handles the agent's tools were built with — the owner assembles only the services part of an `AgentBuildContext` and the session completes it (`build_context`), builds and owns the conversation (`create_conversation` / `restore_conversation`) or installs a rebuilt agent on it (`install_agent`; AGE-272) — and `begin_turn` does approval channels → `shape_context` → `stream_prompt` → `run_stream_loop` and returns the turn as a future for the frontend to spawn. Every outcome arrives as a `SessionEvent` (`session/event.rs`, the contract every frontend binds to): `TurnStarted` first, `TurnEnded` exactly once and last, `Error`/`Cancelled` before it saying why, and `FollowUp` after it carrying the next turn's prompt. The per-chunk protocol logic that used to live in each frontend's handler — todo-protocol follow-up, loop guard, malformed-tool-call retry (`MALFORMED_TOOL_CALL_FOLLOW_UP` lives here), usage folding — is `SessionStreamHandler` (`session/handler.rs`), driven by a `TurnPolicy` the session builds from `AgentSessionConfig` (settings by value; the session never calls a repository and holds no `'static` state, so two sessions in one process share nothing). The owner feeds events back through `apply()`, which records the turn on the conversation: streaming text, the trace (tool calls, approvals, clarifications, the delegation row; AGE-274), turn messages, the todo snapshot, usage. On `TurnEnded` it calls `finish_turn(trace, artifacts)`, which commits under the shared empty-turn rule; `None` persists the session's own trace, and the desktop passes the ChatView's instead because it carries the user's clarification answers. `finish_turn` is also the turn barrier for `Conversation`'s lifetime totals (AGE-351): it counts that turn's `TraceItem::ToolCall`s into `tool_call_count`, sets `context_tokens` to the last completed API call's prompt size (`input + cache_read + cache_write`, output excluded), and — since it is the only place a turn's usage is priced — calls `usage.calculate_cost` against the `TokenPricing` (`ModelConfig::token_pricing()`) bound to the conversation wherever its model is bound (`create_conversation` / `restore_conversation` / `install_agent`, which takes `&ModelConfig` rather than a bare `model_id` for this reason); an unpriced model leaves the turn's cost `None`. Because the session holds no `RepositoryRegistry`, prices are fixed at bind time, not looked up live per turn, so a price edited in Settings mid-conversation applies from the next model switch or reload. `recovery_action(&error)` is the owner's stream-error policy (`decide_recovery` for the session's surface) with the attempt bookkeeping inside the session, per error kind and reset by a human turn (AGE-273); headless is the one surface that retries, and sends its recovery prompt as a `ProtocolFollowUp` turn so the budget holds. Silence is not an answer (AGE-401): every chat agent carries the rig hook `EmptyTurnRetry` (`factories/agent_factory/empty_turn_retry.rs`), which rejects the first tool-free model turn with no text and no tool call — reasoning alone does not count, which is how a qwen3 tool call written into the thinking channel looks — and retries it once inside the same turn with `EMPTY_COMPLETION_FOLLOW_UP`; a still-empty final completion is reported by the handler as `StreamErrorKind::EmptyCompletion` (`Stop` on every surface), so headless exits non-zero with the error on stderr and a worker's task ends `failed` instead of reaching its leader as done. Adapters: `From<SessionEvent> for AppEvent` (chatty-tui `events.rs`) and `StreamManager::handle_session_event` (chatty-gpui). Both frontends run on it (AGE-195): chatty-tui's `ChatEngine` owns one `AgentSession` and keeps only display state; on the desktop `ConversationsStore` holds an `AgentSession` per loaded conversation (`get_session`/`get_session_mut`, with `get_conversation` delegating), so each conversation's agent raises approvals on its own stores — there are no app-wide approval-store globals any more, and the approve/deny/clarification UI resolves a request through `ConversationsStore::resolve_execution_approval` / `resolve_write_approval` / `resolve_clarification`, which find the session that raised it. `run_llm_stream` (`message_ops_internals.rs`) is the desktop's event sink (`DesktopSink`): it calls `session.apply` for the conversation, forwards to `StreamManager`, and keeps the desktop-only work — ChatView trace capture before `Error`/`TurnEnded`, the delegation row and plan strip in the view, follow-up injection. Raw `SessionEvent::Text` chunks are coalesced upstream of both calls by a `TextBatch` (`stream_manager.rs`) the sink fills, sharing `StreamManager`'s `should_flush_text` policy (`FLUSH_INTERVAL` = 20ms; see Idiomatic Patterns > StreamManager Pattern), and any non-text event flushes it first (AGE-166). That batch is shared with `StreamManager` (`attach_text_batch`), since the sink dies with the turn's future: the manager's flush timer drains it, and so does every path that drops the task — `stop_stream`, `cancel_pending`, both supersede paths — or Stop would truncate the reply in the UI and in the persisted message (AGE-372). `finalize_completed_stream` and `finalize_stopped_stream` call `finish_turn`; an errored desktop turn is finalized like a stopped one, so no user message is left dangling. `begin_turn_with_flag` lets `StreamManager` register the turn with the cancel token before the turn starts. Each frontend's characterization replays the scenarios through the session and its adapter (`engine/characterization.rs`, `session_characterization.rs`). Session `TurnKind::Regenerate` takes the tail user message off the history snapshot and resends it; nothing is added. Headless and `--pipe` ride the session directly (AGE-196): `chatty-tui/src/headless/runner.rs`'s `HeadlessRunner` owns an `AgentSession` and a `Transcript` (`engine/transcript.rs`, the terminal transcript bookkeeping shared with `ChatEngine`) and nothing of the terminal; both assemble an `AgentServices` and hand it to `AgentBuildContext::from_services` (`chatty-core/src/factories/agent_factory/build_context.rs`), the single place the services half of a build context is written — chatty-gpui and `hive`'s `chatty-server` use the same seam, and `gated_exec_settings` is the shared gate deciding whether an agent gets execution tools at all (AGE-293). A delegated child reports its turn to the parent through the runner's event observer — the broker participant's socket (`chatty-tui/src/participant/`, AGE-301) — and `SessionEvent` is serde-serializable so it can cross that boundary. Stderr is the child's human-readable log and nothing else, which `/agent` shows. There is no other progress protocol. A child's `ask_user` is not cancelled when a parent is listening: it parks the child's task in A2A `input-required` with the question attached, the parent's `invoke_agent` re-asks it on the parent's own clarification store (the same popover, or one more hop up if the parent is a worker too), and the answer comes back down as a broker `input` frame that `chatty_protocol_gateway::worker::answer_clarifications` resolves on the child's store (ADR-0011 C7, AGE-306). Plain `--headless` still cancels, since nobody is listening.
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
  - `MathRendererService::CACHE_VERSION` is hashed into the cache key: base SVGs are written once and never cleaned, so a rendering change needs a bump to reach existing installs
- **Pdfium Library Cache**: The pdfium native library is cached in a persistent user-data directory so it survives bundle corruption, app translocation, and partial auto-update rsyncs. Lookup order in `pdfium_utils::create_pdfium()`: user-data cache → exe-relative → `CHATTY_PDFIUM_LIB_DIR` env → compile-time `PDFIUM_LIB_DIR` → system. On every successful bind, the library is opportunistically copied to the cache (`self_heal_copy()`). Platform paths:
  - **macOS**: `~/Library/Application Support/chatty/lib/`
  - **Linux**: `~/.local/share/chatty/lib/` or `$XDG_DATA_HOME/chatty/lib/`
  - **Windows**: `%APPDATA%\chatty\lib\`
- **Sandbox Container Lifecycle**: `SandboxManager` (`crates/chatty-core/src/sandbox/manager.rs`) lazily starts one long-lived Docker container per language (`sleep infinity`, no `--rm`), so a leaked container outlives the process. There is no explicit call site that tears these down when a conversation ends, is deleted, or gets its agent rebuilt — ordinary `Arc` drop is the only signal — so cleanup is a `Drop` impl on `SandboxManager` rather than a call any of those paths must remember to make. `Drop::drop` can't await the async Docker removal, so it only spawns a detached task on the already-entered Tokio runtime (`Handle::try_current()`); detached tasks are cancelled when the runtime is dropped, which happens immediately after `app.run` returns, so a manager dropped that late would still leak. Every `SandboxManager`'s container map is therefore also held by a process-wide registry (`LIVE_SANDBOXES`) from construction until its cleanup actually completes, and both frontends drain it synchronously on the way out: `chatty-gpui/src/main.rs` blocks on `sandbox::shutdown_all()` after the window closes but before `_tokio_runtime` drops, and `chatty-tui/src/main.rs` awaits it after `app::run`/headless finishes. `destroy()` remains a real `pub async fn` for callers that want a graceful, awaited shutdown instead of the detached best-effort one. A startup sweep for containers orphaned by a crash or `SIGKILL` is deliberately not implemented.
- **Built-in Browser** (AGE-142, `browser` feature): A CDP control layer over a real Chrome, in `crates/chatty-core/src/services/browser/`. Headless. By default the agent's tools reach only `localhost` and workspace-local `file://` URLs (Lane A) — the self-review loop (render → screenshot → critique → fix) carries no credentials and needs no approval gate. When the app's internet-access setting (`ExecutionSettings::fetch_enabled`, the same toggle that gates `fetch_tool`/`search_web`) is on, the agent factory builds an open-web manager (`BrowserManager::open_web`) instead: `NavigationPolicy::Open` allows any public http(s) host too, still refusing private/internal network targets via the same SSRF denylist as `fetch_tool` (`services::ssrf_guard`, shared so the two can't drift). The profile stays ephemeral either way — no stored credentials, so still no approval gate. The navigation policy lives on the profile (`profile.rs`), not in the tools, so Lane B's per-task origin allowlist (`AGE-158`, not implemented yet) extends it rather than replacing it. Element refs from `browser_snapshot` carry a generation that navigation invalidates; a stale ref is refused, never mis-resolved. Console and network output is drained to files under `<workspace>/.chatty/browser/` and summarized, rather than dumped into context. `browser_screenshot` queues the PNG through `PendingArtifacts` (the `add_attachment` path) rather than returning `ToolResultContent::Image`: rig rejects tool-result images for OpenRouter, Ollama and OpenAI Chat Completions, and the conversion error kills the whole stream. The consequence is that the model sees a screenshot on the turn *after* it captures one. When a Lane A browser tool call appears in the transcript, `ChatView::maybe_open_browser_artifact` (chat_view/mod.rs) auto-docks a live artifact panel: it looks up the running conversation's `BrowserManager` via `services::browser::registry` (a `conversation_id → Arc<BrowserManager>` map the agent factory populates, since chatty-core has no other path to chatty-gpui's UI layer) and streams `Page.startScreencast` frames (`screencast.rs`) into it — watching the agent drive the page live, not a one-shot screenshot. The panel forwards mouse/keyboard input over CDP (`input.rs`) so the user can type/click directly. A `ControlLock` (`control.rs`) arbitrates: the agent holds control by default and needs no permission to act, but the user can *take* control at any moment (no negotiation), which refuses mutating session actions (navigate, resize) with `BrowserError::ControlHeldByUser` until released — read-only tools (snapshot, screenshot, console, network) are never gated, since watching never collides. Each real take/release transition (AGE-156) is recorded in the activity trail as a synthetic, already-finished tool row (`ToolCallBlock::browser_control_handoff`, classified as its own `ToolKind::Handoff` so it tallies as "N browser handoffs" instead of files explored) — `ChatView::record_browser_control_change` decides whether the row joins the live streaming trace (mid-turn; the stream's own finalization persists it) or is appended straight to the last assistant message via `Conversation::append_trace_item_to_last_assistant` (post-turn; persisted immediately), so the view and the store never disagree about which path was taken. A release with no turn running also resumes the agent (AGE-379): `ChattyApp::handle_browser_control_changed` sends a user message — "I've handed the browser back to you at `<url>`. Take a fresh look at the page and continue from where I left it." — since every element ref the agent held is stale after a takeover; a release mid-stream leaves it to the running turn's next tool call, which simply succeeds. Click/input mapping (`browser_viewport_position` in `artifact_view.rs`) goes through the frame actually on screen — a `FrameGeometry` built from the raster's pixel size plus the CSS viewport Chrome reports alongside it (`ScreencastFrame::css_width`/`css_height`) — never through `browser_requested_size`, which is only what the viewport was last *asked* to be and disagrees with the frame on screen during a resize debounce, after a refused CDP retarget, or once Chrome downscales the raster to `maxWidth`/`maxHeight`. The frame is painted by hand with `window.paint_image` off the same canvas element that records its bounds, not `img().object_fit(Contain)` — that painted nothing in `ArtifactMode::Full` (not root-caused inside gpui) while the same frame painted fine once the panel was docked again. Chrome is not bundled: `provisioning.rs` prefers a cached pinned Chrome for Testing build, then an installed Chrome/Chromium/Edge at or above `MIN_CHROME_MAJOR`, then downloads the pinned build and verifies it against a SHA-256 committed in that file. Regenerate the pin with `scripts/pin-chrome.sh`. Platform cache paths:
  - **macOS**: `~/Library/Application Support/chatty/browsers/<version>/`
  - **Linux**: `~/.local/share/chatty/browsers/<version>/` or `$XDG_DATA_HOME/chatty/browsers/<version>/`
  - **Windows**: `%APPDATA%\chatty\browsers\<version>\`
- **Transcript Rendering**: The desktop transcript renders conversation history as typed blocks (`crates/chatty-gpui/src/chatty/views/transcript/`) — turns, tool rows, diffs, plans, artifact cards, approvals, etc. — built from `MessageEntry` + `system_trace` JSON via `adapt_message()`/`adapt_messages()`. Persistence stays untyped in chatty-core; these typed block types live only in chatty-gpui. The list itself renders on gpui's `list`/`ListState`, not gpui-component's `v_virtual_list`: `List` measures each item as it lays it out and caches the result, so turn heights are an output, not a hand-estimated input (the old `TranscriptLayout` estimator was deleted along with every height constant it needed). Use `ListAlignment::Top`, not `Bottom` — `Bottom` re-nulls the scroll anchor every frame while pinned, so `bounds_for_item` returns `None` and the plan strip's measured geometry disappears; sticky-to-bottom is instead one line (anchor past the last item, let `layout_items` backfill). `set_scroll_handler`'s callback runs while `ListState`'s internal `RefCell` is mutably borrowed, so it may only set a flag — calling back into the list from inside it panics. `adapt_message()` emits a `Block::Plan` per message that called `write_todos`, but every plan block renders the same live conversation-level snapshot, so a follow-up turn that re-plans would otherwise paint the identical panel twice; `retain_last_plan_block()` (`transcript/adapter.rs`) keeps only the newest plan block per adapted turn list. `turn_fingerprint()` (`chat_view/mod.rs`) is what tells `ListState` a turn's rendered height may have changed, so its contract is that a turn whose height *can* change must hash differently — that includes out-of-band state a block itself doesn't carry: `Block::Plan` renders the live `AgentTaskSnapshot`, not its own fields, so the fingerprint takes the snapshot as a separate argument and hashes `write_todos_called`, todo count/status/text-length per todo; `Block::Approval` hashes `approval.command.len()` since the alert card grows with the command (a heredoc runs to many lines) (AGE-338). `ChatView` does not rebuild the typed transcript from scratch on every `cx.notify()` (a streaming turn notifies once per 20ms text batch): `chat_view/turn_cache.rs`'s `TurnCache` keeps last frame's adapted `Turn`s and re-adapts only the messages whose `adapt_key()` moved (index, role, content length, collapse state, shape of the live/persisted trace), so a frame costs O(changed turns) rather than O(history) (AGE-165). Plan-block placement is expressed as ownership rather than the old two destructive passes over a fresh vec: `plan_owner()` picks which turn draws the panel (the newest turn that planned for itself, else the last assistant turn while a snapshot is live) and `apply_plan_ownership()` re-asserts that against whatever the cache holds every frame, so a cached turn converges on what a full rebuild would produce; `retain_last_plan_block()`/`attach_plan_block()` (`transcript/adapter.rs`) still exist and back that logic but are no longer called directly from `ChatView`. `reset_transcript_list()` clears the cache on conversation switch, since cache entries are keyed by message index.
- **TUI plan card (AGE-342)**: chatty-tui renders the same todo-plan progress chatty-gpui's `Block::Plan` does, but as one card in `crates/chatty-tui/src/ui/plan.rs` — pure, ratatui-free row/status-line layout, testable without a frame — rewritten in place as `write_todos`/`update_todo`/`verify_completion` calls arrive, instead of a tool-call row per call. `is_agent_todo_tool()` and `snapshot_from_tool_output()` (`chatty-core/src/services/agent_task_controller.rs`, re-exported from `services::mod`) are the shared definitions both frontends read a todo tool's output through. `Transcript::plan` (`chatty-tui/src/engine/transcript.rs`) holds the latest snapshot for the status bar's live position (`ui/status_bar.rs`, `plan::status_line`) and is cleared on the next user turn, mirroring `AgentTaskController::reset()` — that reset is what makes collapsing a verified plan to its header affordable. `plan::plan_counts()` mirrors the desktop client's `plan_counts` so the two frontends never disagree about progress. A failed todo call still renders as an ordinary error row (the AGE-340 invariant that errors are never folded or hidden); `/verbose` still shows the raw tool payloads, plan tools included.
- **Tool turns are persisted as rig produced them (AGE-247, D1 = b)**: a turn with tool calls persists every message rig recorded for it, in order — assistant tool-call message(s), user tool-result message(s), then the final assistant text — so the model sees its own tool activity on later turns. rig hands the list over on the final response (`PromptResponse.messages`); `llm_service` yields it as `StreamChunk::TurnMessages` before `Done`, each frontend parks it on the `Conversation` (`set_streaming_turn_messages`), and the turn's finalizer persists the tool round-trips ahead of the final text entry. On the streamed path that finalizer is `Conversation::finalize_turn` (AGE-243, both frontends), which delegates to the same `finalize_response_state` helper that `Conversation::finalize_response` uses for slash-command results — one ordering, not two that can drift. Both take the parked record, so a dropped turn discards it instead of leaking it into the next. Only the final text entry carries the trace JSON and attachments; tool entries carry neither, and `turn_tool_messages` cuts after the last tool result so no tool-call message is ever persisted without its result (OpenAI-compatible endpoints reject orphans). Payloads are kept whole; compaction bounds them (AGE-248). Readers that render, export or count turns skip tool messages via `services::is_tool_message` (transcript `load_history`, markdown/ATIF/JSONL exporters) and count *exchanges* with `services::exchange_count` (title trigger, `generate_title`), while regeneration (`remove_last_assistant_message`) pops the whole turn back to the user text message that started it.
- **Token usage is per provider request, normalised once**: rig-agent's multi-turn stream emits a `CompletionCall` item per provider request and a `FinalResponse` with the turn's aggregate. `llm_service::normalize_usage` turns each into an `ApiCallUsage` (`models/token_usage.rs`) with `input_tokens` meaning the *uncached* prompt share, so `input + cache_read + cache_write` is the whole prompt whichever provider convention the numbers arrived in (OpenAI-compatible reports cached tokens inside `prompt_tokens`; Anthropic native reports them separately). The stream yields `StreamChunk::ApiCallUsage` per request and a final `StreamChunk::TokenUsage` aggregate; `StreamManager` builds the persisted `TokenUsage` from the per-call records (`TokenUsage::from_calls`, which is where `api_turn_count` comes from) and only falls back to the aggregate when no per-call record arrived. Cache hit rate is a per-request property, so never sum first and divide later. Each call is logged as `LLM completion call usage` with `hit_rate`; that log line is the prompt-caching diagnostic (AGE-207). Cost uses `TokenPricing`, with cache read/write rates from `ModelConfig` when set (OpenRouter sync fills them from `pricing.input_cache_read` / `input_cache_write`) and the input rate otherwise. OpenRouter agents are built from `completion_model(..).with_prompt_caching()` rather than `client.agent(..)`, since that is the only place rig's `cache_control` opt-in lives; that marks the system message (preamble + tools). The conversation history caches only if the *latest* message carries a breakpoint too, and rig's hooks cannot add one (`RequestPatch` merges `additional_params` at the top level, so `messages` cannot be patched), so the OpenRouter client is built on `PromptCachingHttpClient` (`factories/agent_factory/prompt_cache_http.rs`), a `reqwest` wrapper implementing rig's `HttpClientExt` that rewrites every `POST …/chat/completions` body to mark the last user/assistant message (AGE-205). Two breakpoints total, within Anthropic's limit of four. The moving one rewrites the previously-last message every turn, so on the OpenRouter path the request history is append-only only modulo `cache_control` markers: `session/append_only_prefix.rs` records both provider paths against a fake daemon and pins exactly that, and `agent_factory/cache_breakpoint_probe.rs` (ignored, needs `OPENROUTER_API_KEY`) measures against the real provider whether the marker counts as cached content (AGE-291). MCP connections are a `BTreeMap` so the tool block is byte-stable across restarts (AGE-206).
- **Tool failure detection is text-based, not a flag**: the streamed `ToolResult` carries no error flag (rig's `is_error()` lives on `ToolExecutionResult`, which never reaches the stream), so `llm_service::tool_result_looks_like_error` recognizes a failure by sniffing the message text (`Error:` prefix, `"the tool failed"`, `"malformed JSON"`). `tools::mod::map_tool_error()` must keep writing that `Error: {tool_name}: {message}` prefix — dropping it once made every typed tool failure get filed as a success, with the error prose landing in `output` (a failed `compile_typst` minted an artifact card for a PDF that was never written). A test (`failures_are_recognisable_as_errors_downstream`) pins the two together; keep it green when touching either side.
- **Desktop boot order (AGE-161)**: `chatty-gpui/src/main.rs` opens the main window *first* inside the `Application::run` callback and registers every task, thread and settings load below it. `cx.open_window` draws its first frame synchronously, so anything above it — a `cx.spawn`, a `std::thread::spawn`, a repository open — runs before the user sees a window. Only constructors with no I/O in them belong above `open_window`. Two kinds of global have to be up there, and they fail differently. The first frame's render tree does exactly six hard `cx.global::<T>()` reads — `GeneralSettingsModel`, `ExecutionSettingsModel`, `ExtensionsModel`, `ErrorStore`, `AutoUpdater`, and `ConversationsStore` — and a missing one *panics on first paint*; the first five are set in `main.rs`, while `ConversationsStore` is set by `ChattyApp::new`, which still runs before the frame is drawn. The rest are `try_global` reads that fail *silently*, which is the more dangerous case: `GlobalStreamManager` and `GlobalModelsNotifier` would skip their `ChattyApp::new` subscriptions (dead streams, dead model picker) and `MemoryInitSignal` would skip the memory-ready wait and build an agent with no memory tools. Everything else the views touch (`TokenTrackingSettings`, `GlobalTokenBudget`, `DiscoveredModulesModel`, …) is a `try_global` read that degrades harmlessly, and is set early only because it costs nothing. `main()` itself must not `block_on` before `Application::run`: the conversation repository is handed to the window as `ConversationSqliteRepository::deferred()` and opens its pool on the first query. `boot_order_tests` in `main.rs` pins all of this against the file's own source.
- **Repository conformance suite (AGE-280)**: `chatty_core::repositories::store_conformance` (behind the `test-support` feature, the same seam as `services::stream_fixtures`) exercises create/read/update/delete against any implementation of the settings repository traits (`define_single_json_repository!`/`define_list_json_repository!` in `settings/repositories/mod.rs`) or `ConversationRepository`, via a `single_settings_conformance!`/`list_settings_conformance!`-generated fn per trait plus `conformance_conversation`. A repository keyed per name rather than holding one singleton value — `OAuthCredentialRepository` (MCP OAuth access/refresh tokens, AGE-317) — fits neither macro, so it gets a hand-written `conformance_oauth_credentials` instead, covering an absent server, a save/load round trip unchanged to the last field, replace-not-merge on a second save, and `clear` touching only the named server. Comparisons go through `serde_json::Value`, not `PartialEq`, so the suite needs no changes to the settings model structs. It's exported so an out-of-tree store-backed implementation (e.g. hive's) can run the identical suite from a dev-dependency and be proven equivalent to the JSON/SQLite backends shipped here; every JSON repository's `with_path` constructor and `ConversationSqliteRepository::deferred_with_path` exist only for this (`#[cfg(any(test, feature = "test-support"))]`). Replaces the deleted `InMemoryConversationRepository`.
- **SettingsSnapshot / SettingsDelta (AGE-283)**: `chatty_core::settings_snapshot` (re-exported as `chatty_core::{SettingsSnapshot, SettingsDelta}`) is a byte-stable view over the 12 `RepositoryRegistry` settings families (`ConversationRepository` is intentionally excluded — it isn't a registry field). A hosted guest boots from `RepositoryRegistry::from_snapshot(snapshot)`, an in-memory-only registry backed by `settings/repositories/in_memory_repository.rs` (production code, not `test-support`-gated), and on release reports back `registry.delta_since(&snapshot)` — a `SettingsDelta` with `Option<T>` per family, `None` meaning unchanged — which the real disk/DB-backed registry writes with `registry.apply(delta)`. `SettingsSnapshot::canonical_bytes()` recursively re-sorts every JSON object's keys (arrays are left in original order) rather than relying on the workspace's ambient serde_json `preserve_order` feature (pulled in transitively by GPUI), so cross-process snapshot bytes are byte-identical regardless of `HashMap`/`IndexMap` iteration order.
- **Broker / local participants (ADR-0011, AGE-301/AGE-314)**: `crates/chatty-protocol-gateway` fronts loaded WASM modules over OpenAI-completions, MCP and A2A, and now also **local participants** — processes that register over a Unix socket (`participant/`) and are served at the same `/a2a/{name}` routes, looked up ahead of modules. This half is Unix-only (`#[cfg(unix)]` on the gateway's `worker`/`participant` modules and on both consumers — `chatty-tui/src/participant/`, `chatty-gpui/src/chatty/services/broker_runner.rs`, and the wiring block in `module_settings_controller.rs`, AGE-339): on Windows the protocol gateway starts as just the gateway, and `--participant-socket` returns an `Err` instead of failing to compile. Keep any new code that touches this path behind the same `#[cfg(unix)]` gate. `ProtocolGateway::with_virtual_agent` is additive — each call inserts one more named worker into a `BTreeMap<String, Arc<dyn VirtualAgent>>` (`GatewayState.runners`), so `/a2a/{name}` and `list_agents` enumerate a whole declared team rather than one fixed participant (ADR-0011 C10 / AGE-377). Roles are declared in settings, not passed on `invoke_agent`, so the leader's tool schema and prompt stay identical whatever the team: `ModuleSettingsModel.virtual_agents: Vec<VirtualAgentConfig { name, model, disable_tools, extra_args }>` (`settings/models/module_settings.rs`), empty meaning the one default worker, `local-agent` (`chatty_core::tools::LOCAL_AGENT_NAME`). `chatty_core::services::virtual_agents::resolve_virtual_agents` is the one function — used by both `chatty-gpui/src/chatty/services/broker_runner.rs`'s `local_runners` and chatty-tui's `--broker` wiring — that turns that config into a `VirtualAgentSpec` per agent (name, a card description carrying its model and disabled tool groups, its children's argv, and the model endpoint to meter it on); each spawns a one-shot `chatty-tui` child per delegated task, waits for it to register, routes the task to it, and reaps it. The desktop wires this up with the gateway started by the module-settings controller, socket at `dirs::runtime_dir()/chatty/participants.sock`. Since AGE-376, chatty-tui gets the same wiring via `--broker` (valid with `--headless`, `--pipe`, and the interactive TUI alike): `chatty-tui/src/participant/broker.rs`'s `Broker::start` runs its own gateway on an ephemeral port and a pid-suffixed participant socket (so multiple headless leaders on one host, e.g. under the Harbor adapter, AGE-285, never collide), with no WASM module registry of its own — it exists only to make the declared virtual agents reachable. The published names reach the agent build as `AgentBuildContext`/`AgentServices.local_agents: Vec<String>` (`factories/agent_factory/build_context.rs`); `invoke_agent` (`tools/invoke_agent_tool.rs`, `with_local_agents`) resolves any of them when the gateway is running, ahead of WASM modules; `list_agents` (`with_local_workers`) advertises them the same way, and lets the broker's live card — carrying each worker's actual model and disabled tool groups — beat the static stand-in text once the gateway answers. This is a second fan-out path alongside `sub_agent`, not a replacement of it — ADR-0011 measured the two (`docs/research/adr-0011-broker-ab-2026-09-08.md`) and found the broker hop costs no more than ~20ms and its progress events are a strict superset of `sub_agent`'s, but not enough of a win to retire the direct path. The two paths share code deliberately, so a comparison between them is a comparison of the hop and nothing else: `chatty_core::services::worker_tree` (ADR-0012) is the one `git worktree`-per-worker implementation used by both `sub_agent_tool.rs` and the broker's `WorkspaceFactory` (`worker_tree::create_with_commit_hook` bundles the commit-on-exit hook both frontends' broker wiring need); `chatty_core::services::worker_endpoint::resolve_worker_endpoint` (resolving each named agent's own `--model`, so a reviewer on another server is metered on that server rather than the leader's) likewise backs `resolve_virtual_agents`' per-agent endpoint budget for both frontends' broker wiring; `tools::worker_executable()` resolves the same `chatty-tui` binary for both; and `tools::progress_text_for_event` renders the same progress lines for both, so the two delegation paths never drift in the parent's transcript. A task frame carries the caller's bearer when the A2A request had one (`BrokerFrame::Task.bearer`, `TaskBearer`, AGE-371): the gateway copies `Authorization: Bearer` off `message/send`/`message/stream` into the `DelegatedTask` it hands `submit_task` / `VirtualAgent::run_task` / `serve_one_task`'s closure, so a hosted worker (hive's `chatty-server` in participant mode) can validate it as the tenant boundary; a local worker ignores it, and a task without one is the pre-AGE-371 frame byte for byte. `TaskBearer`'s `Debug` is redacted; never log a frame's bearer. `create_with_commit_hook` also returns `merge_hint(&tree)` now, so a runner's `WorkerWorkspace.merge_hint: Option<String>` and `WorkerHandle::merge_hint()` (default `None`) let the gateway append the worktree's branch name to the worker's reported answer — on both `send_task` and `stream_task`, and on a failed task too, since a failed worker's partial edits are still on that branch (AGE-399, fixing a `merge_hint` that had existed and been unit-tested since AGE-376 with no caller). On `stream_task` the hint is sent as an artifact chunk *ahead of* the terminal status event: `A2aClient::send_message_stream` returns as soon as it sees a `final` status, so anything sent after would never be seen.
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
    let mut agent_stream = agent.agent.stream_prompt(user_message).history(history).max_turns(max_agent_turns).await;

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
