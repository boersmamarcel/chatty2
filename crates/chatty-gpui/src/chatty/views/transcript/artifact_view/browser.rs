//! The browser half of the artifact panel: session, tabs, screencast,
//! control hand-off and forwarded input.

use super::*;

impl ArtifactView {
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
    pub(super) fn start_browser_session(
        &mut self,
        manager: Arc<BrowserManager>,
        cx: &mut Context<Self>,
    ) {
        self.browser = BrowserPreview::Starting;
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        // Mirror a first-use Chrome download into the panel (AGE-565). The
        // watcher is dropped with the resolve below, so it lives exactly as
        // long as the wait it describes.
        let mut progress_rx = manager.watch_download_progress();
        let progress_watcher = cx.spawn(async move |this, cx| {
            let mut shown_percent = None;
            while progress_rx.changed().await.is_ok() {
                let Some(fraction) = *progress_rx.borrow_and_update() else {
                    continue;
                };
                let percent = (fraction.clamp(0.0, 1.0) * 100.0) as u32;
                if shown_percent == Some(percent) {
                    continue;
                }
                shown_percent = Some(percent);
                let alive = this
                    .update(cx, |this, cx| {
                        if this.load_gen == load_id
                            && matches!(
                                this.browser,
                                BrowserPreview::Starting | BrowserPreview::Downloading(_)
                            )
                        {
                            this.browser = BrowserPreview::Downloading(fraction);
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let session = manager.session().await;
            drop(progress_watcher);
            let session = match session {
                Ok(session) => session,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.load_gen == load_id {
                            this.browser = BrowserPreview::Error(e.to_string());
                            cx.notify();
                        }
                    })
                    .map_err(|e| debug!(error = ?e, "Artifact panel closed before the browser error was shown"))
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
            .map_err(|e| debug!(error = ?e, "Artifact panel closed before the browser session attached"))
            .ok();
        })
        .detach();
    }

    /// Follow the session's tab list into `browser_tabs` (AGE-473). Keyed on
    /// its own generation, not `load_gen`: it must survive a pause (a file
    /// shown in front of the browser) so the tab entries stay live, and is
    /// torn down only by [`Self::drop_browser`].
    pub(super) fn start_tab_watcher(
        &mut self,
        session: Arc<BrowserSession>,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn start_browser_stream(
        &mut self,
        session: Arc<BrowserSession>,
        cx: &mut Context<Self>,
    ) {
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
                    .map_err(|e| debug!(error = ?e, "Artifact panel closed before the browser error was shown"))
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
                    .map_err(|e| debug!(error = ?e, "Artifact panel closed before the browser session end was shown"))
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
    pub(super) fn pause_browser(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn drop_browser(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn navigate_browser_as_user(&mut self, raw_url: String, cx: &mut Context<Self>) {
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
            .map_err(|e| debug!(error = ?e, "Artifact panel closed before the address bar updated"))
            .ok();
        })
        .detach();
    }

    /// The refresh button (AGE-156) reloads the current page — takes
    /// control first, the same as any other user-initiated action. A no-op
    /// if the browser artifact isn't open.
    pub(super) fn reload_browser(&mut self, cx: &mut Context<Self>) {
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
            .map_err(|e| debug!(error = ?e, "Artifact panel closed before the reload finished"))
            .ok();
        })
        .detach();
    }

    /// The user picks a tab in the strip (AGE-473) — takes control first,
    /// like the address bar does: the agent's tools now address that tab.
    /// A no-op if the browser artifact isn't open.
    pub(super) fn select_browser_tab(&mut self, id: String, cx: &mut Context<Self>) {
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
    pub(super) fn close_browser_tab(&mut self, id: String, cx: &mut Context<Self>) {
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

    /// Queue a mouse event for the input-forwarding drain (AGE-156). A
    /// cheap, synchronous, non-blocking send — the actual CDP call happens
    /// on the background task started in `open_browser`.
    pub(super) fn send_browser_mouse(&self, input: MouseInput) {
        if let Some(tx) = &self.browser_mouse_tx {
            let _ = tx.send(input);
        }
    }

    /// Queue a keyboard event for the input-forwarding drain (AGE-156).
    pub(super) fn send_browser_key(&self, input: KeyInput) {
        if let Some(tx) = &self.browser_key_tx {
            let _ = tx.send(input);
        }
    }

    /// Mirror `browser_current_url` into the address bar's `InputState`
    /// (AGE-156). Split from wherever the URL is learned because
    /// `InputState::set_value` needs `&mut Window`, which the background
    /// task watching `BrowserSession::watch_url` does not have.
    pub(super) fn sync_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn sync_browser_viewport_size(&mut self, cx: &mut Context<Self>) {
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
}
