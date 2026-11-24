//! The rendered-body builders for each kind of artifact, and the raster
//! helpers they share.

use super::*;

pub(super) fn headings_to_tree_items(headings: &[ArtifactHeading]) -> Vec<TreeItem> {
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
pub(super) fn render_image_from_rgba(frame: &ScreencastFrame) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.to_vec())
        .expect("screencast frame dimensions match its own buffer length");
    render_image_from_rgba_buffer(buffer)
}

pub(super) fn render_image_from_rgba_buffer(mut buffer: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Render one PDF page at `width` device pixels and decode it for the
/// canvas (AGE-472). Blocking — pdfium plus a PNG round trip through the
/// preview cache — so callers run it under `spawn_blocking`. Returns the
/// document's page count alongside, which the pager needs on first load.
pub(super) fn load_pdf_page_image(
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
pub(super) fn pdf_raster_width_for(panel_width: f32, scale_factor: f32) -> u32 {
    let device = (panel_width * scale_factor).max(0.0).ceil() as u32;
    let bucketed = device.div_ceil(PDF_RASTER_STEP) * PDF_RASTER_STEP;
    bucketed.clamp(PREVIEW_WIDTH, PDF_RASTER_MAX_WIDTH)
}

#[allow(clippy::too_many_arguments)] // Rendering function threading per-frame view state
pub(super) fn browser_rendered_body(
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
        BrowserPreview::Downloading(fraction) => div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(browser_download_label(*fraction))
            .into_any_element(),
        // Kept inside the panel: Chrome's launch errors carry its stderr,
        // one long line that used to be drawn across the whole window
        // (AGE-563). `min_w_0` + `overflow_hidden` bound it to the panel;
        // the inner full-width block lets it wrap.
        BrowserPreview::Error(message) => v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .justify_center()
            .gap_1()
            .p_4()
            .child(
                div()
                    .w_full()
                    .text_sm()
                    .text_center()
                    .text_color(cx.theme().foreground)
                    .child("Couldn't start the browser"),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.clone()),
            )
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

pub(super) fn tabular_rendered_body(tabular: &TabularPreview, cx: &App) -> AnyElement {
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

pub(super) fn image_rendered_body(path: &Path, cx: &App) -> AnyElement {
    if !path.exists() {
        return div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Image not found: {}", path.display()))
            .into_any_element();
    }
    // Fills the panel and follows it: the picture scales down to fit
    // whatever room the panel has, and never past its own pixel size, so a
    // small icon stays crisp instead of blowing up. It used to be capped at
    // a fixed 348x520 whatever the panel's size.
    div()
        .id("artifact-image")
        .flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(
            img(path.to_path_buf())
                .size_full()
                .object_fit(ObjectFit::ScaleDown),
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
pub(super) fn fit_to_width_page(
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

pub(super) fn pdf_rendered_body(
    pdf: &PdfPreview,
    panel_width: Rc<RefCell<Pixels>>,
    entity: Entity<ArtifactView>,
    cx: &App,
) -> AnyElement {
    match pdf {
        PdfPreview::Idle | PdfPreview::Loading => div()
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
pub(super) fn next_slide_index(current: usize, total: usize, next: bool) -> usize {
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
pub(super) fn slide_pager(index: usize, total: usize) -> (String, bool, bool) {
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
pub(super) fn pptx_rendered_body(
    pptx: &PptxPreview,
    panel_width: Rc<RefCell<Pixels>>,
    entity: Entity<ArtifactView>,
    cx: &mut App,
) -> AnyElement {
    match pptx {
        PptxPreview::Idle | PptxPreview::Loading => div()
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

/// "Downloading Chrome… 42%" for the panel while the first launch fetches
/// the pinned build (AGE-565).
pub(super) fn browser_download_label(fraction: f32) -> String {
    let percent = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
    format!("Downloading Chrome… {percent}% (first use only)")
}

pub(super) fn document_text_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(1.15))
        .heading_font_size(|level, base| match level {
            1 => base * 1.8,
            2 => base * 1.45,
            3 => base * 1.2,
            _ => base,
        })
}

pub(super) fn artifact_rendered_markdown(
    parts: &[RenderedPart],
    full: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let mut inner = div()
        .w_full()
        .when(full, |this| this.max_w(px(DOCUMENT_MEASURE_PX)).mx_auto())
        .line_height(relative(1.7));
    for (ix, part) in parts.iter().enumerate() {
        inner = match part {
            // The first markdown run keeps the old id, so a document with no
            // diagrams keeps its TextView state exactly as before.
            RenderedPart::Markdown(text) => {
                let id: ElementId = if ix == 0 {
                    "artifact-md".into()
                } else {
                    ElementId::Name(format!("artifact-md-{ix}").into())
                };
                inner.child(
                    TextView::markdown(
                        id,
                        // AGE-1262: no file:// or custom-scheme links.
                        crate::chatty::views::safe_links::neuter_unsafe_links(text).into_owned(),
                        window,
                        cx,
                    )
                    .style(document_text_style())
                    .selectable(true),
                )
            }
            RenderedPart::Mermaid { source, svg } => {
                let id = ElementId::Name(format!("artifact-mermaid-{ix}").into());
                inner.child(match svg {
                    Some(svg) => MermaidComponent::with_svg_path(source.clone(), id, svg.clone()),
                    None => MermaidComponent::new(source.clone(), id),
                })
            }
        };
    }
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

pub(super) fn artifact_primary_body(
    path: Option<&PathBuf>,
    rendered: &[RenderedPart],
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

/// Ctrl+S, or ⌘S on macOS.
pub(super) fn is_save_keystroke(keystroke: &Keystroke) -> bool {
    let modifier = if cfg!(target_os = "macos") {
        keystroke.modifiers.platform
    } else {
        keystroke.modifiers.control
    };
    modifier && !keystroke.modifiers.shift && !keystroke.modifiers.alt && keystroke.key == "s"
}

/// Anything the Source editor can hold and Save can write back: not a
/// binary the panel renders from disk.
pub(super) fn is_text_artifact_path(path: &Path) -> bool {
    !is_pdf_path(path) && !is_pptx_path(path) && !is_image_path(path)
}

pub(super) fn artifact_source_input(editor: &Entity<InputState>) -> AnyElement {
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
