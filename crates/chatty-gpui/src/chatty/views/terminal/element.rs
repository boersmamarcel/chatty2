//! [`TerminalElement`]: one gpui element that paints the whole grid.
//!
//! Per frame it reads the grid once under the terminal lock, batches each
//! row into a few runs ([`batch_row`]), and shapes only rows it has not seen
//! recently: shaped rows are cached by their run content, so a scrolling
//! screen reshapes just the new line. If the terminal's generation, the
//! size, the font and the palette are all unchanged, the previous frame is
//! painted again without touching the terminal at all.

use std::rc::Rc;
use std::time::Instant;

use chatty_terminal::alacritty_terminal::grid::Dimensions;
use chatty_terminal::alacritty_terminal::index::Line;
use chatty_terminal::alacritty_terminal::term::cell::Flags;
use chatty_terminal::alacritty_terminal::vte::ansi::CursorShape;
use gpui::{
    App, BorderStyle, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId,
    ElementInputHandler, Entity, FocusHandle, Font, FontStyle, FontWeight, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, ShapedLine, SharedString,
    StrikethroughStyle, Style, TextRun, UnderlineStyle, Window, fill, font, outline, point, px,
    relative, size,
};
use gpui_component::ActiveTheme as _;
use rustc_hash::FxHashMap;
use tracing::{debug, trace, warn};

use super::TerminalView;
use super::grid::{
    BgRect, Palette, RunGrid, RunStyle, TextRunSpec, batch_row, cell_colors, to_hsla,
};
use super::input::Geometry;

/// Painted width of a beam cursor and height of an underline cursor.
const CURSOR_BAR: Pixels = px(2.);

/// Per-view render state kept across frames.
#[derive(Default)]
pub(super) struct RenderCache {
    metrics: Option<Metrics>,
    rows: RowCache,
    frame: Option<Rc<Frame>>,
}

/// Font and cell geometry.
struct Metrics {
    key: MetricsKey,
    /// Regular, bold, italic, bold italic.
    fonts: [Font; 4],
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
}

#[derive(Debug, Clone, PartialEq)]
struct MetricsKey {
    family: SharedString,
    size: Pixels,
    line_height: f32,
}

impl Metrics {
    /// Resolve the fonts once per settings change. A family (or a bold or
    /// italic face) that does not load is replaced here by what it resolves
    /// to, so no later frame asks gpui for a font that fails: gpui caches the
    /// failure as an error and pays for it on every lookup (AGE-378).
    fn new(key: MetricsKey, window: &Window) -> Self {
        let text_system = window.text_system();
        let requested = font(key.family.clone());
        let regular_id = text_system.resolve_font(&requested);
        let regular = match text_system.get_font_for_id(regular_id) {
            Some(resolved) if resolved.family != key.family => {
                warn!(
                    requested = %key.family,
                    resolved = %resolved.family,
                    "terminal font is not installed; using the fallback"
                );
                font(resolved.family)
            }
            _ => requested,
        };
        let face = |weight: FontWeight, style: FontStyle| {
            let candidate = Font {
                weight,
                style,
                ..regular.clone()
            };
            let id = text_system.resolve_font(&candidate);
            if text_system
                .get_font_for_id(id)
                .is_some_and(|resolved| resolved.family == regular.family)
            {
                candidate
            } else {
                regular.clone()
            }
        };
        let fonts = [
            regular.clone(),
            face(FontWeight::BOLD, FontStyle::Normal),
            face(FontWeight::NORMAL, FontStyle::Italic),
            face(FontWeight::BOLD, FontStyle::Italic),
        ];
        let font_id = text_system.resolve_font(&regular);
        let cell_width = match text_system.advance(font_id, key.size, 'M') {
            Ok(advance) if advance.width > px(0.) => advance.width,
            _ => key.size * 0.6,
        };
        let line_height = px((f32::from(key.size) * key.line_height).round().max(1.));
        Self {
            font_size: key.size,
            key,
            fonts,
            cell_width,
            line_height,
        }
    }

    fn font(&self, style: &RunStyle) -> &Font {
        &self.fonts[style.bold as usize + 2 * style.italic as usize]
    }

    /// Whole cells that fit in `bounds`, at least 1×1.
    fn grid_size(&self, bounds: Bounds<Pixels>) -> (u16, u16) {
        let cols = (bounds.size.width / self.cell_width).floor().max(1.) as u16;
        let rows = (bounds.size.height / self.line_height).floor().max(1.) as u16;
        (cols, rows)
    }
}

/// Shaped rows by content. Rows used this frame live in `current`; the
/// ones from the frame before survive one more frame in `previous`, so
/// memory stays around two screens' worth.
#[derive(Default)]
struct RowCache {
    current: FxHashMap<Vec<TextRunSpec>, Rc<[ShapedRun]>>,
    previous: FxHashMap<Vec<TextRunSpec>, Rc<[ShapedRun]>>,
    /// Rows shaped in the frame being prepared (for the timing trace).
    misses: usize,
}

impl RowCache {
    fn get_or_shape(
        &mut self,
        runs: Vec<TextRunSpec>,
        shape: impl FnOnce(&[TextRunSpec]) -> Rc<[ShapedRun]>,
    ) -> Rc<[ShapedRun]> {
        if let Some(shaped) = self.current.get(&runs) {
            return shaped.clone();
        }
        let shaped = match self.previous.remove(&runs) {
            Some(shaped) => shaped,
            None => {
                self.misses += 1;
                shape(&runs)
            }
        };
        self.current.insert(runs, shaped.clone());
        shaped
    }

    fn end_frame(&mut self) {
        self.previous = std::mem::take(&mut self.current);
        self.misses = 0;
    }

    fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }
}

struct ShapedRun {
    col: u16,
    line: ShapedLine,
}

/// Everything paint needs, relative to the element's origin.
pub(super) struct Frame {
    generation: u64,
    size: gpui::Size<Pixels>,
    palette: Palette,
    metrics_key: MetricsKey,
    cell_width: Pixels,
    line_height: Pixels,
    background: Hsla,
    rows: Vec<PreparedRow>,
    /// Selected cells, one rect per row.
    selection: Vec<Bounds<Pixels>>,
    cursor: Option<CursorPaint>,
}

struct PreparedRow {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    runs: Rc<[ShapedRun]>,
}

struct CursorPaint {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    color: Hsla,
    /// The character under a block cursor, in the cell's background colour.
    glyph: Option<ShapedLine>,
}

impl TerminalView {
    /// Build (or reuse) the frame for `bounds`. Also requests a PTY resize
    /// when the bounds hold a different number of cells.
    fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Rc<Frame> {
        let theme = cx.theme();
        let palette = Palette::from_theme(&theme.colors, theme.mode.is_dark());
        let key = MetricsKey {
            family: self
                .font
                .family
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| theme.mono_font_family.clone()),
            size: self.font.size.map(px).unwrap_or(theme.mono_font_size),
            line_height: self.font.line_height,
        };

        let metrics = match self.cache.metrics.take() {
            Some(metrics) if metrics.key == key => metrics,
            _ => {
                self.cache.rows.clear();
                self.cache.frame = None;
                Metrics::new(key, window)
            }
        };
        self.request_resize(metrics.grid_size(bounds), cx);
        self.geometry = Some(Geometry {
            bounds,
            cell_width: metrics.cell_width,
            line_height: metrics.line_height,
        });
        let frame = self.build_frame(bounds, palette, &metrics, window);
        self.cache.metrics = Some(metrics);
        frame
    }

    fn build_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        palette: Palette,
        metrics: &Metrics,
        window: &Window,
    ) -> Rc<Frame> {
        let cache = &mut self.cache;
        let generation = self.handle.generation();
        if let Some(frame) = &cache.frame {
            if frame.generation == generation
                && frame.size == bounds.size
                && frame.palette == palette
                && frame.metrics_key == metrics.key
            {
                return frame.clone();
            }
            if frame.palette != palette {
                cache.rows.clear();
            }
        }

        let cell_width = metrics.cell_width;
        let line_height = metrics.line_height;
        let cell_bounds = |col: u16, row: usize, cells: u16| {
            Bounds::new(
                point(cell_width * col as f32, line_height * row as f32),
                size(cell_width * cells as f32, line_height),
            )
        };
        let rows_cache = &mut cache.rows;

        let locking = Instant::now();
        let mut lock_wait = std::time::Duration::ZERO;
        let (rows, selection, cursor) = self.handle.with_term(|term| {
            // Time spent waiting for the PTY thread to release the grid.
            lock_wait = locking.elapsed();
            let grid = term.grid();
            let offset = grid.display_offset() as i32;
            let colors = term.colors();
            let screen_lines = term.screen_lines();

            let rows = (0..screen_lines)
                .map(|i| {
                    let layout = batch_row(&grid[Line(i as i32 - offset)][..], colors, &palette);
                    let backgrounds = layout
                        .backgrounds
                        .iter()
                        .map(|BgRect { col, cells, color }| {
                            (cell_bounds(*col, i, *cells), to_hsla(*color))
                        })
                        .collect();
                    let runs = rows_cache.get_or_shape(layout.runs, |runs| {
                        runs.iter()
                            .map(|run| ShapedRun {
                                col: run.col,
                                line: shape_run(run, metrics, window),
                            })
                            .collect()
                    });
                    PreparedRow { backgrounds, runs }
                })
                .collect::<Vec<_>>();

            let content = term.renderable_content();
            let columns = term.columns();
            let selection = content
                .selection
                .map(|range| {
                    (0..screen_lines)
                        .filter_map(|i| {
                            let line = Line(i as i32 - offset);
                            if line < range.start.line || line > range.end.line {
                                return None;
                            }
                            let first = if range.is_block || line == range.start.line {
                                range.start.column.0
                            } else {
                                0
                            };
                            let last = if range.is_block || line == range.end.line {
                                range.end.column.0
                            } else {
                                columns.saturating_sub(1)
                            };
                            (first <= last)
                                .then(|| cell_bounds(first as u16, i, (last - first + 1) as u16))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let point = content.cursor.point;
            let row = point.line.0 + offset;
            let cursor = (content.cursor.shape != CursorShape::Hidden
                && (0..screen_lines as i32).contains(&row)
                && point.column.0 < term.columns())
            .then(|| {
                let cell = &grid[point];
                let wide = cell.flags.contains(Flags::WIDE_CHAR);
                let (_, bg) = cell_colors(cell, colors, &palette);
                let glyph = (content.cursor.shape == CursorShape::Block
                    && cell.c != ' '
                    && !cell.flags.contains(Flags::HIDDEN))
                .then(|| {
                    let run = TextRunSpec {
                        col: point.column.0 as u16,
                        cells: if wide { 2 } else { 1 },
                        text: cell.c.to_string(),
                        style: RunStyle {
                            fg: bg,
                            bold: cell.flags.contains(Flags::BOLD),
                            italic: cell.flags.contains(Flags::ITALIC),
                            underline: false,
                            strikethrough: false,
                        },
                        grid: if wide { RunGrid::Wide } else { RunGrid::Cells },
                    };
                    shape_run(&run, metrics, window)
                });
                CursorPaint {
                    bounds: cell_bounds(
                        point.column.0 as u16,
                        row as usize,
                        if wide { 2 } else { 1 },
                    ),
                    shape: content.cursor.shape,
                    color: to_hsla(palette.cursor),
                    glyph,
                }
            });
            (rows, selection, cursor)
        });

        trace!(
            target: "terminal_frame",
            shaped_rows = cache.rows.misses,
            lock_wait_us = lock_wait.as_micros() as u64,
            "terminal frame rebuilt"
        );
        cache.rows.end_frame();

        let frame = Rc::new(Frame {
            generation,
            size: bounds.size,
            palette,
            metrics_key: metrics.key.clone(),
            cell_width,
            line_height,
            background: to_hsla(palette.background),
            rows,
            selection,
            cursor,
        });
        cache.frame = Some(frame.clone());
        frame
    }
}

fn shape_run(run: &TextRunSpec, metrics: &Metrics, window: &Window) -> ShapedLine {
    let color = to_hsla(run.style.fg);
    let text_run = TextRun {
        len: run.text.len(),
        font: metrics.font(&run.style).clone(),
        color,
        background_color: None,
        underline: run.style.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: run.style.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(color),
        }),
    };
    // Snap glyphs to the cell grid so a fallback font's different advance
    // cannot drift the rest of the run.
    let force_width = match run.grid {
        RunGrid::Cells => Some(metrics.cell_width),
        RunGrid::Wide => Some(metrics.cell_width * 2.),
        RunGrid::Cluster => None,
    };
    window.text_system().shape_line(
        SharedString::from(run.text.clone()),
        metrics.font_size,
        &[text_run],
        force_width,
    )
}

/// Paints a [`TerminalView`]'s grid. Fills whatever box its parent gives it.
pub(super) struct TerminalElement {
    view: Entity<TerminalView>,
    focus_handle: FocusHandle,
}

impl TerminalElement {
    pub(super) fn new(view: Entity<TerminalView>, focus_handle: FocusHandle) -> Self {
        Self { view, focus_handle }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = (Rc<Frame>, Hitbox);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let started = Instant::now();
        let frame = self
            .view
            .update(cx, |view, cx| view.prepare_frame(bounds, window, cx));
        trace!(
            target: "terminal_frame",
            prepaint_us = started.elapsed().as_micros() as u64,
            "terminal prepaint"
        );
        (frame, window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        (frame, hitbox): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let started = Instant::now();
        self.register_input(bounds, hitbox, window, cx);
        let origin = bounds.origin;
        let selection_color = cx.theme().selection;
        window.paint_quad(fill(bounds, frame.background));
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in &frame.rows {
                for (rect, color) in &row.backgrounds {
                    window.paint_quad(fill(*rect + origin, *color));
                }
            }
            for rect in &frame.selection {
                window.paint_quad(fill(*rect + origin, selection_color));
            }
            for (i, row) in frame.rows.iter().enumerate() {
                let y = origin.y + frame.line_height * i as f32;
                for run in row.runs.iter() {
                    let x = origin.x + frame.cell_width * run.col as f32;
                    if let Err(e) = run.line.paint(point(x, y), frame.line_height, window, cx) {
                        debug!("terminal: painting a run failed: {e}");
                    }
                }
            }
            if let Some(cursor) = &frame.cursor {
                paint_cursor(cursor, origin, frame.line_height, window, cx);
            }
        });
        trace!(
            target: "terminal_frame",
            paint_us = started.elapsed().as_micros() as u64,
            "terminal paint"
        );
    }
}

impl TerminalElement {
    /// Text input, mouse and wheel listeners for this frame.
    fn register_input(
        &self,
        bounds: Bounds<Pixels>,
        hitbox: &Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let view = self.view.clone();
        let hovered = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hovered.is_hovered(window) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
                cx.stop_propagation();
            }
        });
        let view = self.view.clone();
        let hovered = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hovered.is_hovered(window);
                view.update(cx, |view, cx| view.mouse_move(event, hovered, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, _| view.mouse_up(event));
            }
        });
        let view = self.view.clone();
        let hovered = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hovered.is_hovered(window) {
                view.update(cx, |view, cx| view.scroll_wheel(event, cx));
                cx.stop_propagation();
            }
        });
    }
}

fn paint_cursor(
    cursor: &CursorPaint,
    origin: gpui::Point<Pixels>,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let cell = cursor.bounds + origin;
    match cursor.shape {
        CursorShape::Block => {
            window.paint_quad(fill(cell, cursor.color));
            if let Some(glyph) = &cursor.glyph
                && let Err(e) = glyph.paint(cell.origin, line_height, window, cx)
            {
                debug!("terminal: painting the cursor glyph failed: {e}");
            }
        }
        CursorShape::Beam => {
            window.paint_quad(fill(
                Bounds::new(cell.origin, size(CURSOR_BAR, cell.size.height)),
                cursor.color,
            ));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(cell.origin.x, cell.bottom() - CURSOR_BAR),
                    size(cell.size.width, CURSOR_BAR),
                ),
                cursor.color,
            ));
        }
        CursorShape::HollowBlock => {
            window.paint_quad(outline(cell, cursor.color, BorderStyle::Solid));
        }
        CursorShape::Hidden => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::views::terminal::grid::RunStyle;

    fn spec(text: &str) -> Vec<TextRunSpec> {
        vec![TextRunSpec {
            col: 0,
            cells: text.len() as u16,
            text: text.into(),
            style: RunStyle {
                fg: 0xffffff,
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
            },
            grid: RunGrid::Cells,
        }]
    }

    #[test]
    fn row_cache_shapes_each_row_once_and_forgets_unused_rows() {
        let mut cache = RowCache::default();
        let mut shaped = 0;
        let mut shape = |_: &[TextRunSpec]| {
            shaped += 1;
            Rc::from(Vec::new())
        };

        // Frame 1: two distinct rows, one repeated (`yes` output).
        cache.get_or_shape(spec("y"), &mut shape);
        cache.get_or_shape(spec("y"), &mut shape);
        cache.get_or_shape(spec("$ yes"), &mut shape);
        cache.end_frame();
        // Frame 2: same rows again, nothing is shaped.
        cache.get_or_shape(spec("y"), &mut shape);
        cache.end_frame();
        // Frame 3: "$ yes" was unused in frame 2 and is gone.
        cache.get_or_shape(spec("$ yes"), &mut shape);
        cache.end_frame();

        assert_eq!(shaped, 3);
    }
}
