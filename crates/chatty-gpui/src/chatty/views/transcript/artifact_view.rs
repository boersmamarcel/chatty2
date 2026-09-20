use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::assets::CustomIcon;
use chatty_core::services::browser::{
    BrowserManager, BrowserSession, BrowserTab, ControlHolder, InputModifiers, KeyInput,
    MouseAction, MouseButtonKind, MouseInput, ScreencastFrame, ScreencastUpdate,
};
use chatty_core::services::pdf_thumbnail::{
    PREVIEW_WIDTH, PdfThumbnailError, pdf_page_count, render_pdf_page,
};
use chatty_core::tools::chart_tool::ChartSpec;
use chatty_core::tools::data_query_tool::{
    FILE_PREVIEW_MAX_ROWS, TablePreview, load_file_table_preview,
};
use chatty_core::tools::pptx_tool::{PptxSlide, pptx_slides_to_text, read_pptx_slides};
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
use super::artifact_header::{
    ArtifactCopy, ArtifactCopyKind, ArtifactHeaderKind, artifact_copy_control, artifact_header_tabs,
};
use super::artifact_kind::{
    ArtifactHeading, ArtifactVersion, ViewAnchor, artifact_format_token,
    artifact_language_for_path, artifact_panel_title, artifact_version, block_index_from_anchor,
    is_code_artifact_path, is_image_path, is_markdown_artifact_path, is_pdf_path, is_pptx_path,
    is_tabular_path, markdown_headings, read_artifact_source, source_line_from_anchor,
};
use super::diff::DiffHunkList;
use super::run_pin::{RunPin, RunPinKind};
use super::session_review_panel::{ReviewFileSection, SessionReviewPanel};
use super::table::render_table_preview_view;
use crate::chatty::views::chart_renderer::render_chart_panel;
use crate::chatty::views::diff_view_component::diff_line_stats_fast;

const IMAGE_DISPLAY_WIDTH: f32 = 348.0;
/// PDF page rasters are requested in steps of this many device pixels
/// (AGE-472), so a split drag settles on one pdfium render rather than one
/// per pixel of travel; `PDF_RASTER_MAX_WIDTH` keeps a full-window page on a
/// HiDPI display a sane atlas upload.
const PDF_RASTER_STEP: u32 = 256;
const PDF_RASTER_MAX_WIDTH: u32 = 2560;
/// How long a PDF panel has to hold a new width before the page is
/// re-rasterised at it — same reasoning as the browser retarget debounce
/// in `sync_browser_viewport_size`.
const PDF_RERASTER_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
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
    /// The browser changed hands (AGE-156): the user took control
    /// (`taken`) or handed it back, while the page showed `url`. Emitted
    /// only on a real transition, never when the lock was already held by
    /// the new holder, so the activity trail records each handoff once.
    BrowserControlChanged {
        taken: bool,
        url: String,
    },
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
        /// Decoded off the main thread and painted by hand from a `canvas()`
        /// (AGE-472) rather than through `img(path)`: swapping in a sharper
        /// raster is then atomic — no frame where the new file is still
        /// loading and the page is blank — and the panel width decides the
        /// on-screen size, not the raster.
        image: Arc<RenderImage>,
        /// Device-pixel width the raster was rendered at; compared against
        /// `ArtifactView::pdf_raster_width` to know when to re-render.
        raster_width: u32,
    },
    Error(String),
}

/// Slide workbench state (AGE-138), next to [`PdfPreview`] rather than a
/// second entity: `ArtifactView` stays one entity with three slots.
///
/// The whole deck is held once, because a deck's extracted text is small and
/// the parse is a single ZIP walk. Paging is then an index change, not the
/// re-raster `PdfPreview` needs per page.
#[derive(Clone, Debug, Default)]
enum PptxPreview {
    #[default]
    Idle,
    Loading,
    Ready {
        slide: usize,
        slides: Arc<Vec<PptxSlide>>,
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
    pptx: PptxPreview,
    tabular: TabularPreview,
    chart: Option<ChartSpec>,
    browser: BrowserPreview,
    /// Set while a browser is open, even paused behind a file (AGE-473):
    /// `pause_browser` keeps it so the browser can be brought back, and
    /// `drop_browser` takes it and stops the session (AGE-155).
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
    /// The session's open tabs (AGE-473), mirrored from
    /// `BrowserSession::watch_tabs` by the tab watcher, the same way the
    /// address bar follows `watch_url`. Rendered as entries in the header's
    /// `artifact-files` tab bar next to the open files; the session is the
    /// source of truth. Kept alive across a pause so the entries stay while
    /// a file is shown in front of the browser.
    browser_tabs: Vec<BrowserTab>,
    /// Lifecycle of the tab watcher (AGE-473), separate from `load_gen`
    /// because the watcher outlives a pause while the screencast does not.
    browser_tabs_gen: u64,
    /// Whether the browser is the artifact on screen right now (AGE-473).
    /// `browser_manager`/`browser_session` can be `Some` while a file is
    /// shown in front of a paused browser, so this — not their presence —
    /// is what `render` keys "the browser is showing" off.
    browser_shown: bool,
    /// CDP viewport size (AGE-156) — kept matched to the panel's actual
    /// rendered size by `sync_browser_viewport_size` rather than staying
    /// fixed at `BROWSER_VIEWPORT_WIDTH`/`HEIGHT` for the session's whole
    /// life, so expanding the artifact window shows more of the real page
    /// instead of just scaling a static-size capture up. `(0, 0)` before
    /// the first screencast frame's bounds are known.
    browser_requested_size: (u32, u32),
    /// Geometry of the frame currently on screen (AGE-379): the raster's
    /// pixel size and the CSS viewport it shows, from the frame's own
    /// metadata. Click mapping goes through this rather than through
    /// `browser_requested_size`, which is only what the viewport was last
    /// *asked* to be — during the resize debounce, after a refused retarget,
    /// or when Chrome downscales the raster, the two disagree and every
    /// click would land off-target.
    browser_frame_geometry: Option<FrameGeometry>,
    /// The in-flight debounced CDP retarget, if any (AGE-156). Replacing
    /// this drops (and so cancels, per GPUI's `Task`) whatever retarget
    /// was previously scheduled — see `sync_browser_viewport_size`.
    browser_resize_task: Option<Task<()>>,
    /// Width the PDF page is laid out at (AGE-472): the page scroll
    /// container's width, recorded by its `canvas()` prepaint each frame
    /// the way `browser_frame_bounds` is. Zero until the first paint; the
    /// page follows the window and the chat/artifact split through it.
    pdf_panel_width: Rc<RefCell<Pixels>>,
    /// Device-pixel width the next `render_pdf_page` call asks for — the
    /// bucketed panel width, kept current by `sync_pdf_raster_width`, so a
    /// wide panel gets a sharp raster instead of an upscaled preview.
    pdf_raster_width: u32,
    /// The in-flight debounced re-raster, if any; dropping it cancels the
    /// superseded one, exactly like `browser_resize_task`.
    pdf_reraster_task: Option<Task<()>>,
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
            pptx: PptxPreview::Idle,
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
            browser_tabs: Vec::new(),
            browser_tabs_gen: 0,
            browser_shown: false,
            browser_requested_size: (0, 0),
            browser_frame_geometry: None,
            browser_resize_task: None,
            pdf_panel_width: Rc::new(RefCell::new(Pixels::ZERO)),
            pdf_raster_width: PREVIEW_WIDTH,
            pdf_reraster_task: None,
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
        self.pause_browser(cx);
        let next_path = match &preview.source {
            chatty_core::tools::data_query_tool::TableSource::File { path } => {
                Some(PathBuf::from(path))
            }
            _ => None,
        };
        self.mode = presentation_on_open(self.mode, next_path.as_ref() == self.path.as_ref());
        self.path = next_path;
        self.tabular = TabularPreview::Ready(preview);
        self.set_pdf(PdfPreview::Idle, cx);
        self.pptx = PptxPreview::Idle;
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
        self.pause_browser(cx);
        let next_path = spec.saved_path.as_ref().map(PathBuf::from);
        self.mode = presentation_on_open(self.mode, next_path.as_ref() == self.path.as_ref());
        self.path = next_path;
        self.chart = Some(spec);
        self.set_pdf(PdfPreview::Idle, cx);
        self.pptx = PptxPreview::Idle;
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
    ///
    /// Called for the first tool call (a fresh manager), and again to bring
    /// the browser back to the screen after a file was shown in front of it
    /// (AGE-473): the same manager, its session and tab list still alive,
    /// only the screencast having been paused. The latter case restarts the
    /// stream on the cached session rather than launching anything — unless
    /// the pause landed while the session was still being resolved, in which
    /// case the resolve is issued again (see [`browser_reopen`]).
    pub fn open_browser(&mut self, manager: Arc<BrowserManager>, cx: &mut Context<Self>) {
        self.session_review = false;
        self.review_sections.clear();
        let already_open = self
            .browser_manager
            .as_ref()
            .is_some_and(|existing| Arc::ptr_eq(existing, &manager));
        self.mode = presentation_on_open(self.mode, already_open);
        self.path = None;
        self.set_pdf(PdfPreview::Idle, cx);
        self.pptx = PptxPreview::Idle;
        self.tabular = TabularPreview::Idle;
        self.chart = None;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.tab = 0;
        self.stale = false;
        self.headings.clear();
        cx.emit(ArtifactViewEvent::PresentationChanged);

        match browser_reopen(
            already_open,
            self.browser_shown,
            self.browser_session.is_some(),
        ) {
            // Same manager, already on screen: nothing to do but re-present.
            BrowserReopen::Present => {}
            // Paused behind a file (AGE-473): restart the cast on the
            // still-live session.
            BrowserReopen::ResumeStream => {
                self.browser_shown = true;
                if let Some(session) = self.browser_session.clone() {
                    self.start_browser_stream(session, cx);
                }
            }
            // Paused before the session ever resolved — the resolve was
            // dropped at its `load_gen` guard, so issue it again; doing
            // nothing would leave the panel on "Starting…" for good.
            BrowserReopen::ResolveSession => {
                self.browser_shown = true;
                self.start_browser_session(manager, cx);
            }
            // A different manager: tear the previous browser down for good.
            BrowserReopen::Fresh => {
                self.drop_browser(cx);
                self.browser_manager = Some(manager.clone());
                self.browser_shown = true;
                self.start_browser_session(manager, cx);
            }
        }
        cx.notify();
    }

    /// Resolve the manager's session (launching Chrome on the first call)
    /// and, once it is there, start the tab watcher and the stream. Keyed on
    /// `load_gen`: a pause or a different browser while the resolve is in
    /// flight drops the result, and the next `open_browser` resolves again.
    fn start_browser_session(&mut self, manager: Arc<BrowserManager>, cx: &mut Context<Self>) {
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
            this.update(cx, |this, cx| {
                // A pause or a different browser may have superseded us while
                // the session was being resolved.
                if this.load_gen != load_id {
                    return;
                }
                this.browser_session = Some(session.clone());
                // The tab list outlives a pause; the screencast does not.
                this.start_tab_watcher(session.clone(), cx);
                this.start_browser_stream(session, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Follow the session's tab list into `browser_tabs` (AGE-473). Keyed on
    /// its own generation, not `load_gen`: it must survive a pause (a file
    /// shown in front of the browser) so the tab entries stay live, and is
    /// torn down only by [`Self::drop_browser`].
    fn start_tab_watcher(&mut self, session: Arc<BrowserSession>, cx: &mut Context<Self>) {
        self.browser_tabs_gen = self.browser_tabs_gen.wrapping_add(1);
        let tabs_gen = self.browser_tabs_gen;
        let mut tabs_rx = session.watch_tabs();
        self.browser_tabs = tabs_rx.borrow_and_update().clone();
        cx.spawn(async move |this, cx| {
            loop {
                if tabs_rx.changed().await.is_err() {
                    return;
                }
                let tabs = tabs_rx.borrow_and_update().clone();
                let alive = this
                    .update(cx, |this, cx| {
                        if this.browser_tabs_gen == tabs_gen {
                            this.browser_tabs = tabs;
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
        cx.notify();
    }

    /// Stand up the screencast and the input/address forwarding for a live
    /// session (AGE-156). Used both for a fresh browser and to resume one
    /// that was paused behind a file (AGE-473); the tab watcher is started
    /// separately because it outlives a pause.
    fn start_browser_stream(&mut self, session: Arc<BrowserSession>, cx: &mut Context<Self>) {
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.browser = BrowserPreview::Starting;
        self.browser_control = session.control_holder();
        self.browser_requested_size = (BROWSER_VIEWPORT_WIDTH, BROWSER_VIEWPORT_HEIGHT);

        // AGE-156: ordered input-forwarding drains, one consumer task per
        // stream rather than one spawned task per event — mouse moves are
        // too frequent for that, and independent tasks racing the CDP
        // connection could deliver events out of order.
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
        self.browser_mouse_tx = Some(mouse_tx);
        self.browser_key_tx = Some(key_tx);

        // AGE-156: seed the address bar with the current URL, then keep it
        // live as either side navigates.
        {
            let mut url_rx = session.watch_url();
            self.browser_current_url = url_rx.borrow_and_update().clone();
            self.browser_address_dirty = true;
            cx.spawn(async move |this, cx| {
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

        cx.spawn(async move |this, cx| {
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
                    // The sender dropped — either the cast was torn down
                    // (`load_gen` will already have moved on, so the stale
                    // check below is what silences this) or the browser
                    // crashed out from under a still-active view.
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
                        let geometry = FrameGeometry::from(&frame);
                        (
                            BrowserPreview::Frame(render_image_from_rgba(&frame)),
                            Some(geometry),
                        )
                    }
                    ScreencastUpdate::Error(message) => (BrowserPreview::Error(message), None),
                };
                let (next, geometry) = next;
                let superseded = this
                    .update(cx, |this, cx| {
                        if this.load_gen != load_id {
                            return true;
                        }
                        this.browser = next;
                        if geometry.is_some() {
                            this.browser_frame_geometry = geometry;
                        }
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

    /// The browser leaves the screen but stays open (AGE-473): a file, table
    /// or chart is shown in front of it. Stop the screencast (AGE-155 — a
    /// cast nobody watches is pure CPU) and drop the on-screen state, but
    /// keep the manager, the session and the tab list alive so the browser's
    /// tab entries stay in the header bar and stay live, and bringing it back
    /// only has to restart the cast.
    fn pause_browser(&mut self, cx: &mut Context<Self>) {
        if !self.browser_shown && self.browser_mouse_tx.is_none() {
            return;
        }
        self.browser_shown = false;
        self.browser = BrowserPreview::Idle;
        // Bump so the frame/URL loops notice they are stale even if their
        // channel never fires `changed()` again (a static page sends no
        // further frames, so a loop would otherwise block forever).
        self.load_gen = self.load_gen.wrapping_add(1);
        self.browser_control = ControlHolder::Agent;
        self.browser_current_url.clear();
        self.browser_address_dirty = true;
        self.browser_requested_size = (0, 0);
        self.browser_frame_geometry = None;
        self.browser_resize_task = None;
        // Dropping the senders ends the drain loops (AGE-156).
        self.browser_mouse_tx = None;
        self.browser_key_tx = None;
        if let Some(manager) = self.browser_manager.clone() {
            cx.background_spawn(async move {
                manager.stop_screencast().await;
            })
            .detach();
        }
    }

    /// Tear the browser down for good: everything [`Self::pause_browser`]
    /// does, plus dropping the manager, the session and the tab list and
    /// ending the tab watcher. Used when the panel closes or a different
    /// artifact (a new browser, session review) takes over.
    fn drop_browser(&mut self, cx: &mut Context<Self>) {
        self.pause_browser(cx);
        self.browser_session = None;
        self.browser_tabs.clear();
        // Ends the tab watcher, which pause deliberately leaves running.
        self.browser_tabs_gen = self.browser_tabs_gen.wrapping_add(1);
        self.browser_manager = None;
    }

    /// immediately. A no-op if the browser artifact isn't open.
    pub fn take_browser_control(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.as_ref() else {
            return;
        };
        let previous = session.take_control();
        self.browser_control = ControlHolder::User;
        if previous != ControlHolder::User {
            cx.emit(ArtifactViewEvent::BrowserControlChanged {
                taken: true,
                url: self.browser_current_url.clone(),
            });
        }
        cx.notify();
    }

    /// Hand control back to the agent (AGE-156).
    pub fn release_browser_control(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.as_ref() else {
            return;
        };
        let previous = session.release_control();
        self.browser_control = ControlHolder::Agent;
        if previous == ControlHolder::User {
            cx.emit(ArtifactViewEvent::BrowserControlChanged {
                taken: false,
                url: self.browser_current_url.clone(),
            });
        }
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
        // Record the handoff here; `navigate_as_user` takes the lock again
        // but that is a no-op once the user already holds it.
        self.take_browser_control(cx);
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
        self.take_browser_control(cx);
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

    /// The user picks a tab in the strip (AGE-473) — takes control first,
    /// like the address bar does: the agent's tools now address that tab.
    /// A no-op if the browser artifact isn't open.
    fn select_browser_tab(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.clone() else {
            return;
        };
        self.take_browser_control(cx);
        // Foreground, like `reload_browser`: the main thread has the Tokio
        // runtime entered, gpui's worker threads do not.
        cx.spawn(async move |_, _| {
            if let Err(e) = session.select_tab(&id).await {
                warn!(error = %e, tab = %id, "browser: switching tabs failed");
            }
        })
        .detach();
    }

    /// The user closes a tab from the strip (AGE-473): the CDP target is
    /// closed, not just hidden. The session picks what to show next.
    fn close_browser_tab(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(session) = self.browser_session.clone() else {
            return;
        };
        self.take_browser_control(cx);
        cx.spawn(async move |_, _| {
            if let Err(e) = session.close_tab(&id).await {
                warn!(error = %e, tab = %id, "browser: closing the tab failed");
            }
        })
        .detach();
    }

    /// Route a click on a header tab-bar entry (AGE-473): a file entry opens
    /// (or re-presents) that file — which pauses the browser if it was
    /// showing — and a browser entry brings the browser back to the screen
    /// (if a file was in front of it) and switches to that tab unless it is
    /// already the active one.
    fn select_artifact_tab(&mut self, target: ArtifactTabTarget, cx: &mut Context<Self>) {
        match target {
            ArtifactTabTarget::File(ix) => {
                let Some((path, source, old)) = self.files.get(ix).cloned() else {
                    return;
                };
                // Already the file on screen and no browser in front of it:
                // nothing to do.
                if !self.browser_shown && self.path.as_ref() == Some(&path) {
                    return;
                }
                let workspace = self.workspace_root.clone();
                self.open(path, source, old, workspace, cx);
            }
            ArtifactTabTarget::Browser(id) => {
                if !self.browser_shown {
                    let Some(manager) = self.browser_manager.clone() else {
                        return;
                    };
                    // Resumes the paused browser onto its active tab.
                    self.open_browser(manager, cx);
                }
                let is_active = self
                    .browser_tabs
                    .iter()
                    .any(|tab| tab.id == id && tab.active);
                if !is_active {
                    self.select_browser_tab(id, cx);
                }
            }
        }
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
        self.pause_browser(cx);
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
            self.pptx = PptxPreview::Idle;
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.tab = 0;
            self.headings.clear();
            self.start_pdf_load(0, cx);
        } else if is_pptx_path(&path) {
            // A deck is binary: `source` arrived empty from
            // `read_artifact_source`, and both the slide pager and the Source
            // tab are filled by the parser once `start_pptx_load` returns.
            self.set_pdf(PdfPreview::Idle, cx);
            self.source.clear();
            self.rendered.clear();
            // Never diff a deck against binary. `old` only ever carries text a
            // diff/edit tool reported, which no PPTX tool does, so this keeps
            // the Diff tab hidden rather than showing ZIP bytes.
            self.old.clear();
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.tab = 0;
            self.headings.clear();
            self.start_pptx_load(cx);
        } else if is_tabular_path(&path) {
            self.set_pdf(PdfPreview::Idle, cx);
            self.pptx = PptxPreview::Idle;
            self.chart = None;
            self.source = source.clone();
            self.rendered = source.clone();
            self.old = old_snapshot.clone().unwrap_or_default();
            self.tab = 0;
            self.start_tabular_load(path, workspace_root, cx);
        } else if is_image_path(&path) {
            self.set_pdf(PdfPreview::Idle, cx);
            self.pptx = PptxPreview::Idle;
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.source.clear();
            self.rendered.clear();
            self.old.clear();
            self.tab = 0;
            self.headings.clear();
        } else {
            self.set_pdf(PdfPreview::Idle, cx);
            self.pptx = PptxPreview::Idle;
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
        self.drop_browser(cx);
        self.session_review = true;
        self.files = files;
        self.workspace_root = workspace_root;
        self.rebuild_review_sections(focus.as_ref(), cx);
        self.mode = presentation_on_open(self.mode, false);
        self.path = None;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.set_pdf(PdfPreview::Idle, cx);
        self.pptx = PptxPreview::Idle;
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
                self.drop_browser(cx);
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

    /// Copy one of the artifact's two possible payloads.
    ///
    /// There are exactly two: the file's source text, and its rendered text
    /// where those differ (markdown, tabular). The old three-item menu had
    /// "Markdown" and "Rendered" sharing a match arm and "Plain" duplicating
    /// the button it hung off, so for an HTML file all four controls copied
    /// the same string (AGE-181). The caller decides which are offered; this
    /// only has to make the two it can produce actually distinct.
    fn copy_kind(&self, kind: ArtifactCopyKind, cx: &mut App) {
        let text = match kind {
            ArtifactCopyKind::Rendered if !self.rendered.is_empty() => self.rendered.clone(),
            // Nothing rendered to copy — fall back rather than clearing the
            // user's clipboard.
            ArtifactCopyKind::Rendered | ArtifactCopyKind::Source => self.source.clone(),
        };
        if text.is_empty() {
            tracing::warn!(
                ?kind,
                "Artifact copy produced no text; leaving the clipboard alone"
            );
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Replace the PDF slot, releasing the outgoing page's raster from the
    /// sprite atlas (AGE-472). Hand-painted `RenderImage`s are not evicted
    /// by any cache, so without this every page turn and every re-raster
    /// would leave a texture behind. Deferred because callers may be inside
    /// a window update, during which that window is absent from
    /// `App::windows` and `drop_image` would skip it.
    fn set_pdf(&mut self, next: PdfPreview, cx: &mut App) {
        if let PdfPreview::Ready { image, .. } = mem::replace(&mut self.pdf, next) {
            cx.defer(move |cx| cx.drop_image(image, None));
        }
    }

    fn start_pdf_load(&mut self, page: u32, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        let width = self.pdf_raster_width;
        self.set_pdf(PdfPreview::Loading { page }, cx);
        cx.spawn(async move |this, cx| {
            let outcome =
                tokio::task::spawn_blocking(move || load_pdf_page_image(&path, page, width)).await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok((total, image))) => {
                        this.set_pdf(
                            PdfPreview::Ready {
                                page,
                                total,
                                image,
                                raster_width: width,
                            },
                            cx,
                        );
                        // The panel may have changed size while this render
                        // was in flight; the re-raster path only acts on a
                        // `Ready` page, so pick that up now.
                        this.reraster_pdf_page(cx);
                    }
                    Ok(Err(e)) => this.set_pdf(PdfPreview::Error(e.to_string()), cx),
                    Err(e) => this.set_pdf(PdfPreview::Error(e.to_string()), cx),
                }
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "Failed to apply PDF preview"))
            .ok();
        })
        .detach();
    }

    /// Keep the raster width matched to the panel's device-pixel width
    /// (AGE-472). The page is *laid out* at the panel width every frame
    /// regardless; this only decides when the 720 px preview (or whatever
    /// the page was last rendered at) is worth replacing with a sharper or
    /// cheaper one. Debounced like `sync_browser_viewport_size`: a split
    /// drag calls this every frame, and only the width that is still current
    /// once it settles reaches pdfium.
    fn sync_pdf_raster_width(&mut self, window: &Window, cx: &mut Context<Self>) {
        let panel_width = f32::from(*self.pdf_panel_width.borrow());
        if panel_width <= 0.0 {
            return;
        }
        let target = pdf_raster_width_for(panel_width, window.scale_factor());
        if target == self.pdf_raster_width {
            return;
        }
        self.pdf_raster_width = target;
        self.pdf_reraster_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PDF_RERASTER_DEBOUNCE).await;
            this.update(cx, |this, cx| this.reraster_pdf_page(cx)).ok();
        }));
    }

    /// Re-render the page on screen at `pdf_raster_width` if it is not
    /// already there, swapping the image in place once decoded — the old
    /// raster stays up meanwhile, so the panel never blanks. A page turn or
    /// another resize while the render is in flight makes the result stale;
    /// it is then dropped rather than applied over the newer state.
    fn reraster_pdf_page(&mut self, cx: &mut Context<Self>) {
        let PdfPreview::Ready {
            page, raster_width, ..
        } = &self.pdf
        else {
            return;
        };
        let width = self.pdf_raster_width;
        if *raster_width == width {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let page = *page;
        let load_id = self.load_gen;
        cx.spawn(async move |this, cx| {
            let outcome =
                tokio::task::spawn_blocking(move || load_pdf_page_image(&path, page, width)).await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id || this.pdf_raster_width != width {
                    return;
                }
                let PdfPreview::Ready {
                    page: current,
                    image,
                    raster_width,
                    ..
                } = &mut this.pdf
                else {
                    return;
                };
                if *current != page {
                    return;
                }
                match outcome {
                    Ok(Ok((_, new_image))) => {
                        let old = mem::replace(image, new_image);
                        *raster_width = width;
                        cx.defer(move |cx| cx.drop_image(old, None));
                        cx.notify();
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, width, "PDF re-raster failed; keeping the current page")
                    }
                    Err(e) => {
                        warn!(error = %e, width, "PDF re-raster failed; keeping the current page")
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Parse the open deck once (AGE-138), off the main thread.
    ///
    /// Fills both halves of the workbench: the slide list the Rendered tab
    /// pages through, and `source` — the parser's extracted text — for the
    /// Source tab, which is why the editor's sync generation is invalidated
    /// on the way out. `read_artifact_source` deliberately hands back `""`
    /// for a `.pptx`, so without this the Source tab stays blank.
    fn start_pptx_load(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.pptx = PptxPreview::Loading;
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || read_pptx_slides(&path, false)).await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok(slides)) => {
                        this.source = pptx_slides_to_text(&slides);
                        // The editor already synced against the empty source
                        // for this generation; force it to pick the extracted
                        // text up. `u64::MAX` is the same "never synced"
                        // sentinel `new()` uses.
                        this.editor_synced_gen = u64::MAX;
                        this.pptx = PptxPreview::Ready {
                            slide: 0,
                            slides: Arc::new(slides),
                        };
                    }
                    Ok(Err(e)) => this.pptx = PptxPreview::Error(e.to_string()),
                    Err(e) => this.pptx = PptxPreview::Error(e.to_string()),
                }
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "Failed to apply PPTX preview"))
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

    /// Same pager semantics as [`Self::turn_pdf_page`] — clamped at both ends,
    /// no wrap — but the deck is already parsed, so it is only an index move.
    fn turn_pptx_slide(&mut self, next: bool, cx: &mut Context<Self>) {
        let PptxPreview::Ready { slide, slides } = &self.pptx else {
            return;
        };
        let new_slide = next_slide_index(*slide, slides.len(), next);
        if new_slide == *slide {
            return;
        }
        let slides = slides.clone();
        self.pptx = PptxPreview::Ready {
            slide: new_slide,
            slides,
        };
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
    /// Debounced two ways: `RESIZE_THRESHOLD_PX` skips sub-pixel layout
    /// jitter outright, and a real size change schedules the actual CDP
    /// call `RESIZE_DEBOUNCE` out rather than firing immediately. A window
    /// drag re-renders (and so calls this) many times a second; without the
    /// delay, each intermediate size fires its own `start_screencast`, and
    /// those calls race — nothing guarantees they land at Chrome in the
    /// order they were sent, so a fast shrink-then-expand can have the
    /// shrink arrive *last* and the panel gets stuck showing (or, worse,
    /// briefly has no valid frame for) the wrong resolution. Storing the
    /// scheduled retarget in `browser_resize_task` and replacing it on
    /// every call means only the size that's still current once the drag
    /// settles ever reaches CDP — GPUI cancels a `Task` when it's dropped,
    /// so superseded retargets never fire at all.
    ///
    /// `browser_frame_bounds` is one paint behind `render()` (the
    /// `canvas()` prepaint that fills it runs after layout), which just
    /// means the retarget lags a frame further; harmless for a live view.
    fn sync_browser_viewport_size(&mut self, cx: &mut Context<Self>) {
        const RESIZE_THRESHOLD_PX: u32 = 24;
        const RESIZE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
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
        self.browser_requested_size = (width, height);
        // GPUI's own timer, not `tokio::time::sleep`: `cx.background_spawn`
        // runs on GPUI's background thread pool, which has no Tokio reactor
        // entered on it, so a Tokio timer panics there ("no reactor
        // running") the moment it fires.
        let executor = cx.background_executor().clone();
        self.browser_resize_task = Some(cx.background_spawn(async move {
            executor.timer(RESIZE_DEBOUNCE).await;
            tracing::debug!(
                width,
                height,
                "browser: retargeting screencast to panel size"
            );
            if let Err(e) = session.start_screencast(width, height).await {
                tracing::warn!(error = %e, width, height, "browser: viewport retarget failed");
            }
        }));
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
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.to_vec())
        .expect("screencast frame dimensions match its own buffer length");
    render_image_from_rgba_buffer(buffer)
}

fn render_image_from_rgba_buffer(mut buffer: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Render one PDF page at `width` device pixels and decode it for the
/// canvas (AGE-472). Blocking — pdfium plus a PNG round trip through the
/// preview cache — so callers run it under `spawn_blocking`. Returns the
/// document's page count alongside, which the pager needs on first load.
fn load_pdf_page_image(
    path: &Path,
    page: u32,
    width: u32,
) -> Result<(u32, Arc<RenderImage>), PdfThumbnailError> {
    let total = pdf_page_count(path)?;
    let png = render_pdf_page(path, page, width)?;
    let decoded = image::open(&png)
        .map_err(|e| PdfThumbnailError::Image(e.to_string()))?
        .into_rgba8();
    Ok((total, render_image_from_rgba_buffer(decoded)))
}

/// Device-pixel width to raster a page at for a panel `panel_width` logical
/// pixels wide on a `scale_factor` display (AGE-472). Rounded up to the next
/// `PDF_RASTER_STEP` so the raster is never narrower than the pixels it
/// covers, never below the `PREVIEW_WIDTH` first render, and capped at
/// `PDF_RASTER_MAX_WIDTH`.
fn pdf_raster_width_for(panel_width: f32, scale_factor: f32) -> u32 {
    let device = (panel_width * scale_factor).max(0.0).ceil() as u32;
    let bucketed = device.div_ceil(PDF_RASTER_STEP) * PDF_RASTER_STEP;
    bucketed.clamp(PREVIEW_WIDTH, PDF_RASTER_MAX_WIDTH)
}

/// What the frame on screen is: its raster size and the CSS viewport it
/// shows (AGE-379). The two differ whenever Chrome downscaled the raster to
/// the screencast's `maxWidth`/`maxHeight`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameGeometry {
    width: f32,
    height: f32,
    css_width: f64,
    css_height: f64,
}

impl From<&ScreencastFrame> for FrameGeometry {
    fn from(frame: &ScreencastFrame) -> Self {
        Self {
            width: frame.width as f32,
            height: frame.height as f32,
            css_width: frame.css_width,
            css_height: frame.css_height,
        }
    }
}

/// Map a window-relative position to CDP viewport space (AGE-156), honoring
/// the same "contain" letterbox math the frame is painted with — the raster
/// occupies a centered sub-rect of the container whenever the container's
/// aspect ratio doesn't match the capture's. `None` for a position that
/// lands in the letterbox padding rather than the image.
///
/// `frame` is the frame actually on screen, not the size the viewport was
/// last asked for (AGE-379): the letterbox follows the raster's aspect,
/// and the raster maps onto the CSS viewport Chrome reported with it. That
/// stays exact through a resize debounce, a refused retarget, or a raster
/// Chrome downscaled — every case where the requested size lies.
///
/// `gpui::Pixels` are device-independent, as are CDP's CSS `x`/`y`, so the
/// display's DPI scale factor never enters here.
fn browser_viewport_position(
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    frame: FrameGeometry,
) -> Option<(f64, f64)> {
    let (bx, by) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if bw <= 0.0 || bh <= 0.0 {
        return None;
    }
    let (fw, fh) = (frame.width, frame.height);
    if fw <= 0.0 || fh <= 0.0 || frame.css_width <= 0.0 || frame.css_height <= 0.0 {
        return None;
    }
    let scale = (bw / fw).min(bh / fh);
    if scale <= 0.0 {
        return None;
    }
    let (dw, dh) = (fw * scale, fh * scale);
    let (ox, oy) = ((bw - dw) / 2.0, (bh - dh) / 2.0);
    let local_x = f32::from(position.x) - bx - ox;
    let local_y = f32::from(position.y) - by - oy;
    if local_x < 0.0 || local_y < 0.0 || local_x > dw || local_y > dh {
        return None;
    }
    // Displayed pixels → raster pixels → CSS pixels.
    let raster_x = f64::from(local_x / scale);
    let raster_y = f64::from(local_y / scale);
    Some((
        raster_x * (frame.css_width / f64::from(fw)),
        raster_y * (frame.css_height / f64::from(fh)),
    ))
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

/// What a browser tab is called in the header tab bar (AGE-473): its title,
/// else its URL, else "New tab" for a page that has neither yet — cut to fit
/// a tab entry.
fn browser_tab_label(tab: &BrowserTab) -> String {
    const MAX_CHARS: usize = 24;
    let name = if !tab.title.trim().is_empty() {
        tab.title.trim()
    } else if !tab.url.is_empty() && tab.url != "about:blank" {
        &tab.url
    } else {
        "New tab"
    };
    let mut chars = name.chars();
    let short: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

/// What `open_browser` has to do for a manager (AGE-473), decided from
/// whether it is the manager already open, whether the browser is on
/// screen, and whether its session has resolved yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserReopen {
    /// A different manager: drop the old browser, resolve the new session.
    Fresh,
    /// The same manager, already on screen: nothing to start.
    Present,
    /// The same manager, paused behind a file with a live session: restart
    /// the screencast on it.
    ResumeStream,
    /// The same manager, paused before its session resolved: the resolve
    /// was dropped, so it has to be issued again.
    ResolveSession,
}

fn browser_reopen(already_open: bool, browser_shown: bool, has_session: bool) -> BrowserReopen {
    match (already_open, browser_shown, has_session) {
        (false, _, _) => BrowserReopen::Fresh,
        (true, true, _) => BrowserReopen::Present,
        (true, false, true) => BrowserReopen::ResumeStream,
        (true, false, false) => BrowserReopen::ResolveSession,
    }
}

/// What one entry in the artifact panel's header tab bar points at
/// (AGE-473): an open file, by its index into `files`, or an open browser
/// tab, by its CDP target id.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ArtifactTabTarget {
    File(usize),
    Browser(String),
}

/// One entry in the header tab bar: a file or a browser tab, laid out in one
/// bar (AGE-473). `browser_blocked` is `None` for a file and `Some(blocked)`
/// for a browser tab, so the renderer can give a page a globe (or a blocked
/// marker) and a close button that a file does not get.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ArtifactTabEntry {
    label: String,
    browser_blocked: Option<bool>,
    target: ArtifactTabTarget,
}

/// The header tab bar's entries and its selected index (AGE-473): the open
/// files followed by the open browser tabs, in one bar. `selected_file` is
/// the index of the file on screen (when a file/table/chart is showing);
/// while the browser is showing the active browser tab is selected instead.
/// `selected` is `None` when nothing in the bar is on screen.
fn artifact_tab_model(
    files: &[(PathBuf, String, Option<String>)],
    browser_tabs: &[BrowserTab],
    selected_file: Option<usize>,
    browser_shown: bool,
) -> (Vec<ArtifactTabEntry>, Option<usize>) {
    let mut entries: Vec<ArtifactTabEntry> = files
        .iter()
        .enumerate()
        .map(|(ix, (path, _, _))| ArtifactTabEntry {
            label: path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            browser_blocked: None,
            target: ArtifactTabTarget::File(ix),
        })
        .collect();
    let file_count = entries.len();
    entries.extend(browser_tabs.iter().map(|tab| ArtifactTabEntry {
        label: browser_tab_label(tab),
        browser_blocked: Some(tab.blocked.is_some()),
        target: ArtifactTabTarget::Browser(tab.id.clone()),
    }));

    let selected = if browser_shown {
        browser_tabs
            .iter()
            .position(|tab| tab.active)
            .map(|ix| file_count + ix)
    } else {
        selected_file
    };
    (entries, selected)
}

#[allow(clippy::too_many_arguments)] // Rendering function threading per-frame view state
fn browser_rendered_body(
    browser: &BrowserPreview,
    control: ControlHolder,
    frame_bounds: Rc<RefCell<Bounds<Pixels>>>,
    geometry: Option<FrameGeometry>,
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
            // The button is the thing that resumes the task (AGE-379): a
            // release with no turn running sends the agent a message naming
            // the page the user left it on, so it re-snapshots and continues.
            ControlHolder::User => Button::new("artifact-browser-release-control")
                .ghost()
                .small()
                .label("Hand back & continue")
                .tooltip("Give the browser back to the agent and let it continue from this page")
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
            let frame_image = image.clone();
            let corner_radius = cx.theme().radius;
            // The frame is painted by hand from the canvas that also records
            // its bounds, so the letterbox the user sees and the letterbox
            // `browser_viewport_position` maps clicks through come from one
            // rect (AGE-379). It used to be an `img().object_fit(Contain)`
            // sibling, which in `ArtifactMode::Full` laid out but painted
            // nothing — the canvas next to it still painted, and the same
            // frame painted fine once the panel was docked again; not
            // root-caused inside gpui — so Full mode was a blank panel.
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
                        move |bounds, _, window, _| {
                            let fitted = ObjectFit::Contain.get_bounds(bounds, frame_image.size(0));
                            let corners =
                                Corners::all(corner_radius).clamp_radii_for_quad_size(fitted.size);
                            if let Err(e) =
                                window.paint_image(fitted, corners, frame_image.clone(), 0, false)
                            {
                                tracing::warn!(error = %e, "browser: painting the frame failed");
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                );

            // Input forwarding (AGE-156) only listens while the user holds
            // control — with the agent driving, the frame behaves like a
            // plain image and never steals the mouse or the keyboard.
            // A `Frame` always has geometry; the fallback only keeps the
            // handlers total.
            let geometry = geometry.unwrap_or(FrameGeometry {
                width: image.size(0).width.0 as f32,
                height: image.size(0).height.0 as f32,
                css_width: f64::from(image.size(0).width.0),
                css_height: f64::from(image.size(0).height.0),
            });
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
                                geometry,
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
                                geometry,
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
                                geometry,
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
                                geometry,
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

fn pdf_rendered_body(
    pdf: &PdfPreview,
    panel_width: Rc<RefCell<Pixels>>,
    entity: Entity<ArtifactView>,
    cx: &App,
) -> AnyElement {
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
            // Fit to width (AGE-472): the page is as wide as the scroll
            // container and as tall as its aspect ratio says, and the
            // container's width comes from the previous frame's prepaint.
            // The first frame after a size change therefore lays the page
            // out at the old width; `Contain` keeps that frame unstretched
            // and the prepaint asks for another frame, which lands on the
            // new width. Zero width (nothing painted yet) means a 1 px
            // strip whose only job is to run that prepaint.
            let width = *panel_width.borrow();
            let raster = image.size(0);
            let page_height = if raster.width.0 > 0 {
                f32::from(width) * raster.height.0 as f32 / raster.width.0 as f32
            } else {
                f32::from(width)
            };
            let width_for_prepaint = panel_width.clone();
            let page_image = image.clone();
            let corner_radius = cx.theme().radius;
            let page_canvas = canvas(
                move |bounds, window, _cx| {
                    let mut known = width_for_prepaint.borrow_mut();
                    if (*known - bounds.size.width).abs() >= px(1.0) {
                        *known = bounds.size.width;
                        window.request_animation_frame();
                    }
                },
                move |bounds, _, window, _| {
                    let fitted = ObjectFit::Contain.get_bounds(bounds, page_image.size(0));
                    let corners =
                        Corners::all(corner_radius).clamp_radii_for_quad_size(fitted.size);
                    if let Err(e) =
                        window.paint_image(fitted, corners, page_image.clone(), 0, false)
                    {
                        warn!(error = %e, "pdf: painting the page failed");
                    }
                },
            )
            .w_full()
            .h(px(page_height.max(1.0)));
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
                        .child(page_canvas),
                )
                .into_any_element()
        }
    }
}

/// Where a Prev/Next click lands: clamped at both ends, never wrapping —
/// the same rule `turn_pdf_page` applies to pages.
fn next_slide_index(current: usize, total: usize, next: bool) -> usize {
    if next {
        current.saturating_add(1).min(total.saturating_sub(1))
    } else {
        current.saturating_sub(1)
    }
}

/// The pager's `(label, prev enabled, next enabled)` for a slide.
///
/// Split out of the render so the panel's contract — opens on slide 1 with
/// Prev disabled, Next walks to the last slide and stops — is testable
/// without a window.
fn slide_pager(index: usize, total: usize) -> (String, bool, bool) {
    (
        format!("Slide {} of {}", index + 1, total),
        index > 0,
        index + 1 < total,
    )
}

/// One slide as a card: title, body paragraphs (bulleted where the deck said
/// so), then any tables. Deliberately not a markdown dump of the deck — the
/// pager above it decides which slide this is.
fn pptx_slide_card(slide: &PptxSlide, window: &mut Window, cx: &mut App) -> AnyElement {
    let has_text = slide.title.is_some() || !slide.body.is_empty() || !slide.tables.is_empty();
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border);

    if let Some(title) = &slide.title {
        card = card.child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.clone()),
        );
    }

    for block in &slide.body {
        let bulleted = block.bulleted;
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .children(block.lines.iter().map(|line| {
                    div()
                        .text_sm()
                        .line_height(relative(1.5))
                        .child(if bulleted {
                            format!("• {line}")
                        } else {
                            line.clone()
                        })
                })),
        );
    }

    if !slide.tables.is_empty() {
        // Tables come out of the parser as markdown, so reuse the markdown
        // renderer rather than re-implementing a grid here.
        let markdown = slide.tables.join("\n\n");
        let id = ElementId::Name(format!("artifact-pptx-tables-{}", slide.number).into());
        card = card.child(
            TextView::markdown(id, markdown, window, cx)
                .style(document_text_style())
                .selectable(true),
        );
    }

    if !has_text {
        card = card.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("This slide has no text content."),
        );
    }

    card.into_any_element()
}

/// Slide workbench (AGE-138). Same chrome as [`pdf_rendered_body`]: Prev,
/// a position label, Next, and one page/slide at a time in a scroller.
fn pptx_rendered_body(
    pptx: &PptxPreview,
    entity: Entity<ArtifactView>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match pptx {
        PptxPreview::Idle | PptxPreview::Loading => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Reading slides…")
            .into_any_element(),
        // A deck we cannot parse gets the muted one-liner, never a panic and
        // never a dump of the ZIP.
        PptxPreview::Error(message) => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        // A valid ZIP with no `ppt/slides/slideN.xml` parses fine and yields
        // nothing to page through — say so rather than showing "Slide 1 of 0".
        PptxPreview::Ready { slides, .. } if slides.is_empty() => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("This presentation has no slides.")
            .into_any_element(),
        PptxPreview::Ready { slide, slides } => {
            let index = (*slide).min(slides.len().saturating_sub(1));
            let (label, can_prev, can_next) = slide_pager(index, slides.len());
            let card = pptx_slide_card(&slides[index], window, cx);
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
                            Button::new("artifact-pptx-prev")
                                .ghost()
                                .small()
                                .label("Prev")
                                .disabled(!can_prev)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity
                                            .update(cx, |this, cx| this.turn_pptx_slide(false, cx));
                                    }
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        )
                        .child(
                            Button::new("artifact-pptx-next")
                                .ghost()
                                .small()
                                .label("Next")
                                .disabled(!can_next)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity
                                            .update(cx, |this, cx| this.turn_pptx_slide(true, cx));
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .id("artifact-pptx-slide")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .child(card),
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
            if self.browser_shown {
                self.sync_browser_address(window, cx);
                self.sync_browser_viewport_size(cx);
            }
            self.sync_pdf_raster_width(window, cx);
        }

        let tab = self.tab;
        let source = self.source.clone();
        let rendered = self.rendered.clone();
        let old = self.old.clone();
        let full = self.mode == ArtifactMode::Full;
        let entity = cx.entity();
        let is_browser = self.browser_shown;
        let is_pdf = !is_browser && self.path.as_ref().is_some_and(|path| is_pdf_path(path));
        // Unlike a PDF, a deck is not opaque: it keeps the tab bar, because
        // the extracted text under Source is a real second view of it.
        let is_pptx = !is_browser && self.path.as_ref().is_some_and(|path| is_pptx_path(path));
        let is_chart = !is_browser && self.chart.is_some();
        let is_image = !is_chart && self.path.as_ref().is_some_and(|path| is_image_path(path));
        let is_tabular = matches!(self.tabular, TabularPreview::Ready(_))
            || self.path.as_ref().is_some_and(|path| is_tabular_path(path));
        let path_ref = self.path.clone();
        let pdf = self.pdf.clone();
        let pptx = self.pptx.clone();
        let tabular = self.tabular.clone();
        let chart = self.chart.clone();
        let browser = self.browser.clone();
        let browser_control = self.browser_control;
        let browser_frame_bounds = self.browser_frame_bounds.clone();
        let pdf_panel_width = self.pdf_panel_width.clone();
        let browser_geometry = self.browser_frame_geometry;
        let browser_focus = self.browser_focus.clone();
        let browser_address = self.browser_address.clone();
        let has_diff = !old.is_empty() && old != source;
        // The header only offers choices that exist for this artifact and that
        // do different things (AGE-181).
        let header_kind = ArtifactHeaderKind::resolve(
            path_ref.as_deref(),
            is_tabular,
            is_pdf || is_image || is_chart || is_browser,
        );
        let header_tabs = artifact_header_tabs(header_kind, has_diff);
        let copy_control = artifact_copy_control(header_kind);
        // Selection carries across artifacts, so fall back to the primary view
        // whenever the remembered tab is not one this artifact offers.
        let visible_tab = if header_tabs.iter().any(|spec| spec.index == tab) {
            tab
        } else {
            0
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
                    browser_geometry,
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
                .child(pdf_rendered_body(&pdf, pdf_panel_width, entity.clone(), cx))
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
                .when(!header_tabs.is_empty(), |this| {
                    let selected = header_tabs
                        .iter()
                        .position(|spec| spec.index == visible_tab)
                        .unwrap_or(0);
                    let indices: Vec<usize> = header_tabs.iter().map(|spec| spec.index).collect();
                    this.child(
                        TabBar::new("artifact-modes")
                            .segmented()
                            .children(
                                header_tabs
                                    .iter()
                                    .map(|spec| Tab::new().label(spec.label))
                                    .collect::<Vec<_>>(),
                            )
                            .selected_index(selected)
                            .on_click({
                                let entity = entity.clone();
                                move |ix, window, cx| {
                                    // Map the visible position back to the
                                    // viewer's own view index, which does not
                                    // change with which tabs are shown.
                                    let next = indices.get(*ix).copied().unwrap_or(0);
                                    entity.update(cx, |this, cx| {
                                        this.select_tab(next, window, cx);
                                    });
                                }
                            }),
                    )
                })
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
                                    } else if is_pptx {
                                        this.flex_1().min_h_0().p_2().child(pptx_rendered_body(
                                            &pptx,
                                            entity.clone(),
                                            window,
                                            cx,
                                        ))
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
        // AGE-473: one header tab bar carries the open files and the open
        // browser tabs together — a file/table/chart entry, then a page
        // entry per browser tab (globe, blocked marker, its own × ).
        let selected_file = self
            .path
            .as_ref()
            .and_then(|active| self.files.iter().position(|(path, _, _)| path == active));
        let (tab_entries, selected_tab) = artifact_tab_model(
            &self.files,
            &self.browser_tabs,
            selected_file,
            self.browser_shown,
        );
        let file_tab_bar = (!session_review && tab_entries.len() > 1).then(|| {
            let targets: Vec<ArtifactTabTarget> = tab_entries
                .iter()
                .map(|entry| entry.target.clone())
                .collect();
            TabBar::new("artifact-files")
                .small()
                .menu(true)
                .when_some(selected_tab, |this, ix| this.selected_index(ix))
                .on_click({
                    let entity = entity.clone();
                    let targets = targets.clone();
                    move |ix, _, cx| {
                        let Some(target) = targets.get(*ix).cloned() else {
                            return;
                        };
                        entity.update(cx, |this, cx| this.select_artifact_tab(target, cx));
                    }
                })
                .children(tab_entries.iter().map(|entry| {
                    let mut tab = Tab::new().label(entry.label.clone());
                    if let Some(blocked) = entry.browser_blocked {
                        let icon = if blocked {
                            IconName::CircleX
                        } else {
                            IconName::Globe
                        };
                        tab = tab.prefix(Icon::new(icon).size_3());
                        if let ArtifactTabTarget::Browser(id) = &entry.target {
                            let id = id.clone();
                            tab = tab.suffix(
                                Button::new(SharedString::from(format!(
                                    "artifact-browser-tab-close-{id}"
                                )))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::Close).size_3())
                                .tooltip("Close tab")
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        // The click must not bubble on to the
                                        // tab underneath, or closing a tab
                                        // would also select it first.
                                        cx.stop_propagation();
                                        let id = id.clone();
                                        entity
                                            .update(cx, |this, cx| this.close_browser_tab(id, cx));
                                    }
                                }),
                            );
                        }
                    }
                    tab
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
                            .font_family(cx.theme().mono_font_family.clone())
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
                    // One button where there is one payload, a menu only where
                    // source and rendered genuinely differ, nothing at all for
                    // an artifact with no text (AGE-181).
                    .when(
                        !session_review && matches!(copy_control, ArtifactCopy::Source),
                        |this| {
                            this.child(
                                Button::new("artifact-copy-main")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Copy).size_3())
                                    .tooltip("Copy file contents")
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, _, cx| {
                                            entity.update(cx, |this, cx| {
                                                this.copy_kind(ArtifactCopyKind::Source, cx);
                                            });
                                        }
                                    }),
                            )
                        },
                    )
                    .when(
                        !session_review && matches!(copy_control, ArtifactCopy::Menu),
                        |this| {
                            let rendered_label = if is_tabular {
                                "Copy table"
                            } else {
                                "Copy text"
                            };
                            this.child(
                                DropdownButton::new("artifact-copy")
                                    .small()
                                    .ghost()
                                    .button(
                                        Button::new("artifact-copy-main")
                                            .ghost()
                                            .small()
                                            .label("Copy")
                                            .tooltip("Copy the file's source")
                                            .on_click({
                                                let entity = entity.clone();
                                                move |_, _, cx| {
                                                    entity.update(cx, |this, cx| {
                                                        this.copy_kind(
                                                            ArtifactCopyKind::Source,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }),
                                    )
                                    .dropdown_menu({
                                        let entity = entity.clone();
                                        move |menu, _, _| {
                                            menu.item(PopupMenuItem::new("Copy source").on_click({
                                                let entity = entity.clone();
                                                move |_, _, cx| {
                                                    entity.update(cx, |this, cx| {
                                                        this.copy_kind(
                                                            ArtifactCopyKind::Source,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }))
                                            .item(
                                                PopupMenuItem::new(rendered_label).on_click({
                                                    let entity = entity.clone();
                                                    move |_, _, cx| {
                                                        entity.update(cx, |this, cx| {
                                                            this.copy_kind(
                                                                ArtifactCopyKind::Rendered,
                                                                cx,
                                                            );
                                                        });
                                                    }
                                                }),
                                            )
                                        }
                                    }),
                            )
                        },
                    )
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

/// AGE-138: the slide pager's contract, minus the pixels.
#[cfg(test)]
mod pptx_pager_tests {
    use super::{next_slide_index, slide_pager};

    /// "Rendered shows slide 1, Prev disabled" — the panel opens at index 0.
    #[test]
    fn opens_on_slide_one_with_prev_disabled() {
        let (label, can_prev, can_next) = slide_pager(0, 4);
        assert_eq!(label, "Slide 1 of 4");
        assert!(!can_prev, "there is nothing before the first slide");
        assert!(can_next);
    }

    /// "Next walks every slide" — and stops on the last one.
    #[test]
    fn next_walks_every_slide_then_stops() {
        let total = 3;
        let mut index = 0;
        let mut seen = vec![slide_pager(index, total).0];
        for _ in 0..5 {
            index = next_slide_index(index, total, true);
            seen.push(slide_pager(index, total).0);
        }
        assert_eq!(
            seen,
            vec![
                "Slide 1 of 3",
                "Slide 2 of 3",
                "Slide 3 of 3",
                "Slide 3 of 3",
                "Slide 3 of 3",
                "Slide 3 of 3",
            ],
            "every slide is reachable and the pager clamps at the end"
        );
        assert!(
            !slide_pager(total - 1, total).2,
            "Next dies on the last slide"
        );
    }

    #[test]
    fn prev_clamps_at_the_first_slide() {
        assert_eq!(next_slide_index(1, 3, false), 0);
        assert_eq!(next_slide_index(0, 3, false), 0);
    }

    /// A deck with one slide has neither button live; an empty one must not
    /// underflow the `total - 1` clamp.
    #[test]
    fn degenerate_decks_do_not_underflow() {
        let (label, can_prev, can_next) = slide_pager(0, 1);
        assert_eq!(label, "Slide 1 of 1");
        assert!(!can_prev);
        assert!(!can_next);
        assert_eq!(next_slide_index(0, 0, true), 0);
        assert_eq!(next_slide_index(0, 0, false), 0);
    }
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

/// AGE-473: the header tab bar's entry list, selected index and per-entry
/// label. The bar itself is a `TabBar` and needs a window; this is the part
/// with cases.
#[cfg(test)]
mod artifact_tab_bar_tests {
    use std::path::PathBuf;

    use super::{ArtifactTabTarget, BrowserTab, artifact_tab_model, browser_tab_label};

    fn tab(title: &str, url: &str) -> BrowserTab {
        BrowserTab {
            id: format!("target-{title}-{url}"),
            title: title.to_string(),
            url: url.to_string(),
            active: false,
            blocked: None,
        }
    }

    fn active(mut t: BrowserTab) -> BrowserTab {
        t.active = true;
        t
    }

    fn blocked(mut t: BrowserTab) -> BrowserTab {
        t.blocked = Some("https://blocked.example/".to_string());
        t
    }

    fn files(names: &[&str]) -> Vec<(PathBuf, String, Option<String>)> {
        names
            .iter()
            .map(|n| (PathBuf::from(n), String::new(), None))
            .collect()
    }

    #[test]
    fn files_come_first_then_browser_tabs() {
        let fs = files(&["a.rs", "b.rs"]);
        let tabs = vec![tab("Opener", "file:///x"), tab("Popup", "file:///y")];
        let (entries, _) = artifact_tab_model(&fs, &tabs, Some(0), false);
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
            vec!["a.rs", "b.rs", "Opener", "Popup"]
        );
        assert_eq!(entries[0].target, ArtifactTabTarget::File(0));
        assert_eq!(entries[1].target, ArtifactTabTarget::File(1));
        assert_eq!(
            entries[2].target,
            ArtifactTabTarget::Browser(tabs[0].id.clone())
        );
        assert_eq!(entries[0].browser_blocked, None, "a file is not a page");
        assert_eq!(
            entries[2].browser_blocked,
            Some(false),
            "an allowed page is a page but not blocked"
        );
    }

    #[test]
    fn a_file_is_selected_while_a_file_is_showing() {
        let fs = files(&["a.rs", "b.rs"]);
        let tabs = vec![active(tab("Opener", "file:///x"))];
        // File index 1 is on screen; the browser is not.
        let (_, selected) = artifact_tab_model(&fs, &tabs, Some(1), false);
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn the_active_browser_tab_is_selected_while_the_browser_is_showing() {
        let fs = files(&["a.rs"]);
        let tabs = vec![
            tab("Opener", "file:///x"),
            active(tab("Popup", "file:///y")),
        ];
        // files.len() (1) + active-tab index (1) = 2.
        let (_, selected) = artifact_tab_model(&fs, &tabs, None, true);
        assert_eq!(selected, Some(2));
    }

    #[test]
    fn a_blocked_browser_tab_is_marked() {
        let fs = files(&["a.rs"]);
        let tabs = vec![active(blocked(tab("Popup", "https://blocked.example/")))];
        let (entries, _) = artifact_tab_model(&fs, &tabs, None, true);
        assert_eq!(entries[1].browser_blocked, Some(true));
    }

    #[test]
    fn one_entry_is_a_hidden_bar() {
        // A single file, no browser: the caller hides the bar at len < 2.
        let (entries, _) = artifact_tab_model(&files(&["only.rs"]), &[], Some(0), false);
        assert_eq!(entries.len(), 1);
        // A single browser tab, no file: still one entry.
        let (entries, _) =
            artifact_tab_model(&[], &[active(tab("Opener", "file:///x"))], None, true);
        assert_eq!(entries.len(), 1);
    }

    /// The pause-before-resolve case: a file shown while Chrome was still
    /// launching drops the resolve at its `load_gen` guard, so bringing the
    /// browser back must resolve again rather than wait for a stream that
    /// will never start.
    #[test]
    fn reopening_a_paused_browser_resolves_again_when_its_session_never_arrived() {
        use super::{BrowserReopen, browser_reopen};
        assert_eq!(
            browser_reopen(true, false, false),
            BrowserReopen::ResolveSession
        );
        assert_eq!(
            browser_reopen(true, false, true),
            BrowserReopen::ResumeStream
        );
        assert_eq!(browser_reopen(true, true, true), BrowserReopen::Present);
        assert_eq!(
            browser_reopen(true, true, false),
            BrowserReopen::Present,
            "on screen with the resolve still in flight: let it land"
        );
        assert_eq!(browser_reopen(false, false, false), BrowserReopen::Fresh);
        assert_eq!(
            browser_reopen(false, true, true),
            BrowserReopen::Fresh,
            "a different manager always replaces the open one"
        );
    }

    #[test]
    fn index_routes_to_file_or_browser_tab() {
        let fs = files(&["a.rs"]);
        let popup = tab("Popup", "file:///y");
        let tabs = vec![active(tab("Opener", "file:///x")), popup.clone()];
        let (entries, _) = artifact_tab_model(&fs, &tabs, None, true);
        // Index 0 → the file; index 2 → the second browser tab.
        assert_eq!(entries[0].target, ArtifactTabTarget::File(0));
        assert_eq!(
            entries[2].target,
            ArtifactTabTarget::Browser(popup.id.clone())
        );
    }

    #[test]
    fn a_titled_tab_is_named_by_its_title() {
        assert_eq!(
            browser_tab_label(&tab("Opener", "file:///tmp/index.html")),
            "Opener"
        );
        assert_eq!(
            browser_tab_label(&tab("  Padded  ", "about:blank")),
            "Padded"
        );
    }

    #[test]
    fn a_tab_without_a_title_falls_back_to_its_url_then_to_new_tab() {
        assert_eq!(
            browser_tab_label(&tab("", "http://localhost:3000/")),
            "http://localhost:3000/"
        );
        assert_eq!(browser_tab_label(&tab("", "about:blank")), "New tab");
        assert_eq!(browser_tab_label(&tab("", "")), "New tab");
    }

    #[test]
    fn a_long_name_is_cut_with_an_ellipsis() {
        let label = browser_tab_label(&tab(
            "A very long document title that would flood the strip",
            "",
        ));
        assert_eq!(label, "A very long document tit…");
        assert_eq!(label.chars().count(), 25);
    }
}

#[cfg(test)]
mod browser_viewport_position_tests {
    use super::{FrameGeometry, browser_viewport_position};
    use gpui::{Bounds, Pixels, Point, point, px, size};

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn frame(width: f32, height: f32, css_width: f64, css_height: f64) -> FrameGeometry {
        FrameGeometry {
            width,
            height,
            css_width,
            css_height,
        }
    }

    fn assert_close(actual: Option<(f64, f64)>, expected: (f64, f64)) {
        let (x, y) = actual.expect("position lands on the frame");
        assert!(
            (x - expected.0).abs() < 0.01 && (y - expected.1).abs() < 0.01,
            "got ({x}, {y}), expected {expected:?}"
        );
    }

    /// The frame fills the container: window pixels map one to one.
    #[test]
    fn a_frame_that_fills_the_container_maps_one_to_one() {
        let f = frame(800.0, 600.0, 800.0, 600.0);
        assert_close(
            browser_viewport_position(bounds(100.0, 50.0, 800.0, 600.0), at(500.0, 350.0), f),
            (400.0, 300.0),
        );
    }

    /// A 4:3 frame in a wide container is centred with bars left and
    /// right; a click in a bar is not a click on the page.
    #[test]
    fn a_letterboxed_frame_maps_through_its_own_aspect() {
        let f = frame(800.0, 600.0, 800.0, 600.0);
        // 1000x600 container: the frame shows at 800x600 with 100px bars.
        let b = bounds(0.0, 0.0, 1000.0, 600.0);
        assert_eq!(browser_viewport_position(b, at(50.0, 300.0), f), None);
        assert_close(browser_viewport_position(b, at(100.0, 0.0), f), (0.0, 0.0));
        assert_close(
            browser_viewport_position(b, at(500.0, 300.0), f),
            (400.0, 300.0),
        );
        assert_close(
            browser_viewport_position(b, at(900.0, 600.0), f),
            (800.0, 600.0),
        );
    }

    /// Chrome downscaled the raster to `maxWidth`: 400x300 pixels showing an
    /// 800x600 CSS viewport. A click on the raster still lands on the CSS
    /// point it shows.
    #[test]
    fn a_frame_smaller_than_its_viewport_scales_back_to_css_pixels() {
        let f = frame(400.0, 300.0, 800.0, 600.0);
        assert_close(
            browser_viewport_position(bounds(0.0, 0.0, 400.0, 300.0), at(100.0, 75.0), f),
            (200.0, 150.0),
        );
        // Displayed larger than the raster, too.
        assert_close(
            browser_viewport_position(bounds(0.0, 0.0, 800.0, 600.0), at(200.0, 150.0), f),
            (200.0, 150.0),
        );
    }

    /// AGE-379: the panel was resized (or Full mode toggled) and the
    /// requested viewport is already the new 1200x900, but the frame on
    /// screen is still the old 600x450 capture stretched into the panel.
    /// Mapping through the frame lands the click where the user sees it;
    /// mapping through the requested size would halve every coordinate.
    #[test]
    fn a_stale_requested_size_does_not_move_the_click() {
        let stale_frame = frame(600.0, 450.0, 600.0, 450.0);
        let panel = bounds(0.0, 0.0, 1200.0, 900.0);
        assert_close(
            browser_viewport_position(panel, at(600.0, 450.0), stale_frame),
            (300.0, 225.0),
        );
        // Once the retarget lands, the same window point is the same page
        // point in the new viewport — no discontinuity for the user.
        let fresh_frame = frame(1200.0, 900.0, 1200.0, 900.0);
        assert_close(
            browser_viewport_position(panel, at(600.0, 450.0), fresh_frame),
            (600.0, 450.0),
        );
    }

    #[test]
    fn degenerate_geometry_maps_nothing() {
        let b = bounds(0.0, 0.0, 800.0, 600.0);
        assert_eq!(
            browser_viewport_position(b, at(10.0, 10.0), frame(0.0, 600.0, 800.0, 600.0)),
            None
        );
        assert_eq!(
            browser_viewport_position(b, at(10.0, 10.0), frame(800.0, 600.0, 0.0, 600.0)),
            None
        );
        assert_eq!(
            browser_viewport_position(
                bounds(0.0, 0.0, 0.0, 0.0),
                at(0.0, 0.0),
                frame(800.0, 600.0, 800.0, 600.0)
            ),
            None
        );
    }
}

#[cfg(test)]
mod pdf_raster_width_tests {
    use super::{PDF_RASTER_MAX_WIDTH, PDF_RASTER_STEP, pdf_raster_width_for};
    use chatty_core::services::pdf_thumbnail::PREVIEW_WIDTH;

    #[test]
    fn narrow_panels_keep_the_preview_raster() {
        // The docked default (~350 px at 1x) never needs more than the
        // 720 px preview, so a page turn there is a cache hit.
        assert_eq!(pdf_raster_width_for(348.0, 1.0), PREVIEW_WIDTH);
        assert_eq!(pdf_raster_width_for(0.0, 1.0), PREVIEW_WIDTH);
    }

    #[test]
    fn rasters_cover_the_device_pixels_in_steps() {
        // 900 logical px at 1x → next multiple of 256 above 900.
        assert_eq!(pdf_raster_width_for(900.0, 1.0), 1024);
        // A HiDPI panel counts device pixels: 700 × 2 = 1400 → 1536.
        assert_eq!(pdf_raster_width_for(700.0, 2.0), 1536);
        // Never narrower than the pixels covered, so 1024.5 needs a bigger bucket.
        assert_eq!(pdf_raster_width_for(1024.5, 1.0), 1024 + PDF_RASTER_STEP);
        // Nudging a split by a few pixels lands in the same bucket.
        assert_eq!(
            pdf_raster_width_for(901.0, 1.0),
            pdf_raster_width_for(1020.0, 1.0)
        );
    }

    #[test]
    fn full_window_on_a_5k_display_is_capped() {
        assert_eq!(pdf_raster_width_for(2560.0, 2.0), PDF_RASTER_MAX_WIDTH);
    }
}
