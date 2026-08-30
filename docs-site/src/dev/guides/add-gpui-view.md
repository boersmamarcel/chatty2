---
audience: [contributor]
source_files:
  - crates/chatty-gpui/src/chatty/views/
  - crates/chatty-gpui/src/chatty/controllers/app_controller/mod.rs
  - docs/entity-communication.md
related:
  - ./dev/architecture/entity-communication.md
  - ./dev/guides/add-slash-command.md
---

# Add a desktop GPUI view

**When to read this:** Add a new panel, dialog, or sidebar surface in the
desktop app.

> **Pair review pending (DOC-32 / AGE-108):** Scaffold from
> `docs/entity-communication.md` and existing views. Marcel reviews the
> walkthrough against a real view (for example `SidebarView`) before this is
> treated as the canonical how-to.

## Steps

1. Create the view under `crates/chatty-gpui/src/chatty/views/`
   (`mod.rs` + `Render`). Export it from `views/mod.rs`.
2. Define a typed event enum and `impl EventEmitter<YourEvent> for YourView`.
3. Construct the entity with `cx.new(|cx| …)` in `ChattyApp` (or the parent
   that owns the view). Store `Entity<YourView>` on the parent.
4. Subscribe once in `ChattyApp::setup_callbacks()`:

   ```rust
   cx.subscribe(&self.your_view, |app, _view, event: &YourEvent, cx| {
       match event {
           YourEvent::DoThing => { app.handle_thing(cx); }
       }
   })
   .detach();
   ```

5. Emit from the view with `cx.emit(YourEvent::DoThing)`. Call `cx.notify()`
   after mutating view state so it re-renders.
6. Compose the view in the parent's `Render` tree
   (`.child(self.your_view.clone())`).

## Rules

- Entity-to-entity communication is **only** `EventEmitter` / `cx.subscribe()`.
  No `Arc<dyn Fn>` between entities.
- `IntoElement` widgets (row items, buttons) may take callbacks, but those
  closures must `entity.update(cx, |_, cx| cx.emit(…))` on the parent entity.
- Store `WeakEntity<T>` in globals, never a strong `Entity` (avoids cycles).
- Use `cx.defer()` if you need to update an entity that is already being
  updated.

## Reference

- [Entity communication](../architecture/entity-communication.md)
- Existing emitters: `SidebarEvent`, `ChatInputEvent`, `StreamManagerEvent`
  — [event catalog](../reference/event-catalog.md)
