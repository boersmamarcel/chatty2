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
use tracing::{debug, warn};

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
use super::file_tree::{FileOp, rebase_path};
use super::markdown_mermaid::{DocPart, split_mermaid};
use super::run_pin::{RunPin, RunPinKind};
use super::session_review_panel::{ReviewFileSection, SessionReviewPanel};
use super::table::render_table_preview_view;
use crate::chatty::services::MermaidRendererService;
use crate::chatty::views::chart_renderer::render_chart_panel;
use crate::chatty::views::diff_view_component::diff_line_stats_fast;
use crate::chatty::views::mermaid_component::MermaidComponent;

mod bodies;
mod browser;
mod browser_input;
mod documents;

use bodies::*;
use browser_input::*;

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
/// AGE-486: how wide one outline entry may draw. `tree()` renders into a
/// `uniform_list` with `ListSizingBehavior::Auto`, so a row is as wide as its
/// own content and as tall as the *first* row — `w_full` never constrains it,
/// and a heading long enough to wrap paints its second line over the entry
/// below. Pinning the label to the rail's usable width (minus the rail's own
/// `p_1` and the `ListItem`'s `px_3`) keeps every entry on one ellipsized line.
const OUTLINE_LABEL_WIDTH: f32 = OUTLINE_WIDTH - 2.0 * 4.0 - 2.0 * 12.0;

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

/// What the panel shows ([`ArtifactView::showing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Showing {
    File(PathBuf),
    Browser(String),
    Review,
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
    Loading,
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
    Loading,
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
    /// First launch fetching the pinned Chrome build (AGE-565), 0.0..=1.0.
    Downloading(f32),
    Frame(Arc<RenderImage>),
    Error(String),
}

/// The Rendered tab's markdown, split around its mermaid diagrams with each
/// diagram already rendered to an SVG file (AGE-566). Rebuilt only when the
/// text or the theme's light/dark mode changes, not every frame.
#[derive(Default)]
struct RenderedDoc {
    key: Option<(u64, bool)>,
    parts: Rc<Vec<RenderedPart>>,
}

enum RenderedPart {
    Markdown(SharedString),
    /// `svg` is `None` when the diagram failed to render; the component then
    /// shows its source.
    Mermaid {
        source: String,
        svg: Option<PathBuf>,
    },
}

/// One artifact workbench entity: Closed | Docked | Full. Reparent, do not rebuild.
pub struct ArtifactView {
    pub mode: ArtifactMode,
    pub path: Option<PathBuf>,
    pub rendered: String,
    rendered_doc: RenderedDoc,
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
        Self {
            mode: ArtifactMode::Closed,
            path: None,
            rendered: String::new(),
            rendered_doc: RenderedDoc::default(),
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

    /// What the panel shows, for the composer to name in the next message:
    /// the session's changes, the browser's page, or a file on disk. `None`
    /// while closed, or for a table or chart that is not a file.
    pub fn showing(&self) -> Option<Showing> {
        if self.mode == ArtifactMode::Closed {
            return None;
        }
        if self.session_review {
            return Some(Showing::Review);
        }
        if self.browser_shown {
            return (!self.browser_current_url.is_empty())
                .then(|| Showing::Browser(self.browser_current_url.clone()));
        }
        self.path
            .as_ref()
            .filter(|path| path.is_file())
            .map(|path| Showing::File(path.clone()))
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

    /// Re-split the Rendered tab's markdown around its mermaid diagrams when
    /// the text or light/dark mode changed (AGE-566). Renders each diagram
    /// once per change; every other frame reuses the cached parts.
    fn sync_rendered_doc(&mut self, cx: &App) {
        if !self
            .path
            .as_ref()
            .is_some_and(|path| is_markdown_artifact_path(path))
        {
            return;
        }
        use std::hash::{Hash, Hasher};
        let is_dark = cx.theme().mode.is_dark();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.rendered.hash(&mut hasher);
        let key = (hasher.finish(), is_dark);
        if self.rendered_doc.key == Some(key) {
            return;
        }
        let parts = split_mermaid(&self.rendered)
            .into_iter()
            .map(|part| match part {
                DocPart::Markdown(text) => RenderedPart::Markdown(text.into()),
                DocPart::Mermaid(source) => {
                    let svg = cx.try_global::<MermaidRendererService>().and_then(|svc| {
                        svc.render_to_svg_file(&source, is_dark)
                            .map_err(|e| warn!(error = ?e, "Failed to render mermaid diagram"))
                            .ok()
                    });
                    RenderedPart::Mermaid { source, svg }
                }
            })
            .collect();
        self.rendered_doc = RenderedDoc {
            key: Some(key),
            parts: Rc::new(parts),
        };
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
            self.sync_rendered_doc(cx);
            if self.browser_shown {
                self.sync_browser_address(window, cx);
                self.sync_browser_viewport_size(cx);
            }
            self.sync_pdf_raster_width(window, cx);
        }
        let tab = self.tab;
        let source = self.source.clone();
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
        let rendered_parts = self.rendered_doc.parts.clone();
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

        // Nothing on screen: no tab is open (AGE-480: opening one is now the
        // sidebar's job, not this panel's).
        let nothing_open = !session_review
            && !is_browser
            && path_ref.is_none()
            && chart.is_none()
            && !matches!(tabular, TabularPreview::Ready(_));
        let dirty = self.dirty;

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
                .child("Nothing open.")
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
                                        // AGE-486: one ellipsized line per entry, so a
                                        // wrapped heading can't paint over the row below
                                        // it (see `OUTLINE_LABEL_WIDTH`).
                                        ListItem::new(entry.item().id.clone()).child(
                                            div()
                                                .w(px(OUTLINE_LABEL_WIDTH))
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .pl(px(8.) * entry.depth() as f32)
                                                .truncate()
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
                                                &rendered_parts,
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
        // A single open file needs no bar; the bar carries its dot/× once a
        // second file (or browser tab) is open.
        let show_file_tabs = !session_review && tab_entries.len() > 1;
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

        // AGE-480: the tree used to sit left of this as a column (AGE-476);
        // it now lives in the sidebar, so `body` is the whole panel.

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
