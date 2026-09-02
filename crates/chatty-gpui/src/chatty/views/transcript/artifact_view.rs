use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chatty_core::services::browser::{BrowserManager, ScreencastFrame, ScreencastUpdate};
use chatty_core::services::pdf_thumbnail::{
    PREVIEW_WIDTH, PdfThumbnailError, pdf_page_count, render_pdf_page,
};
use chatty_core::tools::chart_tool::ChartSpec;
use chatty_core::tools::data_query_tool::{
    FILE_PREVIEW_MAX_ROWS, TablePreview, load_file_table_preview,
};
use std::ops::Range;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::input::{Input, InputState, Position};
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
const BROWSER_VIEWPORT_WIDTH: u32 = 960;
const BROWSER_VIEWPORT_HEIGHT: u32 = 600;

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
        if let Some(manager) = self.browser_manager.take() {
            cx.background_spawn(async move {
                manager.stop_screencast().await;
            })
            .detach();
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
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

fn browser_rendered_body(browser: &BrowserPreview, cx: &App) -> AnyElement {
    match browser {
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
        BrowserPreview::Frame(image) => div()
            .id("artifact-browser-frame")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .child(
                img(image.clone())
                    .w_full()
                    .object_fit(ObjectFit::Contain)
                    .rounded_md(),
            )
            .into_any_element(),
    }
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
                .child(browser_rendered_body(&browser, cx))
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
