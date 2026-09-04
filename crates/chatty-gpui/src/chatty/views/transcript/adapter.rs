use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chatty_core::models::message_types::{
    ApprovalState, SystemTrace, ToolCallBlock, ToolCallState, TraceItem,
};
use gpui::{Pixels, Size, px, size};

use super::activity::{RunTally, classify_tool};
use super::artifact_kind::{
    artifact_old_content_from_tool, attachment_image_path, chart_artifact_path,
    is_produced_file_tool, is_standalone_artifact_path, is_transcript_artifact_receipt,
    produced_path_is_openable, tool_file_path,
};
use super::session_changes::{
    file_change_from_tool, file_changes_from_turn, file_changes_height, merge_file_changes,
};
use super::table::{extract_table_preview, inline_table_card_height};
use super::types::{Block, BlockId, Turn, TurnRole};
use crate::chatty::views::message_component::{DisplayMessage, MessageRole};
use crate::chatty::views::message_parsing::{ContentSegment, parse_content_segments};

/// Fixed height reported by a collapsed finished turn.
pub const COLLAPSED_TURN_HEIGHT: f32 = 36.0;

/// Map one [`DisplayMessage`] onto one [`Turn`].
pub fn adapt_message(msg: &DisplayMessage, message_index: usize, collapsed: bool) -> Turn {
    adapt_message_with_trace(msg, message_index, collapsed, None)
}

/// Same as [`adapt_message`], but a persisted history trace can supply blocks
/// when `live_trace` is empty (finalized conversations).
pub fn adapt_message_with_trace(
    msg: &DisplayMessage,
    message_index: usize,
    collapsed: bool,
    history_trace: Option<&SystemTrace>,
) -> Turn {
    let namespace = (message_index as u64).saturating_add(1);
    let mut blocks = Vec::new();
    let trace = msg.live_trace.as_ref().or(history_trace);

    match msg.role {
        MessageRole::User => {
            blocks.push(Block::User {
                id: BlockId::from_parts(namespace, "user"),
                content: msg.content.clone(),
                attachments: msg.attachments.clone(),
            });
        }
        MessageRole::Assistant => {
            if let Some(trace) = trace {
                push_trace_blocks(&mut blocks, namespace, trace);
                consolidate_receipt_artifacts(&mut blocks, namespace);
                // An image already rendered inline under the bubble does not
                // also need a receipt card. `add_attachment` on a file a tool
                // already queued (a browser screenshot, say) would otherwise
                // show the same picture twice.
                drop_artifact_cards_shown_inline(&mut blocks, &msg.attachments);
            }
            if !msg.content.is_empty() {
                blocks.push(Block::Text {
                    id: BlockId::from_parts(namespace, "text"),
                    content: msg.content.clone(),
                    streaming: msg.is_streaming,
                    attachments: msg.attachments.clone(),
                });
            }
        }
    }

    let elapsed = trace.and_then(|trace| trace.total_duration);

    Turn {
        id: namespace,
        message_index,
        role: match msg.role {
            MessageRole::User => TurnRole::User,
            MessageRole::Assistant => TurnRole::Assistant,
        },
        blocks,
        elapsed,
        collapsed: collapsed && matches!(msg.role, MessageRole::Assistant),
        streaming: msg.is_streaming,
    }
}

fn push_trace_blocks(blocks: &mut Vec<Block>, namespace: u64, trace: &SystemTrace) {
    let mut activity_tools = Vec::new();
    let mut plan_emitted = false;
    let flush_activity = |blocks: &mut Vec<Block>, tools: &mut Vec<_>| {
        if tools.is_empty() {
            return;
        }
        let key = tools
            .first()
            .map(|t: &chatty_core::models::message_types::ToolCallBlock| t.id.clone())
            .unwrap_or_else(|| "activity".into());
        blocks.push(Block::Activity {
            id: BlockId::from_parts(namespace, &format!("activity-{key}")),
            tools: std::mem::take(tools),
        });
    };

    for (idx, item) in trace.items.iter().enumerate() {
        match item {
            TraceItem::Thinking(thinking) => {
                flush_activity(blocks, &mut activity_tools);
                blocks.push(Block::Thinking {
                    id: BlockId::from_parts(namespace, &format!("thinking-{idx}")),
                    block: thinking.clone(),
                });
            }
            TraceItem::ApprovalPrompt(approval) => {
                flush_activity(blocks, &mut activity_tools);
                blocks.push(Block::Approval {
                    id: BlockId::from_parts(namespace, &approval.id),
                    approval: approval.clone(),
                });
            }
            TraceItem::ToolCall(tool) => {
                if is_agent_todo_tool(&tool.tool_name) {
                    if tool.tool_name == "write_todos" && !plan_emitted {
                        flush_activity(blocks, &mut activity_tools);
                        blocks.push(Block::Plan {
                            id: BlockId::from_parts(namespace, "plan"),
                        });
                        plan_emitted = true;
                    }
                } else if is_diff_tool(tool) {
                    // Keep the edit in the activity tally/rows, then show the
                    // rich hunk list as its own block (AGE-131).
                    activity_tools.push(tool.clone());
                    flush_activity(blocks, &mut activity_tools);
                    blocks.push(Block::Diff {
                        id: BlockId::from_parts(namespace, &tool.id),
                        tool: tool.clone(),
                    });
                } else if tool.tool_name == "query_data"
                    && matches!(tool.state, ToolCallState::Success)
                    && extract_table_preview(tool).is_some()
                {
                    activity_tools.push(tool.clone());
                    flush_activity(blocks, &mut activity_tools);
                    if let Some(preview) = extract_table_preview(tool) {
                        blocks.push(Block::TablePreview {
                            id: BlockId::from_parts(namespace, &tool.id),
                            preview,
                        });
                    }
                } else if tool.tool_name == "create_chart"
                    && matches!(tool.state, ToolCallState::Success)
                    && chart_artifact_path(tool).is_some()
                {
                    activity_tools.push(tool.clone());
                    flush_activity(blocks, &mut activity_tools);
                    if let Some(path) = chart_artifact_path(tool)
                        && is_transcript_artifact_receipt(&path)
                    {
                        blocks.push(Block::Artifact {
                            id: BlockId::from_parts(namespace, &tool.id),
                            path,
                            old_content: None,
                        });
                    }
                } else if matches!(tool.state, ToolCallState::Success)
                    && attachment_image_path(tool).is_some()
                {
                    activity_tools.push(tool.clone());
                    flush_activity(blocks, &mut activity_tools);
                    if let Some(path) = attachment_image_path(tool)
                        && is_transcript_artifact_receipt(&path)
                    {
                        blocks.push(Block::Artifact {
                            id: BlockId::from_parts(namespace, &tool.id),
                            path,
                            old_content: None,
                        });
                    }
                } else if matches!(tool.state, ToolCallState::Success)
                    && is_produced_file_tool(&tool.tool_name, &tool.input)
                {
                    // Count/show the write in the activity group, then attach
                    // an artifact receipt so provenance stays next to the turn.
                    //
                    // Success-gated like every other receipt above: without it
                    // this arm also swallowed *failed* writes, which both hid
                    // the error (the `tool_error` arm below never ran) and
                    // minted a card for a file that was never written.
                    activity_tools.push(tool.clone());
                    flush_activity(blocks, &mut activity_tools);
                    if let Some(path) = artifact_path(tool)
                        && is_transcript_artifact_receipt(&path)
                    {
                        blocks.push(Block::Artifact {
                            id: BlockId::from_parts(namespace, &tool.id),
                            path,
                            old_content: artifact_old_content_from_tool(tool),
                        });
                    }
                } else if let Some(err) = tool_error(tool) {
                    flush_activity(blocks, &mut activity_tools);
                    blocks.push(Block::Error {
                        id: BlockId::from_parts(namespace, &tool.id),
                        message: err,
                        detail: tool.output.clone(),
                    });
                } else {
                    activity_tools.push(tool.clone());
                }
            }
        }
    }
    flush_activity(blocks, &mut activity_tools);
}

/// Merge consecutive text/code artifact receipts into one batch card.
fn consolidate_receipt_artifacts(blocks: &mut Vec<Block>, namespace: u64) {
    let mut out = Vec::with_capacity(blocks.len());
    let mut pending: Vec<(PathBuf, Option<String>)> = Vec::new();

    let flush_batch = |pending: &mut Vec<(PathBuf, Option<String>)>, out: &mut Vec<Block>| {
        if pending.is_empty() {
            return;
        }
        if pending.len() == 1 {
            let (path, old_content) = pending.pop().expect("batch");
            out.push(Block::Artifact {
                id: BlockId::from_parts(namespace, &format!("artifact-{}", path.display())),
                path,
                old_content,
            });
        } else {
            let key = pending[0].0.display().to_string();
            out.push(Block::ArtifactBatch {
                id: BlockId::from_parts(namespace, &format!("artifact-batch-{key}")),
                files: std::mem::take(pending),
            });
        }
    };

    for block in blocks.drain(..) {
        match block {
            Block::Artifact {
                path, old_content, ..
            } if is_transcript_artifact_receipt(&path) && !is_standalone_artifact_path(&path) => {
                pending.push((path, old_content));
            }
            Block::Artifact {
                path,
                old_content,
                id,
                ..
            } if is_transcript_artifact_receipt(&path) => {
                flush_batch(&mut pending, &mut out);
                out.push(Block::Artifact {
                    id,
                    path,
                    old_content,
                });
            }
            Block::Artifact { .. } => {}
            other => {
                flush_batch(&mut pending, &mut out);
                out.push(other);
            }
        }
    }
    flush_batch(&mut pending, &mut out);
    *blocks = out;
}

pub(crate) fn is_agent_todo_tool(name: &str) -> bool {
    matches!(name, "write_todos" | "update_todo" | "verify_completion")
}

/// Insert a single plan block onto the last assistant turn when a snapshot
/// exists but `write_todos` never appeared in the trace.
pub fn attach_plan_block(turns: &mut [Turn], has_plan: bool) {
    if !has_plan || plan_turn_index(turns).is_some() {
        return;
    }
    let Some(turn) = turns
        .iter_mut()
        .rev()
        .find(|turn| matches!(turn.role, TurnRole::Assistant))
    else {
        return;
    };
    let insert_at = turn
        .blocks
        .iter()
        .position(|block| !matches!(block, Block::Thinking { .. }))
        .unwrap_or(turn.blocks.len());
    turn.blocks.insert(
        insert_at,
        Block::Plan {
            id: BlockId::from_parts(turn.id, "plan"),
        },
    );
}

pub fn plan_turn_index(turns: &[Turn]) -> Option<usize> {
    turns.iter().position(|turn| {
        turn.blocks
            .iter()
            .any(|block| matches!(block, Block::Plan { .. }))
    })
}

/// Layout inputs the height estimator needs.
///
/// Bundled into one struct because the estimator kept growing dimensions
/// (font size, per-block expansion state) that would otherwise be threaded
/// through every block arm one parameter at a time.
#[derive(Clone, Copy, Debug)]
pub struct TranscriptLayout<'a> {
    /// Usable width inside a bubble, in px.
    pub content_width_px: f32,
    /// The app's configured font size. `GeneralSettingsModel` defaults to 14.
    pub font_size: f32,
    /// Number of steps in the active plan, for `Block::Plan`.
    pub plan_steps: usize,
    /// Expansion state of activity cards, keyed by `BlockId.0`. An expanded
    /// card is several times taller than a collapsed one; estimating every
    /// card as collapsed is what made later blocks paint over it (AGE-183).
    pub activity_expanded: &'a HashMap<u64, bool>,
}

/// Font size the estimator's pixel constants were measured at.
pub const BASE_FONT_SIZE: f32 = 14.0;

impl TranscriptLayout<'_> {
    /// How much taller/wider everything is than at [`BASE_FONT_SIZE`].
    ///
    /// Clamped below at 1.0: a smaller font may render shorter than modeled,
    /// and shrinking the estimate is the direction that overlaps text.
    fn scale(&self) -> f32 {
        (self.font_size / BASE_FONT_SIZE).max(1.0)
    }

    fn line_height(&self) -> f32 {
        LINE_HEIGHT * self.scale()
    }

    /// Approximate usable text width inside a message bubble for wrapping.
    fn chars_per_line(&self) -> f32 {
        const MIN_CHARS: f32 = 16.0;
        const MAX_CHARS: f32 = 80.0;
        (self.content_width_px / (AVG_CHAR_WIDTH * self.scale())).clamp(MIN_CHARS, MAX_CHARS)
    }

    /// Same, for the narrower monospace column inside a code fence.
    fn mono_chars_per_line(&self) -> f32 {
        const MIN_CHARS: f32 = 12.0;
        const MAX_CHARS: f32 = 100.0;
        // Fences are inset by their frame, so subtract the horizontal padding.
        ((self.content_width_px - CODE_FENCE_INSET) / (MONO_CHAR_WIDTH * self.scale()))
            .clamp(MIN_CHARS, MAX_CHARS)
    }

    fn is_activity_expanded(&self, id: u64, tools: &[ToolCallBlock]) -> bool {
        // Mirrors `render_typed_block`: a failed group is always expanded.
        let default_open = RunTally::has_failure(tools);
        self.activity_expanded
            .get(&id)
            .copied()
            .unwrap_or(default_open)
    }
}

/// Base line box for prose at [`BASE_FONT_SIZE`].
const LINE_HEIGHT: f32 = 22.0;
/// Average prose glyph advance at [`BASE_FONT_SIZE`].
const AVG_CHAR_WIDTH: f32 = 7.2;
/// Average monospace glyph advance at [`BASE_FONT_SIZE`].
const MONO_CHAR_WIDTH: f32 = 8.4;
/// Line box inside a fenced code block.
const CODE_LINE_HEIGHT: f32 = 20.0;
/// A fence renders through `CodeBlockComponent`: header row + vertical padding.
const CODE_FENCE_CHROME: f32 = 44.0;
/// Horizontal frame a fence takes out of the bubble width.
const CODE_FENCE_INSET: f32 = 32.0;
/// Vertical rhythm between paragraphs (what a blank source line becomes).
const PARAGRAPH_MARGIN: f32 = 8.0;
/// A rendered table row.
const TABLE_ROW_HEIGHT: f32 = 28.0;
/// A ```mermaid fence renders as a diagram, not as its source lines.
const MERMAID_DIAGRAM: f32 = 320.0;
/// A `$$ … $$` display-math region renders as an SVG.
const MATH_BLOCK: f32 = 48.0;
/// Padding around the bubble body.
const BUBBLE_PADDING: f32 = 28.0;
/// Chrome of a `<think>` card in `render_thinking_block`: bottom margin,
/// vertical padding, and the “Thinking” header row with its own margin.
const THINKING_CARD_CHROME: f32 = 60.0;
/// Horizontal frame a `<think>` card takes out of the bubble width
/// (`p_3` both sides plus the 4px left rule).
const THINKING_CARD_INSET: f32 = 28.0;
/// Floor for an inset card's usable width, so a narrow window cannot drive the
/// wrap estimate to zero or negative.
const MIN_CARD_WIDTH: f32 = 120.0;
/// A non-image attachment renders as a filename chip.
const FILE_ATTACHMENT_ROW: f32 = 56.0;
/// An image renders as a thumbnail whose wrapper `render_attachments` clamps
/// to [`INLINE_IMAGE_MAX_PX`] square, plus its border and the row's bottom
/// margin. Estimating a chip here is what made screenshots paint over the
/// following turn, so this is derived from the renderer's own bound rather
/// than written out again.
const IMAGE_ATTACHMENT_ROW: f32 = super::artifact_kind::INLINE_IMAGE_MAX_PX + 12.0;
/// Deliberate over-estimate applied to every bubble.
///
/// The asymmetry is the point: an over-estimate leaves a small gap, an
/// under-estimate paints the next turn on top of this one. They are not
/// equally bad, so the estimator does not treat them as if they were.
const SAFETY_FACTOR: f32 = 1.05;

/// Height of a user/assistant bubble rendered via [`render_message`].
///
/// Virtual-list slots use a fixed height; under-estimating here makes the next
/// turn paint on top of this one (e.g. user prompt + “Worked for…” + artifact).
///
/// The walk is structural rather than flat: `render_message` renders markdown,
/// so headings, fences, tables, math and mermaid each get their rendered
/// footprint instead of being charged one prose line apiece.
pub fn estimate_message_bubble_height(
    content: &str,
    attachments: &[PathBuf],
    layout: &TranscriptLayout<'_>,
) -> f32 {
    let attachment_height: f32 = attachments
        .iter()
        .map(|path| {
            if super::artifact_kind::is_image_path(path) {
                IMAGE_ATTACHMENT_ROW
            } else {
                FILE_ATTACHMENT_ROW
            }
        })
        .sum();

    let body = estimate_body_height(content, layout);

    (BUBBLE_PADDING + body) * SAFETY_FACTOR + attachment_height
}

/// Rendered height of a message body, thinking cards included.
///
/// `render_message` splits the content on `<think>` tags and renders each
/// thinking segment as a bordered card, not as prose. Charging those segments
/// as plain lines left every card's chrome unreserved, so the next turn's
/// “Working for …” header painted across the card.
///
/// The split goes through the renderer's own [`parse_content_segments`] so the
/// two cannot disagree about where a card starts.
fn estimate_body_height(content: &str, layout: &TranscriptLayout<'_>) -> f32 {
    let segments = parse_content_segments(content);
    if segments.is_empty() {
        return estimate_markdown_height(content, layout);
    }

    segments
        .iter()
        .map(|segment| match segment {
            ContentSegment::Text(text) => estimate_markdown_height(text, layout),
            ContentSegment::Thinking(text) => estimate_thinking_card_height(text, layout),
        })
        .sum()
}

/// Height of one `<think>` card as `render_thinking_block` draws it.
///
/// The body is charged at prose line height even though the card renders it at
/// `text_sm`: the smaller face is the direction that leaves a gap, and a gap
/// is the failure this estimator is allowed to have.
fn estimate_thinking_card_height(text: &str, layout: &TranscriptLayout<'_>) -> f32 {
    let inner = TranscriptLayout {
        content_width_px: (layout.content_width_px - THINKING_CARD_INSET).max(MIN_CARD_WIDTH),
        ..*layout
    };

    THINKING_CARD_CHROME * layout.scale() + estimate_markdown_height(text, &inner)
}

/// Rendered height of a markdown body, walked line by line.
///
/// Every branch sums per line rather than taking `max(wrapped, explicit)`.
/// Both of those were lower bounds, and the max of two lower bounds is still a
/// lower bound — mixed markdown (many short list lines plus a few long
/// paragraphs) defeated both at once (AGE-179).
fn estimate_markdown_height(content: &str, layout: &TranscriptLayout<'_>) -> f32 {
    let line_height = layout.line_height();
    let scale = layout.scale();
    let chars_per_line = layout.chars_per_line();
    let mono_chars_per_line = layout.mono_chars_per_line();

    let mut height = 0.0_f32;
    let mut fence: Option<FenceKind> = None;
    let mut fence_lines = 0.0_f32;
    let mut in_math = false;
    let mut math_lines = 0.0_f32;

    for raw in content.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        if let Some(kind) = fence {
            if is_fence_delimiter(trimmed) {
                height += close_fence(kind, fence_lines, scale);
                fence = None;
                fence_lines = 0.0;
            } else {
                fence_lines += wrapped_lines(line.len(), mono_chars_per_line);
            }
            continue;
        }

        if is_fence_delimiter(trimmed) {
            fence = Some(FenceKind::from_info(&trimmed[3..]));
            fence_lines = 0.0;
            continue;
        }

        // `$$` toggles a display-math region that renders as one SVG. A long
        // equation renders taller than the one-line minimum, so the region
        // costs at least its source's worth of rows.
        if trimmed == "$$" {
            if in_math {
                height += math_region_height(math_lines, line_height, scale);
                math_lines = 0.0;
            }
            in_math = !in_math;
            continue;
        }
        if in_math {
            math_lines += 1.0;
            continue;
        }

        if trimmed.is_empty() {
            height += PARAGRAPH_MARGIN * scale;
            continue;
        }

        if let Some(level) = heading_level(trimmed) {
            height += heading_height(level, scale);
            continue;
        }

        if trimmed.starts_with('|') {
            height += TABLE_ROW_HEIGHT * scale;
            continue;
        }

        height += wrapped_lines(line.len(), chars_per_line) * line_height;
    }

    // An unterminated fence still renders as a frame.
    if let Some(kind) = fence {
        height += close_fence(kind, fence_lines, scale);
    }
    if in_math {
        height += math_region_height(math_lines, line_height, scale);
    }

    // An empty body still occupies one line.
    height.max(line_height)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FenceKind {
    Code,
    Mermaid,
}

impl FenceKind {
    fn from_info(info: &str) -> Self {
        if info.trim().eq_ignore_ascii_case("mermaid") {
            Self::Mermaid
        } else {
            Self::Code
        }
    }
}

/// Height a fence contributes once closed.
///
/// A mermaid fence is charged its rendered diagram, not its source: ten lines
/// of source that render as a 300px diagram were being charged 220px.
fn close_fence(kind: FenceKind, source_lines: f32, scale: f32) -> f32 {
    match kind {
        FenceKind::Mermaid => MERMAID_DIAGRAM * scale,
        FenceKind::Code => CODE_FENCE_CHROME * scale + source_lines * CODE_LINE_HEIGHT * scale,
    }
}

fn is_fence_delimiter(trimmed: &str) -> bool {
    trimmed.starts_with("```")
}

/// ATX heading level, if this line is one.
fn heading_level(trimmed: &str) -> Option<u8> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // `#hashtag` is not a heading; a heading needs a space after the hashes.
    match trimmed.chars().nth(hashes) {
        Some(' ') => Some(hashes as u8),
        _ => None,
    }
}

/// Headings render larger than body text and carry their own margin.
fn heading_height(level: u8, scale: f32) -> f32 {
    let factor = match level {
        1 => 1.8,
        2 => 1.5,
        3 => 1.3,
        _ => 1.15,
    };
    (LINE_HEIGHT * factor + PARAGRAPH_MARGIN) * scale
}

/// Height of one `$$ … $$` region.
///
/// At least the rendered SVG's minimum, and never less than the source it was
/// written on — a multi-line equation renders tall.
fn math_region_height(source_lines: f32, line_height: f32, scale: f32) -> f32 {
    (MATH_BLOCK * scale).max(source_lines * line_height)
}

/// Rows a line of `len` bytes wraps into. Never below one: an empty line in a
/// list or fence still occupies a row.
///
/// Bytes, not chars, on purpose: for non-ASCII text a byte count over-counts
/// rows, and over-estimating leaves a gap where under-estimating overlaps text.
fn wrapped_lines(len: usize, chars_per_line: f32) -> f32 {
    ((len as f32) / chars_per_line).ceil().max(1.0)
}

#[cfg(test)]
mod attachment_height_tests {
    use super::*;

    /// Default layout for tests: 600px wide, default font, no plan, nothing
    /// expanded.
    fn layout(expanded: &HashMap<u64, bool>) -> TranscriptLayout<'_> {
        TranscriptLayout {
            content_width_px: 600.0,
            font_size: BASE_FONT_SIZE,
            plan_steps: 0,
            activity_expanded: expanded,
        }
    }

    fn text_block(attachments: Vec<PathBuf>) -> Block {
        Block::Text {
            id: BlockId::from_parts(1, "text"),
            content: "Here is the screenshot.".to_string(),
            streaming: false,
            attachments,
        }
    }

    /// An image renders up to 300px tall inside the bubble. The virtual list
    /// assigns each turn a fixed slot, so an under-estimate here paints the
    /// image straight over the following turn.
    #[test]
    fn assistant_image_attachment_reserves_thumbnail_height() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let bare = block_estimated_height(&text_block(vec![]), &l);
        let with_image = block_estimated_height(&text_block(vec![PathBuf::from("shot.png")]), &l);

        assert!(
            with_image - bare >= 300.0,
            "an image attachment must reserve at least its 300px thumbnail              (bare={bare}, with_image={with_image})"
        );
    }

    /// The renderer clamps a thumbnail's wrapper to `INLINE_IMAGE_MAX_PX`
    /// square; the estimator must reserve at least that, or a full-page
    /// screenshot paints over the block after it (AGE-183).
    #[test]
    fn image_reserve_covers_the_renderer_clamp() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let bare = block_estimated_height(&text_block(vec![]), &l);
        let with_image = block_estimated_height(&text_block(vec![PathBuf::from("shot.png")]), &l);

        assert!(
            with_image - bare >= super::super::artifact_kind::INLINE_IMAGE_MAX_PX,
            "the reserved slot must cover the thumbnail the renderer can draw"
        );
    }

    #[test]
    fn non_image_attachment_reserves_only_a_chip() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let bare = block_estimated_height(&text_block(vec![]), &l);
        let with_file = block_estimated_height(&text_block(vec![PathBuf::from("report.pdf")]), &l);
        let delta = with_file - bare;
        assert!(
            (40.0..100.0).contains(&delta),
            "a filename chip should not reserve thumbnail height, got {delta}"
        );
    }

    // -------------------------------------------------------------------
    // Height estimator regressions (AGE-179, AGE-183)
    //
    // The virtual list gives each turn a fixed slot and does not clip its
    // contents, so an under-estimate paints the next turn on top of this one.
    // Over-estimating leaves a small gap. The two are not equally bad, and
    // these tests are written in that direction.
    // -------------------------------------------------------------------

    fn tool(name: &str, state: ToolCallState) -> ToolCallBlock {
        ToolCallBlock {
            id: name.to_string(),
            tool_name: name.to_string(),
            display_name: name.to_string(),
            input: String::new(),
            output: None,
            output_preview: None,
            state,
            duration: None,
            text_before: String::new(),
            source: chatty_core::models::message_types::ToolSource::Local,
            execution_engine: None,
        }
    }

    fn activity(id: u64, tools: Vec<ToolCallBlock>) -> Block {
        Block::Activity {
            id: BlockId(id),
            tools,
        }
    }

    /// AGE-183: an expanded "Explored 2 files" card was charged the same flat
    /// 40px as a collapsed one, so the blocks after it — the streaming status
    /// header and the to-do panel — were placed inside its bounds.
    #[test]
    fn expanded_activity_card_is_taller_than_collapsed() {
        let tools = vec![
            tool("browser_navigate", ToolCallState::Success),
            tool("browser_screenshot", ToolCallState::Success),
        ];
        let block = activity(7, tools);

        let collapsed_map = HashMap::from([(7u64, false)]);
        let expanded_map = HashMap::from([(7u64, true)]);
        let collapsed = block_estimated_height(&block, &layout(&collapsed_map));
        let expanded = block_estimated_height(&block, &layout(&expanded_map));

        assert!(
            expanded > collapsed,
            "an expanded card must reserve more than a collapsed one \
             (collapsed={collapsed}, expanded={expanded})"
        );
        assert!(
            expanded >= ACTIVITY_HEADER + 2.0 * ACTIVITY_ROW,
            "an expanded 2-row card must reserve header + both rows, got {expanded}"
        );
    }

    /// A failed group renders expanded whether or not the user opened it, so
    /// the estimate has to follow that default.
    #[test]
    fn failed_activity_card_is_estimated_expanded_by_default() {
        let block = activity(
            9,
            vec![
                tool("browser_navigate", ToolCallState::Error("boom".into())),
                tool("browser_navigate", ToolCallState::Error("boom".into())),
            ],
        );
        let empty = HashMap::new();
        let height = block_estimated_height(&block, &layout(&empty));
        assert!(
            height >= ACTIVITY_HEADER + 2.0 * ACTIVITY_ROW,
            "a failed group renders expanded, got {height}"
        );
    }

    #[test]
    fn expanded_activity_grows_with_row_count() {
        let expanded_map = HashMap::from([(1u64, true)]);
        let two = block_estimated_height(
            &activity(
                1,
                vec![
                    tool("a", ToolCallState::Success),
                    tool("b", ToolCallState::Success),
                ],
            ),
            &layout(&expanded_map),
        );
        let five = block_estimated_height(
            &activity(
                1,
                (0..5)
                    .map(|i| tool(&format!("t{i}"), ToolCallState::Success))
                    .collect(),
            ),
            &layout(&expanded_map),
        );
        assert!(
            five >= two + 3.0 * ACTIVITY_ROW,
            "each row needs its own space"
        );
    }

    /// AGE-179 defect 1: `max(wrapped, explicit)` is the max of two lower
    /// bounds, which is still a lower bound. Mixed markdown defeats both.
    #[test]
    fn estimate_never_below_the_per_line_sum() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        // Many short list lines plus a few long wrapping paragraphs — the
        // shape `max()` under-counts.
        let mut content = String::new();
        for i in 0..120 {
            content.push_str(&format!("- short item {i}\n"));
        }
        for _ in 0..5 {
            content.push_str(&"word ".repeat(80));
            content.push('\n');
        }

        let per_line_sum: f32 = content
            .lines()
            .map(|line| wrapped_lines(line.len(), l.chars_per_line()))
            .sum::<f32>()
            * l.line_height();

        let estimate = estimate_message_bubble_height(&content, &[], &l);
        assert!(
            estimate >= per_line_sum,
            "estimate {estimate} is below the per-line sum {per_line_sum}"
        );
    }

    #[test]
    fn estimate_is_monotone_in_content_length() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let mut previous = 0.0_f32;
        for n in [1usize, 10, 50, 200, 800] {
            let content = "some prose line here\n".repeat(n);
            let height = estimate_message_bubble_height(&content, &[], &l);
            assert!(
                height >= previous,
                "estimate shrank as content grew ({previous} -> {height})"
            );
            previous = height;
        }
    }

    /// AGE-179 defect 2: the rendered content is markdown, so a heading and a
    /// fence cost more than the flat 22px per source line they were charged.
    #[test]
    fn markdown_structure_costs_more_than_flat_prose() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let prose = "aaaa\nbbbb\ncccc\ndddd\neeee\n";
        let structured = "# Heading\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n";

        let prose_h = estimate_message_bubble_height(prose, &[], &l);
        let structured_h = estimate_message_bubble_height(structured, &[], &l);
        assert!(
            structured_h > prose_h,
            "markdown structure must cost more than the same number of plain \
             lines (prose={prose_h}, structured={structured_h})"
        );
    }

    /// A `<think>` segment renders as a bordered card, not as prose. The card
    /// has to be reserved with its chrome, or the following turn's “Working
    /// for …” header lands on top of the thinking text.
    #[test]
    fn thinking_card_reserves_more_than_the_same_text_as_prose() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let text = "That's not the right URL. Let me click the third article.";
        let prose = estimate_message_bubble_height(text, &[], &l);
        let card = estimate_message_bubble_height(&format!("<think>{text}</think>"), &[], &l);

        assert!(
            card >= prose + THINKING_CARD_CHROME,
            "a thinking card must reserve its chrome on top of its text \
             (prose={prose}, card={card})"
        );
    }

    /// Text around a thinking card still costs its own lines — the card must
    /// add to the body, not replace it.
    #[test]
    fn thinking_card_adds_to_surrounding_text() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let card_only = estimate_message_bubble_height("<think>reasoning</think>", &[], &l);
        let with_answer = estimate_message_bubble_height(
            "<think>reasoning</think>\nHere is the answer.\nSecond line.",
            &[],
            &l,
        );

        assert!(
            with_answer > card_only,
            "the answer after a thinking card needs its own lines \
             (card_only={card_only}, with_answer={with_answer})"
        );
    }

    #[test]
    fn fenced_block_reserves_its_frame() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let without = estimate_message_bubble_height("intro\n", &[], &l);
        let with = estimate_message_bubble_height("intro\n```\nlet x = 1;\n```\n", &[], &l);
        assert!(
            with - without >= CODE_FENCE_CHROME,
            "a fence must reserve its header and padding, got {}",
            with - without
        );
    }

    /// Mirrors the existing image-attachment test: a mermaid source block
    /// renders as a diagram, not as its handful of source lines.
    #[test]
    fn mermaid_segment_reserves_its_rendered_footprint() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let bare = estimate_message_bubble_height("intro\n", &[], &l);
        let diagram =
            estimate_message_bubble_height("intro\n```mermaid\ngraph TD;\nA-->B;\n```\n", &[], &l);
        assert!(
            diagram - bare >= MERMAID_DIAGRAM,
            "a mermaid fence must reserve its rendered diagram, got {}",
            diagram - bare
        );
    }

    #[test]
    fn display_math_reserves_its_svg() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let bare = estimate_message_bubble_height("intro\n", &[], &l);
        let math = estimate_message_bubble_height("intro\n$$\nx = 1\n$$\n", &[], &l);
        assert!(math > bare, "display math must reserve its rendered height");
    }

    /// AGE-179 defect 3: the constants encode the 14px default, but font size
    /// is user-configurable, so the shortfall scaled with answer length.
    #[test]
    fn estimate_grows_with_font_size() {
        let expanded = HashMap::new();
        let content = "# Plan\n\n1. first step\n2. second step\n".repeat(20);

        let small = estimate_message_bubble_height(
            &content,
            &[],
            &TranscriptLayout {
                content_width_px: 600.0,
                font_size: BASE_FONT_SIZE,
                plan_steps: 0,
                activity_expanded: &expanded,
            },
        );
        let large = estimate_message_bubble_height(
            &content,
            &[],
            &TranscriptLayout {
                content_width_px: 600.0,
                font_size: 18.0,
                plan_steps: 0,
                activity_expanded: &expanded,
            },
        );
        assert!(
            large > small,
            "a larger font must estimate taller (14px={small}, 18px={large})"
        );
    }

    #[test]
    fn hashtag_is_not_a_heading() {
        assert_eq!(heading_level("#tag"), None);
        assert_eq!(heading_level("####### too many"), None);
        assert_eq!(heading_level("## Real heading"), Some(2));
    }

    #[test]
    fn multiple_images_accumulate() {
        let expanded = HashMap::new();
        let l = layout(&expanded);
        let one = block_estimated_height(&text_block(vec![PathBuf::from("a.png")]), &l);
        let two = block_estimated_height(
            &text_block(vec![PathBuf::from("a.png"), PathBuf::from("b.png")]),
            &l,
        );
        assert!(two - one >= 300.0, "each image needs its own slot");
    }
}

/// Remove artifact cards whose image is already shown inline on this message.
fn drop_artifact_cards_shown_inline(blocks: &mut Vec<Block>, attachments: &[PathBuf]) {
    if attachments.is_empty() {
        return;
    }
    blocks.retain(|block| match block {
        Block::Artifact { path, .. } => !attachments.contains(path),
        _ => true,
    });
}

#[cfg(test)]
mod duplicate_render_tests {
    use super::*;

    fn artifact(path: &str) -> Block {
        Block::Artifact {
            id: BlockId::from_parts(1, path),
            path: PathBuf::from(path),
            old_content: None,
        }
    }

    #[test]
    fn drops_the_card_when_the_image_renders_inline() {
        let shot = PathBuf::from("/ws/.chatty/browser/shot.png");
        let mut blocks = vec![artifact("/ws/.chatty/browser/shot.png")];
        drop_artifact_cards_shown_inline(&mut blocks, &[shot]);
        assert!(
            blocks.is_empty(),
            "the inline image is the only render needed"
        );
    }

    #[test]
    fn keeps_cards_for_files_not_shown_inline() {
        let mut blocks = vec![artifact("/ws/report.pdf"), artifact("/ws/other.png")];
        drop_artifact_cards_shown_inline(&mut blocks, &[PathBuf::from("/ws/shot.png")]);
        assert_eq!(blocks.len(), 2, "unrelated artifacts keep their cards");
    }

    #[test]
    fn no_attachments_changes_nothing() {
        let mut blocks = vec![artifact("/ws/a.png")];
        drop_artifact_cards_shown_inline(&mut blocks, &[]);
        assert_eq!(blocks.len(), 1);
    }
}

/// Header row of an activity card ("Explored 2 files ›").
const ACTIVITY_HEADER: f32 = 40.0;
/// One tool row inside an expanded activity card.
const ACTIVITY_ROW: f32 = 28.0;
/// Vertical padding the expanded content area adds around its rows.
const ACTIVITY_CONTENT_PADDING: f32 = 12.0;

pub fn block_estimated_height(block: &Block, layout: &TranscriptLayout<'_>) -> f32 {
    let scale = layout.scale();
    match block {
        Block::User {
            content,
            attachments,
            ..
        } => estimate_message_bubble_height(content, attachments, layout),
        Block::Text {
            content,
            attachments,
            ..
        } => estimate_message_bubble_height(content, attachments, layout),
        Block::Thinking { .. } => 56.0 * scale,
        Block::Activity { id, tools } => {
            // An expanded card is header + one row per tool. Charging every
            // card its collapsed height is what placed the following blocks
            // inside it (AGE-183).
            if layout.is_activity_expanded(id.0, tools) {
                (ACTIVITY_HEADER + ACTIVITY_CONTENT_PADDING) * scale
                    + tools.len() as f32 * ACTIVITY_ROW * scale
            } else {
                ACTIVITY_HEADER * scale
            }
        }
        Block::Diff { .. } => 120.0 * scale,
        Block::Approval { approval, .. } => match approval.state {
            ApprovalState::Pending => 72.0 * scale,
            ApprovalState::Approved | ApprovalState::Denied => 28.0 * scale,
        },
        Block::Plan { .. } => (40.0 + 32.0 * layout.plan_steps.max(1) as f32) * scale,
        Block::Artifact { path, .. } => {
            if super::artifact_kind::is_image_path(path) {
                160.0
            } else {
                76.0
            }
        }
        Block::ArtifactBatch { files, .. } => {
            if files.len() <= 1 {
                76.0
            } else {
                52.0 + files.len().min(4) as f32 * 28.0
            }
        }
        Block::TablePreview { preview, .. } => inline_table_card_height(preview) * scale,
        Block::Error { .. } => 64.0 * scale,
    }
}

fn is_work_trace_block(block: &Block) -> bool {
    matches!(
        block,
        Block::Thinking { .. }
            | Block::Activity { .. }
            | Block::Diff { .. }
            | Block::Artifact { .. }
            | Block::ArtifactBatch { .. }
            | Block::TablePreview { .. }
            | Block::Approval { .. }
            | Block::Plan { .. }
            | Block::Error { .. }
    )
}

fn turn_has_work_fold(turn: &Turn) -> bool {
    matches!(turn.role, TurnRole::Assistant)
        && (turn.streaming || turn.blocks.iter().any(is_work_trace_block))
}

fn turn_message_is_empty(turn: &Turn) -> bool {
    !turn.blocks.iter().any(|block| match block {
        Block::User {
            content,
            attachments,
            ..
        } => !content.is_empty() || !attachments.is_empty(),
        Block::Text { content, .. } => !content.is_empty(),
        _ => false,
    })
}

fn block_visible_in_turn(turn: &Turn, block: &Block) -> bool {
    if turn.collapsed
        && matches!(
            block,
            Block::Thinking { .. } | Block::Activity { .. } | Block::Diff { .. }
        )
    {
        return false;
    }
    !matches!(block, Block::User { .. } | Block::Text { .. })
}

/// Content Y of the top of the inline plan block, including list top padding.
pub fn plan_block_top(
    turns: &[Turn],
    padding_top: Pixels,
    layout: &TranscriptLayout<'_>,
) -> Option<Pixels> {
    let ix = plan_turn_index(turns)?;
    let mut y = padding_top;
    for turn in &turns[..ix] {
        y += estimate_turn_height(turn, layout).height;
    }
    let turn = &turns[ix];
    if turn.collapsed {
        return Some(y);
    }
    y += px(48.0);
    for block in &turn.blocks {
        if matches!(block, Block::Plan { .. }) {
            return Some(y);
        }
        y += px(block_estimated_height(block, layout));
    }
    Some(y)
}

/// Content Y of the bottom of the inline plan block, including list top padding.
pub fn plan_block_bottom(
    turns: &[Turn],
    padding_top: Pixels,
    layout: &TranscriptLayout<'_>,
) -> Option<Pixels> {
    let top = plan_block_top(turns, padding_top, layout)?;
    let ix = plan_turn_index(turns)?;
    let turn = &turns[ix];
    if turn.collapsed {
        return Some(top + px(COLLAPSED_TURN_HEIGHT));
    }
    Some(
        top + px(block_estimated_height(
            &Block::Plan { id: BlockId(0) },
            layout,
        )),
    )
}

/// True when the plan card has fully scrolled above the viewport.
pub fn plan_is_above_viewport(plan_bottom: Pixels, viewport_top: Pixels) -> bool {
    plan_bottom + px(8.0) <= viewport_top
}

fn is_diff_tool(tool: &chatty_core::models::message_types::ToolCallBlock) -> bool {
    matches!(
        classify_tool(&tool.tool_name),
        super::activity::ToolKind::Edit
    ) && !is_produced_file_tool(&tool.tool_name, &tool.input)
}

/// Where a produced-file tool wrote, as far as its result can be trusted.
///
/// Output first (e.g. Typst's absolute `saved_path`), falling back to the
/// input, because tools like `write_file` report no structured output and the
/// requested path is all there is. Either way the path has to be openable: an
/// absolute path that is not on disk is a path the tool never wrote.
fn artifact_path(
    tool: &chatty_core::models::message_types::ToolCallBlock,
) -> Option<std::path::PathBuf> {
    tool.output
        .as_deref()
        .and_then(tool_file_path)
        .or_else(|| tool_file_path(&tool.input))
        .filter(|path| produced_path_is_openable(path))
}

fn tool_error(tool: &chatty_core::models::message_types::ToolCallBlock) -> Option<String> {
    match &tool.state {
        chatty_core::models::message_types::ToolCallState::Error(msg) => Some(msg.clone()),
        _ => None,
    }
}

pub fn adapt_messages(messages: &[DisplayMessage], collapsed_turns: &[bool]) -> Vec<Turn> {
    adapt_messages_with_traces(messages, collapsed_turns, &[])
}

pub fn adapt_messages_with_traces(
    messages: &[DisplayMessage],
    collapsed_turns: &[bool],
    traces: &[Option<SystemTrace>],
) -> Vec<Turn> {
    messages
        .iter()
        .enumerate()
        .map(|(index, msg)| {
            let collapsed = collapsed_turns.get(index).copied().unwrap_or(false);
            adapt_message_with_trace(
                msg,
                index,
                collapsed,
                traces.get(index).and_then(|t| t.as_ref()),
            )
        })
        .collect()
}

pub fn estimate_turn_height(turn: &Turn, layout: &TranscriptLayout<'_>) -> Size<Pixels> {
    // Mirror `render_visible_turns`: optional work header, typed blocks (not
    // User/Text), then `render_message` for the bubble body.
    let has_work_fold = turn_has_work_fold(turn);
    let empty_message = turn_message_is_empty(turn);

    // Consecutive collapsed work headers should sit tight — no empty bubble
    // shell and no extra turn padding.
    if has_work_fold && turn.collapsed && empty_message {
        let mut receipt_height = 0.0_f32;
        let mut receipts = 0u32;
        for block in &turn.blocks {
            if block_visible_in_turn(turn, block) {
                receipt_height += block_estimated_height(block, layout);
                receipts += 1;
            }
        }
        let mut height = COLLAPSED_TURN_HEIGHT + receipt_height;
        if receipts > 0 {
            height += 8.0 * receipts as f32;
        }
        return size(px(800.), px(height.max(28.0)));
    }

    let mut height = 0.0_f32;
    let mut flex_children = 0u32;

    if has_work_fold {
        height += COLLAPSED_TURN_HEIGHT;
        flex_children += 1;
    }

    if has_work_fold && !turn.collapsed {
        let overview = file_changes_height(&file_changes_from_turn(turn));
        if overview > 0.0 {
            height += overview;
            flex_children += 1;
        }
    }

    let mut message_height = 0.0_f32;
    for block in &turn.blocks {
        match block {
            Block::User {
                content,
                attachments,
                ..
            } => {
                message_height = message_height.max(estimate_message_bubble_height(
                    content,
                    attachments,
                    layout,
                ));
            }
            Block::Text {
                content,
                attachments,
                ..
            } => {
                message_height = message_height.max(estimate_message_bubble_height(
                    content,
                    attachments,
                    layout,
                ));
            }
            _ if block_visible_in_turn(turn, block) => {
                height += block_estimated_height(block, layout);
                flex_children += 1;
            }
            _ => {}
        }
    }

    let show_message = !empty_message || (!has_work_fold && message_height > 0.0);
    if show_message {
        let stacks_message_shell = has_work_fold || flex_children > 0;
        if stacks_message_shell {
            height += message_height.max(32.0);
        } else {
            height += message_height;
        }
        flex_children += 1;
    }

    // `gap_2` between work header, typed blocks, and message bubble.
    if flex_children > 1 {
        height += 8.0 * (flex_children - 1) as f32;
    }

    height += 8.0;
    size(px(800.), px(height.max(36.0)))
}

pub fn format_worked_for(elapsed: Option<Duration>) -> String {
    let Some(duration) = elapsed else {
        return "Worked for a moment".to_string();
    };
    let secs = duration.as_secs();
    if secs < 1 {
        format!("Worked for a moment · {}ms", duration.as_millis().max(1))
    } else if secs < 60 {
        format!("Worked for {secs}s")
    } else {
        format!("Worked for {}m {}s", secs / 60, secs % 60)
    }
}

pub fn format_working_for(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("Working for {secs}s")
    } else {
        format!("Working for {}m {}s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF that really is on disk. The adapter refuses to build a card for an
    /// absolute path that is not there, which is the point — a fixture that
    /// skips this is testing a case the renderer no longer accepts.
    fn written_pdf(dir_name: &str, file_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(dir_name);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(file_name);
        std::fs::write(&path, b"%PDF-1.4").expect("write pdf");
        path
    }

    /// Layout at a given content width, default font, nothing expanded.
    fn at_width(width: f32, expanded: &HashMap<u64, bool>) -> TranscriptLayout<'_> {
        TranscriptLayout {
            content_width_px: width,
            font_size: BASE_FONT_SIZE,
            plan_steps: 0,
            activity_expanded: expanded,
        }
    }

    #[test]
    fn worked_for_formats_seconds_and_minutes() {
        assert_eq!(format_worked_for(None), "Worked for a moment");
        assert_eq!(
            format_worked_for(Some(Duration::from_millis(40))),
            "Worked for a moment · 40ms"
        );
        assert_eq!(
            format_worked_for(Some(Duration::from_secs(4))),
            "Worked for 4s"
        );
        assert_eq!(
            format_worked_for(Some(Duration::from_secs(65))),
            "Worked for 1m 5s"
        );
        assert_eq!(format_working_for(Duration::from_secs(0)), "Working for 0s");
        assert_eq!(
            format_working_for(Duration::from_secs(12)),
            "Working for 12s"
        );
        assert_eq!(
            format_working_for(Duration::from_secs(75)),
            "Working for 1m 15s"
        );
    }

    #[test]
    fn long_user_prompt_gets_enough_virtual_list_height() {
        let prompt = "Create a short 2-page PDF report at workspace_notes.pdf using Typst. \
            Page 1: title 'Chatty PDF QA', a one-paragraph poem, and the formula E = mc^2. \
            Page 2: a small 2-column table (Item / Notes) with three rows. \
            Then open it so I can flip pages in the document panel.";
        let msg = DisplayMessage {
            role: MessageRole::User,
            content: prompt.into(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: None,
            is_markdown: false,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        let expanded = HashMap::new();
        let wide = estimate_turn_height(&turn, &at_width(520.0, &expanded)).height;
        let narrow = estimate_turn_height(&turn, &at_width(220.0, &expanded)).height;
        assert!(
            narrow > wide,
            "narrow width should reserve more height (wide={wide:?}, narrow={narrow:?})"
        );
        assert!(
            narrow >= px(170.0),
            "narrow user bubble under-estimated at {narrow:?} for {} chars",
            prompt.len()
        );
    }

    #[test]
    fn narrow_width_increases_wrapped_line_estimate() {
        let prompt = "Create a short 2-page PDF report at workspace_notes.pdf using Typst.";
        let expanded = HashMap::new();
        let wide = estimate_message_bubble_height(prompt, &[], &at_width(520.0, &expanded));
        let narrow = estimate_message_bubble_height(prompt, &[], &at_width(220.0, &expanded));
        assert!(narrow > wide);
    }

    #[test]
    fn collapsed_assistant_with_artifact_reserves_work_header_and_receipt() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "pdf".into(),
            tool_name: "compile_typst".into(),
            display_name: "Generating PDF".into(),
            input: r#"{"content":"= Hi","output_path":"workspace_notes.pdf"}"#.into(),
            output: Some(format!(
                r#"{{"saved_path":"{}","page_count":2}}"#,
                written_pdf("adapter_collapsed_receipt", "workspace_notes.pdf").display()
            )),
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: Some(Duration::from_secs(3)),
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, true);
        let expanded = HashMap::new();
        let height = estimate_turn_height(&turn, &at_width(400.0, &expanded)).height;
        assert!(
            height >= px(100.0),
            "assistant receipt turn under-estimated at {height:?}"
        );
        assert!(
            height < px(140.0),
            "collapsed receipt should not reserve an empty message shell, got {height:?}"
        );
    }

    #[test]
    fn collapsed_work_only_turn_is_compact() {
        let mut msg = assistant_with_tools(vec![sample_tool("a", "read_file")]);
        msg.content.clear();
        let turn = adapt_message(&msg, 0, true);
        let expanded = HashMap::new();
        let height = estimate_turn_height(&turn, &at_width(400.0, &expanded)).height;
        assert!(
            height <= px(48.0),
            "collapsed work header should sit tight, got {height:?}"
        );
    }

    #[test]
    fn streaming_empty_assistant_keeps_a_working_row() {
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: true,
            system_trace_view: None,
            live_trace: Some(chatty_core::models::message_types::SystemTrace::new()),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turns = adapt_messages(&[msg], &[true]);
        assert_eq!(turns.len(), 1, "live turn must stay in the transcript");
        assert!(turns[0].streaming);
        let expanded = HashMap::new();
        let height = estimate_turn_height(&turns[0], &at_width(400.0, &expanded)).height;
        assert!(
            height <= px(48.0),
            "collapsed Working header should sit tight, got {height:?}"
        );
    }

    #[test]
    fn block_ids_are_stable_and_not_indexes() {
        let a = BlockId::from_parts(1, "user");
        let b = BlockId::from_parts(1, "user");
        let c = BlockId::from_parts(1, "text");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a.0, 0);
    }

    #[test]
    fn pdf_tool_becomes_artifact_block() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "t1".into(),
            tool_name: "pdf_extract_text".into(),
            display_name: "pdf_extract_text".into(),
            input: r#"{"path":"docs/report.pdf"}"#.into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            turn.blocks
                .iter()
                .any(|block| matches!(block, Block::Artifact { path, .. } if path.ends_with("report.pdf"))),
            "pdf_* tools that name a .pdf should open as artifact cards, got {:?}",
            turn.blocks
        );
    }

    #[test]
    fn compile_typst_becomes_artifact_from_saved_path() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "typ".into(),
            tool_name: "compile_typst".into(),
            display_name: "Generating PDF".into(),
            input: r#"{"content":"= Hi","output_path":"reports/sales.pdf"}"#.into(),
            output: Some(format!(
                r#"{{"saved_path":"{}","page_count":1}}"#,
                written_pdf("adapter_typst_saved_path", "sales.pdf").display()
            )),
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            turn.blocks.iter().any(|block| {
                matches!(block, Block::Artifact { path, .. } if path.ends_with("sales.pdf"))
            }),
            "compile_typst should produce an artifact card, got {:?}",
            turn.blocks
        );
    }

    #[test]
    fn query_data_becomes_table_preview_block() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "qd1".into(),
            tool_name: "query_data".into(),
            display_name: "Querying data".into(),
            input: r#"{"query":"SELECT 1"}"#.into(),
            output: Some(
                r#"{"markdown_table":"| a |\n| --- |\n| 1 |","row_count":1,"column_count":1,"columns":[{"name":"a","data_type":"INTEGER"}],"truncated":false,"preview":{"title":"query_data","columns":[{"name":"a","data_type":"INTEGER"}],"rows":[["1"]],"row_count":1,"truncated":false,"source":{"kind":"query","sql":"SELECT 1"}}}"#.into(),
            ),
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: "One row returned.".into(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            turn.blocks
                .iter()
                .any(|block| matches!(block, Block::TablePreview { .. })),
            "query_data should produce a table preview receipt, got {:?}",
            turn.blocks
        );
    }

    #[test]
    fn create_chart_becomes_artifact_block() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let dir = std::env::temp_dir().join("adapter_chart_png");
        let _ = std::fs::create_dir_all(&dir);
        let png = dir.join("sales.png");
        std::fs::write(&png, b"\x89PNG").expect("write png");

        let tool = ToolCallBlock {
            id: "ch1".into(),
            tool_name: "create_chart".into(),
            display_name: "Creating chart".into(),
            input: r#"{"chart_type":"bar","save_path":"charts/sales.png","data":[]}"#.into(),
            output: Some(format!(
                r#"{{"chart_type":"bar","data":[],"saved_path":"{}"}}"#,
                png.display()
            )),
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: "Chart saved.".into(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            turn.blocks
                .iter()
                .any(|block| matches!(block, Block::Artifact { .. })),
            "create_chart with saved_path should produce an artifact card, got {:?}",
            turn.blocks
        );
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_dir(&dir);
    }

    fn sample_tool(id: &str, name: &str) -> chatty_core::models::message_types::ToolCallBlock {
        use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};
        ToolCallBlock {
            id: id.into(),
            tool_name: name.into(),
            display_name: name.into(),
            input: "{}".into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        }
    }

    fn assistant_with_tools(
        tools: Vec<chatty_core::models::message_types::ToolCallBlock>,
    ) -> DisplayMessage {
        use chatty_core::models::message_types::{SystemTrace, TraceItem};
        DisplayMessage {
            role: MessageRole::Assistant,
            content: "done".into(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: tools.into_iter().map(TraceItem::ToolCall).collect(),
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        }
    }

    #[test]
    fn write_todos_becomes_a_single_plan_block() {
        let msg = assistant_with_tools(vec![
            sample_tool("a", "read_file"),
            sample_tool("p", "write_todos"),
            sample_tool("u", "update_todo"),
            sample_tool("b", "read_file"),
        ]);
        let turn = adapt_message(&msg, 2, false);
        let plans: Vec<_> = turn
            .blocks
            .iter()
            .filter(|block| matches!(block, Block::Plan { .. }))
            .collect();
        assert_eq!(
            plans.len(),
            1,
            "plan must mutate in place, got {:?}",
            turn.blocks
        );
        let plan_id = turn
            .blocks
            .iter()
            .find_map(|block| match block {
                Block::Plan { id } => Some(*id),
                _ => None,
            })
            .unwrap();
        assert_eq!(plan_id, BlockId::from_parts(3, "plan"));
        assert!(
            !turn.blocks.iter().any(|block| match block {
                Block::Activity { tools, .. } =>
                    tools.iter().any(|tool| is_agent_todo_tool(&tool.tool_name)),
                _ => false,
            }),
            "todo tools must not appear as activity rows: {:?}",
            turn.blocks
        );
    }

    #[test]
    fn attach_plan_block_is_idempotent() {
        let msg = assistant_with_tools(vec![sample_tool("p", "write_todos")]);
        let mut turns = vec![adapt_message(&msg, 0, false)];
        attach_plan_block(&mut turns, true);
        attach_plan_block(&mut turns, true);
        assert_eq!(
            turns[0]
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Plan { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn plan_strip_hides_while_block_straddles_and_shows_when_fully_past() {
        let bottom = px(120.0);
        assert!(
            !plan_is_above_viewport(bottom, px(100.0)),
            "straddling the top edge must not show the strip"
        );
        assert!(
            !plan_is_above_viewport(bottom, px(126.0)),
            "8px hysteresis keeps the boundary quiet"
        );
        assert!(plan_is_above_viewport(bottom, px(130.0)));
    }

    fn tally_sentence_matches_linear_1a_order() {
        let sentence = RunTally {
            edits: 4,
            explore: 6,
            searches: 2,
            external: 1,
            commands: 1,
            added: 0,
            removed: 0,
        }
        .sentence();
        assert_eq!(
            sentence,
            "Edited 4 files, explored 6 files, 2 searches, 1 tool, ran 1 command"
        );
    }

    #[test]
    fn apply_diff_becomes_a_file_change() {
        let mut t = sample_tool("a", "apply_diff");
        t.input =
            r#"{"path":"src/main.rs","old_content":"fn a()\n","new_content":"fn b()\nfn c()\n"}"#
                .into();
        t.output = Some(r#"{"path":"src/main.rs","insertions":2,"deletions":1}"#.into());
        let change = file_change_from_tool(&t).expect("edit");
        assert_eq!(change.path, std::path::PathBuf::from("src/main.rs"));
        assert_eq!(change.added, 2);
        assert_eq!(change.removed, 1);
        assert!(change.old.contains("fn a"));
        assert!(change.new.contains("fn c"));
    }

    #[test]
    fn merge_sums_repeated_edits_to_the_same_path() {
        let mut first = sample_tool("a", "apply_diff");
        first.input = r#"{"path":"a.yml","old_content":"x","new_content":"y"}"#.into();
        first.output = Some(r#"{"path":"a.yml","insertions":4,"deletions":1}"#.into());
        let mut second = sample_tool("b", "apply_diff");
        second.input = r#"{"path":"a.yml","old_content":"y","new_content":"z"}"#.into();
        second.output = Some(r#"{"path":"a.yml","insertions":2,"deletions":0}"#.into());
        let merged = merge_file_changes([
            file_change_from_tool(&first).unwrap(),
            file_change_from_tool(&second).unwrap(),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].added, 6);
        assert_eq!(merged[0].removed, 1);
        assert_eq!(merged[0].old, "x");
        assert_eq!(merged[0].new, "z");
    }

    #[test]
    fn write_file_html_does_not_become_artifact_block() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "w1".into(),
            tool_name: "write_file".into(),
            display_name: "Writing file".into(),
            input: r#"{"path":"index.html","content":"<html></html>"}"#.into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            !turn
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Artifact { .. } | Block::ArtifactBatch { .. })),
            "code/html writes should not produce artifact cards, got {:?}",
            turn.blocks
        );
        assert!(
            turn.blocks
                .iter()
                .any(|block| matches!(block, Block::Activity { .. })),
            "write should still appear in activity, got {:?}",
            turn.blocks
        );
    }

    #[test]
    fn write_file_markdown_becomes_artifact_block() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "w2".into(),
            tool_name: "write_file".into(),
            display_name: "Writing file".into(),
            input: r#"{"path":"README.md","content":"Hello world"}"#.into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            turn.blocks.iter().any(|block| {
                matches!(block, Block::Artifact { path, .. } if path.ends_with("README.md"))
            }),
            "markdown writes should produce artifact cards, got {:?}",
            turn.blocks
        );
    }

    #[test]
    fn create_directory_does_not_become_artifact_block() {
        use chatty_core::models::message_types::{
            SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
        };

        let tool = ToolCallBlock {
            id: "d1".into(),
            tool_name: "create_directory".into(),
            display_name: "Creating directory".into(),
            input: r#"{"path":"src/components"}"#.into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let msg = DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: Some(SystemTrace {
                items: vec![TraceItem::ToolCall(tool)],
                total_duration: None,
                active_tool_index: None,
            }),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        };
        let turn = adapt_message(&msg, 0, false);
        assert!(
            !turn
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Artifact { .. } | Block::ArtifactBatch { .. })),
            "create_directory should not produce artifact cards, got {:?}",
            turn.blocks
        );
    }

    #[test]
    fn explore_tools_are_not_file_changes() {
        let t = sample_tool("r", "read_file");
        assert!(file_change_from_tool(&t).is_none());
    }
}
