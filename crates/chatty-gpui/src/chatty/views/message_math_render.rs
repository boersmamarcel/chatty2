//! Math-aware rendering for message content.
//!
//! [`resolve_math_segments`] runs at parse time and turns the parser's
//! [`MathSegment`]s into [`CachedMathSegment`]s with every equation's styled
//! SVG path already looked up. The render functions convert those into GPUI
//! elements, handling:
//! - **Block math**: Standalone LaTeX expressions rendered as SVG via [`MathComponent`]
//! - **Inline math**: Interleaved with text in full-width flex rows
//! - **Text-only runs**: Passed through as [`MarkdownContent`] with full formatting
//!
//! Nothing on the render side touches [`MathRendererService`]: that lookup
//! (three digests, a `stat`, a clone of the cached SVG) used to run for every
//! equation on every frame, which is what made a math-heavy answer scroll in
//! multi-hundred-millisecond stalls (AGE-394).

use crate::chatty::services::MathRendererService;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::text::TextView;
use std::path::PathBuf;
use tracing::warn;

use super::math_parser::MathSegment;
use super::math_renderer::MathComponent;
use super::parsed_cache::CachedMathSegment;

/// Wrapper component for rendering markdown content
#[derive(IntoElement, Clone)]
pub(super) struct MarkdownContent {
    pub content: String,
    pub message_index: usize,
}

impl RenderOnce for MarkdownContent {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Use message index for stable ID during streaming
        let id = ElementId::Name(format!("msg-{}-markdown", self.message_index).into());

        TextView::markdown(id, self.content, window, cx).selectable(true)
    }
}

/// Resolve every equation's styled SVG path, once, at parse time.
///
/// Uses the theme foreground colour for the SVG; an equation whose render
/// fails keeps `svg_path: None` and falls back to raw LaTeX at render time.
pub(super) fn resolve_math_segments(
    segments: Vec<MathSegment>,
    cx: &App,
) -> Vec<CachedMathSegment> {
    let service = cx.try_global::<MathRendererService>();
    let rgb = cx.theme().foreground.to_rgb();
    let theme_color = chatty_core::services::math_renderer_service::RgbColor {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    };
    let resolve = |latex: &str, is_inline: bool| -> Option<PathBuf> {
        let Some(service) = service else {
            warn!(content = %latex, is_inline, "Math renderer service unavailable");
            return None;
        };
        match service.render_to_styled_svg_file(latex, is_inline, theme_color) {
            Ok(svg_path) => Some(svg_path),
            Err(e) => {
                warn!(error = ?e, content = %latex, is_inline, "Failed to pre-render math");
                None
            }
        }
    };
    segments
        .into_iter()
        .map(|segment| match segment {
            MathSegment::Text(text) => CachedMathSegment::Text(text),
            MathSegment::InlineMath(latex) => {
                let svg_path = resolve(&latex, true);
                CachedMathSegment::InlineMath { latex, svg_path }
            }
            MathSegment::BlockMath(latex) => {
                let svg_path = resolve(&latex, false);
                CachedMathSegment::BlockMath { latex, svg_path }
            }
        })
        .collect()
}

/// Build a `MathComponent` from a resolved equation. No I/O: the path was
/// looked up by [`resolve_math_segments`].
fn make_math_component(
    latex: &str,
    is_inline: bool,
    svg_path: &Option<PathBuf>,
    element_id: ElementId,
) -> MathComponent {
    match svg_path {
        Some(path) => {
            MathComponent::with_svg_path(latex.to_string(), is_inline, element_id, path.clone())
        }
        None => MathComponent::new(latex.to_string(), is_inline, element_id),
    }
}

/// Render pre-parsed math segments to GPUI elements.
///
/// Accepts `&[CachedMathSegment]` — the cached path (`render_from_cached`) for
/// both finalized and streaming messages.
///
/// Segments are processed in **batches** separated by `BlockMath` boundaries.
/// Within each batch `has_inline_math` is determined locally:
///
/// * **Text-only batch** -- `MarkdownContent` is pushed directly, preserving
///   full markdown formatting (headings, bold, lists, etc.).
/// * **Inline-math batch** -- text is split at newline characters so that each
///   logical line (with its adjacent math SVGs) becomes its own full-width flex
///   row.  This prevents long text from pushing the closing `)` or other
///   trailing text onto the next screen row.  The entire inline-math batch is
///   wrapped in a **single container div** so the parent layout sees only one
///   element, preventing the blank-space issue during streaming that occurred
///   when multiple top-level elements (heading + flex rows) were emitted.
pub(super) fn render_math_segments(
    math_segments: &[CachedMathSegment],
    base_index: usize,
) -> Vec<AnyElement> {
    let mut elements = Vec::new();
    let n = math_segments.len();
    let mut batch_start = 0;

    // Iterate one past the end so the final batch is always flushed.
    for i in 0..=n {
        let at_block_math =
            i < n && matches!(math_segments[i], CachedMathSegment::BlockMath { .. });

        if at_block_math || i == n {
            // -- Flush the current batch [batch_start..i] ---------------------
            let batch = &math_segments[batch_start..i];
            if !batch.is_empty() {
                let batch_has_inline = batch
                    .iter()
                    .any(|s| matches!(s, CachedMathSegment::InlineMath { .. }));

                if batch_has_inline {
                    render_inline_math_batch(batch, base_index, batch_start, &mut elements);
                } else {
                    // Text-only batch: push MarkdownContent directly so that
                    // headings, bold, lists, etc. render with full formatting.
                    for (batch_idx, segment) in batch.iter().enumerate() {
                        let element_index = base_index * 1000 + batch_start + batch_idx;
                        if let CachedMathSegment::Text(text) = segment {
                            elements.push(
                                MarkdownContent {
                                    content: text.clone(),
                                    message_index: element_index,
                                }
                                .into_any_element(),
                            );
                        }
                    }
                }
            }

            // -- Render the BlockMath element itself --------------------------
            if at_block_math {
                if let CachedMathSegment::BlockMath { latex, svg_path } = &math_segments[i] {
                    let element_index = base_index * 1000 + i;
                    let element_id =
                        ElementId::Name(format!("math-block-{}", element_index).into());
                    elements.push(
                        make_math_component(latex, false, svg_path, element_id).into_any_element(),
                    );
                }
                batch_start = i + 1;
            }
        }
    }

    elements
}

/// Render an inline-math batch (a slice of [`CachedMathSegment`]s that contains
/// at least one [`CachedMathSegment::InlineMath`]).
///
/// **Two kinds of content are interleaved:**
///
/// * **Math-containing lines** -- logical lines (delimited by `\n`) that have at
///   least one [`CachedMathSegment::InlineMath`].  These are emitted as a full-width
///   `flex_row` with `.min_w_0()` plain-text divs flanking the SVG so that long
///   text wraps instead of overflowing.
///
/// * **Text-only runs** -- one or more consecutive logical lines that contain
///   no math.  All such lines are accumulated into a *single* `MarkdownContent`
///   element (with their `\n` characters preserved) and emitted together.
///
/// All output is collected into a local vector and wrapped in a **single
/// container `div`** (`flex_col`, `w_full`) before being pushed to the parent
/// `elements`.  This prevents the blank-space-during-streaming bug that occurred
/// when multiple top-level elements (e.g. a heading `MarkdownContent` followed
/// by a flex row) were emitted directly into the parent layout.
fn render_inline_math_batch(
    batch: &[CachedMathSegment],
    base_index: usize,
    batch_start: usize,
    elements: &mut Vec<AnyElement>,
) {
    // Local vector: everything goes here first, then gets wrapped in ONE div.
    let mut batch_elements: Vec<AnyElement> = Vec::new();

    // `full_text_buf` accumulates consecutive text-only lines (including their
    // `\n`) to be emitted as a single `MarkdownContent`.  It is flushed when
    // an `InlineMath` is encountered (so the math can begin a new flex row) or
    // at the end of the batch.
    let mut full_text_buf = String::new();

    // `text_buf` holds the text for the *current* logical line.
    let mut text_buf = String::new();

    // Children for the current math-containing flex row.
    let mut math_row: Vec<AnyElement> = Vec::new();

    // Whether the current logical line has seen at least one `InlineMath`.
    let mut line_has_math = false;

    // Counter for stable `MarkdownContent` element IDs within this batch.
    let mut md_counter = 0usize;

    for (batch_idx, segment) in batch.iter().enumerate() {
        let element_index = base_index * 1000 + batch_start + batch_idx;
        match segment {
            CachedMathSegment::Text(text) => {
                let mut remainder = text.as_str();
                while let Some(nl_pos) = remainder.find('\n') {
                    text_buf.push_str(&remainder[..nl_pos]);

                    if line_has_math {
                        flush_math_row(&mut text_buf, &mut math_row, &mut batch_elements);
                        line_has_math = false;
                    } else {
                        full_text_buf.push_str(&text_buf);
                        full_text_buf.push('\n');
                        text_buf.clear();
                    }

                    remainder = &remainder[nl_pos + 1..];
                }
                text_buf.push_str(remainder);
            }
            CachedMathSegment::InlineMath { latex, svg_path } => {
                // Flush any preceding text-only lines as ONE MarkdownContent.
                let trimmed = full_text_buf.trim_end();
                if !trimmed.is_empty() {
                    let md_idx = base_index * 100_000 + batch_start * 100 + md_counter;
                    md_counter += 1;
                    batch_elements.push(
                        MarkdownContent {
                            content: trimmed.to_string(),
                            message_index: md_idx,
                        }
                        .into_any_element(),
                    );
                }
                full_text_buf.clear();
                // Move the current line's plain-text prefix into the math row.
                if !text_buf.is_empty() {
                    math_row.push(
                        div()
                            .min_w_0()
                            .child(std::mem::take(&mut text_buf))
                            .into_any_element(),
                    );
                }
                let element_id = ElementId::Name(format!("math-inline-{}", element_index).into());
                math_row.push(
                    make_math_component(latex, true, svg_path, element_id).into_any_element(),
                );
                line_has_math = true;
            }
            CachedMathSegment::BlockMath { .. } => {
                unreachable!(
                    "BlockMath segments are split out as batch boundaries in \
                     render_math_segments and must never appear inside an inline batch"
                )
            }
        }
    }

    // Final flush.
    if line_has_math {
        flush_math_row(&mut text_buf, &mut math_row, &mut batch_elements);
    } else {
        if !text_buf.is_empty() {
            full_text_buf.push_str(&text_buf);
        }
        let trimmed = full_text_buf.trim_end();
        if !trimmed.is_empty() {
            let md_idx = base_index * 100_000 + batch_start * 100 + md_counter;
            batch_elements.push(
                MarkdownContent {
                    content: trimmed.to_string(),
                    message_index: md_idx,
                }
                .into_any_element(),
            );
        }
    }

    // Wrap all batch children in a single container div to prevent the
    // blank-space-during-streaming bug.  The parent layout sees only ONE
    // element for the entire inline-math batch.
    if !batch_elements.is_empty() {
        elements.push(
            div()
                .flex()
                .flex_col()
                .w_full()
                .children(batch_elements)
                .into_any_element(),
        );
    }
}

/// Flush the current math-containing line as a full-width `flex_row`.
///
/// Any remaining plain text in `text_buf` is moved into the row first (as a
/// `.min_w_0()` div so it can shrink and wrap), then all row children are
/// wrapped in `div().w_full().flex().flex_row().flex_wrap().items_center()`.
fn flush_math_row(
    text_buf: &mut String,
    math_row: &mut Vec<AnyElement>,
    elements: &mut Vec<AnyElement>,
) {
    if !text_buf.is_empty() {
        math_row.push(
            div()
                .min_w_0()
                .child(std::mem::take(text_buf))
                .into_any_element(),
        );
    }
    if !math_row.is_empty() {
        let children = std::mem::take(math_row);
        elements.push(
            div()
                .w_full()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .children(children)
                .into_any_element(),
        );
    }
}
