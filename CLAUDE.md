# Chatty

A desktop chat application built with Rust and GPUI.

## Tech Stack

- **UI Framework**: [GPUI](https://crates.io/crates/gpui) - Zed's GPU-accelerated UI framework
- **Components**: gpui-component for UI components
- **LLM Integration**: [rig-core](https://crates.io/crates/rig-core) for LLM operations
- **Async Runtime**: Tokio
- **Serialization**: serde/serde_json for persistence

## Project Structure

```
src/
├── main.rs              # Application entry point, initialization, theme handling
├── chatty/              # Main chat application module
└── settings/            # Settings system
    ├── controllers/     # Settings window controllers
    ├── models/          # Data models (providers, models, general settings)
    ├── providers/       # Provider implementations (e.g., Ollama)
    ├── repositories/    # Persistence layer (JSON file storage)
    ├── utils/           # Utilities (theme helpers)
    └── views/           # Settings UI views
```

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

# Run clippy lints
cargo clippy -- -D warnings
```

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
- **Stream Lifecycle**: LLM response streams are managed by `StreamManager` (`src/chatty/models/stream_manager.rs`), a centralized GPUI entity that owns all stream state, emits events for decoupled UI updates, and uses cancellation tokens for graceful shutdown. See Idiomatic Patterns > StreamManager Pattern for details.
- **Theme System**: Themes are loaded from `./themes` directory. User preferences (theme name + dark mode) are persisted to JSON.
- **Math Cache**: LaTeX math expressions are compiled to SVG using Typst and cached in platform-specific directories:
  - **macOS**: `~/Library/Application Support/chatty/math_cache/`
  - **Linux**: `~/.local/share/chatty/math_cache/` or `$XDG_DATA_HOME/chatty/math_cache/`
  - **Windows**: `%APPDATA%\chatty\math_cache\`
  - Base SVGs (`{hash}.svg`) are cached indefinitely for reuse
  - Styled SVGs (`{hash}.styled.{color_hash}.svg`) are theme-specific variants that are cleaned up on app restart
  - The `inject_svg_color()` method strips inline color attributes and injects CSS to apply theme colors

## CI/CD

### Workflows

| Workflow | Trigger | Purpose |
|:---------|:--------|:--------|
| **CI** (`ci.yml`) | PR to `main` | Tests, formatting check, clippy lints. Cargo dependencies and build artifacts are cached. |
| **Prepare Release** (`prepare-release.yml`) | PR merged with `release:patch`/`release:minor`/`release:major` label, or manual `workflow_dispatch` | Bumps version in `Cargo.toml`, generates categorized changelog, commits, creates tag + GitHub Release, then calls Release workflow directly via `workflow_call`. |
| **Release** (`release.yml`) | Called by Prepare Release via `workflow_call`, or manual GitHub Release publish | Builds cross-platform artifacts (Linux AppImage, macOS DMG, Windows EXE), generates checksums, uploads to release. Cargo cached per platform. |
| **Claude Code Review** (`claude-code-review.yml`) | PR opened/updated | Automated AI code review via Claude. |
| **Claude** (`claude.yml`) | `@claude` mention on issues/PRs | Interactive AI assistance. |
| **Update README** (`update-readme.yml`) | PR merged to `main` | Claude analyzes the diff; if user-facing features changed, opens a follow-up PR with README updates. Add `skip-readme` label to opt out. |
| **Dependency Check** (`dependency-check.yml`) | Weekly (Monday 9:00 UTC) or manual | Checks crates.io for dependency updates, creates grouped tech-debt issues. |

### Release Flow

The recommended release flow from Claude Code:

```
/create-release patch   # Adds release:patch label to current PR
```

Then merge the PR. The full pipeline runs as a single workflow:

```
PR merge → Prepare Release ──────────────────────────────────────────►
           (bump, changelog,    calls release.yml    (build 3 platforms,
            tag, GH release) ── via workflow_call ──► checksums, upload)
```

Key design: Prepare Release calls Release directly via `workflow_call` — no event-based handoff, no PAT needed, build status appears inline.

Alternative triggers:
- **Manual**: Actions UI → Prepare Release → Run workflow (with bump type selector and dry run option)
- **Manual release**: GitHub UI → Create Release → Release workflow runs standalone
- **On `main`**: `/create-release patch` triggers `workflow_dispatch` directly

### Changelog Generation

The Prepare Release workflow auto-generates release notes by parsing commits since the last tag:
- **Features & Improvements**: commits starting with Add, Feat, New, Implement, Wire
- **Bug Fixes**: commits starting with Fix, Bug, Resolve, Correct
- **Other Changes**: everything else (version bumps and merge commits are excluded)

The changelog becomes the GitHub Release body automatically.

## Idiomatic Patterns

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

**Entity references in globals**: Store `WeakEntity<T>` to avoid circular references:

```rust
// Create entity and store weak reference
let models_notifier = cx.new(|_cx| settings::models::ModelsNotifier::new());
cx.set_global(settings::models::GlobalModelsNotifier {
    entity: Some(models_notifier.downgrade()),
});

// Access later
if let Some(weak_notifier) = cx.try_global::<GlobalModelsNotifier>()
    .and_then(|g| g.entity.clone())
    && let Some(notifier) = weak_notifier.upgrade()
{
    notifier.update(cx, |_notifier, cx| {
        // Use notifier
    });
}
```

**Gotcha**: Always use `WeakEntity` when storing entities in globals to prevent memory leaks.

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

// Emit events (app_controller.rs)
if let Some(notifier) = weak_notifier.upgrade() {
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

**Pattern**: Use `async_stream::stream!` macro with pattern matching for clean stream processing.

```rust
// llm_service.rs
macro_rules! process_agent_stream {
    ($stream:expr) => {
        Box::pin(async_stream::stream! {
            while let Some(item) = $stream.next().await {
                match item {
                    Ok(StreamItem::Text(content)) => {
                        yield Ok(StreamChunk::Text(content.text));
                    }
                    Ok(StreamItem::ToolCall(tool_call)) => {
                        yield Ok(StreamChunk::ToolCallStarted {
                            id: tool_call.id.clone(),
                            name: tool_call.function.name.clone(),
                        });
                    }
                    Err(e) => {
                        yield Ok(StreamChunk::Error(e.to_string()));
                        return;
                    }
                    _ => {}
                }
            }
            yield Ok(StreamChunk::Done);
        })
    };
}

pub async fn stream_prompt(
    agent: &AgentClient,
    history: &[Message],
    contents: Vec<UserContent>,
) -> Result<(ResponseStream, Message)> {
    let stream = match agent {
        AgentClient::Anthropic(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history.to_vec())
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        // ... other providers
    };
    
    Ok((stream, user_message))
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
    response_text: String,
    status: StreamStatus,
    token_usage: Option<(u32, u32)>,
    trace_json: Option<serde_json::Value>,
    task: Option<Task<anyhow::Result<()>>>,
    cancel_flag: Arc<AtomicBool>,
}

pub enum StreamManagerEvent {
    StreamStarted { conversation_id },
    TextChunk { conversation_id, text },
    ToolCallStarted { conversation_id, id, name },
    // ... 8 more variants
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

**Implementation**:
```rust
impl ProviderType {
    pub fn default_capabilities(&self) -> (bool, bool) {
        match self {
            ProviderType::Anthropic => (true, true),   // Images + PDFs
            ProviderType::Gemini => (true, true),      // Images + PDFs
            ProviderType::OpenAI => (true, false),     // Images only (PDF lossy)
            ProviderType::Ollama => (false, false),    // Per-model detection
            ProviderType::Mistral => (false, false),   // No multimodal support
        }
    }
}
```

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

When adding a new provider (e.g., "Cohere"):

1. Add variant to `ProviderType` enum
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

### Sensitive Env Var Masking

MCP server env vars may contain API keys, tokens, and other secrets. The LLM must never see real values.

**Rule**: Any path that sends `McpServerConfig` data to the LLM **must** call `masked_env()` instead of accessing `.env` directly.

```rust
// WRONG — sends real secrets to the LLM
let env = server.env.clone();

// CORRECT — masks sensitive values before LLM sees them
let env = server.masked_env(); // KEY/TOKEN/SECRET/etc. → "****"
```

**Sensitive key detection** (`is_sensitive_env_key` in `mcp_store.rs`): matches keys containing KEY, SECRET, TOKEN, PASSWORD, CREDENTIAL, AUTH, or API (case-insensitive).

**Where masking is applied today:**
- `list_mcp_services` tool output (`McpServerSummary.env`)

**Where masking must be added if new LLM-facing surfaces are added:**
- Any future tool that returns `McpServerConfig` data
- Any future "show config" or "status" tool output
- Log statements that could capture tool args/results in a trace visible to users

### Masked Sentinel Preservation

`MASKED_VALUE_SENTINEL = "****"` means "preserve the existing stored value". This is implemented in `edit_mcp_service`:

```rust
// If LLM sends back "****", keep the real stored value — don't overwrite
if v == MASKED_VALUE_SENTINEL {
    let existing = server.env.get(&k).cloned().unwrap_or_default();
    (k, existing)
} else {
    (k, v)  // LLM sent a new real value — store it
}
```

**If a new tool accepts env vars as input** (e.g., a future `patch_mcp_service`), apply the same sentinel resolution pattern.

**If adding a new server** (`add_mcp_service`): reject `****` values with a clear error — there is no existing value to preserve.

### Logging Rules

Never log sensitive values. Log key *names* only.

```rust
// WRONG
tracing::info!(env = ?server.env, "Server configured");

// CORRECT
tracing::info!(env_keys = ?server.env.keys().collect::<Vec<_>>(), "Server configured");
```

### New LLM-Facing Output Structs

When adding a new `#[derive(Serialize)]` struct that will be returned as tool output:

1. If it wraps `McpServerConfig`, use `masked_env()` — never `.env`
2. If it includes any `ProviderConfig` fields, exclude `api_key` (it is never exposed to the LLM)
3. Add a test that the output contains `"****"` for a server with a real API key

### Where Real Values Are Safe

These paths use raw (unmasked) values intentionally:

| Location | Why raw values are safe |
|:---------|:------------------------|
| `McpService::start_server` | Sets env vars on child process, never sent to LLM |
| `mcp_json_repository` | Disk persistence, private config directory |
| `providers_store.rs` → disk | API keys for LLM API auth, private storage only |
