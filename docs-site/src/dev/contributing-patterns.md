# Contributing patterns

**When to read this:** You are about to change Rust code in `chatty-core` or `chatty-gpui` and want to know which conventions a review will hold you to, and why.

Chatty is a GPUI desktop app with a shared, UI-agnostic core. Most of the rules below exist because GPUI's entity/context model has sharp edges: ownership cycles are easy to create, async work has to re-enter the app through a context, and a swallowed error is invisible in a GPU-rendered UI. The rules are short; the "why" is what makes them stick.

## The four non-negotiables

### 1. Entities talk through events, not callbacks

Entity-to-entity communication uses `EventEmitter` and `cx.subscribe()`. Never hand one entity an `Arc<dyn Fn>` pointing into another: the closure captures an entity handle, the handle keeps the other side alive, and you have built a reference cycle that GPUI cannot see. Events keep ownership one-directional and let the parent decide what a child event means. `IntoElement` components (plain structs such as `ConversationItem`) may still take `on_click`-style callbacks, but those callbacks must do nothing except `cx.emit()` on the owning entity.

```rust
// WRONG — callback captured by another entity, ownership cycle
sidebar.set_on_select(Arc::new(move |id, cx| app.load_conversation(id, cx)));

// RIGHT — child emits, parent subscribes
impl EventEmitter<SidebarEvent> for SidebarView {}

cx.subscribe(&self.sidebar_view, |app, _sidebar, event: &SidebarEvent, cx| {
    if let SidebarEvent::SelectConversation(id) = event {
        app.load_conversation(id, cx);
    }
})
.detach();
```

### 2. Weak entity references in globals, strong only when the global is the owner

`crates/chatty-gpui/src/global_entity.rs` provides two wrappers. `GlobalWeakEntity<T>` (`new(WeakEntity<T>)`, `try_upgrade() -> Option<Entity<T>>`) is the default: the entity's lifetime is owned by a window or a parent entity, and the global is just a lookup. `GlobalStrongEntity<T>` (`new(Entity<T>)`, `get() -> Option<Entity<T>>`) is for the handful of entities that nothing else owns and that must outlive any single window — `StreamManager` and `ModelsNotifier` are the existing examples. Reaching for the strong wrapper by habit reintroduces the cycles rule 1 avoids.

```rust
// WRONG — the global now keeps a view alive after its window closes
pub type GlobalChatView = GlobalStrongEntity<ChatView>;

// RIGHT — weak by default; upgrade at the call site
pub type GlobalMyNotifier = GlobalWeakEntity<MyNotifier>;
cx.set_global(GlobalMyNotifier::new(entity.downgrade()));

if let Some(notifier) = cx.try_global::<GlobalMyNotifier>().and_then(|g| g.try_upgrade()) {
    notifier.update(cx, |_, cx| cx.emit(MyEvent::Ready));
}
```

### 3. No silent `.ok()` on UI updates

`cx.update(...)`, `entity.update(cx, ...)` and friends return `Result` because the target may already be gone. Discarding that result with a bare `.ok()` hides the one signal you have when a background task outlives its view. Log it with `warn!` first. Propagate with `?` instead when the failure is a real error (file I/O, downloads, persistence).

```rust
// WRONG — failure disappears
cx.update(|cx| cx.refresh_windows()).ok();

// RIGHT — logged, then discarded on purpose
cx.update(|cx| cx.refresh_windows())
    .map_err(|e| warn!(error = ?e, "Failed to refresh windows"))
    .ok();
```

### 4. Tool errors go through `map_tool_error`

Every `impl Tool` implements `map_error` by calling `map_tool_error(Self::NAME, error)` from `crates/chatty-core/src/tools/mod.rs`. rig's default `Tool::map_error` redacts the source error to a per-kind string — for `ToolErrorKind::Other` that is the literal `"the tool failed"`, which is all the model and the transcript would ever see. `map_tool_error` formats the message as `Error: {tool_name}: {message}` and classifies it (timeout, permission, not-found, network, other) for rig's retry hint. The prefix is load-bearing: the streamed tool result carries no error flag, so `llm_service::tool_result_looks_like_error` decides success or failure by sniffing for `Error:`. Drop or reword it and every failure is filed as a success. Only route errors the tool authored itself through it; raw provider text is what rig's redaction exists for.

```rust
// WRONG — model sees "the tool failed"
fn map_error(&self, error: Self::Error) -> ToolExecutionError {
    ToolExecutionError::from_error(error)
}

// RIGHT — tool name and real message survive; prefix stays sniffable
fn map_error(&self, error: Self::Error) -> ToolExecutionError {
    crate::tools::map_tool_error(Self::NAME, error)
}
```

## Six everyday patterns

**Globals.** App-wide state (settings, model lists, stores) implements `gpui::Global` and lives in the app: `cx.set_global(value)` at startup, `cx.global::<T>()` to read, `cx.update_global::<T, _>(|state, cx| ...)` to mutate, `cx.try_global::<T>()` when it may not be initialised yet. chatty-core types get their `Global` impls in `chatty-core/src/gpui_globals.rs` behind the `gpui-globals` feature, so the terminal app never links GPUI.

```rust
cx.update_global::<ModelsModel, _>(|models, _cx| models.replace_all(new_models));
```

**Event subscriptions.** Define an event enum, implement `EventEmitter<E>` for the entity, emit with `cx.emit(...)`, and subscribe from the interested entity. Always `.detach()` the subscription (or store the `Subscription` handle); a dropped subscription silently stops delivering.

```rust
cx.subscribe(&notifier, |app, _notifier, event: &ModelsNotifierEvent, cx| {
    if matches!(event, ModelsNotifierEvent::ModelsReady) {
        app.refresh_chat_input_models(cx);
    }
})
.detach();
```

**Async work.** A Tokio runtime is entered in `main.rs` for the whole process, so `cx.spawn` futures can await Tokio-based I/O directly. Inside the future you hold an `AsyncApp`, not an `App`: every touch of app state goes through `cx.update(|cx| ...)`, and the closure returns a `Result` because the app may be shutting down. Detach fire-and-forget tasks; return the `Task` when the caller needs the result.

```rust
cx.spawn(async move |cx: &mut AsyncApp| {
    match GENERAL_SETTINGS_REPOSITORY.load().await {
        Ok(settings) => cx
            .update(|cx| cx.set_global(settings))
            .map_err(|e| warn!(error = ?e, "Failed to store settings"))
            .ok(),
        Err(e) => warn!(error = ?e, "Failed to load settings"),
    };
})
.detach();
```

**The four contexts.** `App` is the full application context (globals, entity creation) and is what `main.rs` and top-level constructors receive. `Context<T>` is `App` plus "you are inside entity `T`": it adds `cx.emit`, `cx.notify`, `cx.subscribe`, and `cx.entity()`. `AsyncApp` is what a spawned future holds; it can only reach the app through `cx.update`. `Window` carries window-scoped operations (focus, sizing) and is passed alongside `cx` to render and constructor code. If a function needs to emit or notify, it must take `Context<Self>`; if it only reads globals, `&App` is enough and keeps it callable from more places.

**Optimistic update.** For anything persisted: mutate the global first, refresh the UI, then save asynchronously and log the failure. The user never waits on disk, and a failed save shows up in the log rather than as a stuck button.

```rust
cx.global_mut::<ModelsModel>().add_model(config);
let to_save = cx.global::<ModelsModel>().models().to_vec();
cx.refresh_windows();
cx.spawn(async move |_cx: &mut AsyncApp| {
    if let Err(e) = MODELS_REPOSITORY.save_all(to_save).await {
        error!(error = ?e, "Failed to save models");
    }
})
.detach();
```

**Deferred updates.** Calling `entity.update(cx, ...)` on an entity that is already being updated higher in the call stack panics with a double borrow. When an event handler needs to reach back into the entity that dispatched it, schedule the work with `cx.defer(move |cx| ...)`; it runs after the current update returns.

```rust
let chat_view = self.chat_view.clone();
cx.defer(move |cx| {
    chat_view.update(cx, |view, cx| {
        view.chat_input_state().update(cx, |input, _cx| {
            input.set_capabilities(supports_images, supports_pdf);
        });
    });
});
```

## Security in short

MCP server configuration reaches the model through tools, so secrets must never be serialised into tool output. Today `McpServerConfig` (`crates/chatty-core/src/settings/models/mcp_store.rs`) carries a single secret, `api_key: Option<String>`, sent as a bearer token by `McpService`. Two conventions keep it out of the transcript:

- **Never expose the value.** LLM-facing summaries report `has_api_key: bool` (via `McpServerConfig::has_api_key()`), not the key. `list_mcp_services` (`tools/list_mcp_tool.rs`) is the existing example; its `McpServerSummary` has no key field at all, and `test_list_masks_api_key` pins that shape. Any new `#[derive(Serialize)]` tool output that wraps a server config follows the same shape.
- **Masked round-trips preserve the stored value.** `MASKED_API_KEY_SENTINEL` (`"****"`, same file) is the placeholder a model may send back when editing a server. A write path that receives the sentinel keeps the existing key rather than storing four asterisks; a path that creates a new server rejects it, because there is nothing to preserve.

Write operations on the MCP config take `MCP_WRITE_LOCK` so load → modify → save is atomic across tools. In logs, record `has_api_key` or key *names*, never values.

## Read next

- [Entity communication](./architecture/entity-communication.md) — the full rationale for rule 1, with the component-vs-entity split.
- [Stream manager](./architecture/stream-manager.md) — how LLM streams are owned, cancelled and finalised through events.
- [System overview](./architecture/system-overview.md) — the three-layer mental model these patterns live in.
- [Testing](./guides/test.md) — what to run before a PR and how to reproduce CI's single-threaded test run.
- [`CLAUDE.md`](https://github.com/boersmamarcel/chatty2/blob/main/CLAUDE.md) — the agent-facing version of these rules, with longer examples.
