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
- **Theme System**: Themes are loaded from `./themes` directory. User preferences (theme name + dark mode) are persisted to JSON.

## CI/CD

- **CI**: Runs on pull requests to `main` - tests, formatting check, and clippy
- **Release**: Runs on push to `main` - builds for Linux x86_64, macOS Intel, and macOS ARM, then creates GitHub releases

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
    on_new_chat: Option<NewChatCallback>,
}

// Callbacks for communication
pub type NewChatCallback = Arc<dyn Fn(&mut App) + Send + Sync>;

impl SidebarView {
    pub fn set_on_new_chat<F>(&mut self, callback: F)
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_new_chat = Some(Arc::new(callback));
    }
    
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

**When to use**: Passing entities or data into event handlers and callbacks.

**Pattern**: Clone entities/references before closures to avoid borrow checker issues.

```rust
// app_controller.rs
let app_entity = cx.entity();  // Get entity reference

sidebar.update(cx, |sidebar, _cx| {
    let app = app_entity.clone();  // Clone before closure
    sidebar.set_on_select_conversation(move |conv_id, cx| {
        let app = app.clone();      // Clone again inside closure
        let id = conv_id.to_string();
        app.update(cx, |app, cx| {
            app.load_conversation(&id, cx);
        });
    });
});
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


