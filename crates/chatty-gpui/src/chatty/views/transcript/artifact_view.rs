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
use chatty_core::services::pptx_render::{PptxRenderError, render_slide, slide_count};
use chatty_core::tools::chart_tool::ChartSpec;
use chatty_core::tools::data_query_tool::{
    FILE_PREVIEW_MAX_ROWS, TablePreview, load_file_table_preview,
};
use chatty_core::tools::pptx_tool::{pptx_slides_to_text, read_pptx_slides};
use std::ops::Range;
use tokio::sync::mpsc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::Selectable;
use gpui_component::WindowExt;
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
use super::file_explorer::render_file_explorer;
use super::file_tree::{FileOp, FileTree, PendingEdit, SelectGesture, rebase_path};
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
/// Since AGE-343 a slide is a raster, not a card built from extracted text, so
/// this mirrors [`PdfPreview`] exactly: one image at a time, re-rendered on
/// every turn. The deck's own PDF is cached by `pptx_render`, so a turn costs
/// one pdfium raster rather than a re-render of the deck.
#[derive(Clone, Debug, Default)]
enum PptxPreview {
    #[default]
    Idle,
    Loading {
        slide: usize,
    },
    Ready {
        slide: usize,
        total: usize,
        /// Decoded off the main thread and painted by hand from a `canvas()`
        /// like the PDF page, so the slide can follow the panel width.
        image: Arc<RenderImage>,
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
    /// The workspace explorer column (AGE-476), `Some` while it is shown.
    /// It outlives the document: closing the last file tab leaves the tree
    /// up with an empty body, and the panel closing keeps it for next time.
    explorer: Option<FileTree>,
    /// The inline name input the tree shows for a create/rename; one
    /// entity reused for every edit, like `browser_address`.
    explorer_input: Entity<InputState>,
    explorer_scroll: UniformListScrollHandle,
    /// Focus `explorer_input` on the next render: an edit begun from a
    /// context-menu item is focused after the menu's own dismissal has
    /// restored focus, or the menu wins.
    explorer_focus_pending: bool,
    /// The editor's text differs from `source` for the file on screen
    /// (AGE-476). Kept from the editor's own change events, so the Save
    /// button and the tab's dot follow every keystroke.
    dirty: bool,
    /// Edited-but-unsaved buffers of files that are not on screen, keyed by
    /// path: switching tabs stashes the current buffer here and `sync_editor`
    /// loads it back over the disk text when that file returns.
    unsaved: HashMap<PathBuf, String>,
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
        // Dirty tracking (AGE-476): the editor tells us about every change,
        // including our own `set_value` in `sync_editor`, so "dirty" is
        // simply "differs from the text loaded from disk" — a programmatic
        // sync compares equal and clears it, a restored unsaved buffer
        // compares different and sets it.
        cx.subscribe(&editor, |this: &mut Self, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.note_editor_change(&input, cx);
            }
        })
        .detach();
        let explorer_input = cx.new(|cx| InputState::new(window, cx).placeholder("name"));
        cx.subscribe(
            &explorer_input,
            |this: &mut Self, input, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let name = input.read(cx).value().to_string();
                    let _ = this.explorer_commit_edit(name, cx);
                }
                // Clicking away commits a typed name and drops an empty or
                // rejected one, the way an IDE's inline rename behaves. A
                // blur from before the render that focuses the input (the
                // context menu closing) is not the user leaving.
                InputEvent::Blur => {
                    if this.explorer_focus_pending {
                        return;
                    }
                    let name = input.read(cx).value().to_string();
                    if name.trim().is_empty() || !this.explorer_commit_edit(name, cx) {
                        this.explorer_cancel_edit(cx);
                    }
                }
                _ => {}
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
            explorer: None,
            explorer_input,
            explorer_scroll: UniformListScrollHandle::default(),
            explorer_focus_pending: false,
            dirty: false,
            unsaved: HashMap::new(),
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
        self.leave_current_file(cx);
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
        self.leave_current_file(cx);
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
        self.leave_current_file(cx);
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
                // Switching tabs is the user moving between open files, not
                // a new artifact arriving: keep full-window if that is where
                // they are (`presentation_on_open` docks for the latter).
                let keep_mode = self.mode;
                self.open(path, source, old, workspace, cx);
                self.mode = keep_mode;
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

    // ----- Workspace explorer and editing (AGE-476) -----

    /// Show the explorer column rooted at `root`, opening the panel docked
    /// if it was closed. A tree already up on the same root is kept as is
    /// (its expanded folders and selection survive); another root replaces
    /// it.
    pub fn show_explorer(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self
            .explorer
            .as_ref()
            .is_none_or(|tree| tree.root() != root)
        {
            let mut tree = FileTree::new(root);
            if let Some(path) = self.path.as_ref() {
                tree.reveal(path);
            }
            self.explorer = Some(tree);
        }
        if self.mode == ArtifactMode::Closed {
            self.set_mode(ArtifactMode::Docked, cx);
        }
        cx.notify();
    }

    pub fn explorer_shown(&self) -> bool {
        self.explorer.is_some()
    }

    /// The header button: hide the tree, or bring it up on the workspace.
    fn toggle_explorer(&mut self, cx: &mut Context<Self>) {
        if self.explorer.is_some() {
            self.explorer = None;
            cx.notify();
        } else {
            let root = self.explorer_root(cx);
            self.show_explorer(root, cx);
        }
    }

    /// Where the tree is rooted: the workspace the open artifact came from,
    /// else the configured working directory, else the process cwd (which
    /// is what `workspace_dir = None` means for the tools too).
    fn explorer_root(&self, cx: &App) -> PathBuf {
        explorer_root_for(
            self.workspace_root.as_deref(),
            cx.try_global::<crate::settings::models::execution_settings::ExecutionSettingsModel>()
                .and_then(|settings| settings.workspace_dir.as_deref()),
        )
    }

    /// A click on a tree row. A plain click selects that row alone and
    /// activates it (folders toggle, files open); Ctrl/⌘ and Shift only
    /// change the selection (AGE-476 multi-select).
    pub(super) fn explorer_click(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        gesture: SelectGesture,
        cx: &mut Context<Self>,
    ) {
        match gesture {
            SelectGesture::Single => self.explorer_activate(path, is_dir, cx),
            SelectGesture::Toggle | SelectGesture::Range => {
                if let Some(tree) = self.explorer.as_mut() {
                    tree.click_select(path, gesture);
                    cx.notify();
                }
            }
        }
    }

    /// Drop of dragged entries onto `dest` (AGE-476 drag-to-move): the
    /// tree moves what can move and the panel's tabs follow.
    pub(super) fn explorer_drop(
        &mut self,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        let ops = tree.move_entries(&paths, &dest);
        for op in ops {
            self.apply_file_op(op, cx);
        }
        cx.notify();
    }

    /// A plain click on a tree row: folders toggle, files open in the panel
    /// the way a tool-card click does.
    pub(super) fn explorer_activate(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        tree.select(Some(path.clone()));
        if is_dir {
            tree.toggle(&path);
            cx.notify();
            return;
        }
        let source = read_artifact_source(&path);
        let workspace = Some(self.explorer_root(cx).display().to_string());
        let keep_mode = self.mode;
        self.open(path, source, None, workspace, cx);
        // Opening from the tree is browsing, not a new artifact arriving:
        // stay full-window if that is where the user is.
        self.mode = keep_mode;
        cx.notify();
    }

    /// The header's "new file"/"new folder" buttons: create next to the
    /// selection (inside it, if it is a folder).
    pub(super) fn explorer_begin_new(
        &mut self,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_ref() else {
            return;
        };
        let dir = tree.target_dir();
        let edit = if folder {
            PendingEdit::NewFolder { dir }
        } else {
            PendingEdit::NewFile { dir }
        };
        self.explorer_begin_edit(edit, window, cx);
    }

    pub(super) fn explorer_begin_edit(
        &mut self,
        edit: PendingEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        let text = edit.initial_text();
        tree.begin_edit(edit);
        self.explorer_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        // Focused from the next render, once the row exists.
        self.explorer_focus_pending = true;
        cx.notify();
    }

    pub(super) fn explorer_cancel_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = self.explorer.as_mut()
            && tree.pending().is_some()
        {
            tree.cancel_edit();
            cx.notify();
        }
    }

    /// `false` when the name was refused; the tree then carries the reason.
    fn explorer_commit_edit(&mut self, name: String, cx: &mut Context<Self>) -> bool {
        let Some(tree) = self.explorer.as_mut() else {
            return true;
        };
        if tree.pending().is_none() {
            return true;
        }
        let Some(op) = tree.commit_edit(&name) else {
            // The edit stays open with the tree's error under it.
            cx.notify();
            return false;
        };
        self.apply_file_op(op, cx);
        true
    }

    /// Ask before deleting one entry or a multi-selection.
    pub(super) fn explorer_confirm_delete(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let what = delete_prompt(&paths);
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let paths = paths.clone();
            dialog
                .confirm()
                .title("Delete")
                .child(div().px_4().py_2().text_sm().child(what.clone()))
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger),
                )
                .on_ok(move |_, _, cx| {
                    let paths = paths.clone();
                    entity.update(cx, |this, cx| this.explorer_delete(paths, cx));
                    true
                })
        });
    }

    fn explorer_delete(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        let ops = tree.delete_many(&paths);
        for op in ops {
            self.apply_file_op(op, cx);
        }
        cx.notify();
    }

    pub(super) fn explorer_refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = self.explorer.as_mut() {
            tree.sync(true);
            cx.notify();
        }
    }

    /// Keep the panel's own state in step with what the tree just did to
    /// the disk: a new file opens for editing, a renamed file's tabs and
    /// buffers follow it, a deleted file's tabs close.
    fn apply_file_op(&mut self, op: FileOp, cx: &mut Context<Self>) {
        match op {
            FileOp::Created(path) => {
                if path.is_file() {
                    self.explorer_activate(path, false, cx);
                    // A fresh file has nothing to render; go straight to
                    // the editor.
                    self.tab = if self
                        .path
                        .as_ref()
                        .is_some_and(|p| is_markdown_artifact_path(p))
                    {
                        1
                    } else {
                        0
                    };
                }
            }
            FileOp::Renamed { from, to } => {
                let renamed = |path: &Path| rebase_path(path, &from, &to);
                for (path, _, _) in self.files.iter_mut() {
                    if let Some(next) = renamed(path) {
                        *path = next;
                    }
                }
                let moved: Vec<(PathBuf, String)> = self
                    .unsaved
                    .iter()
                    .filter_map(|(path, text)| renamed(path).map(|next| (next, text.clone())))
                    .collect();
                self.unsaved.retain(|path, _| renamed(path).is_none());
                self.unsaved.extend(moved);
                if let Some(next) = self.path.as_ref().and_then(|p| renamed(p)) {
                    self.path = Some(next.clone());
                    self.loaded_version = artifact_version(&next);
                    // The editor keeps its text; only the name changed.
                }
            }
            FileOp::Deleted(path) => {
                let closing: Vec<usize> = self
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, (p, _, _))| p.starts_with(&path))
                    .map(|(ix, _)| ix)
                    .collect();
                for ix in closing.into_iter().rev() {
                    self.remove_file_tab(ix, cx);
                }
            }
        }
        cx.notify();
    }

    /// Editor change event: dirty is "differs from the disk text".
    fn note_editor_change(&mut self, input: &Entity<InputState>, cx: &mut Context<Self>) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if !is_text_artifact_path(path) {
            return;
        }
        let dirty = input.read(cx).value().as_ref() != self.source.as_str();
        if dirty != self.dirty {
            self.dirty = dirty;
            cx.notify();
        }
    }

    /// Before the panel shows something else: keep the current file's
    /// unsaved buffer so coming back to its tab restores it.
    fn leave_current_file(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            if self.dirty {
                let text = self.editor.read(cx).value().to_string();
                self.unsaved.insert(path, text);
            } else {
                self.unsaved.remove(&path);
            }
        }
        self.dirty = false;
    }

    /// Write the editor's buffer to the file on screen (Save button,
    /// Ctrl/Cmd+S). The rendered view and the outline follow the new text;
    /// the editor is left alone so the cursor stays put.
    fn save_current(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if !self.dirty || !is_text_artifact_path(&path) {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        if let Err(e) = std::fs::write(&path, &text) {
            warn!(path = %path.display(), error = %e, "Saving the artifact failed");
            if let Some(tree) = self.explorer.as_mut() {
                tree.set_error(format!("Could not save '{}': {e}", path.display()));
            }
            cx.notify();
            return;
        }
        self.source = text.clone();
        self.rendered = text.clone();
        self.headings = markdown_headings(&text);
        // Re-sync the outline without re-syncing the editor.
        self.outline_synced_gen = u64::MAX;
        if let Some((_, source, _)) = self.files.iter_mut().find(|(p, _, _)| p == &path) {
            *source = text;
        }
        self.loaded_version = artifact_version(&path);
        self.stale = false;
        self.unsaved.remove(&path);
        self.dirty = false;
        if is_tabular_path(&path) {
            let workspace = self.workspace_root.clone();
            self.start_tabular_load(path, workspace, cx);
        }
        if let Some(tree) = self.explorer.as_mut() {
            tree.sync(true);
        }
        cx.notify();
    }

    /// Whether the file tab at `ix` carries edits that are not on disk.
    fn file_tab_dirty(&self, ix: usize) -> bool {
        let Some((path, _, _)) = self.files.get(ix) else {
            return false;
        };
        (self.dirty && self.path.as_ref() == Some(path)) || self.unsaved.contains_key(path)
    }

    /// The × on a file tab: a dirty tab asks first.
    fn close_file_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.file_tab_dirty(ix) {
            self.remove_file_tab(ix, cx);
            return;
        }
        let Some((path, _, _)) = self.files.get(ix) else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            dialog
                .confirm()
                .title("Unsaved changes")
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .child(format!("Discard unsaved changes to '{name}'?")),
                )
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default()
                        .ok_text("Discard")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger),
                )
                .on_ok(move |_, _, cx| {
                    entity.update(cx, |this, cx| this.remove_file_tab(ix, cx));
                    true
                })
        });
    }

    /// Drop a file tab; if it was on screen, show its neighbour, or an empty
    /// panel (kept open for the explorer) when it was the last one.
    fn remove_file_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.files.len() {
            return;
        }
        let (path, _, _) = self.files.remove(ix);
        self.unsaved.remove(&path);
        if self.path.as_ref() != Some(&path) || self.browser_shown {
            cx.notify();
            return;
        }
        self.dirty = false;
        let neighbour = self
            .files
            .get(ix.min(self.files.len().saturating_sub(1)))
            .cloned();
        match neighbour {
            Some((next, source, old)) if !self.files.is_empty() => {
                let workspace = self.workspace_root.clone();
                let keep_mode = self.mode;
                self.open(next, source, old, workspace, cx);
                self.mode = keep_mode;
            }
            _ => self.clear_document(cx),
        }
    }

    /// Nothing on screen: the state `new` starts in, minus the explorer.
    fn clear_document(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.set_pdf(PdfPreview::Idle, cx);
        self.pptx = PptxPreview::Idle;
        self.tabular = TabularPreview::Idle;
        self.chart = None;
        self.headings.clear();
        self.tab = 0;
        self.stale = false;
        self.dirty = false;
        self.load_gen = self.load_gen.wrapping_add(1);
        if let Some(tree) = self.explorer.as_mut() {
            tree.select(None);
        }
        if self.explorer.is_none() && self.browser_manager.is_none() {
            self.set_mode(ArtifactMode::Closed, cx);
        }
        cx.notify();
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
        self.leave_current_file(cx);
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
        if let Some(tree) = self.explorer.as_mut() {
            tree.reveal(&path);
        }
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
            // `read_artifact_source`, and both the slide raster and the Source
            // tab are filled once `start_pptx_load` returns.
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
            self.start_pptx_load(0, true, cx);
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
        self.leave_current_file(cx);
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
        // Reload means "give me the disk version": drop the edits.
        self.unsaved.remove(&path);
        self.dirty = false;
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

    /// Render one slide off the main thread (AGE-343).
    ///
    /// `with_text` also fills `source` — the deck's extracted text — for the
    /// Source tab, which is why the editor's sync generation is invalidated on
    /// the way out. `read_artifact_source` deliberately hands back `""` for a
    /// `.pptx`, so without this the Source tab stays blank. Only the open
    /// pays for it; turning a slide is a raster and nothing else.
    fn start_pptx_load(&mut self, slide: usize, with_text: bool, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.pptx = PptxPreview::Loading { slide };
        // The slide shares the PDF page's raster width, so a wide panel gets
        // a sharp slide from the first turn; unlike the PDF page it is not
        // re-rastered on resize — the fit-to-width canvas scales it instead.
        let width = self.pdf_raster_width;
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || -> Result<_, PptxRenderError> {
                let total = slide_count(&path)? as usize;
                let png = render_slide(&path, slide as u32, width)?;
                let decoded = image::open(&png)
                    .map_err(|e| PptxRenderError::Raster(PdfThumbnailError::Image(e.to_string())))?
                    .into_rgba8();
                let image = render_image_from_rgba_buffer(decoded);
                // Extraction failing must not cost the user the slides: the
                // Source tab is the lesser half of the workbench.
                let text = with_text.then(|| {
                    read_pptx_slides(&path, false)
                        .map(|slides| pptx_slides_to_text(&slides))
                        .unwrap_or_default()
                });
                Ok((total, image, text))
            })
            .await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok((total, image, text))) => {
                        if let Some(text) = text {
                            this.source = text;
                            // The editor already synced against the empty
                            // source for this generation; force it to pick the
                            // extracted text up. `u64::MAX` is the same "never
                            // synced" sentinel `new()` uses.
                            this.editor_synced_gen = u64::MAX;
                        }
                        this.pptx = PptxPreview::Ready {
                            slide,
                            total,
                            image,
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

    /// Same pager semantics as [`Self::turn_pdf_page`] — clamped at both
    /// ends, no wrap. The deck's PDF is already cached, so this re-rasters one
    /// slide rather than re-rendering the deck.
    fn turn_pptx_slide(&mut self, next: bool, cx: &mut Context<Self>) {
        let PptxPreview::Ready { slide, total, .. } = &self.pptx else {
            return;
        };
        let new_slide = next_slide_index(*slide, *total, next);
        if new_slide == *slide {
            return;
        }
        self.start_pptx_load(new_slide, false, cx);
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
        // An unsaved buffer stashed by `leave_current_file` wins over the
        // disk text (AGE-476); `note_editor_change` then marks it dirty again.
        let text = self
            .path
            .as_ref()
            .and_then(|path| self.unsaved.get(path).cloned())
            .unwrap_or_else(|| self.source.clone());
        self.editor.update(cx, |editor, cx| {
            editor.set_highlighter(language, cx);
            editor.set_value(text, window, cx);
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

/// A rastered page (PDF page or deck slide) fitted to the panel width
/// (AGE-472): as wide as the scroll container and as tall as its aspect
/// ratio says, with the container's width coming from the previous frame's
/// prepaint. The first frame after a size change therefore lays the page
/// out at the old width; `Contain` keeps that frame unstretched and the
/// prepaint asks for another frame, which lands on the new width. Zero
/// width (nothing painted yet) means a 1 px strip whose only job is to run
/// that prepaint.
fn fit_to_width_page(
    image: &Arc<RenderImage>,
    panel_width: &Rc<RefCell<Pixels>>,
    cx: &App,
) -> impl IntoElement {
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
    canvas(
        move |bounds, window, _cx| {
            let mut known = width_for_prepaint.borrow_mut();
            if (*known - bounds.size.width).abs() >= px(1.0) {
                *known = bounds.size.width;
                window.request_animation_frame();
            }
        },
        move |bounds, _, window, _| {
            let fitted = ObjectFit::Contain.get_bounds(bounds, page_image.size(0));
            let corners = Corners::all(corner_radius).clamp_radii_for_quad_size(fitted.size);
            if let Err(e) = window.paint_image(fitted, corners, page_image.clone(), 0, false) {
                warn!(error = %e, "painting the page failed");
            }
        },
    )
    .w_full()
    .h(px(page_height.max(1.0)))
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
            let page_canvas = fit_to_width_page(image, &panel_width, cx);
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

/// Slide workbench (AGE-138). Same chrome as [`pdf_rendered_body`]: Prev,
/// a position label, Next, and one slide at a time in a scroller. Since
/// AGE-343 the slide itself is a raster from `pptx_render`, so a deck's
/// theme, images and charts survive into the panel.
fn pptx_rendered_body(
    pptx: &PptxPreview,
    panel_width: Rc<RefCell<Pixels>>,
    entity: Entity<ArtifactView>,
    cx: &mut App,
) -> AnyElement {
    match pptx {
        PptxPreview::Idle | PptxPreview::Loading { .. } => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Rendering slide…")
            .into_any_element(),
        // A deck we cannot open or render gets the muted one-liner, never a
        // panic and never a dump of the ZIP.
        PptxPreview::Error(message) => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        // An otherwise valid package with no slides renders to an empty PDF —
        // say so rather than showing "Slide 1 of 0".
        PptxPreview::Ready { total, .. } if *total == 0 => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("This presentation has no slides.")
            .into_any_element(),
        PptxPreview::Ready {
            slide,
            total,
            image,
        } => {
            let index = (*slide).min(total.saturating_sub(1));
            let (label, can_prev, can_next) = slide_pager(index, *total);
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
                        .child(fit_to_width_page(image, &panel_width, cx)),
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

/// The confirm text for deleting `paths`: names the one entry, or counts
/// several and lists the first few.
fn delete_prompt(paths: &[PathBuf]) -> String {
    let name = |path: &PathBuf| {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    };
    match paths {
        [one] if one.is_dir() => format!("Delete the folder '{}' and everything in it?", name(one)),
        [one] => format!("Delete '{}'?", name(one)),
        many => {
            const SHOWN: usize = 5;
            let mut listed: Vec<String> = many.iter().take(SHOWN).map(name).collect();
            if many.len() > SHOWN {
                listed.push(format!("… and {} more", many.len() - SHOWN));
            }
            format!(
                "Delete {} items? Folders go with everything in them. {}",
                many.len(),
                listed.join(", ")
            )
        }
    }
}

/// Ctrl+S, or ⌘S on macOS.
fn is_save_keystroke(keystroke: &Keystroke) -> bool {
    let modifier = if cfg!(target_os = "macos") {
        keystroke.modifiers.platform
    } else {
        keystroke.modifiers.control
    };
    modifier && !keystroke.modifiers.shift && !keystroke.modifiers.alt && keystroke.key == "s"
}

/// Anything the Source editor can hold and Save can write back: not a
/// binary the panel renders from disk.
fn is_text_artifact_path(path: &Path) -> bool {
    !is_pdf_path(path) && !is_pptx_path(path) && !is_image_path(path)
}

/// Where the explorer roots (AGE-476): the artifact's own workspace, the
/// configured working directory, or the process cwd — the same fallback
/// order the tools use for a relative path.
fn explorer_root_for(workspace_root: Option<&str>, setting: Option<&str>) -> PathBuf {
    workspace_root
        .or(setting)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
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
        // The explorer follows the disk on a timer (AGE-476); a `true`
        // means a listing changed, which this render already reflects.
        if let Some(tree) = self.explorer.as_mut() {
            tree.sync(false);
            if self.explorer_focus_pending {
                self.explorer_focus_pending = false;
                if tree.pending().is_some() {
                    self.explorer_input
                        .update(cx, |input, cx| input.focus(window, cx));
                }
            }
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

        // Nothing on screen (AGE-476): the explorer is up with no file
        // chosen yet, or the last tab was closed.
        let nothing_open = !session_review
            && !is_browser
            && path_ref.is_none()
            && chart.is_none()
            && !matches!(tabular, TabularPreview::Ready(_));
        let dirty = self.dirty;
        let explorer_shown = self.explorer.is_some();

        let body = if nothing_open {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::File).size_6())
                .child(if explorer_shown {
                    "Select a file to open it here."
                } else {
                    "Nothing open."
                })
                .into_any_element()
        } else if session_review {
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
                                            pdf_panel_width.clone(),
                                            entity.clone(),
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

        let title = if nothing_open {
            "Explorer".to_string()
        } else if session_review {
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
        // With the explorer up the bar shows for a single file too, so its
        // dot and × are there (AGE-476); without it, one file needs no bar.
        let show_file_tabs = !session_review
            && (tab_entries.len() > 1 || (explorer_shown && !tab_entries.is_empty()));
        let file_tab_bar = show_file_tabs.then(|| {
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
                    if let ArtifactTabTarget::File(ix) = entry.target {
                        // A file tab gets a dot while it carries unsaved
                        // edits and its own × (AGE-476), laid out like the
                        // browser tab's close button below.
                        let is_dirty = self.file_tab_dirty(ix);
                        tab = tab.suffix(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .when(is_dirty, |this| {
                                    this.child(
                                        div()
                                            .w(px(6.))
                                            .h(px(6.))
                                            .rounded_full()
                                            .bg(cx.theme().foreground),
                                    )
                                })
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "artifact-file-tab-close-{ix}"
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::Close).size_3())
                                    .tooltip("Close")
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            entity.update(cx, |this, cx| {
                                                this.close_file_tab(ix, window, cx)
                                            });
                                        }
                                    }),
                                ),
                        );
                    }
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
                        !session_review
                            && !nothing_open
                            && matches!(copy_control, ArtifactCopy::Source),
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
                        !session_review
                            && !nothing_open
                            && matches!(copy_control, ArtifactCopy::Menu),
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
                    .when(!session_review && dirty, |this| {
                        this.child(
                            Button::new("artifact-save")
                                .primary()
                                .small()
                                .label("Save")
                                .tooltip(if cfg!(target_os = "macos") {
                                    "Save (⌘S)"
                                } else {
                                    "Save (Ctrl+S)"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.save_current(cx);
                                })),
                        )
                    })
                    .when(!session_review, |this| {
                        this.child(
                            Button::new("artifact-explorer-toggle")
                                .small()
                                .icon(Icon::new(IconName::PanelLeft).size_3())
                                .tooltip(if explorer_shown {
                                    "Hide the file explorer"
                                } else {
                                    "Show the file explorer"
                                })
                                .when(explorer_shown, |b| b.selected(true))
                                .when(!explorer_shown, |b| b.ghost())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_explorer(cx);
                                })),
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

        // The explorer column sits left of whatever the body is, under the
        // shared header; review mode has no per-file body to sit next to.
        let explorer_column = self
            .explorer
            .as_ref()
            .filter(|_| !session_review)
            .map(|tree| {
                render_file_explorer(
                    tree,
                    &self.explorer_input,
                    self.explorer_scroll.clone(),
                    entity.clone(),
                    cx,
                )
            });
        let body = match explorer_column {
            Some(column) => div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .size_full()
                .child(column)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .h_full()
                        .child(body),
                )
                .into_any_element(),
            None => body,
        };

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
                    } else if is_save_keystroke(&event.keystroke) {
                        entity.update(cx, |this, cx| this.save_current(cx));
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
