# Entity Communication Pattern

## Design Principle

All entity-to-entity communication in Chatty uses GPUI's `EventEmitter`/`cx.subscribe()` pattern. No `Arc<dyn Fn>` callbacks are used between GPUI entities.

## Why EventEmitter Over Callbacks

| | EventEmitter | Callbacks (`Arc<dyn Fn>`) |
|---|---|---|
| Handler context | `&mut Self` directly (subscriber closure) | `&mut App` only — must clone entity + `entity.update()` |
| Wiring location | One `cx.subscribe()` per entity in `setup_callbacks()` | Scattered `set_on_*()` calls, one per action |
| Type safety | Typed enum — exhaustive `match` catches missing arms | `Option<Arc<dyn Fn>>` — silent no-op if not wired |
| Adding a new action | Add enum variant + match arm | Add type alias + field + None init + setter + clone chain |
| Traceability | Events are `#[derive(Debug)]` — easy to log | Opaque closures |

## Event Topology

```
ChatInputState  ──emit ChatInputEvent──────►  ChattyApp (subscriber)
SidebarView     ──emit SidebarEvent────────►  ChattyApp (subscriber)
StreamManager   ──emit StreamManagerEvent──►  ChattyApp (subscriber)
McpNotifier     ──emit McpNotifierEvent────►  ChattyApp (subscriber)
SystemTraceView ──emit TraceEvent──────────►  ChatView  (subscriber)
```

4 subscriptions are set up in `ChattyApp::setup_callbacks()`, 1 in `ChatView::new()`.

## Adding a New Event

1. Add a variant to the entity's event enum (e.g., `SidebarEvent::RenameConversation(String)`)
2. Emit it: `cx.emit(SidebarEvent::RenameConversation(new_title))`
3. Handle it: add a match arm in the subscriber closure in `setup_callbacks()`

That's it — no type alias, no field, no setter, no clone chain.

## Boundary: IntoElement Components Keep Callbacks

GPUI's `EventEmitter` can only be implemented by entities (types created with `cx.new()`). Render-once components (`#[derive(IntoElement)]`) like `ConversationItem` and `ApprovalPromptBar` cannot emit events.

They accept `Arc<dyn Fn>` callbacks, but these closures route through the parent entity to emit events:

```rust
// ConversationItem (IntoElement) accepts a callback...
ConversationItem::new(id, title)
    .on_click({
        let entity = sidebar_entity.clone();
        let id = id.clone();
        move |_conv_id, cx| {
            // ...which routes through SidebarView (entity) to emit
            entity.update(cx, |_, cx| {
                cx.emit(SidebarEvent::SelectConversation(id.clone()));
            });
        }
    })
```

This keeps callbacks at the entity/component boundary only — all inter-entity communication flows through events.

## Subscription Pattern

Subscribers receive `&mut Self` directly, eliminating clone gymnastics:

```rust
// In ChattyApp::setup_callbacks()
cx.subscribe(&self.sidebar_view, |app, _sidebar, event: &SidebarEvent, cx| {
    match event {
        SidebarEvent::NewChat => { app.create_and_load_conversation(cx); }
        SidebarEvent::SelectConversation(id) => { app.load_conversation(id, cx); }
        SidebarEvent::DeleteConversation(id) => { app.delete_conversation(id, cx); }
        // ...
    }
}).detach();
```

Compare with the old callback pattern (removed):

```rust
// OLD — required: type alias + field + None init + setter + clone chain
sidebar.update(cx, |sidebar, _cx| {
    let app = app_entity.clone();
    sidebar.set_on_select_conversation(move |conv_id, cx| {
        let app = app.clone();
        let id = conv_id.to_string();
        app.update(cx, |app, cx| {
            app.load_conversation(&id, cx);
        });
    });
});
```
