//! File tabs, the source editor and saving, and the PDF, PPTX and tabular
//! previews.

use super::*;

impl ArtifactView {
    // ----- Sidebar Files-mode interop (AGE-480) -----
    //
    // The workspace tree used to be a column inside this panel (AGE-476);
    // it now lives in the left sidebar (`sidebar_view.rs` /
    // `sidebar_file_tree.rs`), which owns the `FileTree` and all of the
    // create/rename/delete/drag logic. This panel is back to being a pure
    // viewer: it only needs to (1) open a path the sidebar activated, and
    // (2) keep its own open tabs and unsaved buffers in step with what the
    // tree just did to the disk.

    /// Open a path the sidebar's tree (or the Cmd+P quick-open picker)
    /// activated, the way a tool card's "open" click does. Mirrors the old
    /// `explorer_activate`'s file branch: opening this way is browsing, not
    /// a new artifact arriving, so the panel's current mode is kept unless
    /// it was closed (in which case it docks, the way `show_explorer` used
    /// to).
    pub fn open_from_sidebar(
        &mut self,
        path: PathBuf,
        source: String,
        workspace_root: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let was_closed = self.mode == ArtifactMode::Closed;
        let keep_mode = self.mode;
        self.open(path, source, None, workspace_root, cx);
        self.mode = if was_closed {
            ArtifactMode::Docked
        } else {
            keep_mode
        };
        cx.notify();
    }

    /// Keep this panel's own state in step with what the sidebar's tree
    /// just did to the disk: a new file opens for editing, a renamed file's
    /// tabs and buffers follow it, a deleted file's tabs close.
    pub fn apply_file_op(
        &mut self,
        op: FileOp,
        workspace_root: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match op {
            FileOp::Created(path) => {
                if path.is_file() {
                    let source = read_artifact_source(&path);
                    self.open_from_sidebar(path, source, workspace_root, cx);
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
    pub(super) fn note_editor_change(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn leave_current_file(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn save_current(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if !self.dirty || !is_text_artifact_path(&path) {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        if let Err(e) = std::fs::write(&path, &text) {
            warn!(path = %path.display(), error = %e, "Saving the artifact failed");
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
        cx.notify();
    }

    /// Whether the file tab at `ix` carries edits that are not on disk.
    pub(super) fn file_tab_dirty(&self, ix: usize) -> bool {
        let Some((path, _, _)) = self.files.get(ix) else {
            return false;
        };
        (self.dirty && self.path.as_ref() == Some(path)) || self.unsaved.contains_key(path)
    }

    /// The × on a file tab: a dirty tab asks first.
    pub(super) fn close_file_tab(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Drop a file tab; if it was on screen, show its neighbour, or close
    /// the panel (unless a browser is still up) when it was the last one.
    pub(super) fn remove_file_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
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

    /// Nothing on screen: the state `new` starts in.
    pub(super) fn clear_document(&mut self, cx: &mut Context<Self>) {
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
        if self.browser_manager.is_none() {
            self.set_mode(ArtifactMode::Closed, cx);
        }
        cx.notify();
    }

    /// Replace the PDF slot, releasing the outgoing page's raster from the
    /// sprite atlas (AGE-472). Hand-painted `RenderImage`s are not evicted
    /// by any cache, so without this every page turn and every re-raster
    /// would leave a texture behind. Deferred because callers may be inside
    /// a window update, during which that window is absent from
    /// `App::windows` and `drop_image` would skip it.
    pub(super) fn set_pdf(&mut self, next: PdfPreview, cx: &mut App) {
        if let PdfPreview::Ready { image, .. } = mem::replace(&mut self.pdf, next) {
            cx.defer(move |cx| cx.drop_image(image, None));
        }
    }

    pub(super) fn start_pdf_load(&mut self, page: u32, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        let width = self.pdf_raster_width;
        self.set_pdf(PdfPreview::Loading, cx);
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
    pub(super) fn sync_pdf_raster_width(&mut self, window: &Window, cx: &mut Context<Self>) {
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
            this.update(cx, |this, cx| this.reraster_pdf_page(cx)).map_err(|e| debug!(error = ?e, "Artifact panel closed before the PDF page was re-rastered")).ok();
        }));
    }

    /// Re-render the page on screen at `pdf_raster_width` if it is not
    /// already there, swapping the image in place once decoded — the old
    /// raster stays up meanwhile, so the panel never blanks. A page turn or
    /// another resize while the render is in flight makes the result stale;
    /// it is then dropped rather than applied over the newer state.
    pub(super) fn reraster_pdf_page(&mut self, cx: &mut Context<Self>) {
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
            .map_err(
                |e| debug!(error = ?e, "Artifact panel closed before the PDF re-raster landed"),
            )
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
    pub(super) fn start_pptx_load(
        &mut self,
        slide: usize,
        with_text: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.pptx = PptxPreview::Loading;
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

    pub(super) fn start_tabular_load(
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

    pub(super) fn turn_pdf_page(&mut self, next: bool, cx: &mut Context<Self>) {
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
    pub(super) fn turn_pptx_slide(&mut self, next: bool, cx: &mut Context<Self>) {
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
}
