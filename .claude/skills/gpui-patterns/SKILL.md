---
name: gpui-patterns
description: GPUI framework patterns, idioms, and best practices used in Chatty. Auto-loaded when creating or modifying GPUI views, entities, events, or render methods. Covers entity lifecycle, event-subscribe, async spawning, render fluent API, and context types.
user-invocable: false
---

# GPUI Patterns for Chatty

## Entity Lifecycle

Entities are created with `cx.new(|cx| MyEntity::new(cx))` and stored as `Entity<T>`. Store `WeakEntity<T>` in globals to avoid circular references. Access globals with `cx.global::<T>()` and mutate with `cx.update_global::<T, _>(|model, cx| { ... })`.

## Event Communication

All entity-to-entity communication uses `EventEmitter` + `cx.subscribe()`. Never use `Arc<dyn Fn>` callbacks between entities.

```rust
// Define events
#[derive(Clone, Debug)]
pub enum MyEvent { ItemSelected(String), Closed }
impl EventEmitter<MyEvent> for MyEntity {}

// Emit
cx.emit(MyEvent::ItemSelected(id));

// Subscribe (always .detach())
cx.subscribe(&entity, |parent, _entity, event: &MyEvent, cx| {
    match event { /* ... */ }
}).detach();
```

## Render Pattern

Use the fluent API. Call `cx.notify()` after state changes.

```rust
impl Render for MyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(header)
            .children(self.items.iter().map(|i| render_item(i)).collect::<Vec<_>>())
            .when(self.show_footer, |this| this.child(footer))
    }
}
```

## Async Operations

Use `cx.spawn()` — never raw `tokio::spawn`. In async context, use `cx.update()` for UI access.

```rust
cx.spawn(async move |_, cx| {
    let result = fetch_data().await;
    cx.update(|cx| {
        cx.update_global::<MyModel, _>(|model, _cx| model.set_data(result));
        cx.refresh_windows();
    }).map_err(|e| warn!(error = ?e, "Failed to update")).ok();
}).detach();
```

## Deferred Updates

Use `cx.defer()` to avoid re-entrancy (updating self during own update).

## Closure Captures

Clone entity references before closures:
```rust
let entity = entity.clone();
let id = id.clone();
move |_event, _window, cx| {
    entity.update(cx, |e, cx| { cx.emit(MyEvent::Selected(id.clone())); });
}
```

## Error Handling

Never silently discard errors:
```rust
// Bad: .ok()
// Good: .map_err(|e| warn!(error = ?e, "description")).ok()
```

## Context Types

- `App` — full access to globals and entities
- `AsyncApp` — limited async context, requires `cx.update()`
- `Context<T>` — entity-specific, can emit events and notify
- `Window` — window operations (sizing, positioning)
