---
description: Guide for creating a new GPUI view or UI component in Chatty. Use when adding new UI elements like dialogs, panels, or custom components.
user-invocable: true
---

# Add New GPUI View

This skill walks through creating a new GPUI view component in Chatty.

## GPUI View vs IntoElement

Choose the right abstraction:

- **Entity with `Render` trait** (view): For stateful components that need lifecycle, events, and subscriptions. Created with `cx.new(|cx| MyView::new(cx))`. Used for: sidebar, chat view, settings pages.
- **`IntoElement` struct**: For stateless/presentational components that receive data via props. Used for: conversation items, buttons, list rows.

## Creating a View (Stateful Entity)

### 1. Define the struct and events

**File**: `src/chatty/views/my_view.rs`

```rust
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme, v_flex};

/// Events emitted by this view for parent communication
#[derive(Clone, Debug)]
pub enum MyViewEvent {
    SomeAction(String),
    Close,
}

impl EventEmitter<MyViewEvent> for MyView {}

pub struct MyView {
    // View state
    title: String,
    items: Vec<String>,
}

impl MyView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            title: String::new(),
            items: Vec::new(),
        }
    }

    /// Public setter that triggers re-render
    pub fn set_items(&mut self, items: Vec<String>, cx: &mut Context<Self>) {
        self.items = items;
        cx.notify(); // Always call cx.notify() after state changes
    }
}
```

### 2. Implement Render

```rust
impl Render for MyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                div()
                    .p_4()
                    .text_color(cx.theme().foreground)
                    .child(self.title.clone())
            )
            .children(
                self.items.iter().enumerate().map(|(ix, item)| {
                    let item_clone = item.clone();
                    let entity = entity.clone();
                    div()
                        .id(ix)
                        .p_2()
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().list.active))
                        .on_click(move |_event, _window, cx| {
                            entity.update(cx, |_, cx| {
                                cx.emit(MyViewEvent::SomeAction(item_clone.clone()));
                            });
                        })
                        .child(item.clone())
                }).collect::<Vec<_>>()
            )
    }
}
```

### 3. Register in module

**File**: `src/chatty/views/mod.rs`

```rust
pub mod my_view;
```

### 4. Create and subscribe from parent

**File**: `src/chatty/controllers/app_controller.rs` (or wherever the parent entity lives)

```rust
// Create the view entity
let my_view = cx.new(|cx| MyView::new(cx));

// Subscribe to its events (entity-to-entity communication)
cx.subscribe(&my_view, |app, _view, event: &MyViewEvent, cx| {
    match event {
        MyViewEvent::SomeAction(id) => {
            // Handle action
        }
        MyViewEvent::Close => {
            // Handle close
        }
    }
}).detach(); // Always .detach() subscriptions

// Include in parent's render as a child
// self.my_view.clone() in the Render impl
```

## Creating an IntoElement Component (Stateless)

For presentational components that don't need their own entity:

```rust
use gpui::*;
use gpui_component::ActiveTheme;

pub struct MyItem {
    id: String,
    label: String,
    on_click: Option<Box<dyn Fn(&String, &mut App) + 'static>>,
}

impl MyItem {
    pub fn new(id: String, label: String) -> Self {
        Self { id, label, on_click: None }
    }

    pub fn on_click(mut self, handler: impl Fn(&String, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl IntoElement for MyItem {
    type Element = Div;

    fn into_element(self) -> Self::Element {
        let id = self.id.clone();
        let on_click = self.on_click;

        div()
            .id(SharedString::from(self.id.clone()))
            .p_2()
            .child(self.label)
            .when_some(on_click, |this, handler| {
                this.on_click(move |_event, _window, cx| {
                    handler(&id, cx);
                })
            })
    }
}
```

## Key Patterns

### Conditional Rendering
```rust
.when(self.is_visible, |this| this.child(some_element))
.when_some(self.optional_data.as_ref(), |this, data| this.child(data.clone()))
```

### Theme Colors
```rust
cx.theme().background    // Main background
cx.theme().foreground    // Main text color
cx.theme().border        // Border color
cx.theme().primary       // Primary accent color
cx.theme().muted         // Muted/secondary text
```

### Layout
```rust
div().flex().flex_row()   // Horizontal layout
v_flex()                  // Vertical flex (shorthand)
h_flex()                  // Horizontal flex (shorthand)
div().flex_1()            // Take remaining space
div().size_full()         // Fill parent
```

### Deferred Updates (avoid re-entrancy)
```rust
// If you need to update an entity that's currently being updated:
cx.defer(move |cx| {
    my_view.update(cx, |view, cx| {
        view.set_items(new_items, cx);
    });
});
```

## Architecture Rules

- All entity-to-entity communication uses `EventEmitter`/`cx.subscribe()` — no `Arc<dyn Fn>` callbacks between entities
- `IntoElement` components may use callbacks, but they should route through the parent entity's `cx.emit()`
- Store `WeakEntity<T>` in globals, never strong `Entity<T>` references
- Always call `cx.notify()` after mutating entity state
- Always call `.detach()` on subscriptions
