---
name: add-view
description: Step-by-step guide for creating a new GPUI view or component in Chatty. Use when adding new UI panels, dialogs, or reusable components.
disable-model-invocation: true
allowed-tools: Read, Write, Edit, Grep, Glob, Bash
argument-hint: [view-name]
---

# Add a New GPUI View to Chatty

Create a new GPUI view or component named `$ARGUMENTS`. Follow the established GPUI patterns used throughout the codebase.

## Phase 1: Understand GPUI View Patterns

Read these reference implementations:

1. `src/chatty/views/sidebar_view.rs` - Entity with events, rendering, state management
2. `src/chatty/views/chat_view.rs` - Complex view with subscriptions and child entities
3. `src/chatty/controllers/app_controller.rs` - How views are composed in the app

## Phase 2: Create the View

1. **Create the file** in `src/chatty/views/` (e.g., `my_view.rs`)

2. **Define the struct**:
   ```rust
   pub struct MyView {
       // State fields
   }
   ```

3. **Define events** (if the view communicates with parent components):
   ```rust
   #[derive(Clone, Debug)]
   pub enum MyViewEvent {
       // Event variants
   }
   impl EventEmitter<MyViewEvent> for MyView {}
   ```

4. **Implement `Render`**:
   ```rust
   impl Render for MyView {
       fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
           div()
               .size_full()
               // ... fluent API
       }
   }
   ```

5. **Add constructor and methods**:
   ```rust
   impl MyView {
       pub fn new(cx: &mut Context<Self>) -> Self {
           Self { /* ... */ }
       }
   }
   ```

## Phase 3: Integrate the View

1. **Export the module** in `src/chatty/views/mod.rs`

2. **Add to parent view** - Create as an entity and add as child:
   ```rust
   let my_view = cx.new(|cx| MyView::new(cx));
   // In render: .child(self.my_view.clone())
   ```

3. **Subscribe to events** in the parent:
   ```rust
   cx.subscribe(&my_view, |parent, _view, event: &MyViewEvent, cx| {
       // Handle events
   }).detach();
   ```

## Phase 4: Key GPUI Patterns to Follow

- Call `cx.notify()` after mutating state to trigger re-renders
- Use `cx.defer()` to avoid re-entrancy when updating entities inside their own update
- Use `WeakEntity` when storing entity references in globals
- Always `.detach()` subscriptions
- Clone entities before closures: `let entity = entity.clone(); move |...| { ... }`
- Use `.when(condition, |this| this.child(...))` for conditional rendering
- Use `.children(iter.map(...).collect::<Vec<_>>())` for collections

## Phase 5: Verify

1. Run `cargo build` to ensure compilation
2. Run `cargo clippy -- -D warnings`
3. Run `cargo test`
