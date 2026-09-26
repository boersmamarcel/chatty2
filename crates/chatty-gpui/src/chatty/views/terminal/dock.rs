//! The bottom terminal dock under the chat column (AGE-582), laid out like
//! the VS Code / Zed terminal panel: a tab strip, the active terminal below
//! it, a drag handle on the top edge, a maximise toggle.
//!
//! Terminals belong to the window, not to a conversation: the dock lives on
//! the window's [`ChatView`](crate::chatty::views::ChatView), which swaps
//! conversations in place, so switching conversations neither kills nor
//! hides them. A new terminal starts in the active conversation's workspace
//! (the directory the file explorer shows), else in `$HOME`.
//!
//! The dock owns the tabs and the open/maximised flags. Its height lives in
//! [`TerminalSettings::dock_height`] and the chat column (which knows its
//! own height) applies drags to it; focus moves between the dock and the
//! composer in `ChatView`, which has both.
//!
//! Every tab is registered in [`EmbeddedTerminals`], what the agent's
//! `terminal_read` sees (AGE-583). A tab starts unshared; the eye icon on it
//! shares it (after a Read only / Read + run choice, unless one was
//! remembered) or unshares it, and a tab the agent reads flashes briefly.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chatty_core::services::terminal::{TerminalAccess, TerminalKind};
use chatty_core::settings::models::ExecutionSettingsModel;
use chatty_core::settings::models::general_model::TerminalSettings;
use chatty_terminal::{TerminalConfig, TerminalHandle};
use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable, WindowExt as _, h_flex, v_flex};
use tracing::warn;

use super::registry::{
    EmbeddedTerminals, ShareClick, remembered_default, share_click, share_tooltip,
};
use super::{TerminalView, TerminalViewEvent};
use crate::settings::controllers::general_settings_controller::update_terminal_settings;
use crate::settings::models::GeneralSettingsModel;

/// Chords that show or hide the dock. Each is in
/// [`RESERVED_KEYS`](super::keys::RESERVED_KEYS), so it reaches the app
/// while a terminal has the keyboard.
#[cfg(target_os = "macos")]
pub const TOGGLE_DOCK_KEYS: &[&str] = &["cmd-j", "ctrl-`"];
/// Chords that show or hide the dock; see the macOS variant.
#[cfg(not(target_os = "macos"))]
pub const TOGGLE_DOCK_KEYS: &[&str] = &["ctrl-j", "ctrl-`"];
/// Chords that open a new terminal. X11 reports Ctrl+Shift+` as `ctrl-~`,
/// so both spellings are bound (the reserved list folds `~` back).
pub const NEW_TERMINAL_KEYS: &[&str] = &["ctrl-shift-`", "ctrl-~"];

/// How often tab titles re-read the foreground program and directory.
const LIVE_REFRESH: Duration = Duration::from_secs(1);

/// Height of the tab strip.
const HEADER_HEIGHT: f32 = 30.;
/// The dock never gets shorter than this when dragged.
pub const MIN_DOCK_HEIGHT: f32 = 100.;
/// Room the dock always leaves the transcript when dragged up.
pub const MIN_CHAT_HEIGHT: f32 = 160.;
/// Quiet time after a drag before the new height is written to disk.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
/// How long a tab stays highlighted after the agent read it.
const READ_FLASH: Duration = Duration::from_millis(1500);

/// What the dock asks of the chat view around it.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalDockEvent {
    /// The dock hid itself (its hide button, or its last tab closed): give
    /// the keyboard back to the composer.
    Hidden,
}

/// Payload of a tab being dragged to a new place in the strip.
#[derive(Clone)]
pub struct DraggedTerminalTab {
    id: u64,
    label: SharedString,
}

/// Payload of a drag on the dock's top edge. The chat column listens for
/// its moves, since only it knows how tall the column is.
#[derive(Clone)]
pub struct DockResizeDrag;

struct DockTab {
    id: u64,
    view: Entity<TerminalView>,
    /// Program name for the title fallback (`bash`, `zsh`, `pwsh`).
    shell_name: String,
    /// Where the terminal started.
    cwd: PathBuf,
    /// Foreground program and the shell's current directory, polled every
    /// [`LIVE_REFRESH`] (Linux; `None` elsewhere).
    live_program: Option<String>,
    live_cwd: Option<PathBuf>,
    /// The tab's id in [`EmbeddedTerminals`] (`term-1`), what the agent
    /// names it by.
    registry_id: String,
    _events: Subscription,
}

pub struct TerminalDock {
    tabs: Vec<DockTab>,
    active: usize,
    next_id: u64,
    open: bool,
    maximized: bool,
    save_height: Option<Task<()>>,
    focus_handle: FocusHandle,
    _quit: Subscription,
    /// Polls tab titles every [`LIVE_REFRESH`]; only while the dock is open.
    live_refresh: Option<Task<()>>,
    /// What the agent may read of the tabs (shared with its tool).
    registry: Arc<EmbeddedTerminals>,
    /// The tab the agent just read, and the timer that clears it.
    read_flash: Option<(String, Task<()>)>,
    _reads: Task<()>,
}

impl EventEmitter<TerminalDockEvent> for TerminalDock {}

impl Focusable for TerminalDock {
    /// The active terminal's focus, or the dock's own while it has none.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tabs
            .get(self.active)
            .map(|tab| tab.view.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl TerminalDock {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let registry = EmbeddedTerminals::global(cx);
        let mut reads = registry.subscribe_reads();
        let _reads = cx.spawn(async move |this, cx| {
            while let Some(id) = reads.next().await {
                if this.update(cx, |dock, cx| dock.flash_read(id, cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            open: false,
            maximized: false,
            save_height: None,
            focus_handle: cx.focus_handle(),
            // Quitting kills every shell now rather than leaving it to the
            // process teardown: `TerminalHandle`'s drop kills the whole
            // process group and reaps it.
            _quit: cx.on_app_quit(|dock, _cx| {
                for tab in dock.tabs.drain(..) {
                    dock.registry.remove(&tab.registry_id);
                }
                async {}
            }),
            live_refresh: None,
            registry,
            read_flash: None,
            _reads,
        }
    }

    /// Highlight the tab the agent read, if it is one of ours, for
    /// [`READ_FLASH`].
    fn flash_read(&mut self, registry_id: String, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|tab| tab.registry_id == registry_id) {
            return;
        }
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(READ_FLASH).await;
            let _ = this.update(cx, |dock, cx| {
                dock.read_flash = None;
                cx.notify();
            });
        });
        self.read_flash = Some((registry_id, timer));
        cx.notify();
    }

    /// The access the human gave tab `id` (the dock's own id).
    pub fn tab_access(&self, id: u64) -> TerminalAccess {
        self.tabs
            .iter()
            .find(|tab| tab.id == id)
            .map_or(TerminalAccess::None, |tab| {
                self.registry.access(&tab.registry_id)
            })
    }

    /// Share tab `id` at `access`, or unshare it with
    /// [`TerminalAccess::None`].
    pub fn set_tab_access(&mut self, id: u64, access: TerminalAccess, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) {
            self.registry.set_access(&tab.registry_id, access);
            cx.notify();
        }
    }

    /// The eye icon on tab `id`: unshare a shared tab at once; share an
    /// unshared one at the remembered level, or ask how much.
    pub fn share_clicked(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let remembered = cx
            .try_global::<GeneralSettingsModel>()
            .map(|s| s.terminal.share_default)
            .unwrap_or_default();
        match share_click(self.tab_access(id), remembered) {
            ShareClick::Unshare => self.set_tab_access(id, TerminalAccess::None, cx),
            ShareClick::Share(access) => self.set_tab_access(id, access, cx),
            ShareClick::Ask => self.open_share_dialog(id, window, cx),
        }
    }

    /// The share choice: Read only or Read + run, and whether to remember
    /// it as the "When sharing a terminal" setting.
    fn open_share_dialog(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        let label = tab.title();
        let dock = cx.entity();
        let remember = Rc::new(Cell::new(false));
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let (muted, border, hover) = (theme.muted_foreground, theme.border, theme.list_hover);
            let choice = |key: &'static str,
                          title: &'static str,
                          detail: &'static str,
                          access: TerminalAccess| {
                let dock = dock.clone();
                let remember = remember.clone();
                v_flex()
                    .id(key)
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_0p5()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(div().text_xs().text_color(muted).child(detail))
                    .on_click(move |_, window, cx| {
                        if remember.get() {
                            update_terminal_settings(cx, |t| {
                                t.share_default = remembered_default(access)
                            });
                        }
                        dock.update(cx, |dock, cx| dock.set_tab_access(id, access, cx));
                        window.close_dialog(cx);
                    })
            };
            let remember_box = {
                let remember = remember.clone();
                Checkbox::new("terminal-share-remember")
                    .label("Remember this, don't ask again")
                    .checked(remember.get())
                    .on_click(move |checked, window, _| {
                        remember.set(*checked);
                        window.refresh();
                    })
            };
            dialog
                .title("Share this terminal with the agent?")
                .w(px(420.))
                .child(
                    v_flex()
                        .gap_2()
                        .px_1()
                        .child(div().text_sm().text_color(muted).child(format!(
                            "'{label}' is yours: the agent cannot see it until you share it."
                        )))
                        .child(choice(
                            "terminal-share-read",
                            "Read only",
                            "The agent can see the screen and scrollback.",
                            TerminalAccess::Read,
                        ))
                        .child(choice(
                            "terminal-share-read-run",
                            "Read + run",
                            "The agent can also run commands here, each after your approval.",
                            TerminalAccess::ReadRun,
                        ))
                        .child(div().pt_1().text_sm().child(remember_box))
                        .child(
                            div().text_xs().text_color(muted).child(
                                "A remembered choice can be changed in Settings → Terminal.",
                            ),
                        ),
                )
        });
    }

    /// Start polling tab titles, if not already: they follow `cd` and the
    /// program started at the prompt, which print nothing the dock would
    /// otherwise hear about. Only while the dock is open, so a closed or
    /// never-opened dock never wakes the app.
    fn start_live_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_live(cx);
        if self.live_refresh.is_some() {
            return;
        }
        self.live_refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LIVE_REFRESH).await;
                let alive = this.update(cx, |dock, cx| {
                    if dock.refresh_live(cx) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// Whether the title poll is running (tests).
    #[cfg(test)]
    pub fn is_polling_titles(&self) -> bool {
        self.live_refresh.is_some()
    }

    /// Re-read each tab's foreground program and directory; whether any
    /// changed.
    fn refresh_live(&mut self, cx: &App) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            let handle = tab.view.read(cx).handle();
            let program = handle.foreground_process_name();
            let cwd = handle.current_dir();
            if program != tab.live_program || cwd != tab.live_cwd {
                tab.live_program = program;
                tab.live_cwd = cwd;
                changed = true;
            }
        }
        changed
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[cfg(test)]
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The active terminal, if any.
    pub fn active_view(&self) -> Option<&Entity<TerminalView>> {
        self.tabs.get(self.active).map(|tab| &tab.view)
    }

    /// Tab ids in strip order.
    #[cfg(test)]
    pub fn tab_ids(&self) -> Vec<u64> {
        self.tabs.iter().map(|tab| tab.id).collect()
    }

    /// Working directory of each tab, in strip order.
    #[cfg(test)]
    pub fn tab_cwds(&self) -> Vec<PathBuf> {
        self.tabs.iter().map(|tab| tab.cwd.clone()).collect()
    }

    /// Show the dock, starting a terminal if it has none, and focus the
    /// active one.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        if self.tabs.is_empty() {
            self.spawn_tab(cx);
        }
        self.start_live_refresh(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Hide the dock. Its terminals keep running; the title poll stops.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.live_refresh = None;
        cx.notify();
    }

    /// Open a new terminal as the last tab, show the dock and focus it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.spawn_tab(cx);
        self.start_live_refresh(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn toggle_maximized(&mut self, cx: &mut Context<Self>) {
        self.maximized = !self.maximized;
        cx.notify();
    }

    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active = index;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &App) {
        if let Some(tab) = self.tabs.get(self.active) {
            window.focus(&tab.view.read(cx).focus_handle(cx));
            // The default `terminal_read` target, when shared.
            self.registry.focused(&tab.registry_id);
        }
    }

    /// Start a terminal for a new tab and make it active. A shell that
    /// fails to start is logged and leaves the tabs as they were.
    fn spawn_tab(&mut self, cx: &mut Context<Self>) {
        let settings = cx
            .try_global::<GeneralSettingsModel>()
            .map(|s| s.terminal.clone())
            .unwrap_or_default();
        let cwd = terminal_cwd(
            active_conversation_dir(cx),
            cx.try_global::<ExecutionSettingsModel>()
                .and_then(|s| s.workspace_dir.clone()),
            home_dir(),
        );
        let config = terminal_config(&settings, cwd.clone());
        let shell_name = shell_display_name(config.shell.as_deref());
        let (handle, events) = match TerminalHandle::spawn(config) {
            Ok(spawned) => spawned,
            Err(e) => {
                warn!("terminal: the shell did not start: {e}");
                return;
            }
        };

        let view = cx.new(|cx| TerminalView::new(handle, events, cx));
        let events = cx.subscribe(&view, |_, _, _: &TerminalViewEvent, cx| cx.notify());
        let id = self.next_id;
        self.next_id += 1;
        let cwd = cwd.unwrap_or_default();
        let registry_id = self.registry.register(
            view.read(cx).handle(),
            TerminalKind::Human,
            shell_name.clone(),
            cwd.clone(),
        );
        self.tabs.push(DockTab {
            id,
            view,
            shell_name,
            cwd,
            live_program: None,
            live_cwd: None,
            registry_id,
            _events: events,
        });
        self.active = self.tabs.len() - 1;
    }

    /// Close a tab, asking first if a program other than the shell is
    /// running in it.
    pub fn request_close(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        let handle = tab.view.read(cx).handle();
        if !close_needs_confirmation(handle.has_exited(), handle.has_foreground_job()) {
            self.close(id, window, cx);
            return;
        }
        let label = tab.title();
        let dock = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let dock = dock.clone();
            dialog
                .confirm()
                .title("Close terminal?")
                .child(div().px_4().py_2().text_sm().child(format!(
                    "A program is still running in '{label}'. Closing the terminal ends it."
                )))
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default()
                        .ok_text("Close")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger),
                )
                .on_ok(move |_, window, cx| {
                    dock.update(cx, |dock, cx| dock.close(id, window, cx));
                    true
                })
        });
    }

    /// Close a tab now; its shell is killed. Closing the last tab hides the
    /// dock, as in VS Code.
    pub fn close(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let was_active = index == self.active;
        let tab = self.tabs.remove(index);
        self.registry.remove(&tab.registry_id);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.open = false;
            self.maximized = false;
            self.live_refresh = None;
            cx.emit(TerminalDockEvent::Hidden);
        } else if was_active {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Move the tab `id` to where `target` is.
    pub fn move_tab(&mut self, id: u64, target: u64, cx: &mut Context<Self>) {
        let from = self.tabs.iter().position(|tab| tab.id == id);
        let to = self.tabs.iter().position(|tab| tab.id == target);
        if let (Some(from), Some(to)) = (from, to) {
            self.active = move_item(&mut self.tabs, from, to, self.active);
            cx.notify();
        }
    }

    /// Set the dock height (a drag on its top edge) and save it once the
    /// drag settles.
    pub fn set_height(&mut self, height: f32, cx: &mut Context<Self>) {
        let settings = cx.global_mut::<GeneralSettingsModel>();
        if settings.terminal.dock_height == height {
            return;
        }
        settings.terminal.dock_height = height;
        cx.notify();
        self.save_height = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = cx.update(|cx| {
                crate::settings::controllers::general_settings_controller::save_general_settings(cx)
            });
        }));
    }
}

impl DockTab {
    fn title(&self) -> String {
        tab_title(
            self.live_program.as_deref(),
            &self.shell_name,
            self.live_cwd.as_deref().unwrap_or(&self.cwd),
        )
    }

    /// The program's full OSC title, else the directory's full path.
    fn tooltip(&self, cx: &App) -> String {
        tab_tooltip(
            self.view.read(cx).title(),
            self.live_cwd.as_deref().unwrap_or(&self.cwd),
        )
    }
}

impl Render for TerminalDock {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let dock = cx.entity();
        // VS Code uses Ctrl+Shift+` for a new terminal on every platform.
        let new_key = "Ctrl+Shift+`";
        let toggle_key = if cfg!(target_os = "macos") {
            "Cmd+J"
        } else {
            "Ctrl+J"
        };

        let flashed = self.read_flash.as_ref().map(|(id, _)| id.as_str());
        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let active = index == self.active;
            let access = self.registry.access(&tab.registry_id);
            let shared = access.can_read();
            let read_now = flashed == Some(tab.registry_id.as_str());
            let exit = exit_text(tab.view.read(cx).exit_status());
            let exited = exit.is_some();
            let label: SharedString = tab.title().into();
            let tooltip: SharedString = tab.tooltip(cx).into();
            let id = tab.id;
            h_flex()
                .id(("terminal-tab", id))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                })
                .h_full()
                .max_w(px(260.))
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .pl_2()
                .pr_1()
                .border_r_1()
                .border_color(theme.border)
                .text_xs()
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.background).text_color(theme.foreground)
                })
                .when(!active, |this| {
                    this.text_color(theme.muted_foreground)
                        .hover(|s| s.bg(theme.list_hover))
                })
                // The agent just read this tab.
                .when(read_now, |this| this.bg(theme.info.opacity(0.25)))
                .on_click({
                    let dock = dock.clone();
                    move |_, window, cx| {
                        dock.update(cx, |dock, cx| dock.activate(index, window, cx));
                    }
                })
                .on_drag(
                    DraggedTerminalTab {
                        id,
                        label: label.clone(),
                    },
                    |tab: &DraggedTerminalTab, _, _, cx| {
                        let label = tab.label.clone();
                        cx.new(|_| TabDragPreview { label })
                    },
                )
                .drag_over::<DraggedTerminalTab>(|style, _, _, cx| style.bg(cx.theme().drop_target))
                .on_drop::<DraggedTerminalTab>({
                    let dock = dock.clone();
                    move |dragged, _, cx| {
                        let moved = dragged.id;
                        dock.update(cx, |dock, cx| dock.move_tab(moved, id, cx));
                    }
                })
                .child(
                    Icon::new(IconName::SquareTerminal)
                        .size_3()
                        .flex_none()
                        .when(exited, |icon| icon.text_color(theme.muted_foreground)),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .when(exited, |this| this.italic())
                        .child(label),
                )
                // Outside the ellipsised title, so a long title never
                // hides it.
                .when_some(exit, |this, exit| {
                    this.child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child(exit),
                    )
                })
                .child(
                    Button::new(("terminal-tab-share", id))
                        .ghost()
                        .xsmall()
                        .icon(
                            Icon::new(if shared {
                                IconName::Eye
                            } else {
                                IconName::EyeOff
                            })
                            .when(shared, |icon| icon.text_color(theme.info)),
                        )
                        .tooltip(share_tooltip(access))
                        .on_click({
                            let dock = dock.clone();
                            move |_, window, cx| {
                                cx.stop_propagation();
                                dock.update(cx, |dock, cx| dock.share_clicked(id, window, cx));
                            }
                        }),
                )
                .child(
                    Button::new(("terminal-tab-close", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close terminal")
                        .on_click({
                            let dock = dock.clone();
                            move |_, window, cx| {
                                cx.stop_propagation();
                                dock.update(cx, |dock, cx| dock.request_close(id, window, cx));
                            }
                        }),
                )
        });

        let header = h_flex()
            .id("terminal-dock-header")
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .w_full()
            .items_center()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            // T8b puts the pinned Agent tab first in this strip, before
            // the human terminals.
            .child(
                h_flex()
                    .id("terminal-dock-tabs")
                    .h_full()
                    .min_w_0()
                    .flex_shrink()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(
                Button::new("terminal-dock-new")
                    .ghost()
                    .xsmall()
                    .ml_1()
                    .icon(IconName::Plus)
                    .tooltip(format!("New terminal ({new_key})"))
                    .on_click({
                        let dock = dock.clone();
                        move |_, window, cx| {
                            dock.update(cx, |dock, cx| dock.new_terminal(window, cx));
                        }
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("terminal-dock-maximize")
                    .ghost()
                    .xsmall()
                    .icon(if self.maximized {
                        IconName::Minimize
                    } else {
                        IconName::Maximize
                    })
                    .tooltip(if self.maximized {
                        "Restore panel size"
                    } else {
                        "Maximize panel size"
                    })
                    .on_click({
                        let dock = dock.clone();
                        move |_, _, cx| dock.update(cx, |dock, cx| dock.toggle_maximized(cx))
                    }),
            )
            .child(
                Button::new("terminal-dock-hide")
                    .ghost()
                    .xsmall()
                    .mr_1()
                    .icon(IconName::ChevronDown)
                    .tooltip(format!("Hide panel ({toggle_key})"))
                    .on_click({
                        let dock = dock.clone();
                        move |_, _, cx| {
                            dock.update(cx, |dock, cx| {
                                dock.hide(cx);
                                cx.emit(TerminalDockEvent::Hidden);
                            })
                        }
                    }),
            );

        v_flex()
            .id("terminal-dock")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .px_2()
                    .pt_1()
                    .when_some(self.active_view().cloned(), |this, view| this.child(view)),
            )
            // The top-edge resize handle, over the border. Hidden while
            // maximised, as in VS Code.
            .when(!self.maximized, |this| {
                this.child(
                    div()
                        .id("terminal-dock-resize")
                        .absolute()
                        .top(px(-3.))
                        .left_0()
                        .right_0()
                        .h(px(6.))
                        .cursor_row_resize()
                        .on_drag(DockResizeDrag, |_, _, _, cx| cx.new(|_| gpui::Empty)),
                )
            })
    }
}

/// The label under the pointer while a tab is dragged.
struct TabDragPreview {
    label: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .text_xs()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_sm()
            .child(self.label.clone())
    }
}

/// The active conversation's workspace directory, if it has one: the same
/// directory the file explorer shows for it.
fn active_conversation_dir(cx: &App) -> Option<PathBuf> {
    cx.try_global::<chatty_core::models::ConversationsStore>()
        .and_then(|store| {
            store
                .active_id()
                .and_then(|id| store.get_conversation(id))
                .and_then(|conv| conv.working_dir().cloned())
        })
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    home.map(PathBuf::from)
}

/// Where a new terminal starts: the conversation's workspace, else the
/// app-wide workspace setting (what the file explorer falls back to), else
/// the home directory. Directories that no longer exist are skipped.
/// `None` only when even the home directory is unknown (then the shell
/// inherits the app's own).
pub fn terminal_cwd(
    conversation_dir: Option<PathBuf>,
    global_workspace: Option<String>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    conversation_dir
        .into_iter()
        .chain(
            global_workspace
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        )
        .find(|dir| !dir.as_os_str().is_empty() && dir.is_dir())
        .or(home)
}

/// The spawn configuration for a dock terminal under `settings`.
pub fn terminal_config(settings: &TerminalSettings, cwd: Option<PathBuf>) -> TerminalConfig {
    TerminalConfig {
        shell: settings
            .shell
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        cwd,
        scrollback: settings.scrollback_lines,
        ..TerminalConfig::default()
    }
}

/// The shell's name for a tab title: the override's file name, else the
/// platform default's (`$SHELL` on Unix).
fn shell_display_name(shell: Option<&str>) -> String {
    #[cfg(unix)]
    let default = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
    #[cfg(windows)]
    let default = String::from("pwsh");
    let program = shell.map(str::to_string).unwrap_or(default);
    Path::new(&program)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or(program)
}

/// A tab's title, as in VS Code and Zed: the program in the foreground
/// (the shell itself at its prompt) and the basename of the directory, e.g.
/// `bash — ws` or `cargo — chatty2`.
pub fn tab_title(program: Option<&str>, shell_name: &str, cwd: &Path) -> String {
    let program = program
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(shell_name);
    let dir = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string());
    if dir.is_empty() {
        program.to_string()
    } else {
        format!("{program} — {dir}")
    }
}

/// A tab's tooltip: the program's full OSC title when it set one (bash's
/// `user@host: /long/path`), else the directory's full path.
pub fn tab_tooltip(osc_title: Option<&str>, cwd: &Path) -> String {
    osc_title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| cwd.display().to_string())
}

/// The exit status shown after an exited tab's title.
pub fn exit_text(exit: Option<Option<i32>>) -> Option<String> {
    exit.map(|code| match code {
        Some(code) => format!("exited {code}"),
        None => "exited".to_string(),
    })
}

/// Whether closing a terminal needs a confirmation: only while a program
/// other than the shell is running in it. Where that cannot be told
/// (Windows), a running terminal always asks.
pub fn close_needs_confirmation(exited: bool, foreground_job: Option<bool>) -> bool {
    !exited && foreground_job.unwrap_or(true)
}

/// Move `items[from]` to index `to`, returning where the item at `active`
/// ends up.
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize, active: usize) -> usize {
    if from == to || from >= items.len() || to >= items.len() {
        return active;
    }
    let item = items.remove(from);
    items.insert(to, item);
    if active == from {
        to
    } else if from < active && active <= to {
        active - 1
    } else if to <= active && active < from {
        active + 1
    } else {
        active
    }
}

/// The dock height for a drag whose pointer is `pointer_y` pixels below the
/// top of a chat column `column_height` tall: the distance to the column's
/// bottom, kept between [`MIN_DOCK_HEIGHT`] and the height that still
/// leaves [`MIN_CHAT_HEIGHT`] to the chat.
pub fn dragged_dock_height(column_height: f32, pointer_y: f32) -> f32 {
    let max = (column_height - MIN_CHAT_HEIGHT).max(MIN_DOCK_HEIGHT);
    (column_height - pointer_y)
        .round()
        .clamp(MIN_DOCK_HEIGHT, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_prefers_the_conversation_workspace() {
        let conv = std::env::temp_dir();
        let got = terminal_cwd(
            Some(conv.clone()),
            Some("/definitely/not/here".into()),
            Some("/home/me".into()),
        );
        assert_eq!(got, Some(conv));
    }

    #[test]
    fn cwd_falls_back_to_the_global_workspace_then_home() {
        let global = std::env::temp_dir();
        assert_eq!(
            terminal_cwd(
                None,
                Some(global.to_string_lossy().into_owned()),
                Some("/home/me".into())
            ),
            Some(global)
        );
        assert_eq!(
            terminal_cwd(None, None, Some("/home/me".into())),
            Some(PathBuf::from("/home/me"))
        );
        assert_eq!(
            terminal_cwd(None, Some(String::new()), Some("/home/me".into())),
            Some(PathBuf::from("/home/me"))
        );
    }

    #[test]
    fn cwd_skips_a_workspace_that_no_longer_exists() {
        assert_eq!(
            terminal_cwd(
                Some("/definitely/not/here".into()),
                None,
                Some("/home/me".into())
            ),
            Some(PathBuf::from("/home/me"))
        );
    }

    #[test]
    fn title_is_program_and_directory_basename() {
        let cwd = Path::new("/tmp/age582/run/ws");
        // The shell at its prompt.
        assert_eq!(tab_title(Some("bash"), "bash", cwd), "bash — ws");
        // A program running in it.
        assert_eq!(tab_title(Some("cargo"), "bash", cwd), "cargo — ws");
        // Foreground unknown (not Linux): the shell's name.
        assert_eq!(tab_title(None, "zsh", cwd), "zsh — ws");
        assert_eq!(tab_title(Some("  "), "zsh", cwd), "zsh — ws");
        // The root directory has no basename.
        assert_eq!(tab_title(Some("bash"), "bash", Path::new("/")), "bash — /");
        assert_eq!(tab_title(None, "bash", Path::new("")), "bash");
    }

    #[test]
    fn tooltip_is_the_full_osc_title_or_the_path() {
        let cwd = Path::new("/tmp/age582/run/ws");
        assert_eq!(
            tab_tooltip(Some("marcel@marcel-beast: /tmp/age582/run/ws"), cwd),
            "marcel@marcel-beast: /tmp/age582/run/ws"
        );
        assert_eq!(tab_tooltip(None, cwd), "/tmp/age582/run/ws");
        assert_eq!(tab_tooltip(Some(" "), cwd), "/tmp/age582/run/ws");
    }

    #[test]
    fn exit_status_text() {
        assert_eq!(exit_text(None), None);
        assert_eq!(exit_text(Some(Some(0))).as_deref(), Some("exited 0"));
        assert_eq!(exit_text(Some(Some(130))).as_deref(), Some("exited 130"));
        assert_eq!(exit_text(Some(None)).as_deref(), Some("exited"));
    }

    #[test]
    fn shell_name_is_the_program_file_name() {
        assert_eq!(shell_display_name(Some("/usr/bin/zsh")), "zsh");
        assert_eq!(shell_display_name(Some("fish")), "fish");
    }

    #[test]
    fn close_confirms_only_for_a_running_foreground_program() {
        // Shell at its prompt: close at once.
        assert!(!close_needs_confirmation(false, Some(false)));
        // `vim`, `cargo build`, …: ask.
        assert!(close_needs_confirmation(false, Some(true)));
        // Cannot tell (Windows): ask while it runs.
        assert!(close_needs_confirmation(false, None));
        // Exited: one click, whatever the query says.
        assert!(!close_needs_confirmation(true, None));
        assert!(!close_needs_confirmation(true, Some(true)));
    }

    #[test]
    fn move_item_keeps_the_active_tab_selected() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        // Drag the active tab itself.
        assert_eq!(move_item(&mut v, 0, 2, 0), 2);
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        // Drag another tab from before the active one to after it.
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert_eq!(move_item(&mut v, 0, 3, 2), 1);
        assert_eq!(v, ['b', 'c', 'd', 'a']);
        assert_eq!(v[1], 'c');
        // …and from after it to before it.
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert_eq!(move_item(&mut v, 3, 0, 1), 2);
        assert_eq!(v, ['d', 'a', 'b', 'c']);
        assert_eq!(v[2], 'b');
        // Out of range is a no-op.
        assert_eq!(move_item(&mut v, 9, 0, 1), 1);
    }

    #[test]
    fn dragged_height_is_clamped() {
        // 800 px column, pointer 500 px down: 300 px dock.
        assert_eq!(dragged_dock_height(800., 500.), 300.);
        // Whole pixels, so the saved setting reads cleanly.
        assert_eq!(dragged_dock_height(800., 274.006), 526.);
        // Dragged past the bottom: the minimum.
        assert_eq!(dragged_dock_height(800., 790.), MIN_DOCK_HEIGHT);
        // Dragged to the top: leaves the chat its minimum.
        assert_eq!(dragged_dock_height(800., 0.), 800. - MIN_CHAT_HEIGHT);
        // A column too short for both: the dock's minimum wins.
        assert_eq!(dragged_dock_height(200., 0.), MIN_DOCK_HEIGHT);
    }

    #[test]
    fn config_applies_shell_override_and_scrollback() {
        let settings = TerminalSettings {
            shell: Some(" /bin/zsh ".into()),
            scrollback_lines: 500,
            ..TerminalSettings::default()
        };
        let config = terminal_config(&settings, Some("/tmp".into()));
        assert_eq!(config.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(config.scrollback, 500);
        assert_eq!(config.cwd, Some(PathBuf::from("/tmp")));

        let blank = TerminalSettings {
            shell: Some("  ".into()),
            ..TerminalSettings::default()
        };
        assert_eq!(terminal_config(&blank, None).shell, None);
    }

    /// Every dock chord is reserved, so a focused terminal hands it to the
    /// app instead of the shell.
    #[test]
    fn dock_chords_are_reserved_keys() {
        for key in TOGGLE_DOCK_KEYS.iter().chain(NEW_TERMINAL_KEYS) {
            let keystroke = gpui::Keystroke::parse(key).unwrap();
            assert!(super::super::keys::is_reserved(&keystroke), "{key}");
        }
    }

    /// The settings default and the terminal crate's default agree.
    #[test]
    fn scrollback_defaults_agree() {
        assert_eq!(
            TerminalSettings::default().scrollback_lines,
            chatty_terminal::DEFAULT_SCROLLBACK
        );
    }
}
