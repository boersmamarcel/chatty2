use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::assets::CustomIcon;
use chatty_core::services::browser::{
    BrowserManager, BrowserSession, ControlHolder, InputModifiers, KeyInput, MouseAction,
    MouseButtonKind, MouseInput, ScreencastFrame, ScreencastUpdate,
};
use chatty_core::services::pdf_thumbnail::{
    PREVIEW_WIDTH, PdfThumbnailError, pdf_page_count, render_pdf_page,
};
use chatty_core::tools::chart_tool::ChartSpec;
use chatty_core::tools::data_query_tool::{
    FILE_PREVIEW_MAX_ROWS, TablePreview, load_file_table_preview,
};
use std::ops::Range;
use tokio::sync::mpsc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::input::{Input, InputEvent, InputState, Position};
use gpui_component::list::ListItem;
use gpui_component::menu::PopupMenuItem;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::tree::{TreeItem, TreeState, tree};
use gpui_component::{Icon, IconName, Sizable, VirtualListScrollHandle, v_flex};
use tracing::warn;

use super::artifact_card::reveal_path_in_os;
use super::artifact_kind::{
    ArtifactHeading, ArtifactVersion, ViewAnchor, artifact_format_token,
    artifact_language_for_path, artifact_panel_title, artifact_version, block_index_from_anchor,
    is_code_artifact_path, is_image_path, is_markdown_artifact_path, is_pdf_path, is_tabular_path,
    markdown_headings, read_artifact_source, source_line_from_anchor,
};
use super::diff::DiffHunkList;
use super::run_pin::{RunPin, RunPinKind};
use super::session_review_panel::{ReviewFileSection, SessionReviewPanel};
use super::table::render_table_preview_view;
use crate::chatty::views::chart_renderer::render_chart_panel;
use crate::chatty::views::diff_view_component::diff_line_stats_fast;

const PDF_PAGE_DISPLAY_WIDTH: f32 = 348.0;
const IMAGE_DISPLAY_WIDTH: f32 = PDF_PAGE_DISPLAY_WIDTH;
const DOCUMENT_MEASURE_PX: f32 = 680.0;
const OUTLINE_WIDTH: f32 = 220.0;

/// Fixed CDP viewport the browser artifact screencasts at. Matches how the
/// PDF/image previews already work in this file: fetch one canonical
/// resolution, let the flex layout scale it to fit rather than re-requesting
/// a new capture on every layout pass.
///
/// A normal laptop-desktop size, not a narrow one: most responsive sites
/// switch to a cramped, oversized-nav "tablet" layout below ~1024px wide,
/// which reads as "zoomed in" once it's scaled up to fill the panel. A
/// wider source viewport downscales to fit a smaller panel instead (sharp);
/// only a source narrower than the panel would need to scale up (blurry).
const BROWSER_VIEWPORT_WIDTH: u32 = 1280;
const BROWSER_VIEWPORT_HEIGHT: u32 = 800;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ArtifactMode {
    #[default]
    Closed,
    Docked,
    Full,
}

/// Full-window is a posture for *this* document. Opening another file docks.
pub fn presentation_on_open(current: ArtifactMode, opening_same_path: bool) -> ArtifactMode {
    match current {
        ArtifactMode::Closed => ArtifactMode::Docked,
        ArtifactMode::Full if !opening_same_path => ArtifactMode::Docked,
        other => other,
    }
}

#[derive(Clone, Debug)]
pub enum ArtifactViewEvent {
    Closed,
    PresentationChanged,
}

#[derive(Clone, Debug, Default)]
enum PdfPreview {
    #[default]
    Idle,
    Loading {
        page: u32,
    },
    Ready {
        page: u32,
        total: u32,
        image: PathBuf,
    },
    Error(String),
}

#[derive(Clone, Debug, Default)]
enum TabularPreview {
    #[default]
    Idle,
    Loading,
    Ready(TablePreview),
    Error(String),
}

/// Live screencast state (AGE-155). `Arc<RenderImage>` clones cheaply — it's
/// just a refcount bump, not a pixel copy.
#[derive(Clone, Default)]
enum BrowserPreview {
    #[default]
    Idle,
    Starting,
    Frame(Arc<RenderImage>),
    Error(String),
}

/// One artifact workbench entity: Closed | Docked | Full. Reparent, do not rebuild.
pub struct ArtifactView {
    pub mode: ArtifactMode,
    pub path: Option<PathBuf>,
    pub rendered: String,
    pub source: String,
    pub old: String,
    files: Vec<(PathBuf, String, Option<String>)>,
    tab: usize,
    pdf: PdfPreview,
    tabular: TabularPreview,
    chart: Option<ChartSpec>,
    browser: BrowserPreview,
    /// Set while a browser artifact is open — also the idle-teardown handle:
    /// `stop_browser_screencast` takes it and stops the session (AGE-155).
    browser_manager: Option<Arc<BrowserManager>>,
    /// The live session backing `browser_manager`, cached so input
    /// forwarding and control-lock toggles (AGE-156) don't have to resolve
    /// it through the manager on every mouse move.
    browser_session: Option<Arc<BrowserSession>>,
    /// Mirrors the session's control holder for the UI badge/button. The
    /// session is the source of truth; this is a display cache updated on
    /// open and on every take/release this view initiates.
    browser_control: ControlHolder,
    /// Forwarded input drains through a single ordered consumer per stream
    /// (AGE-156) rather than one spawned task per event — mouse moves are
    /// too frequent for that, and CDP calls from independent tasks could
    /// arrive out of order.
    browser_mouse_tx: Option<mpsc::UnboundedSender<MouseInput>>,
    browser_key_tx: Option<mpsc::UnboundedSender<KeyInput>>,
    /// The rendered browser frame's window-space bounds, captured via a
    /// `canvas()` prepaint each frame so mouse handlers can map a
    /// window-relative position back into CDP viewport space.
    browser_frame_bounds: Rc<RefCell<Bounds<Pixels>>>,
    /// Routes forwarded key events: the frame must hold focus for
    /// `on_key_down` to fire at all.
    browser_focus: FocusHandle,
    /// Address bar (AGE-156) — lets the user navigate directly instead of
    /// only forwarding clicks/keys to whatever page is already loaded.
    browser_address: Entity<InputState>,
    /// Last URL this view knows about, from either side's navigation.
    /// Mirrored into `browser_address` during render (needs `Window`),
    /// not from the background task that learns about it.
    browser_current_url: String,
    browser_address_dirty: bool,
    /// CDP viewport size (AGE-156) — kept matched to the panel's actual
    /// rendered size by `sync_browser_viewport_size` rather than staying
    /// fixed at `BROWSER_VIEWPORT_WIDTH`/`HEIGHT` for the session's whole
    /// life, so expanding the artifact window shows more of the real page
    /// instead of just scaling a static-size capture up. `(0, 0)` before
    /// the first screencast frame's bounds are known.
    browser_requested_size: (u32, u32),
    workspace_root: Option<String>,
    load_gen: u64,
    editor: Entity<InputState>,
    outline: Entity<TreeState>,
    headings: Vec<ArtifactHeading>,
    loaded_version: Option<ArtifactVersion>,
    stale: bool,
    run_visible: bool,
    pending_approval: bool,
    editor_synced_gen: u64,
    outline_synced_gen: u64,
    pending_jump_line: Option<u32>,
    anchors: HashMap<(String, usize), ViewAnchor>,
    session_review: bool,
    review_sections: Vec<Entity<ReviewFileSection>>,
    review_total_added: usize,
    review_total_removed: usize,
    review_scroll: VirtualListScrollHandle,
    review_layout_gen: u64,
}

impl ArtifactView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("markdown")
                .searchable(true)
        });
        let outline = cx.new(|cx| TreeState::new(cx));
        cx.observe(&outline, |this, state, cx| {
            let line = state
                .read(cx)
                .selected_entry()
                .and_then(|entry| entry.item().id.parse::<u32>().ok());
            if let Some(line) = line {
                this.jump_to_heading_line(line, cx);
            }
        })
        .detach();
        let browser_address = cx.new(|cx| InputState::new(window, cx).placeholder("Enter a URL…"));
        cx.subscribe(
            &browser_address,
            |this: &mut Self, input, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let url = input.read(cx).value().to_string();
                    this.navigate_browser_as_user(url, cx);
                }
            },
        )
        .detach();
        Self {
            mode: ArtifactMode::Closed,
            path: None,
            rendered: String::new(),
            source: String::new(),
            old: String::new(),
            files: Vec::new(),
            tab: 0,
            pdf: PdfPreview::Idle,
            tabular: TabularPreview::Idle,
            chart: None,
            browser: BrowserPreview::Idle,
            browser_manager: None,
            browser_session: None,
            browser_control: ControlHolder::Agent,
            browser_mouse_tx: None,
            browser_key_tx: None,
            browser_frame_bounds: Rc::new(RefCell::new(Bounds::default())),
            browser_focus: cx.focus_handle(),
            browser_address,
            browser_current_url: String::new(),
            browser_address_dirty: false,
            browser_requested_size: (0, 0),
            workspace_root: None,
            load_gen: 0,
            editor,
            outline,
            headings: Vec::new(),
            loaded_version: None,
            stale: false,
            run_visible: false,
            pending_approval: false,
            editor_synced_gen: u64::MAX,
            outline_synced_gen: u64::MAX,
            pending_jump_line: None,
            anchors: HashMap::new(),
            session_review: false,
            review_sections: Vec::new(),
            review_total_added: 0,
            review_total_removed: 0,
            review_scroll: VirtualListScrollHandle::new(),
            review_layout_gen: 0,
        }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn set_chrome(
        &mut self,
        run_visible: bool,
        pending_approval: bool,
        cx: &mut Context<Self>,
    ) {
        if self.session_review {
            self.run_visible = run_visible;
            self.pending_approval = pending_approval;
            return;
        }
        let approval_break = pending_approval && self.mode == ArtifactMode::Full;
        if self.run_visible == run_visible
            && self.pending_approval == pending_approval
            && !approval_break
        {
            return;
        }
        self.run_visible = run_visible;
        self.pending_approval = pending_approval;
        if approval_break {
            self.mode = ArtifactMode::Docked;
            cx.emit(ArtifactViewEvent::PresentationChanged);
        }
        cx.notify();
    }

    pub fn open_table(&mut self, preview: TablePreview, cx: &mut Context<Self>) {
        self.session_review = false;
        self.review_sections.clear();
        self.stop_browser_screencast(cx);
        let next_path = match &preview.source {
            chatty_core::tools::data_query_tool::TableSource::File { path } => {
                Some(PathBuf::from(path))
            }
            _ => None,
        };
        self.mode = presentation_on_open(self.mode, next_path.as_ref() == self.path.as_ref());
        self.path = next_path;
        self.tabular = TabularPreview::Ready(preview);
        self.pdf = PdfPreview::Idle;
        self.chart = None;
        self.tab = 0;
        self.stale = false;
        self.headings.clear();
        cx.emit(ArtifactViewEvent::PresentationChanged);
        cx.notify();
    }

    pub fn open_chart(&mut self, spec: ChartSpec, cx: &mut Context<Self>) {
        self.session_review = false;
        self.review_sections.clear();
        self.stop_browser_screencast(cx);
        let next_path = spec.saved_path.as_ref().map(PathBuf::from);
        self.mode = presentation_on_open(self.mode, next_path.as_ref() == self.path.as_ref());
        self.path = next_path;
        self.chart = Some(spec);
        self.pdf = PdfPreview::Idle;
        self.tabular = TabularPreview::Idle;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.tab = 0;
        self.stale = false;
        self.headings.clear();
        cx.emit(ArtifactViewEvent::PresentationChanged);
        cx.notify();
    }

    /// Open the live browser viewport (AGE-155): the browser is another
    /// artifact the agent produced, opened the same way as a diff or a
    /// generated file — just backed by a screencast instead of a path.
    pub fn open_browser(&mut self, manager: Arc<BrowserManager>, cx: &mut Context<Self>) {
        self.session_review = false;
        self.review_sections.clear();
        let already_open = self
            .browser_manager
            .as_ref()
            .is_some_and(|existing| Arc::ptr_eq(existing, &manager));
        self.mode = presentation_on_open(self.mode, already_open);
        if !already_open {
            self.stop_browser_screencast(cx);
        }
        self.path = None;
        self.pdf = PdfPreview::Idle;
        self.tabular = TabularPreview::Idle;
        self.chart = None;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.tab = 0;
        self.stale = false;
        self.headings.clear();
        cx.emit(ArtifactViewEvent::PresentationChanged);

        if already_open {
            cx.notify();
            return;
        }

        self.browser_manager = Some(manager.clone());
        self.browser = BrowserPreview::Starting;
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        cx.spawn(async move |this, cx| {
            let session = match manager.session().await {
                Ok(session) => session,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.load_gen == load_id {
                            this.browser = BrowserPreview::Error(e.to_string());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };

            // AGE-156: cache the session and stand up ordered input-forwarding
            // drains before the first frame arrives, so the control button
            // works from the "Starting…" placeholder onward. One consumer
            // task per stream, not one spawned task per event — mouse moves
            // are too frequent for that, and independent tasks racing the
            // CDP connection could deliver events out of order.
            let (mouse_tx, mut mouse_rx) = mpsc::unbounded_channel::<MouseInput>();
            let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyInput>();
            {
                let session = session.clone();
                cx.background_spawn(async move {
                    // Coalesce a backlog of trailing same-kind Move/Wheel
                    // events before dispatching: each dispatch is a real
                    // CDP round trip, slower than a trackpad or fast mouse
                    // move can fire, so draining one event per await here
                    // (as this loop used to) builds a growing lag between
                    // the input and what the page does. Wheel deltas are
                    // summed so the total scroll distance stays correct;
                    // Move keeps only the latest position. Down/Up are
                    // never merged or dropped — hitting one stops the
                    // coalescing run, and it carries over to the next
                    // outer iteration via `pending` rather than being lost.
                    let mut pending: Option<MouseInput> = None;
                    loop {
                        let mut input = match pending.take() {
                            Some(input) => input,
                            None => match mouse_rx.recv().await {
                                Some(input) => input,
                                None => break,
                            },
                        };
                        while let Ok(next) = mouse_rx.try_recv() {
                            match (&mut input.action, next.action) {
                                (MouseAction::Move, MouseAction::Move) => {
                                    input.x = next.x;
                                    input.y = next.y;
                                    input.modifiers = next.modifiers;
                                }
                                (
                                    MouseAction::Wheel { delta_x, delta_y },
                                    MouseAction::Wheel {
                                        delta_x: next_dx,
                                        delta_y: next_dy,
                                    },
                                ) => {
                                    *delta_x += next_dx;
                                    *delta_y += next_dy;
                                    input.x = next.x;
                                    input.y = next.y;
                                    input.modifiers = next.modifiers;
                                }
                                _ => {
                                    pending = Some(next);
                                    break;
                                }
                            }
                        }
                        let _ = session.dispatch_mouse(input).await;
                    }
                })
                .detach();
            }
            {
                let session = session.clone();
                cx.background_spawn(async move {
                    while let Some(input) = key_rx.recv().await {
                        let _ = session.dispatch_key(input).await;
                    }
                })
                .detach();
            }
            let control_holder = session.control_holder();
            this.update(cx, |this, cx| {
                if this.load_gen == load_id {
                    this.browser_session = Some(session.clone());
                    this.browser_control = control_holder;
                    this.browser_mouse_tx = Some(mouse_tx);
                    this.browser_key_tx = Some(key_tx);
                    this.browser_requested_size = (BROWSER_VIEWPORT_WIDTH, BROWSER_VIEWPORT_HEIGHT);
                    cx.notify();
                }
            })
            .ok();

            // AGE-156: seed the address bar with the current URL, then keep
            // it live as either side navigates — the agent's browser_navigate
            // tool or the user typing a new one.
            {
                let mut url_rx = session.watch_url();
                let initial_url = url_rx.borrow_and_update().clone();
                this.update(cx, |this, cx| {
                    if this.load_gen == load_id {
                        this.browser_current_url = initial_url;
                        this.browser_address_dirty = true;
                        cx.notify();
                    }
                })
                .ok();
                let this = this.clone();
                cx.spawn(async move |cx| {
                    loop {
                        if url_rx.changed().await.is_err() {
                            return;
                        }
                        let url = url_rx.borrow_and_update().clone();
                        let alive = this
                            .update(cx, |this, cx| {
                                if this.load_gen == load_id {
                                    this.browser_current_url = url;
                                    this.browser_address_dirty = true;
                                    cx.notify();
                                }
                            })
                            .is_ok();
                        if !alive {
                            return;
                        }
                    }
                })
                .detach();
            }

            let mut frames = match session
                .start_screencast(BROWSER_VIEWPORT_WIDTH, BROWSER_VIEWPORT_HEIGHT)
                .await
            {
                Ok(rx) => rx,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.load_gen == load_id {
                            this.browser = BrowserPreview::Error(e.to_string());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            loop {
                if frames.changed().await.is_err() {
                    // The sender dropped — either `stop_screencast` tore it
                    // down (`load_gen` will already have moved on, so the
                    // stale check below is what actually silences this) or
                    // the browser crashed out from under a still-active view.
                    this.update(cx, |this, cx| {
                        if this.load_gen == load_id {
                            this.browser =
                                BrowserPreview::Error("browser session ended".to_string());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
                let update = frames.borrow_and_update().clone();
                let next = match update {
                    // Nothing changed from our own initial state — keep
                    // waiting rather than repainting for no reason.
                    ScreencastUpdate::Starting => continue,
                    ScreencastUpdate::Frame(frame) => {
                        BrowserPreview::Frame(render_image_from_rgba(&frame))
                    }
                    ScreencastUpdate::Error(message) => BrowserPreview::Error(message),
                };
                let superseded = this
                    .update(cx, |this, cx| {
                        if this.load_gen != load_id {
                            return true;
                        }
                        this.browser = next;
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if superseded {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Stop the screencast this view started, if any — called whenever the
    /// browser stops being the active artifact (AGE-155's idle handling): a
    /// screencast nobody is watching is pure CPU.
    fn stop_browser_screencast(&mut self, cx: &mut Context<Self>) {
        self.browser = BrowserPreview::Idle;
        // Bump so the pump loop above notices it's stale even if the
        // channel never fires `changed()` again (a static page sends no
        // further frames, so the loop would otherwise block forever).
        self.load_gen = self.load_gen.wrapping_add(1);
        self.browser_session = None;
        self.browser_control = ControlHolder::Agent;
        self.browser_current_url.clear();
        self.browser_address_dirty = true;
        self.browser_requested_size = (0, 0);
        // Dropping the senders ends the drain loops (AGE-156) — their
        // `.recv()` returns `None` once every sender is gone.
        self.browser_mouse_tx = None;
        self.browser_key_tx = None;
        if let Some(manager) = self.browser_manager.take() {
            cx.background_spawn(async move {
                manager.stop_screencast().await;
            })
            .detach();
        }
    }

    /// The user takes control (AGE-156) — never requested, granted
    /// immediately. A no-op if the browser artifact isn't open.
    pub fn take_browser_control(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.as_ref() else {
            return;
        };
        session.take_control();
        self.browser_control = ControlHolder::User;
        cx.notify();
    }

    /// Hand control back to the agent (AGE-156).
    pub fn release_browser_control(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.as_ref() else {
            return;
        };
        session.release_control();
        self.browser_control = ControlHolder::Agent;
        cx.notify();
    }

    /// The user submits a URL from the address bar (AGE-156) — takes
    /// control first, the same as reaching for the mouse does, then
    /// navigates. A no-op if the browser artifact isn't open or the field
    /// is empty; on failure (e.g. a policy-refused host) the address bar
    /// reverts to the last known-good URL rather than showing an error in
    /// place of the live view.
    fn navigate_browser_as_user(&mut self, raw_url: String, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.clone() else {
            return;
        };
        let url = normalize_address_bar_url(&raw_url);
        if url.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = session.navigate_as_user(&url).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(final_url) => {
                        this.browser_control = ControlHolder::User;
                        this.browser_current_url = final_url;
                    }
                    Err(e) => {
                        warn!(error = %e, url = %url, "browser: user navigation failed");
                        this.browser_control = ControlHolder::User;
                        // Fall back to whatever the page actually shows —
                        // do not leave the bad input sitting in the field.
                    }
                }
                this.browser_address_dirty = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The refresh button (AGE-156) reloads the current page — takes
    /// control first, the same as any other user-initiated action. A no-op
    /// if the browser artifact isn't open.
    fn reload_browser(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = session.reload_as_user().await;
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    warn!(error = %e, "browser: user reload failed");
                }
                this.browser_control = ControlHolder::User;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Queue a mouse event for the input-forwarding drain (AGE-156). A
    /// cheap, synchronous, non-blocking send — the actual CDP call happens
    /// on the background task started in `open_browser`.
    fn send_browser_mouse(&self, input: MouseInput) {
        if let Some(tx) = &self.browser_mouse_tx {
            let _ = tx.send(input);
        }
    }

    /// Queue a keyboard event for the input-forwarding drain (AGE-156).
    fn send_browser_key(&self, input: KeyInput) {
        if let Some(tx) = &self.browser_key_tx {
            let _ = tx.send(input);
        }
    }

    pub fn open(
        &mut self,
        path: PathBuf,
        source: String,
        old: Option<String>,
        workspace_root: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.session_review = false;
        self.review_sections.clear();
        self.stop_browser_screencast(cx);
        let same_path = self.path.as_ref() == Some(&path);
        self.mode = presentation_on_open(self.mode, same_path);
        let old_snapshot = old.clone();
        if !self.files.iter().any(|(existing, _, _)| existing == &path) {
            self.files
                .push((path.clone(), source.clone(), old_snapshot.clone()));
        }
        self.path = Some(path.clone());
        self.workspace_root = workspace_root.clone();
        self.loaded_version = artifact_version(&path);
        self.stale = false;
        self.load_gen = self.load_gen.wrapping_add(1);
        self.headings = markdown_headings(&source);
        if is_pdf_path(&path) {
            self.source.clear();
            self.rendered.clear();
            self.old.clear();
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.tab = 0;
            self.headings.clear();
            self.start_pdf_load(0, cx);
        } else if is_tabular_path(&path) {
            self.pdf = PdfPreview::Idle;
            self.chart = None;
            self.source = source.clone();
            self.rendered = source.clone();
            self.old = old_snapshot.clone().unwrap_or_default();
            self.tab = 0;
            self.start_tabular_load(path, workspace_root, cx);
        } else if is_image_path(&path) {
            self.pdf = PdfPreview::Idle;
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.source.clear();
            self.rendered.clear();
            self.old.clear();
            self.tab = 0;
            self.headings.clear();
        } else {
            self.pdf = PdfPreview::Idle;
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.source = source.clone();
            self.rendered = source.clone();
            self.old = old_snapshot.clone().unwrap_or_default();
            self.tab = if old_snapshot
                .as_ref()
                .is_some_and(|o| !o.is_empty() && o != &source)
            {
                2
            } else {
                0
            };
        }
        cx.emit(ArtifactViewEvent::PresentationChanged);
        cx.notify();
    }

    /// Open every session file in stacked review mode. `focus` expands that file.
    pub fn open_review(
        &mut self,
        files: Vec<(PathBuf, String, Option<String>)>,
        workspace_root: Option<String>,
        focus: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            return;
        }
        self.stop_browser_screencast(cx);
        self.session_review = true;
        self.files = files;
        self.workspace_root = workspace_root;
        self.rebuild_review_sections(focus.as_ref(), cx);
        self.mode = presentation_on_open(self.mode, false);
        self.path = None;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.pdf = PdfPreview::Idle;
        self.tabular = TabularPreview::Idle;
        self.chart = None;
        self.tab = 0;
        self.stale = false;
        self.headings.clear();
        cx.emit(ArtifactViewEvent::PresentationChanged);
        cx.notify();
    }

    fn rebuild_review_sections(&mut self, focus: Option<&PathBuf>, cx: &mut Context<Self>) {
        self.review_sections.clear();
        self.review_total_added = 0;
        self.review_total_removed = 0;

        let expand_ix = focus
            .and_then(|want| self.files.iter().position(|(path, _, _)| path == want))
            .or_else(|| {
                self.files.iter().position(|(_, new, old)| {
                    old.as_ref()
                        .is_some_and(|o| !o.is_empty() && o.as_str() != new.as_str())
                })
            });

        let artifact_view = cx.entity();
        let workspace_root = self.workspace_root.clone();
        for (file_ix, (path, new, old)) in self.files.iter().enumerate() {
            let old_text = old.as_deref().unwrap_or("");
            let (added, removed) = diff_line_stats_fast(old_text, new);
            self.review_total_added += added;
            self.review_total_removed += removed;
            let collapsed = Some(file_ix) != expand_ix;
            let section = cx.new(|_| {
                ReviewFileSection::new(
                    path.clone(),
                    new.clone(),
                    old.clone(),
                    file_ix,
                    collapsed,
                    workspace_root.clone(),
                    artifact_view.clone(),
                )
            });
            self.review_sections.push(section);
        }
        self.review_layout_gen = self.review_layout_gen.wrapping_add(1);
    }

    pub fn bump_review_layout(&mut self, cx: &mut Context<Self>) {
        self.review_layout_gen = self.review_layout_gen.wrapping_add(1);
        cx.notify();
    }

    pub fn review_section_sizes(&self, cx: &App) -> Vec<Size<Pixels>> {
        self.review_sections
            .iter()
            .map(|section| {
                let height = section.read(cx).estimated_height(cx);
                size(px(400.), px(height.max(36.0)))
            })
            .collect()
    }

    pub fn render_review_sections(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Entity<ReviewFileSection>> {
        range
            .filter_map(|ix| self.review_sections.get(ix).cloned())
            .collect()
    }

    pub fn open_single_from_review(
        &mut self,
        path: PathBuf,
        source: String,
        old: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace_root.clone();
        self.open(path, source, old, workspace, cx);
    }

    pub fn set_mode(&mut self, mode: ArtifactMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            // Centralized rather than at each caller — `history.rs` closes
            // the panel directly on conversation switch, bypassing
            // `close_panel`, and a screencast nobody is watching (AGE-155)
            // must not keep running regardless of which path closed it.
            if mode == ArtifactMode::Closed {
                self.stop_browser_screencast(cx);
            }
            cx.emit(if mode == ArtifactMode::Closed {
                ArtifactViewEvent::Closed
            } else {
                ArtifactViewEvent::PresentationChanged
            });
        }
        cx.notify();
    }

    fn toggle_full(&mut self, cx: &mut Context<Self>) {
        let next = if self.mode == ArtifactMode::Full {
            ArtifactMode::Docked
        } else {
            ArtifactMode::Full
        };
        self.set_mode(next, cx);
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.session_review = false;
        self.review_sections.clear();
        if self.mode == ArtifactMode::Full {
            self.set_mode(ArtifactMode::Docked, cx);
        } else {
            self.set_mode(ArtifactMode::Closed, cx);
        }
    }

    fn jump_to_heading_line(&mut self, line: u32, cx: &mut Context<Self>) {
        self.pending_jump_line = Some(line);
        if let Some(path) = self.path.as_ref() {
            self.anchors.insert(
                (path.display().to_string(), 1),
                ViewAnchor::SourceLine(line),
            );
            if let Some(ix) = block_index_from_anchor(&self.headings, ViewAnchor::SourceLine(line))
            {
                self.anchors
                    .insert((path.display().to_string(), 0), ViewAnchor::BlockIndex(ix));
            }
        }
        cx.notify();
    }

    fn select_tab(&mut self, next: usize, window: &mut Window, cx: &mut Context<Self>) {
        if next == self.tab {
            return;
        }
        self.capture_anchor(window, cx);
        self.tab = next;
        self.restore_anchor(window, cx);
        cx.notify();
    }

    fn capture_anchor(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.as_ref().map(|p| p.display().to_string()) else {
            return;
        };
        let line = self.editor.read(cx).cursor_position().line;
        let anchor = if self.tab == 1 {
            ViewAnchor::SourceLine(line)
        } else {
            block_index_from_anchor(&self.headings, ViewAnchor::SourceLine(line))
                .map(ViewAnchor::BlockIndex)
                .unwrap_or(ViewAnchor::SourceLine(line))
        };
        self.anchors.insert((path, self.tab), anchor);
    }

    fn restore_anchor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.as_ref().map(|p| p.display().to_string()) else {
            return;
        };
        let Some(anchor) = self.anchors.get(&(path, self.tab)).copied() else {
            return;
        };
        let line = source_line_from_anchor(&self.headings, anchor);
        self.pending_jump_line = Some(line);
        self.apply_pending_jump(window, cx);
    }

    fn apply_pending_jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.pending_jump_line.take() else {
            return;
        };
        self.editor.update(cx, |editor, cx| {
            editor.set_cursor_position(Position { line, character: 0 }, window, cx);
        });
    }

    fn refresh_staleness(&mut self) {
        let Some(path) = self.path.as_ref() else {
            self.stale = false;
            return;
        };
        if is_pdf_path(path) || is_image_path(path) {
            let current = artifact_version(path);
            self.stale = current.is_some() && current != self.loaded_version;
            return;
        }
        let current = artifact_version(path);
        self.stale = current.is_some() && current != self.loaded_version;
    }

    fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let workspace = self.workspace_root.clone();
        let old = if self.old.is_empty() {
            None
        } else {
            Some(self.old.clone())
        };
        let source = read_artifact_source(&path);
        let keep_mode = self.mode;
        self.open(path, source, old, workspace, cx);
        self.mode = keep_mode;
        cx.notify();
    }

    fn copy_kind(&self, kind: &str, cx: &mut App) {
        let text = match kind {
            "markdown" | "rendered" => {
                if self.rendered.is_empty() {
                    self.source.clone()
                } else {
                    self.rendered.clone()
                }
            }
            _ => self.source.clone(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn start_pdf_load(&mut self, page: u32, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.pdf = PdfPreview::Loading { page };
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || -> Result<_, PdfThumbnailError> {
                let total = pdf_page_count(&path)?;
                let image = render_pdf_page(&path, page, PREVIEW_WIDTH)?;
                Ok((total, image))
            })
            .await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok((total, image))) => {
                        this.pdf = PdfPreview::Ready { page, total, image };
                    }
                    Ok(Err(e)) => this.pdf = PdfPreview::Error(e.to_string()),
                    Err(e) => this.pdf = PdfPreview::Error(e.to_string()),
                }
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "Failed to apply PDF preview"))
            .ok();
        })
        .detach();
    }

    fn start_tabular_load(
        &mut self,
        path: PathBuf,
        workspace_root: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace_root) = workspace_root.filter(|root| !root.is_empty()) else {
            self.tabular = TabularPreview::Error(
                "Set a workspace directory in Settings → Execution to preview tabular files."
                    .into(),
            );
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.tabular = TabularPreview::Loading;
        let file_path = path.to_string_lossy().to_string();
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || {
                load_file_table_preview(&workspace_root, &file_path, FILE_PREVIEW_MAX_ROWS)
            })
            .await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok(preview)) => this.tabular = TabularPreview::Ready(preview),
                    Ok(Err(e)) => this.tabular = TabularPreview::Error(e.to_string()),
                    Err(e) => this.tabular = TabularPreview::Error(e.to_string()),
                }
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "Failed to apply tabular preview"))
            .ok();
        })
        .detach();
    }

    fn turn_pdf_page(&mut self, next: bool, cx: &mut Context<Self>) {
        let PdfPreview::Ready { page, total, .. } = &self.pdf else {
            return;
        };
        let new_page = if next {
            page.saturating_add(1).min(total.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if new_page == *page {
            return;
        }
        self.start_pdf_load(new_page, cx);
        cx.notify();
    }

    fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_synced_gen == self.load_gen {
            if self.pending_jump_line.is_some() {
                self.apply_pending_jump(window, cx);
            }
            return;
        }
        let language = self
            .path
            .as_ref()
            .and_then(|p| artifact_language_for_path(p))
            .unwrap_or_else(|| "markdown".to_string());
        let source = self.source.clone();
        self.editor.update(cx, |editor, cx| {
            editor.set_highlighter(language, cx);
            editor.set_value(source, window, cx);
        });
        self.editor_synced_gen = self.load_gen;
        self.apply_pending_jump(window, cx);
    }

    /// Mirror `browser_current_url` into the address bar's `InputState`
    /// (AGE-156). Split from wherever the URL is learned because
    /// `InputState::set_value` needs `&mut Window`, which the background
    /// task watching `BrowserSession::watch_url` does not have.
    fn sync_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_address_dirty {
            return;
        }
        let url = self.browser_current_url.clone();
        self.browser_address.update(cx, |input, cx| {
            input.set_value(url, window, cx);
        });
        self.browser_address_dirty = false;
    }

    /// Keep the CDP screencast's resolution matched to the artifact panel's
    /// actual rendered size (AGE-156) — otherwise the browser always
    /// captures at the fixed `BROWSER_VIEWPORT_WIDTH`/`HEIGHT` regardless of
    /// how big the panel is, and expanding it (e.g. to `ArtifactMode::Full`)
    /// just letterboxes a static-resolution image instead of showing more
    /// of the real page.
    ///
    /// Debounced against `RESIZE_THRESHOLD_PX` so ordinary per-frame
    /// repaints (this runs on every `render()`) don't spam CDP with resize
    /// calls over sub-pixel layout jitter — only a real size change
    /// retargets. `browser_frame_bounds` is one paint behind `render()`
    /// (the `canvas()` prepaint that fills it runs after layout), which
    /// just means the retarget lags a frame; harmless for a live view.
    fn sync_browser_viewport_size(&mut self, cx: &mut Context<Self>) {
        const RESIZE_THRESHOLD_PX: u32 = 24;
        let Some(session) = self.browser_session.clone() else {
            return;
        };
        let bounds = *self.browser_frame_bounds.borrow();
        let width = f32::from(bounds.size.width).round() as u32;
        let height = f32::from(bounds.size.height).round() as u32;
        if width == 0 || height == 0 {
            return;
        }
        let (last_width, last_height) = self.browser_requested_size;
        if last_width != 0
            && width.abs_diff(last_width) < RESIZE_THRESHOLD_PX
            && height.abs_diff(last_height) < RESIZE_THRESHOLD_PX
        {
            return;
        }
        tracing::debug!(
            from_width = last_width,
            from_height = last_height,
            to_width = width,
            to_height = height,
            "browser: retargeting screencast to panel size"
        );
        self.browser_requested_size = (width, height);
        cx.background_spawn(async move {
            if let Err(e) = session.start_screencast(width, height).await {
                tracing::warn!(error = %e, width, height, "browser: viewport retarget failed");
            }
        })
        .detach();
    }

    fn sync_outline(&mut self, cx: &mut Context<Self>) {
        if self.outline_synced_gen == self.load_gen {
            return;
        }
        let items = headings_to_tree_items(&self.headings);
        self.outline.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
        self.outline_synced_gen = self.load_gen;
    }
}

fn headings_to_tree_items(headings: &[ArtifactHeading]) -> Vec<TreeItem> {
    let mut roots: Vec<TreeItem> = Vec::new();
    let mut stack: Vec<(u8, TreeItem)> = Vec::new();
    for heading in headings {
        while stack
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            let (_, node) = stack.pop().expect("stack");
            if let Some((_, parent)) = stack.last_mut() {
                parent.children.push(node);
            } else {
                roots.push(node);
            }
        }
        stack.push((
            heading.level,
            TreeItem::new(heading.line.to_string(), heading.title.clone()).expanded(true),
        ));
    }
    while let Some((_, node)) = stack.pop() {
        if let Some((_, parent)) = stack.last_mut() {
            parent.children.push(node);
        } else {
            roots.push(node);
        }
    }
    roots
}

/// Decode a screencast frame into what `img()` wants. gpui's own image
/// loader does the same channel swap for a plain decoded raster (see
/// `elements/img.rs`) — BGRA, straight alpha, no premultiply/divide, that's
/// only needed for the SVG path.
fn render_image_from_rgba(frame: &ScreencastFrame) -> Arc<RenderImage> {
    let mut buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.to_vec())
        .expect("screencast frame dimensions match its own buffer length");
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Map a window-relative position to CDP viewport space (AGE-156), honoring
/// the same "contain" letterbox math the rendered `img()` uses — the frame
/// is displayed at `w_full()` with `object_fit(Contain)`, so the actual
/// image occupies a centered sub-rect of the container whenever the
/// container's aspect ratio doesn't match the capture's. `None` for a
/// position that lands in the letterbox padding rather than the image.
///
/// `viewport` is whatever the CDP screencast is actually sized to right
/// now — `browser_requested_size`, kept in sync with the panel's real
/// dimensions by `sync_browser_viewport_size` — not a fixed constant, so
/// this stays correct as the artifact panel resizes.
///
/// Both `gpui::Pixels` and CDP's `x`/`y` are already device-independent
/// ("CSS") pixels — the screencast is started with `device_scale_factor:
/// 1.0` — so nothing here needs to know the display's DPI scale factor.
fn browser_viewport_position(
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    viewport: (u32, u32),
) -> Option<(f64, f64)> {
    let (bx, by) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if bw <= 0.0 || bh <= 0.0 {
        return None;
    }
    let (vw, vh) = (viewport.0 as f32, viewport.1 as f32);
    if vw <= 0.0 || vh <= 0.0 {
        return None;
    }
    let scale = (bw / vw).min(bh / vh);
    if scale <= 0.0 {
        return None;
    }
    let (dw, dh) = (vw * scale, vh * scale);
    let (ox, oy) = ((bw - dw) / 2.0, (bh - dh) / 2.0);
    let local_x = f32::from(position.x) - bx - ox;
    let local_y = f32::from(position.y) - by - oy;
    if local_x < 0.0 || local_y < 0.0 || local_x > dw || local_y > dh {
        return None;
    }
    Some(((local_x / scale) as f64, (local_y / scale) as f64))
}

fn browser_modifiers(modifiers: Modifiers) -> InputModifiers {
    InputModifiers {
        alt: modifiers.alt,
        ctrl: modifiers.control,
        meta: modifiers.platform,
        shift: modifiers.shift,
    }
}

fn browser_mouse_button(button: MouseButton) -> Option<MouseButtonKind> {
    match button {
        MouseButton::Left => Some(MouseButtonKind::Left),
        MouseButton::Right => Some(MouseButtonKind::Right),
        MouseButton::Middle => Some(MouseButtonKind::Middle),
        MouseButton::Navigate(_) => None,
    }
}

/// CDP wants `deltaX`/`deltaY` in CSS pixels. A precise trackpad delta
/// converts directly; a coarse line delta gets a standard 16px/line
/// estimate — an approximation, not a real line-height lookup, but one
/// wheel ticks and trackpads both land close enough to feel right.
fn browser_wheel_delta(delta: ScrollDelta) -> (f64, f64) {
    match delta {
        ScrollDelta::Pixels(point) => {
            (f64::from(f32::from(point.x)), f64::from(f32::from(point.y)))
        }
        ScrollDelta::Lines(point) => (f64::from(point.x) * 16.0, f64::from(point.y) * 16.0),
    }
}

/// Best-effort scheme completion for what the user typed into the address
/// bar (AGE-156) — a bare `example.com` becomes `https://example.com`,
/// matching ordinary browser omnibox behavior. Already-schemed URLs
/// (`http://`, `https://`, `file://`, …) pass through unchanged.
fn normalize_address_bar_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.contains("://") {
        return trimmed.to_string();
    }
    format!("https://{trimmed}")
}

#[allow(clippy::too_many_arguments)] // Rendering function threading per-frame view state
fn browser_rendered_body(
    browser: &BrowserPreview,
    control: ControlHolder,
    frame_bounds: Rc<RefCell<Bounds<Pixels>>>,
    viewport: (u32, u32),
    focus: FocusHandle,
    address: &Entity<InputState>,
    entity: Entity<ArtifactView>,
    cx: &App,
) -> AnyElement {
    // AGE-156: lets the user navigate directly — press Enter or click Go,
    // same as any other browser's omnibox. Enter is wired in
    // `ArtifactView::new` (a `PressEnter` subscription on this same
    // `InputState`); Go reads the same field here and calls the same method.
    let address_bar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(Input::new(address).small().flex_1())
        .child(
            Button::new("artifact-browser-refresh")
                .ghost()
                .small()
                .icon(Icon::new(CustomIcon::Refresh).size_3())
                .tooltip("Refresh")
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| this.reload_browser(cx));
                    }
                }),
        )
        .child(
            Button::new("artifact-browser-go")
                .ghost()
                .small()
                .icon(Icon::new(IconName::ArrowRight).size_3())
                .tooltip("Go")
                .on_click({
                    let entity = entity.clone();
                    let address = address.clone();
                    move |_, _, cx| {
                        let url = address.read(cx).value().to_string();
                        entity.update(cx, |this, cx| this.navigate_browser_as_user(url, cx));
                    }
                }),
        )
        .into_any_element();

    let control_bar = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(match control {
                    ControlHolder::Agent => "Agent is driving",
                    ControlHolder::User => "You're driving",
                }),
        )
        .child(match control {
            ControlHolder::Agent => Button::new("artifact-browser-take-control")
                .ghost()
                .small()
                .label("Take control")
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| this.take_browser_control(cx));
                    }
                })
                .into_any_element(),
            ControlHolder::User => Button::new("artifact-browser-release-control")
                .ghost()
                .small()
                .label("Release control")
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| this.release_browser_control(cx));
                    }
                })
                .into_any_element(),
        })
        .into_any_element();

    let frame = match browser {
        BrowserPreview::Idle | BrowserPreview::Starting => div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Starting browser…")
            .into_any_element(),
        BrowserPreview::Error(message) => div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        BrowserPreview::Frame(image) => {
            let bounds_for_prepaint = frame_bounds.clone();
            let mut container = div()
                .id("artifact-browser-frame")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .child(
                    canvas(
                        move |bounds, _window, _cx| {
                            *bounds_for_prepaint.borrow_mut() = bounds;
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    img(image.clone())
                        .w_full()
                        .h_full()
                        .object_fit(ObjectFit::Contain)
                        .rounded_md(),
                );

            // Input forwarding (AGE-156) only listens while the user holds
            // control — with the agent driving, the frame behaves like a
            // plain image and never steals the mouse or the keyboard.
            if control == ControlHolder::User {
                container = container
                    .track_focus(&focus)
                    .on_mouse_down(MouseButton::Left, {
                        let entity = entity.clone();
                        let bounds = frame_bounds.clone();
                        let focus = focus.clone();
                        move |event, window, cx| {
                            window.focus(&focus);
                            let Some((x, y)) = browser_viewport_position(
                                *bounds.borrow(),
                                event.position,
                                viewport,
                            ) else {
                                return;
                            };
                            let Some(button) = browser_mouse_button(event.button) else {
                                return;
                            };
                            let modifiers = browser_modifiers(event.modifiers);
                            let click_count = event.click_count as i64;
                            entity.update(cx, |this, _cx| {
                                this.send_browser_mouse(MouseInput {
                                    action: MouseAction::Down {
                                        button,
                                        click_count,
                                    },
                                    x,
                                    y,
                                    modifiers,
                                });
                            });
                        }
                    })
                    .on_mouse_up(MouseButton::Left, {
                        let entity = entity.clone();
                        let bounds = frame_bounds.clone();
                        move |event, _window, cx| {
                            let Some((x, y)) = browser_viewport_position(
                                *bounds.borrow(),
                                event.position,
                                viewport,
                            ) else {
                                return;
                            };
                            let Some(button) = browser_mouse_button(event.button) else {
                                return;
                            };
                            let modifiers = browser_modifiers(event.modifiers);
                            let click_count = event.click_count as i64;
                            entity.update(cx, |this, _cx| {
                                this.send_browser_mouse(MouseInput {
                                    action: MouseAction::Up {
                                        button,
                                        click_count,
                                    },
                                    x,
                                    y,
                                    modifiers,
                                });
                            });
                        }
                    })
                    .on_mouse_move({
                        let entity = entity.clone();
                        let bounds = frame_bounds.clone();
                        move |event, _window, cx| {
                            let Some((x, y)) = browser_viewport_position(
                                *bounds.borrow(),
                                event.position,
                                viewport,
                            ) else {
                                return;
                            };
                            let modifiers = browser_modifiers(event.modifiers);
                            entity.update(cx, |this, _cx| {
                                this.send_browser_mouse(MouseInput {
                                    action: MouseAction::Move,
                                    x,
                                    y,
                                    modifiers,
                                });
                            });
                        }
                    })
                    .on_scroll_wheel({
                        let entity = entity.clone();
                        let bounds = frame_bounds.clone();
                        move |event, _window, cx| {
                            let Some((x, y)) = browser_viewport_position(
                                *bounds.borrow(),
                                event.position,
                                viewport,
                            ) else {
                                return;
                            };
                            let modifiers = browser_modifiers(event.modifiers);
                            let (delta_x, delta_y) = browser_wheel_delta(event.delta);
                            entity.update(cx, |this, _cx| {
                                this.send_browser_mouse(MouseInput {
                                    action: MouseAction::Wheel { delta_x, delta_y },
                                    x,
                                    y,
                                    modifiers,
                                });
                            });
                        }
                    })
                    .on_key_down({
                        let entity = entity.clone();
                        move |event, _window, cx| {
                            let modifiers = browser_modifiers(event.keystroke.modifiers);
                            let input = if !modifiers.ctrl
                                && !modifiers.meta
                                && !modifiers.alt
                                && let Some(text) = event.keystroke.key_char.clone()
                            {
                                KeyInput::Text(text)
                            } else {
                                KeyInput::Special {
                                    name: event.keystroke.key.clone(),
                                    modifiers,
                                }
                            };
                            entity.update(cx, |this, _cx| {
                                this.send_browser_key(input);
                            });
                        }
                    });
            }

            container.into_any_element()
        }
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap_1()
        .child(address_bar)
        .child(control_bar)
        .child(frame)
        .into_any_element()
}

fn tabular_rendered_body(tabular: &TabularPreview, cx: &App) -> AnyElement {
    match tabular {
        TabularPreview::Idle | TabularPreview::Loading => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Loading preview…")
            .into_any_element(),
        TabularPreview::Error(message) => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        TabularPreview::Ready(preview) => {
            render_table_preview_view("artifact-table", preview, cx).into_any_element()
        }
    }
}

fn image_rendered_body(path: &Path, cx: &App) -> AnyElement {
    if !path.exists() {
        return div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Image not found: {}", path.display()))
            .into_any_element();
    }
    div()
        .id("artifact-image")
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .child(
            img(path.to_path_buf())
                .max_w(px(IMAGE_DISPLAY_WIDTH))
                .max_h(px(520.0))
                .object_fit(ObjectFit::Contain)
                .rounded_md(),
        )
        .into_any_element()
}

fn pdf_rendered_body(pdf: &PdfPreview, entity: Entity<ArtifactView>, cx: &App) -> AnyElement {
    match pdf {
        PdfPreview::Idle | PdfPreview::Loading { .. } => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Rendering page…")
            .into_any_element(),
        PdfPreview::Error(message) => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        PdfPreview::Ready {
            page, total, image, ..
        } => {
            let page = *page;
            let total = *total;
            let can_prev = page > 0;
            let can_next = page + 1 < total;
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .justify_start()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("artifact-pdf-prev")
                                .ghost()
                                .small()
                                .label("Prev")
                                .disabled(!can_prev)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity.update(cx, |this, cx| this.turn_pdf_page(false, cx));
                                    }
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Page {} of {}", page + 1, total.max(1))),
                        )
                        .child(
                            Button::new("artifact-pdf-next")
                                .ghost()
                                .small()
                                .label("Next")
                                .disabled(!can_next)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity.update(cx, |this, cx| this.turn_pdf_page(true, cx));
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .id("artifact-pdf-pages")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .child(
                            img(image.clone())
                                .w(px(PDF_PAGE_DISPLAY_WIDTH))
                                .object_fit(ObjectFit::Fill)
                                .rounded_md(),
                        ),
                )
                .into_any_element()
        }
    }
}

fn document_text_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(1.15))
        .heading_font_size(|level, base| match level {
            1 => base * 1.8,
            2 => base * 1.45,
            3 => base * 1.2,
            _ => base,
        })
}

fn artifact_rendered_markdown(
    rendered: &str,
    full: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let view = TextView::markdown("artifact-md", rendered.to_string(), window, cx)
        .style(document_text_style())
        .selectable(true);
    let inner = div()
        .w_full()
        .when(full, |this| this.max_w(px(DOCUMENT_MEASURE_PX)).mx_auto())
        .line_height(relative(1.7))
        .child(view);
    div()
        .id("artifact-rendered")
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .p_3()
        .child(inner)
        .into_any_element()
}

fn artifact_primary_body(
    path: Option<&PathBuf>,
    rendered: &str,
    editor: &Entity<InputState>,
    full: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match path {
        Some(path) if is_code_artifact_path(path) => artifact_source_input(editor),
        Some(path) if is_markdown_artifact_path(path) => {
            artifact_rendered_markdown(rendered, full, window, cx)
        }
        _ => artifact_source_input(editor),
    }
}

fn artifact_source_input(editor: &Entity<InputState>) -> AnyElement {
    // Match gpui-component inspector: v_flex().flex_1() parent + Input::h_full().
    // The panel body slot must also be flex-col (see render) or flex_1 never resolves.
    v_flex()
        .id("artifact-source")
        .flex_1()
        .min_h_0()
        .h_full()
        .w_full()
        .child(Input::new(editor).h_full().w_full().appearance(true))
        .into_any_element()
}

impl Render for ArtifactView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mode == ArtifactMode::Closed {
            return div().into_any_element();
        }
        let session_review = self.session_review;
        if !session_review {
            self.refresh_staleness();
            self.sync_editor(window, cx);
            self.sync_outline(cx);
            self.sync_browser_address(window, cx);
            self.sync_browser_viewport_size(cx);
        }

        let tab = self.tab;
        let source = self.source.clone();
        let rendered = self.rendered.clone();
        let old = self.old.clone();
        let full = self.mode == ArtifactMode::Full;
        let entity = cx.entity();
        let is_browser = self.browser_manager.is_some();
        let is_pdf = !is_browser && self.path.as_ref().is_some_and(|path| is_pdf_path(path));
        let is_chart = !is_browser && self.chart.is_some();
        let is_image = !is_chart && self.path.as_ref().is_some_and(|path| is_image_path(path));
        let is_tabular = matches!(self.tabular, TabularPreview::Ready(_))
            || self.path.as_ref().is_some_and(|path| is_tabular_path(path));
        let path_ref = self.path.clone();
        let pdf = self.pdf.clone();
        let tabular = self.tabular.clone();
        let chart = self.chart.clone();
        let browser = self.browser.clone();
        let browser_control = self.browser_control;
        let browser_frame_bounds = self.browser_frame_bounds.clone();
        let browser_viewport = self.browser_requested_size;
        let browser_focus = self.browser_focus.clone();
        let browser_address = self.browser_address.clone();
        let has_diff = !old.is_empty() && old != source;
        let visible_tab = if !has_diff && tab > 1 {
            0
        } else if !has_diff && tab > 0 {
            tab.min(1)
        } else {
            tab
        };
        let primary_label = if is_tabular {
            "Table"
        } else {
            path_ref
                .as_ref()
                .map(|path| {
                    if is_code_artifact_path(path) {
                        "Code"
                    } else if is_markdown_artifact_path(path) {
                        "Rendered"
                    } else {
                        "Preview"
                    }
                })
                .unwrap_or("Preview")
        };
        let show_outline =
            full && !self.headings.is_empty() && !is_pdf && !is_image && !is_chart && !is_browser;
        let editor = self.editor.clone();
        let outline = self.outline.clone();
        let run_visible = self.run_visible || self.pending_approval;
        let pin_kind = if self.pending_approval {
            RunPinKind::PendingApproval
        } else {
            RunPinKind::JumpToLatest
        };

        let body = if session_review {
            SessionReviewPanel::new(
                self.files.len(),
                self.review_total_added,
                self.review_total_removed,
                self.review_layout_gen,
                entity.clone(),
                self.review_scroll.clone(),
            )
            .into_any_element()
        } else if is_browser {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .child(browser_rendered_body(
                    &browser,
                    browser_control,
                    browser_frame_bounds,
                    browser_viewport,
                    browser_focus,
                    &browser_address,
                    entity.clone(),
                    cx,
                ))
                .into_any_element()
        } else if is_pdf {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .child(pdf_rendered_body(&pdf, entity.clone(), cx))
                .into_any_element()
        } else if let Some(spec) = chart {
            div()
                .id("artifact-chart")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .overflow_y_scroll()
                .child(render_chart_panel(spec, cx))
                .into_any_element()
        } else if is_image {
            let image_path = path_ref.clone().unwrap_or_default();
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .child(image_rendered_body(&image_path, cx))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .h_full()
                .w_full()
                .child(
                    TabBar::new("artifact-modes")
                        .segmented()
                        .child(Tab::new().label(primary_label))
                        .child(Tab::new().label("Source"))
                        .when(has_diff, |this| this.child(Tab::new().label("Diff")))
                        .selected_index(visible_tab)
                        .on_click({
                            let entity = entity.clone();
                            move |ix, window, cx| {
                                entity.update(cx, |this, cx| {
                                    let next = if has_diff { *ix } else { (*ix).min(1) };
                                    this.select_tab(next, window, cx);
                                });
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h_0()
                        .h_full()
                        .w_full()
                        .when(show_outline, |this| {
                            this.child(
                                div()
                                    .w(px(OUTLINE_WIDTH))
                                    .h_full()
                                    .min_h_0()
                                    .border_r_1()
                                    .border_color(cx.theme().border)
                                    .p_1()
                                    .child(tree(&outline, |_ix, entry, _selected, _, cx| {
                                        ListItem::new(entry.item().id.clone()).child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .pl(px(8.) * entry.depth() as f32)
                                                .child(entry.item().label.clone()),
                                        )
                                    })),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_h_0()
                                .h_full()
                                .w_full()
                                .when(visible_tab == 0, |this| {
                                    if is_tabular {
                                        this.flex_1()
                                            .min_h_0()
                                            .p_2()
                                            .child(tabular_rendered_body(&tabular, cx))
                                    } else {
                                        this.flex_1().min_h_0().h_full().child(
                                            artifact_primary_body(
                                                path_ref.as_ref(),
                                                &rendered,
                                                &editor,
                                                full,
                                                window,
                                                cx,
                                            ),
                                        )
                                    }
                                })
                                .when(visible_tab == 1, |this| {
                                    this.flex_1()
                                        .min_h_0()
                                        .h_full()
                                        .child(artifact_source_input(&editor))
                                })
                                .when(has_diff && visible_tab == 2, |this| {
                                    this.p_2().child(
                                        div()
                                            .id("artifact-diff-scroll")
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scroll()
                                            .child(DiffHunkList::new("artifact-diff", old, source)),
                                    )
                                }),
                        ),
                )
                .into_any_element()
        };

        let title = if session_review {
            format!("Review · {} files", self.files.len())
        } else if is_browser {
            "Browser".to_string()
        } else {
            self.chart
                .as_ref()
                .and_then(|spec| spec.title.clone())
                .or_else(|| self.path.as_ref().map(|p| artifact_panel_title(p)))
                .or_else(|| {
                    if let TabularPreview::Ready(preview) = &self.tabular {
                        Some(preview.title.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    if self.chart.is_some() {
                        "Chart".to_string()
                    } else {
                        "Document".to_string()
                    }
                })
        };
        let format_token = self
            .path
            .as_ref()
            .map(|p| artifact_format_token(p))
            .unwrap_or_default();
        let selected_file_index = self
            .path
            .as_ref()
            .and_then(|active| self.files.iter().position(|(path, _, _)| path == active))
            .unwrap_or(0);
        let file_tab_bar = (!session_review && self.files.len() > 1).then(|| {
            let files = self.files.clone();
            TabBar::new("artifact-files")
                .small()
                .menu(true)
                .selected_index(selected_file_index)
                .on_click({
                    let entity = entity.clone();
                    move |ix, _, cx| {
                        entity.update(cx, |this, cx| {
                            let Some((path, source, old)) = this.files.get(*ix).cloned() else {
                                return;
                            };
                            if this.path.as_ref() == Some(&path) {
                                return;
                            }
                            let workspace = this.workspace_root.clone();
                            this.open(path, source, old, workspace, cx);
                        });
                    }
                })
                .children(files.iter().map(|(path, _, _)| {
                    let label = path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    Tab::new().label(label)
                }))
        });

        let expand_icon = if full {
            IconName::Minimize
        } else {
            IconName::Maximize
        };
        let expand_tooltip = if full { "Collapse" } else { "Full window" };

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family("monospace")
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(
                        !session_review
                            && !format_token.is_empty()
                            && (is_pdf || is_chart || is_image),
                        |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format_token),
                            )
                        },
                    )
                    .when(!session_review, |this| {
                        this.child(
                            DropdownButton::new("artifact-copy")
                                .small()
                                .ghost()
                                .button(
                                    Button::new("artifact-copy-main")
                                        .ghost()
                                        .small()
                                        .label("Copy")
                                        .on_click({
                                            let entity = entity.clone();
                                            move |_, _, cx| {
                                                entity.update(cx, |this, cx| {
                                                    this.copy_kind("plain", cx);
                                                });
                                            }
                                        }),
                                )
                                .dropdown_menu({
                                    let entity = entity.clone();
                                    move |menu, _, _| {
                                        menu.item(PopupMenuItem::new("Markdown").on_click({
                                            let entity = entity.clone();
                                            move |_, _, cx| {
                                                entity.update(cx, |this, cx| {
                                                    this.copy_kind("markdown", cx);
                                                });
                                            }
                                        }))
                                        .item(PopupMenuItem::new("Rendered").on_click({
                                            let entity = entity.clone();
                                            move |_, _, cx| {
                                                entity.update(cx, |this, cx| {
                                                    this.copy_kind("rendered", cx);
                                                });
                                            }
                                        }))
                                        .item(
                                            PopupMenuItem::new("Plain").on_click({
                                                let entity = entity.clone();
                                                move |_, _, cx| {
                                                    entity.update(cx, |this, cx| {
                                                        this.copy_kind("plain", cx);
                                                    });
                                                }
                                            }),
                                        )
                                    }
                                }),
                        )
                    })
                    .when(!session_review, |this| {
                        this.when_some(self.path.clone(), |this, path| {
                            this.child(
                                Button::new("artifact-reveal")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::ExternalLink).size_3())
                                    .tooltip("Reveal")
                                    .on_click(move |_, _, cx| {
                                        reveal_path_in_os(&path, cx);
                                    }),
                            )
                        })
                    })
                    .child(
                        Button::new("artifact-expand")
                            .ghost()
                            .small()
                            .icon(Icon::new(expand_icon).size_3())
                            .tooltip(expand_tooltip)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_full(cx);
                            })),
                    )
                    .child(
                        Button::new("artifact-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Close).size_3())
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_panel(cx);
                            })),
                    ),
            )
            .when_some(file_tab_bar, |this, tabs| this.child(tabs));

        let stale_banner = self.stale.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .child(
                    Alert::warning("artifact-stale", "This file changed on disk.")
                        .banner()
                        .flex_1(),
                )
                .child(
                    Button::new("artifact-reload")
                        .small()
                        .primary()
                        .label("Reload")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reload_from_disk(cx);
                        })),
                )
        });

        let panel = div()
            .id("artifact-view")
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(280.))
            .w_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .on_key_down({
                let entity = entity.clone();
                move |event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        entity.update(cx, |this, cx| this.close_panel(cx));
                        cx.stop_propagation();
                    }
                }
            })
            .child(header)
            .when_some(stale_banner, |this, banner| this.child(banner))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .w_full()
                    .relative()
                    .child(body)
                    .child(RunPin::new(pin_kind).visible(full && run_visible)),
            );

        if full {
            div().size_full().child(panel).into_any_element()
        } else {
            panel.into_any_element()
        }
    }
}

impl EventEmitter<ArtifactViewEvent> for ArtifactView {}

pub fn new_artifact_view(window: &mut Window, cx: &mut App) -> Entity<ArtifactView> {
    cx.new(|cx| ArtifactView::new(window, cx))
}

#[cfg(test)]
mod address_bar_tests {
    use super::normalize_address_bar_url;

    #[test]
    fn adds_https_to_bare_host() {
        assert_eq!(
            normalize_address_bar_url("example.com"),
            "https://example.com"
        );
        assert_eq!(
            normalize_address_bar_url("localhost:3000"),
            "https://localhost:3000"
        );
    }

    #[test]
    fn leaves_schemed_urls_alone() {
        assert_eq!(
            normalize_address_bar_url("http://localhost:3000/"),
            "http://localhost:3000/"
        );
        assert_eq!(
            normalize_address_bar_url("https://example.com/page"),
            "https://example.com/page"
        );
        assert_eq!(
            normalize_address_bar_url("file:///tmp/index.html"),
            "file:///tmp/index.html"
        );
    }

    #[test]
    fn trims_whitespace_and_handles_empty() {
        assert_eq!(
            normalize_address_bar_url("  example.com  "),
            "https://example.com"
        );
        assert_eq!(normalize_address_bar_url("   "), "");
        assert_eq!(normalize_address_bar_url(""), "");
    }
}
