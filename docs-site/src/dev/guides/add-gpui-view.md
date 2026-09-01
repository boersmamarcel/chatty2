---
audience: [contributor, agent]
source_files:
  - crates/chatty-gpui/src/chatty/views/
  - crates/chatty-gpui/src/chatty/views/sidebar_view.rs
  - crates/chatty-gpui/src/chatty/views/conversation_item.rs
  - crates/chatty-gpui/src/chatty/views/app_view.rs
  - crates/chatty-gpui/src/chatty/controllers/app_controller/mod.rs
  - docs/entity-communication.md
related:
  - ./dev/architecture/entity-communication.md
  - ./dev/reference/event-catalog.md
  - ./dev/guides/add-slash-command.md
---

# Add a desktop GPUI view

**When to read this:** Add a new panel, dialog, or persistent surface in the
desktop app (`chatty-gpui`). Settings pages and the TUI live in different
trees — pick from the table first.

Full event lookup: [GPUI event catalog](../reference/event-catalog.md).
Design rationale: [entity communication](../architecture/entity-communication.md).

## Pick the right kind of UI

| Kind | When | Canonical example | Where it lives |
|------|------|-------------------|----------------|
| Owned entity view | Persistent panel with state; talks to `ChattyApp` | `SidebarView`, `ChatView` | `crates/chatty-gpui/src/chatty/views/`, stored as `Entity<T>` on `ChattyApp` |
| Nested entity | State owned by a parent view, not the app | `ChatInputState` (on `ChatView`), `SystemTraceView` | Same `views/` tree; parent or `ChattyApp` subscribes |
| Dialog | Modal overlay, opened on demand | `SearchConversationsDialog`, `ErrorLogDialog` | `views/`, `window.open_dialog` |
| `IntoElement` widget | Render-once row/chrome, no `EventEmitter` | `ConversationItem`, `AppTitleBar`, `StatusFooterView` | `views/`; callbacks must `cx.emit` on a parent entity |
| Settings page | Settings window, not the main chat chrome | `settings/views/models_page/` | `crates/chatty-gpui/src/settings/views/` |
| TUI view | Terminal UI | Ratatui widgets | `crates/chatty-tui/src/ui/` — not GPUI |

Transcript block types (`transcript/`) render history in the desktop app.
Do **not** leak those types into `chatty-core`; persistence stays untyped
(`MessageEntry` + `system_trace` JSON).

## Walkthrough: owned entity (`SidebarView`)

`SidebarView` is the pattern to copy: typed events, `Entity` on `ChattyApp`,
one `cx.subscribe()` in `setup_callbacks()`, composed in `ChattyApp::render`.

### 1. File and module

Create `crates/chatty-gpui/src/chatty/views/your_view.rs` (or a
`your_view/` directory with `mod.rs` if it will grow). Export from
`crates/chatty-gpui/src/chatty/views/mod.rs`:

```rust
pub mod your_view;
pub use your_view::YourView;
```

### 2. Typed event enum + `EventEmitter`

```rust
/// Events emitted by SidebarView for entity-to-entity communication
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    NewChat,
    OpenSettings,
    SelectConversation(String),
    DeleteConversation(String),
    ExportConversation(String),
    ToggleCollapsed(bool),
    LoadMore,
}

impl EventEmitter<SidebarEvent> for SidebarView {}
```

Add a variant when the view needs the app to do something. Exhaustive
`match` in the subscriber is the checklist that nothing is unwired.

### 3. Struct, constructor, mutators

Keep view state on the struct. After mutating, call `cx.notify()` so GPUI
re-renders. Emit from a method when the mutation *is* the user-visible
action:

```rust
pub fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
    self.is_collapsed = !self.is_collapsed;
    cx.emit(SidebarEvent::ToggleCollapsed(self.is_collapsed));
    cx.notify();
}

pub fn set_conversations(
    &mut self,
    conversations: Vec<(String, String, Option<f64>)>,
    cx: &mut Context<Self>,
) {
    self.conversations = conversations;
    cx.notify();
}
```

### 4. `Render`: emit from the entity

In `impl Render`, clone `cx.entity()` once and use it from `on_click`
closures. The view entity emits; `ChattyApp` handles:

```rust
impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_entity = cx.entity().clone();

        v_flex().id("sidebar").child(
            Button::new("new-chat").label("New Chat").on_click({
                let entity = sidebar_entity.clone();
                move |_event, _window, cx| {
                    entity.update(cx, |_, cx| {
                        cx.emit(SidebarEvent::NewChat);
                    });
                }
            }),
        )
        // ...
    }
}
```

### 5. `IntoElement` children keep callbacks — route them through the entity

`ConversationItem` is `#[derive(IntoElement)]`. It cannot implement
`EventEmitter`. It takes `Arc<dyn Fn>` setters, and the parent wires those
to `cx.emit`:

```rust
ConversationItem::new(id.clone(), title.clone())
    .on_click({
        let entity = sidebar_entity.clone();
        let id = id.clone();
        move |_conv_id, cx| {
            entity.update(cx, |_, cx| {
                cx.emit(SidebarEvent::SelectConversation(id.clone()));
            });
        }
    })
```

Same shape for `on_delete` / `on_export`. Callbacks stay at the
entity/widget boundary only.

### 6. Own it on `ChattyApp`

Field + construct in `ChattyApp::new`
(`crates/chatty-gpui/src/chatty/controllers/app_controller/mod.rs`):

```rust
pub struct ChattyApp {
    pub chat_view: Entity<ChatView>,
    pub sidebar_view: Entity<SidebarView>,
    // ...
}

let sidebar_view = cx.new(|_cx| SidebarView::new());
```

Store `WeakEntity<T>` in globals (`GlobalChattyApp`), never a strong
`Entity` — strong refs in globals create cycles.

### 7. Subscribe once in `setup_callbacks()`

```rust
cx.subscribe(
    &self.sidebar_view,
    |app, _sidebar, event: &SidebarEvent, cx| match event {
        SidebarEvent::NewChat => {
            app.start_new_conversation(cx);
        }
        SidebarEvent::OpenSettings => {
            cx.defer(|cx| {
                SettingsView::open_or_focus_settings_window(cx);
            });
        }
        SidebarEvent::SelectConversation(conv_id) => {
            app.load_conversation(conv_id, cx);
        }
        // ...
    },
)
.detach();
```

`.detach()` keeps the subscription alive. Prefer storing a `Subscription`
field (`_sub`) when the subscriber is a short-lived nested view (see
`SearchConversationsView`).

`cx.defer()` is required when the handler would re-enter an entity that is
already being updated (`OpenSettings`, `ChatInputEvent::ModelChanged`,
`TraceEvent`).

`ChattyApp` can subscribe to a **nested** entity without owning it, for
example `self.chat_view.read(cx).chat_input_state()` → `ChatInputEvent`.

### 8. Compose in the parent's `Render`

`impl Render for ChattyApp` lives in
`crates/chatty-gpui/src/chatty/views/app_view.rs`. Clone the entity into
the tree:

```rust
.child(self.sidebar_view.clone())
.child(self.chat_view.clone())
```

`AppTitleBar` and `StatusFooterView` are `RenderOnce` widgets constructed
during render — they are not stored on `ChattyApp`.

### 9. Catalog a new event enum

If you added a **new** event enum (not just a variant on an existing one),
add a row in the `event-catalog.md` block in `scripts/gen-docs-reference.sh`
and run `make docs-gen`.

## Dialogs

Two shapes, both opened with `window.open_dialog`:

| Shape | Example | Use when |
|-------|---------|----------|
| Stateless helper | `ErrorLogDialog::open` | Contents come from a global (`ErrorStore`) and do not need live input |
| Entity inside the dialog | `SearchConversationsDialog::open` creates `SearchConversationsView` and `.child(view.clone())` | Typing/filtering must update without rebuilding the dialog |

Dialogs are not `EventEmitter` parents of `ChattyApp`. To trigger app-level
work, upgrade `GlobalChattyApp` and emit on an owned view (search selection
emits `SidebarEvent::SelectConversation` on `sidebar_view`), or call a
`ChattyApp` method directly.

## Nested entities

`ChatView` subscribes to `SystemTraceView` (`TraceEvent`) and to
`ArtifactView`. Child → parent still uses `EventEmitter` / `cx.subscribe()`.
That `TraceEvent` handler `cx.defer`s into `ChatView` to avoid re-entrancy.

App-level actions (send message, new chat, persist feedback) still go
**up** to `ChattyApp` (`ChatViewEvent`, `ChatInputEvent`, `SidebarEvent`).
Do not add `Arc<dyn Fn>` from `ChatView` to `ChattyApp`.

## Rules

- Entity-to-entity communication is **only** `EventEmitter` / `cx.subscribe()`.
  No `Arc<dyn Fn>` between entities.
- `IntoElement` / `RenderOnce` widgets may take callbacks, but those
  closures must `entity.update(cx, |_, cx| cx.emit(…))` on a parent entity.
- Store `WeakEntity<T>` in globals, never a strong `Entity`.
- `cx.notify()` after mutating view state; `cx.emit` when a parent must act.
- `cx.defer()` if you need to update an entity that is already being updated.
- Always `.detach()` a subscription (or keep the `Subscription` handle).
- Log UI-update failures with `warn!()` — do not `.ok()` them away.

## Checklist

- [ ] File under `crates/chatty-gpui/src/chatty/views/` (settings → `settings/views/`)
- [ ] Exported from `views/mod.rs`
- [ ] Entity view: event enum + `impl EventEmitter<…>`
- [ ] `ChattyApp` (or parent) owns `Entity<T>` and subscribes in `setup_callbacks()`
- [ ] Composed in the parent `Render` tree
- [ ] `IntoElement` children route clicks through `cx.emit` on the parent entity
- [ ] New event enum listed in `scripts/gen-docs-reference.sh` + `make docs-gen`
- [ ] No transcript block types imported into `chatty-core`

## Common mistakes

| Mistake | Do this instead |
|---------|-----------------|
| `set_on_select(Arc<dyn Fn>)` between entities | Event variant + `cx.subscribe` |
| Strong `Entity<T>` in a `Global` | `WeakEntity<T>` (`GlobalChattyApp` pattern) |
| Mutating a view from inside its own `update` / subscribe stack | `cx.defer` |
| Putting a settings form in `chatty/views/` | `settings/views/` |
| Building a TUI panel with GPUI widgets | `chatty-tui/src/ui/` |
| Forgetting `cx.notify()` | Re-render never runs; state looks stuck |

## Reference

- [Entity communication](../architecture/entity-communication.md)
- [GPUI event catalog](../reference/event-catalog.md)
- Existing emitters: `SidebarEvent`, `ChatInputEvent`, `ChatViewEvent`,
  `StreamManagerEvent`, `TraceEvent`
- Layout/styling: `.claude/skills/gpui` and `.claude/skills/gpui-component`
