//! The terminal view's input handlers (T3): keys, text (IME and dead keys),
//! paste, copy, selection, the wheel, and mouse reporting. The byte
//! encodings they send live in [`super::keys`].
//!
//! Keys reach the terminal through a keystroke *interceptor*, which gpui
//! runs before it matches keybindings: an app shortcut bound with no key
//! context (Ctrl+B, Ctrl+N, Ctrl+P, …) would otherwise win over the shell.
//! Only [`keys::RESERVED_KEYS`] pass on to the app. Plain printable keys
//! also pass on, to gpui's text input path, so IME commits and composed
//! dead keys arrive whole in [`EntityInputHandler::replace_text_in_range`].

use std::ops::Range;

use chatty_terminal::alacritty_terminal::grid::{Dimensions, Scroll};
use chatty_terminal::alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use chatty_terminal::alacritty_terminal::selection::{Selection, SelectionType};
use chatty_terminal::alacritty_terminal::term::TermMode;
use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Subscription,
    UTF16Selection, Window, px,
};

use super::keys::{self, ReportButton, ReportKind};
use super::{Copy, Paste, TerminalView};

/// The laid-out grid: where it is and how big a cell is.
#[derive(Debug, Clone, Copy)]
pub(super) struct Geometry {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
}

/// Mouse state across events.
#[derive(Default)]
pub(super) struct MouseState {
    /// A left-button drag is extending the selection.
    selecting: bool,
    /// A button whose press was reported to the child, so its release is too.
    reported: Option<ReportButton>,
    /// Last cell a motion was reported for (motion is reported per cell).
    last_motion_cell: Option<(usize, usize)>,
    /// Pixel scroll (trackpads) not yet a whole line.
    scroll_remainder: f32,
}

/// A cell on screen: 0-based column and row, and which half was hit.
#[derive(Debug, Clone, Copy)]
struct Cell {
    col: usize,
    row: usize,
    side: Side,
}

/// Register the view's keystroke interceptor. It acts only while the view
/// has focus.
pub(super) fn intercept_keys(cx: &mut Context<TerminalView>) -> Subscription {
    let view = cx.entity().downgrade();
    cx.intercept_keystrokes(move |event, window, cx| {
        let Some(view) = view.upgrade() else {
            return;
        };
        if !view.read(cx).focus_handle.is_focused(window) {
            return;
        }
        let handled = view.update(cx, |view, cx| view.handle_keystroke(&event.keystroke, cx));
        if handled {
            cx.stop_propagation();
        }
    })
}

impl TerminalView {
    fn mode(&self) -> TermMode {
        self.handle.with_term(|term| *term.mode())
    }

    /// Decide what a key does; `true` when the terminal consumed it.
    fn handle_keystroke(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if keys::is_reserved(keystroke) {
            return false;
        }
        let mode = self.mode();
        let m = keystroke.modifiers;
        // Shift+PgUp/PgDn page through history in the normal screen.
        if m.shift && !m.control && !m.alt && !m.platform && !mode.contains(TermMode::ALT_SCREEN) {
            let scroll = match keystroke.key.as_str() {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.handle
                    .with_term_mut(|term| term.scroll_display(scroll));
                cx.notify();
                return true;
            }
        }
        if let Some(bytes) = keys::encode_key(keystroke, mode) {
            self.send_input(&bytes, cx);
            return true;
        }
        keys::swallows(keystroke)
    }

    /// Write to the child. Typing snaps the view back to the live screen.
    fn send_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let scrolled = self
            .handle
            .with_term(|term| term.grid().display_offset() != 0);
        if scrolled {
            self.handle
                .with_term_mut(|term| term.scroll_display(Scroll::Bottom));
            cx.notify();
        }
        if let Err(e) = self.handle.write(bytes) {
            tracing::warn!("terminal: write to the shell failed: {e}");
        }
    }

    pub(super) fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.handle.with_term(|term| term.selection_to_string());
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.send_input(&keys::paste_bytes(&text, bracketed), cx);
    }

    /// The cell under `position`, clamped to the grid.
    fn cell_at(&self, position: Point<Pixels>) -> Option<Cell> {
        let geometry = self.geometry?;
        let (cols, rows) = self
            .handle
            .with_term(|term| (term.columns(), term.screen_lines()));
        let local = position - geometry.bounds.origin;
        let x = (local.x / geometry.cell_width).max(0.);
        let y = (local.y / geometry.line_height).max(0.);
        let col = (x.floor() as usize).min(cols.saturating_sub(1));
        let row = (y.floor() as usize).min(rows.saturating_sub(1));
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        Some(Cell { col, row, side })
    }

    /// Grid point for a screen cell, accounting for scrollback.
    fn grid_point(&self, cell: Cell) -> GridPoint {
        let offset = self
            .handle
            .with_term(|term| term.grid().display_offset() as i32);
        GridPoint::new(Line(cell.row as i32 - offset), Column(cell.col))
    }

    /// Send a mouse report if the program asked for this kind of event.
    fn report(
        &self,
        button: ReportButton,
        kind: ReportKind,
        cell: Cell,
        modifiers: gpui::Modifiers,
    ) {
        let mode = self.mode();
        if !keys::wants_mouse(mode, kind, button) {
            return;
        }
        if let Some(bytes) = keys::mouse_report(mode, button, kind, cell.col, cell.row, modifiers)
            && let Err(e) = self.handle.write(&bytes)
        {
            tracing::warn!("terminal: mouse report failed: {e}");
        }
    }

    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(cell) = self.cell_at(event.position) else {
            return;
        };
        let mode = self.mode();
        // Shift forces local selection even when the program wants the mouse.
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let button = match event.button {
                MouseButton::Left => ReportButton::Left,
                MouseButton::Middle => ReportButton::Middle,
                MouseButton::Right => ReportButton::Right,
                _ => return,
            };
            self.mouse.reported = Some(button);
            self.mouse.last_motion_cell = Some((cell.col, cell.row));
            self.report(button, ReportKind::Press, cell, event.modifiers);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let kind = match event.click_count {
            0 | 1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        let point = self.grid_point(cell);
        self.handle.with_term_mut(|term| {
            term.selection = Some(Selection::new(kind, point, cell.side));
        });
        self.mouse.selecting = true;
        cx.notify();
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mouse.selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                self.mouse.selecting = false;
                return;
            }
            let Some(cell) = self.cell_at(event.position) else {
                return;
            };
            let point = self.grid_point(cell);
            self.handle.with_term_mut(|term| {
                if let Some(selection) = term.selection.as_mut() {
                    selection.update(point, cell.side);
                }
            });
            cx.notify();
            return;
        }
        let button = match self.mouse.reported {
            Some(button) => button,
            None if hovered => ReportButton::None,
            None => return,
        };
        let Some(cell) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse.last_motion_cell == Some((cell.col, cell.row)) {
            return;
        }
        self.mouse.last_motion_cell = Some((cell.col, cell.row));
        self.report(button, ReportKind::Motion, cell, event.modifiers);
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent) {
        self.mouse.selecting = false;
        if let Some(button) = self.mouse.reported.take()
            && let Some(cell) = self.cell_at(event.position)
        {
            self.report(button, ReportKind::Release, cell, event.modifiers);
        }
    }

    pub(super) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => {
                let line_height = self
                    .geometry
                    .map_or(px(16.), |geometry| geometry.line_height);
                self.mouse.scroll_remainder += delta.y / line_height;
                let whole = self.mouse.scroll_remainder.trunc();
                self.mouse.scroll_remainder -= whole;
                whole
            }
        };
        let count = lines.abs().round() as usize;
        if count == 0 {
            return;
        }
        // Positive is towards older output (wheel up).
        let up = lines > 0.;
        let mode = self.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let Some(cell) = self.cell_at(event.position) else {
                return;
            };
            let button = if up {
                ReportButton::WheelUp
            } else {
                ReportButton::WheelDown
            };
            for _ in 0..count {
                self.report(button, ReportKind::Press, cell, event.modifiers);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let bytes = keys::alternate_scroll(up, mode).repeat(count);
            if let Err(e) = self.handle.write(&bytes) {
                tracing::warn!("terminal: alternate scroll failed: {e}");
            }
        } else {
            let delta = if up { count as i32 } else { -(count as i32) };
            self.handle
                .with_term_mut(|term| term.scroll_display(Scroll::Delta(delta)));
            cx.notify();
        }
    }

    /// Where the IME candidate window goes: the cursor cell.
    fn cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry?;
        let (col, row) = self.handle.with_term(|term| {
            let cursor = term.grid().cursor.point;
            let row = cursor.line.0 + term.grid().display_offset() as i32;
            (cursor.column.0, row.max(0) as usize)
        });
        Some(Bounds::new(
            geometry.bounds.origin
                + gpui::point(
                    geometry.cell_width * col as f32,
                    geometry.line_height * row as f32,
                ),
            gpui::size(geometry.cell_width, geometry.line_height),
        ))
    }
}

/// The text input path: typed text, IME commits, composed dead keys. The
/// terminal has no editable text of its own, so the ranges are empty and
/// the only state is the open composition.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_text = None;
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.send_input(text.as_bytes(), cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds()
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
