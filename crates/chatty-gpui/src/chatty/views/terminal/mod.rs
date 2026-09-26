//! The embedded terminal's view (AGE-579, stage 1 of the embedded-terminal
//! epic).
//!
//! [`TerminalView`] owns a [`TerminalHandle`] (the PTY and `Term`, from
//! `chatty-terminal`), listens to its events and paints the grid with
//! [`element::TerminalElement`], a single custom element rather than a div
//! per cell. [`grid`] holds the toolkit-free parts (colours, cells → runs).
//!
//! Repaints are driven by the terminal: a `Wakeup` from the PTY thread
//! notifies the view, gpui folds any number of notifies before the next
//! frame into one, and the element reuses the previous frame outright while
//! [`TerminalHandle::generation`] has not moved. An idle terminal draws
//! nothing.
//!
//! Input (T3) lives in [`input`] (the view's key, text, paste, mouse and
//! wheel handlers) over the pure encoders in [`keys`], which also holds the
//! one list of keys the app keeps while a terminal has focus
//! ([`keys::RESERVED_KEYS`]). The docked panel (T4) comes later; until then
//! [`debug_terminal_from_env`] is the only way to open one.

mod element;
pub mod grid;
mod input;
pub mod keys;

use std::time::Duration;

use chatty_terminal::alacritty_terminal::term::ClipboardType;
use chatty_terminal::{TerminalConfig, TerminalEvent, TerminalHandle};
use futures::StreamExt;
use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, Styled, Subscription, Task, Window, actions,
    div,
};
use tracing::warn;

use element::{RenderCache, TerminalElement};

actions!(
    terminal,
    [
        /// Copy the terminal's selection to the clipboard.
        Copy,
        /// Paste the clipboard into the terminal (bracketed when the
        /// program asked for it).
        Paste
    ]
);

/// Key context of a focused terminal.
pub const KEY_CONTEXT: &str = "Terminal";

/// Bind the terminal's own actions (copy, paste) to their reserved keys.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(keys::COPY_KEY, Copy, Some(KEY_CONTEXT)),
        KeyBinding::new(keys::PASTE_KEY, Paste, Some(KEY_CONTEXT)),
    ]);
}

/// How long the grid size must hold still before the PTY is resized, so
/// dragging a panel edge does not reflow the shell on every frame.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);

/// Font settings for the terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFontSettings {
    /// Monospace family; `None` is the theme's code-block font
    /// (`mono_font_family`).
    pub family: Option<String>,
    /// Font size in pixels; `None` is the theme's `mono_font_size`.
    pub size: Option<f32>,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
}

impl Default for TerminalFontSettings {
    fn default() -> Self {
        Self {
            family: None,
            size: None,
            line_height: 1.3,
        }
    }
}

/// A terminal on screen: a running [`TerminalHandle`] and the element that
/// paints it.
pub struct TerminalView {
    handle: TerminalHandle,
    focus_handle: FocusHandle,
    /// Where the grid was last laid out, for mouse → cell.
    geometry: Option<input::Geometry>,
    mouse: input::MouseState,
    /// IME pre-edit text, while a composition is open.
    marked_text: Option<String>,
    font: TerminalFontSettings,
    cache: RenderCache,
    /// Grid size last sent to the PTY.
    grid_size: Option<(u16, u16)>,
    /// Size waiting out [`RESIZE_DEBOUNCE`], and the timer that applies it.
    pending_resize: Option<((u16, u16), Task<()>)>,
    _events: Task<()>,
    /// Sees keys before the app's keybindings (see [`input`]).
    _intercept: Subscription,
}

impl TerminalView {
    /// Wrap a spawned terminal. `events` is the receiver
    /// [`TerminalHandle::spawn`] returned.
    pub fn new(
        handle: TerminalHandle,
        events: std::sync::mpsc::Receiver<TerminalEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            handle,
            focus_handle: cx.focus_handle(),
            geometry: None,
            mouse: input::MouseState::default(),
            marked_text: None,
            _intercept: input::intercept_keys(cx),
            font: TerminalFontSettings::default(),
            cache: RenderCache::default(),
            grid_size: None,
            pending_resize: None,
            _events: Self::pump_events(events, cx),
        }
    }

    /// Forward the handle's blocking channel into the view: each batch of
    /// events that can change the screen becomes one `notify`.
    fn pump_events(
        events: std::sync::mpsc::Receiver<TerminalEvent>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        // The std receiver blocks, so it gets its own thread. It ends when
        // the terminal (the last sender) is dropped, or the view is.
        let spawned = std::thread::Builder::new()
            .name("terminal-events".into())
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    if tx.unbounded_send(event).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            warn!(
                "terminal: cannot start the event thread, the view will not repaint on output: {e}"
            );
        }

        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let mut repaint = changes_screen(&event);
                let mut clipboard = clipboard_store(event);
                // Everything already queued rides on the same frame.
                while let Ok(event) = rx.try_recv() {
                    repaint |= changes_screen(&event);
                    clipboard = clipboard_store(event).or(clipboard);
                }
                let updated = this.update(cx, |_, cx| {
                    if let Some((kind, text)) = clipboard {
                        write_clipboard(kind, text, cx);
                    }
                    if repaint {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        })
    }

    /// Ask for a new grid size. The first one applies at once (the shell
    /// should start at the right width); later ones wait out
    /// [`RESIZE_DEBOUNCE`].
    fn request_resize(&mut self, size: (u16, u16), cx: &mut Context<Self>) {
        if self.grid_size.is_none() {
            self.apply_resize(size);
            return;
        }
        if self.grid_size == Some(size) {
            self.pending_resize = None;
            return;
        }
        if matches!(&self.pending_resize, Some((pending, _)) if *pending == size) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_DEBOUNCE).await;
            let _ = this.update(cx, |view, cx| {
                view.pending_resize = None;
                view.apply_resize(size);
                cx.notify();
            });
        });
        self.pending_resize = Some((size, task));
    }

    fn apply_resize(&mut self, (cols, rows): (u16, u16)) {
        self.grid_size = Some((cols, rows));
        if let Err(e) = self.handle.resize(cols, rows) {
            warn!("terminal: resize to {cols}x{rows} failed: {e}");
        }
    }
}

/// An OSC 52 clipboard write from the program. Reads (`ClipboardLoad`) are
/// never answered: `chatty-terminal` configures `Term` to drop them.
fn clipboard_store(event: TerminalEvent) -> Option<(ClipboardType, String)> {
    match event {
        TerminalEvent::ClipboardStore(kind, text) => Some((kind, text)),
        _ => None,
    }
}

fn write_clipboard(kind: ClipboardType, text: String, cx: &mut App) {
    let item = ClipboardItem::new_string(text);
    match kind {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        ClipboardType::Selection => cx.write_to_primary(item),
        _ => cx.write_to_clipboard(item),
    }
}

fn changes_screen(event: &TerminalEvent) -> bool {
    matches!(
        event,
        TerminalEvent::Wakeup | TerminalEvent::ChildExit(_) | TerminalEvent::Exit
    )
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .child(TerminalElement::new(cx.entity(), self.focus_handle.clone()))
    }
}

/// Debug hook until the docked panel (T4) exists: with
/// `CHATTY_DEBUG_TERMINAL=1` the main window opens a terminal under the
/// chat. `CHATTY_DEBUG_TERMINAL_CMD` runs that command in `bash -c` instead
/// of the login shell (end it with `; exec bash` to keep the shell).
pub(crate) fn debug_terminal_from_env(cx: &mut App) -> Option<Entity<TerminalView>> {
    if std::env::var("CHATTY_DEBUG_TERMINAL").ok().as_deref() != Some("1") {
        return None;
    }
    let mut config = TerminalConfig::default();
    if let Ok(command) = std::env::var("CHATTY_DEBUG_TERMINAL_CMD") {
        config.shell = Some("bash".into());
        config.args = vec!["-c".into(), command];
    }
    match TerminalHandle::spawn(config) {
        Ok((handle, events)) => Some(cx.new(|cx| TerminalView::new(handle, events, cx))),
        Err(e) => {
            warn!("terminal: CHATTY_DEBUG_TERMINAL set but the shell did not start: {e}");
            None
        }
    }
}
