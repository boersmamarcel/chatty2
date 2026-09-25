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
use gpui::prelude::FluentBuilder;
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
        // The id carries the content. gpui-component parses a TextView in
        // the same frame only the first time it sees an id; new text under a
        // known id goes to a background parser behind a 200 ms debounce, and
        // the old parse stays up meanwhile. A streamed line promoted from the
        // plain tail into this view therefore vanished until the parse
        // landed (and stayed gone while tokens kept restarting the debounce),
        // then snapped back restyled. Changed text now gets a fresh state,
        // parsed synchronously; the old one is dropped with its element.
        let id = ElementId::Name(
            format!(
                "msg-{}-markdown-{:x}",
                self.message_index,
                content_key(&self.content)
            )
            .into(),
        );

        TextView::markdown(id, self.content, window, cx).selectable(true)
    }
}

fn content_key(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
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
/// at least one [`CachedMathSegment::InlineMath`]), one logical line at a time.
///
/// * **Text-only lines** accumulate into a single [`MarkdownContent`], so
///   headings, lists and tables without math keep full markdown rendering.
/// * **Lines with inline math** become a wrapping row of text runs and SVGs.
///   gpui-component's markdown cannot hold an element inside a line of text
///   (its inline images are blocks), so the row does the markdown a math line
///   needs itself: the line's list marker, number, heading or quote, and
///   `**bold**`, `*italic*` and `` `code` `` inside its text. Before, all of
///   that showed as literal characters.
/// * **Tables with math in a cell** render as a grid whose cells are rows of
///   the same kind; a table row with math used to fall out of the table as
///   `| … |` text.
///
/// Everything is wrapped in one container `div` so the parent sees a single
/// element per batch (the blank-space-during-streaming fix).
fn render_inline_math_batch(
    batch: &[CachedMathSegment],
    base_index: usize,
    batch_start: usize,
    elements: &mut Vec<AnyElement>,
) {
    let lines = split_lines(batch, base_index * 1000 + batch_start);
    let mut out: Vec<AnyElement> = Vec::new();
    let mut md_buf = String::new();
    let mut md_counter = 0usize;
    let mut flush_md = |md_buf: &mut String, out: &mut Vec<AnyElement>| {
        let trimmed = md_buf.trim_end();
        if !trimmed.is_empty() {
            let md_idx = base_index * 100_000 + batch_start * 100 + md_counter;
            md_counter += 1;
            out.push(
                MarkdownContent {
                    content: trimmed.to_string(),
                    message_index: md_idx,
                }
                .into_any_element(),
            );
        }
        md_buf.clear();
    };

    let mut i = 0;
    while i < lines.len() {
        if is_table_start(&lines, i) {
            let end = i + lines[i..]
                .iter()
                .take_while(|line| line_text(line).trim_start().starts_with('|'))
                .count();
            let block = &lines[i..end];
            if block.iter().any(|line| line_has_math(line)) {
                flush_md(&mut md_buf, &mut out);
                out.push(render_math_table(block));
            } else {
                for line in block {
                    md_buf.push_str(&line_text(line));
                    md_buf.push('\n');
                }
            }
            i = end;
            continue;
        }
        let line = &lines[i];
        if line_has_math(line) {
            flush_md(&mut md_buf, &mut out);
            out.push(render_math_line(line));
        } else {
            md_buf.push_str(&line_text(line));
            md_buf.push('\n');
        }
        i += 1;
    }
    flush_md(&mut md_buf, &mut out);

    if !out.is_empty() {
        elements.push(
            div()
                .flex()
                .flex_col()
                .w_full()
                .children(out)
                .into_any_element(),
        );
    }
}

/// One piece of a logical line: a run of text or an inline equation.
#[derive(Clone, Debug)]
enum Piece<'a> {
    Text(String),
    Math {
        latex: &'a str,
        svg_path: &'a Option<PathBuf>,
        element_index: usize,
    },
}

/// Split a batch into logical lines at `\n`, keeping each equation's element
/// index (its segment's position) for a stable id.
fn split_lines(batch: &[CachedMathSegment], first_index: usize) -> Vec<Vec<Piece<'_>>> {
    let mut lines: Vec<Vec<Piece>> = vec![Vec::new()];
    for (offset, segment) in batch.iter().enumerate() {
        match segment {
            CachedMathSegment::Text(text) => {
                for (n, part) in text.split('\n').enumerate() {
                    if n > 0 {
                        lines.push(Vec::new());
                    }
                    if !part.is_empty() {
                        lines
                            .last_mut()
                            .expect("never empty")
                            .push(Piece::Text(part.to_string()));
                    }
                }
            }
            CachedMathSegment::InlineMath { latex, svg_path } => {
                lines.last_mut().expect("never empty").push(Piece::Math {
                    latex,
                    svg_path,
                    element_index: first_index + offset,
                });
            }
            CachedMathSegment::BlockMath { .. } => unreachable!(
                "BlockMath segments are split out as batch boundaries in \
                 render_math_segments and must never appear inside an inline batch"
            ),
        }
    }
    lines
}

fn line_has_math(line: &[Piece]) -> bool {
    line.iter().any(|piece| matches!(piece, Piece::Math { .. }))
}

/// The line as markdown source, equations back in `$…$`.
fn line_text(line: &[Piece]) -> String {
    line.iter()
        .map(|piece| match piece {
            Piece::Text(text) => text.clone(),
            Piece::Math { latex, .. } => format!("${latex}$"),
        })
        .collect()
}

/// A `|` line followed by a `|---|` separator line.
fn is_table_start(lines: &[Vec<Piece>], i: usize) -> bool {
    let is_row = |line: &[Piece]| line_text(line).trim_start().starts_with('|');
    let is_separator = |line: &[Piece]| {
        let text = line_text(line);
        let text = text.trim();
        !line_has_math(line)
            && text.contains("---")
            && text.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    };
    is_row(&lines[i]) && lines.get(i + 1).is_some_and(|next| is_separator(next))
}

/// How a math line starts, in markdown terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
    Plain,
    Bullet,
    Ordered(u32),
    Heading(u8),
    Quote,
}

/// Read the markdown prefix off the start of a line: its kind, its indent in
/// spaces, and how many bytes of the first text run the prefix used.
fn line_prefix(first_text: &str) -> (LineKind, usize, usize) {
    let indent = first_text.len() - first_text.trim_start().len();
    let rest = &first_text[indent..];
    let kind_and_len = if let Some(hashes) = rest.find(|c| c != '#').filter(|&n| n > 0 && n <= 6) {
        rest[hashes..]
            .starts_with(' ')
            .then(|| (LineKind::Heading(hashes as u8), hashes + 1))
    } else if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        Some((LineKind::Bullet, 2))
    } else if rest.starts_with("> ") {
        Some((LineKind::Quote, 2))
    } else {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        (digits > 0 && digits < 10)
            .then(|| &rest[digits..])
            .filter(|after| after.starts_with(". ") || after.starts_with(") "))
            .and_then(|_| rest[..digits].parse().ok())
            .map(|n| (LineKind::Ordered(n), digits + 2))
    };
    match kind_and_len {
        Some((kind, len)) => (kind, indent, indent + len),
        None => (LineKind::Plain, indent, 0),
    }
}

/// A line with inline math: its markdown prefix as a gutter or style, then a
/// wrapping row of text runs and equations.
fn render_math_line(line: &[Piece]) -> AnyElement {
    let mut pieces = line.to_vec();
    let (kind, indent, consumed) = match pieces.first() {
        Some(Piece::Text(text)) => line_prefix(text),
        _ => (LineKind::Plain, 0, 0),
    };
    if consumed > 0
        && let Some(Piece::Text(text)) = pieces.first_mut()
    {
        text.replace_range(..consumed, "");
    }
    let row = inline_row(&pieces);
    let gutter = |label: String, row: AnyElement| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .pl(px(indent as f32 * 6.0))
            .child(div().flex_shrink_0().w(px(22.)).child(label))
            .child(div().flex_1().min_w_0().child(row))
            .into_any_element()
    };
    match kind {
        LineKind::Plain => div()
            .w_full()
            .pl(px(indent as f32 * 6.0))
            .child(row)
            .into_any_element(),
        LineKind::Bullet => gutter("\u{2022}".to_string(), row),
        LineKind::Ordered(n) => gutter(format!("{n}."), row),
        LineKind::Heading(level) => div()
            .w_full()
            .pt_2()
            .font_weight(FontWeight::SEMIBOLD)
            .when(level <= 2, |this| this.text_xl())
            .when(level > 2, |this| this.text_lg())
            .child(row)
            .into_any_element(),
        LineKind::Quote => div()
            .w_full()
            .pl_3()
            .border_l_2()
            .border_color(hsla(0., 0., 0.5, 0.4))
            .child(row)
            .into_any_element(),
    }
}

/// Text and equations in one wrapping row. Text goes in word by word, so the
/// row wraps between words the way a paragraph does; a whole run as one box
/// dropped to its own line as soon as it did not fit after an equation.
fn inline_row(pieces: &[Piece]) -> AnyElement {
    let children: Vec<AnyElement> = pieces
        .iter()
        .flat_map(|piece| match piece {
            Piece::Text(text) => inline_words(text),
            Piece::Math {
                latex,
                svg_path,
                element_index,
            } => vec![
                make_math_component(
                    latex,
                    true,
                    svg_path,
                    ElementId::Name(format!("math-inline-{element_index}").into()),
                )
                .into_any_element(),
            ],
        })
        .collect();
    div()
        .w_full()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .children(children)
        .into_any_element()
}

/// A text run with its `**bold**`, `*italic*` and `` `code` `` applied, as
/// one element per word (each keeping the spaces after it).
fn inline_words(text: &str) -> Vec<AnyElement> {
    let (plain, spans) = parse_emphasis(text);
    let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = spans
        .into_iter()
        .map(|(range, style)| {
            let highlight = match style {
                Emphasis::Bold => HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
                Emphasis::Italic => HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
                Emphasis::Code => HighlightStyle {
                    background_color: Some(hsla(0., 0., 0.5, 0.15)),
                    ..Default::default()
                },
            };
            (range, highlight)
        })
        .collect();
    word_ranges(&plain)
        .into_iter()
        .map(|word| {
            let local: Vec<_> = highlights
                .iter()
                .filter_map(|(range, style)| {
                    let start = range.start.max(word.start);
                    let end = range.end.min(word.end);
                    (start < end).then(|| (start - word.start..end - word.start, *style))
                })
                .collect();
            StyledText::new(plain[word.clone()].to_string())
                .with_highlights(local)
                .into_any_element()
        })
        .collect()
}

/// Byte ranges of the words in `text`, each running through the whitespace
/// after it; leading whitespace stays with the first word.
fn word_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut in_space = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            in_space = true;
        } else if in_space {
            if !text[start..i].trim().is_empty() {
                ranges.push(start..i);
                start = i;
            }
            in_space = false;
        }
    }
    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Emphasis {
    Bold,
    Italic,
    Code,
}

/// Strip `**…**`, `*…*` and `` `…` `` markers from `text`, returning the plain
/// text and the byte ranges each style covers in it. Unclosed markers stay as
/// written. `_` is left alone: it is far more often part of a name.
fn parse_emphasis(text: &str) -> (String, Vec<(std::ops::Range<usize>, Emphasis)>) {
    let mut plain = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (marker, style) = if rest.starts_with("**") {
            ("**", Emphasis::Bold)
        } else if rest.starts_with('*') {
            ("*", Emphasis::Italic)
        } else if rest.starts_with('`') {
            ("`", Emphasis::Code)
        } else {
            let ch = rest.chars().next().expect("non-empty");
            plain.push(ch);
            rest = &rest[ch.len_utf8()..];
            continue;
        };
        let body = &rest[marker.len()..];
        // CommonMark flanking, roughly: a marker opens only before a
        // non-space and closes only after one, so `2 * 3 * 4` stays text.
        let opens = style == Emphasis::Code || body.starts_with(|c: char| !c.is_whitespace());
        let close = opens
            .then(|| {
                body.match_indices(marker).map(|(end, _)| end).find(|&end| {
                    end > 0
                        && (style == Emphasis::Code || !body[..end].ends_with(char::is_whitespace))
                })
            })
            .flatten();
        match close {
            Some(end) => {
                let start = plain.len();
                plain.push_str(&body[..end]);
                spans.push((start..plain.len(), style));
                rest = &body[end + marker.len()..];
            }
            None => {
                plain.push_str(marker);
                rest = body;
            }
        }
    }
    (plain, spans)
}

/// A table with math in its cells: header row, separator skipped, body rows,
/// each cell a wrapping row of text and equations.
fn render_math_table(block: &[Vec<Piece>]) -> AnyElement {
    let rows: Vec<Vec<Vec<Piece>>> = block
        .iter()
        .enumerate()
        .filter(|(n, _)| *n != 1)
        .map(|(_, line)| split_cells(line))
        .collect();
    let border = hsla(0., 0., 0.5, 0.3);
    div()
        .my_2()
        .w_full()
        .flex()
        .flex_col()
        .border_1()
        .border_color(border)
        .rounded_md()
        .children(rows.into_iter().enumerate().map(|(n, cells)| {
            div()
                .w_full()
                .flex()
                .flex_row()
                .when(n > 0, |this| this.border_t_1().border_color(border))
                .when(n == 0, |this| this.font_weight(FontWeight::SEMIBOLD))
                .children(cells.into_iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .child(inline_row(&cell))
                }))
        }))
        .into_any_element()
}

/// Split a table line into cells at the `|`s in its text runs, dropping the
/// empty cells outside a leading and trailing `|`.
fn split_cells<'a>(line: &[Piece<'a>]) -> Vec<Vec<Piece<'a>>> {
    let mut cells: Vec<Vec<Piece>> = vec![Vec::new()];
    for piece in line {
        match piece {
            Piece::Text(text) => {
                for (n, part) in text.split('|').enumerate() {
                    if n > 0 {
                        cells.push(Vec::new());
                    }
                    let part = part.trim();
                    if !part.is_empty() {
                        cells
                            .last_mut()
                            .expect("never empty")
                            .push(Piece::Text(part.to_string()));
                    }
                }
            }
            math => cells.last_mut().expect("never empty").push(math.clone()),
        }
    }
    if cells.first().is_some_and(|cell| cell.is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.is_empty()) {
        cells.pop();
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::{Emphasis, LineKind, line_prefix, parse_emphasis, word_ranges};

    #[test]
    fn words_keep_the_spaces_after_them() {
        let text = " and the error ";
        let words: Vec<&str> = word_ranges(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(words, vec![" and ", "the ", "error "]);
        assert!(word_ranges("").is_empty());
    }

    #[test]
    fn line_prefixes_are_read_off_the_first_run() {
        assert_eq!(line_prefix("- The mean is "), (LineKind::Bullet, 0, 2));
        assert_eq!(line_prefix("  * nested "), (LineKind::Bullet, 2, 4));
        assert_eq!(line_prefix("12. Twelfth: "), (LineKind::Ordered(12), 0, 4));
        assert_eq!(line_prefix("### Heading "), (LineKind::Heading(3), 0, 4));
        assert_eq!(line_prefix("> quoted "), (LineKind::Quote, 0, 2));
        assert_eq!(line_prefix("plain text "), (LineKind::Plain, 0, 0));
        // A hash without a space, or a number without a dot, is text.
        assert_eq!(line_prefix("#hashtag "), (LineKind::Plain, 0, 0));
        assert_eq!(line_prefix("2024 was "), (LineKind::Plain, 0, 0));
    }

    #[test]
    fn emphasis_markers_become_styles() {
        let (plain, spans) = parse_emphasis("**Bold with math** and *italic* `x`");
        assert_eq!(plain, "Bold with math and italic x");
        assert_eq!(
            spans,
            vec![
                (0..14, Emphasis::Bold),
                (19..25, Emphasis::Italic),
                (26..27, Emphasis::Code),
            ]
        );
    }

    #[test]
    fn unclosed_markers_stay_as_written() {
        let (plain, spans) = parse_emphasis("2 * 3 and **open");
        assert_eq!(plain, "2 * 3 and **open");
        assert!(spans.is_empty());
    }
}
