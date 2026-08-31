use std::time::Duration;

use chatty_core::models::message_types::{SystemTrace, ToolCallState, TraceItem};
use gpui::{Pixels, Size, px, size};

use super::activity::{RunTally, classify_tool};
use super::artifact_kind::{
    artifact_old_content_from_tool, attachment_image_path, chart_artifact_path,
    is_produced_file_tool, tool_file_path,
};
use super::table::{extract_table_preview, inline_table_card_height};
use super::types::{Block, BlockId, Turn, TurnRole};
use crate::chatty::views::message_component::{DisplayMessage, MessageRole};

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
            }
            if !msg.content.is_empty() {
                blocks.push(Block::Text {
                    id: BlockId::from_parts(namespace, "text"),
                    content: msg.content.clone(),
                    streaming: msg.is_streaming,
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
        collapsed: collapsed && !msg.is_streaming && matches!(msg.role, MessageRole::Assistant),
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
                    if let Some(path) = chart_artifact_path(tool) {
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
                    if let Some(path) = attachment_image_path(tool) {
                        blocks.push(Block::Artifact {
                            id: BlockId::from_parts(namespace, &tool.id),
                            path,
                            old_content: None,
                        });
                    }
                } else if is_produced_file_tool(&tool.tool_name, &tool.input) {
                    // Count/show the write in the activity group, then attach
                    // an artifact receipt so provenance stays next to the turn.
                    activity_tools.push(tool.clone());
                    flush_activity(blocks, &mut activity_tools);
                    if let Some(path) = artifact_path(tool) {
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

/// Approximate usable text width inside a message bubble for line wrapping.
pub fn transcript_chars_per_line(content_width_px: f32) -> f32 {
    const AVG_CHAR_WIDTH: f32 = 7.2;
    const MIN_CHARS: f32 = 16.0;
    const MAX_CHARS: f32 = 80.0;
    (content_width_px / AVG_CHAR_WIDTH).clamp(MIN_CHARS, MAX_CHARS)
}

/// Height of a user/assistant bubble rendered via [`render_message`].
///
/// Virtual-list slots use a fixed height; under-estimating here makes the next
/// turn paint on top of this one (e.g. user prompt + “Worked for…” + artifact).
pub fn estimate_message_bubble_height(
    content: &str,
    attachment_count: usize,
    content_width_px: f32,
) -> f32 {
    const LINE_HEIGHT: f32 = 22.0;
    const BUBBLE_PADDING: f32 = 28.0;
    const ATTACHMENT_ROW: f32 = 56.0;

    let chars_per_line = transcript_chars_per_line(content_width_px);
    let wrapped_lines = (content.len().max(1) as f32 / chars_per_line).ceil();
    let explicit_lines = content.lines().count().max(1) as f32;
    let lines = wrapped_lines.max(explicit_lines);

    BUBBLE_PADDING + lines * LINE_HEIGHT + attachment_count as f32 * ATTACHMENT_ROW
}

pub fn block_estimated_height(block: &Block, plan_steps: usize, content_width_px: f32) -> f32 {
    match block {
        Block::User {
            content,
            attachments,
            ..
        } => estimate_message_bubble_height(content, attachments.len(), content_width_px),
        Block::Text { content, .. } => estimate_message_bubble_height(content, 0, content_width_px),
        Block::Thinking { .. } => 56.0,
        Block::Activity { tools, .. } => 40.0 + tools.len() as f32 * 28.0,
        Block::Diff { .. } => 120.0,
        Block::Approval { .. } => 72.0,
        Block::Plan { .. } => 40.0 + 32.0 * plan_steps.max(1) as f32,
        Block::Artifact { .. } => 68.0,
        Block::TablePreview { preview, .. } => inline_table_card_height(preview),
        Block::Error { .. } => 64.0,
    }
}

fn is_work_trace_block(block: &Block) -> bool {
    matches!(
        block,
        Block::Thinking { .. }
            | Block::Activity { .. }
            | Block::Diff { .. }
            | Block::Artifact { .. }
            | Block::TablePreview { .. }
            | Block::Approval { .. }
            | Block::Plan { .. }
            | Block::Error { .. }
    )
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
    plan_steps: usize,
    padding_top: Pixels,
    content_width_px: f32,
) -> Option<Pixels> {
    let ix = plan_turn_index(turns)?;
    let mut y = padding_top;
    for turn in &turns[..ix] {
        y += estimate_turn_height(turn, plan_steps, content_width_px).height;
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
        y += px(block_estimated_height(block, plan_steps, content_width_px));
    }
    Some(y)
}

/// Content Y of the bottom of the inline plan block, including list top padding.
pub fn plan_block_bottom(
    turns: &[Turn],
    plan_steps: usize,
    padding_top: Pixels,
    content_width_px: f32,
) -> Option<Pixels> {
    let top = plan_block_top(turns, plan_steps, padding_top, content_width_px)?;
    let ix = plan_turn_index(turns)?;
    let turn = &turns[ix];
    if turn.collapsed {
        return Some(top + px(COLLAPSED_TURN_HEIGHT));
    }
    Some(
        top + px(block_estimated_height(
            &Block::Plan { id: BlockId(0) },
            plan_steps,
            content_width_px,
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

fn artifact_path(
    tool: &chatty_core::models::message_types::ToolCallBlock,
) -> Option<std::path::PathBuf> {
    // Prefer the resolved on-disk path from tool output (e.g. Typst saved_path).
    tool.output
        .as_deref()
        .and_then(tool_file_path)
        .or_else(|| tool_file_path(&tool.input))
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
        .filter(|(_, msg)| {
            !(msg.is_streaming
                && msg.content.is_empty()
                && !msg
                    .live_trace
                    .as_ref()
                    .is_some_and(|trace| trace.has_items()))
        })
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

pub fn estimate_turn_height(turn: &Turn, plan_steps: usize, content_width_px: f32) -> Size<Pixels> {
    // Mirror `render_visible_turns`: optional work header, typed blocks (not
    // User/Text), then `render_message` for the bubble body.
    let has_work_fold = matches!(turn.role, TurnRole::Assistant)
        && !turn.streaming
        && turn.blocks.iter().any(is_work_trace_block);

    let mut height = 0.0_f32;
    let mut flex_children = 0u32;

    if has_work_fold {
        height += COLLAPSED_TURN_HEIGHT;
        flex_children += 1;
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
                    attachments.len(),
                    content_width_px,
                ));
            }
            Block::Text { content, .. } => {
                message_height = message_height.max(estimate_message_bubble_height(
                    content,
                    0,
                    content_width_px,
                ));
            }
            _ if block_visible_in_turn(turn, block) => {
                height += block_estimated_height(block, plan_steps, content_width_px);
                flex_children += 1;
            }
            _ => {}
        }
    }

    let stacks_message_shell = has_work_fold || flex_children > 0;
    if stacks_message_shell {
        // Empty assistant turns still render a padded message shell under receipts.
        height += message_height.max(32.0);
        flex_children += 1;
    } else {
        height += message_height;
        if message_height > 0.0 {
            flex_children += 1;
        }
    }

    // `gap_2` between work header, typed blocks, and message bubble.
    if flex_children > 1 {
        height += 8.0 * (flex_children - 1) as f32;
    }

    height += 16.0;
    size(px(800.), px(height.max(36.0)))
}

pub fn format_worked_for(elapsed: Option<Duration>) -> String {
    let secs = elapsed.map(|d| d.as_secs()).unwrap_or(0);
    if secs < 1 {
        "Worked for a moment".to_string()
    } else if secs < 60 {
        format!("Worked for {secs}s")
    } else {
        format!("Worked for {}m {}s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worked_for_formats_seconds_and_minutes() {
        assert_eq!(format_worked_for(None), "Worked for a moment");
        assert_eq!(
            format_worked_for(Some(Duration::from_secs(4))),
            "Worked for 4s"
        );
        assert_eq!(
            format_worked_for(Some(Duration::from_secs(65))),
            "Worked for 1m 5s"
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
        let wide = estimate_turn_height(&turn, 0, 520.0).height;
        let narrow = estimate_turn_height(&turn, 0, 220.0).height;
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
        let wide = estimate_message_bubble_height(prompt, 0, 520.0);
        let narrow = estimate_message_bubble_height(prompt, 0, 220.0);
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
            output: Some(r#"{"saved_path":"/tmp/workspace_notes.pdf","page_count":2}"#.into()),
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
        let height = estimate_turn_height(&turn, 0, 400.0).height;
        assert!(
            height >= px(140.0),
            "assistant receipt turn under-estimated at {height:?}"
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
            output: Some(r#"{"saved_path":"/tmp/reports/sales.pdf","page_count":1}"#.into()),
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
}
