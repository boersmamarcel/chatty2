use std::time::Duration;

use chatty_core::models::message_types::{SystemTrace, TraceItem};
use gpui::{Pixels, Size, px, size};

use super::activity::{RunTally, classify_tool};
use super::artifact_kind::{is_produced_file_tool, tool_file_path};
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
                if is_diff_tool(tool) {
                    flush_activity(blocks, &mut activity_tools);
                    blocks.push(Block::Diff {
                        id: BlockId::from_parts(namespace, &tool.id),
                        tool: tool.clone(),
                    });
                } else if is_produced_file_tool(&tool.tool_name, &tool.input) {
                    flush_activity(blocks, &mut activity_tools);
                    if let Some(path) = artifact_path(tool) {
                        blocks.push(Block::Artifact {
                            id: BlockId::from_parts(namespace, &tool.id),
                            path,
                        });
                    } else {
                        activity_tools.push(tool.clone());
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

fn is_diff_tool(tool: &chatty_core::models::message_types::ToolCallBlock) -> bool {
    matches!(
        classify_tool(&tool.tool_name),
        super::activity::ToolKind::Edit
    ) && !is_produced_file_tool(&tool.tool_name, &tool.input)
}

fn artifact_path(
    tool: &chatty_core::models::message_types::ToolCallBlock,
) -> Option<std::path::PathBuf> {
    tool_file_path(&tool.input)
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

pub fn estimate_turn_height(turn: &Turn) -> Size<Pixels> {
    if turn.collapsed {
        return size(px(800.), px(COLLAPSED_TURN_HEIGHT));
    }
    let mut height = 48.0_f32;
    for block in &turn.blocks {
        height += match block {
            Block::User {
                content,
                attachments,
                ..
            } => 28.0 + (content.len() as f32 / 48.0).min(240.0) + attachments.len() as f32 * 48.0,
            Block::Text { content, .. } => 24.0 + (content.len() as f32 / 48.0).min(400.0),
            Block::Thinking { .. } => 56.0,
            Block::Activity { tools, .. } => 40.0 + tools.len() as f32 * 28.0,
            Block::Diff { .. } => 120.0,
            Block::Approval { .. } => 72.0,
            Block::Plan { .. } => 80.0,
            Block::Artifact { .. } => 56.0,
            Block::Error { .. } => 64.0,
        };
    }
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

    fn tally_sentence_matches_linear_1a_order() {
        let sentence = RunTally {
            edits: 4,
            explore: 6,
            searches: 2,
            external: 1,
            commands: 1,
        }
        .sentence();
        assert_eq!(
            sentence,
            "Edited 4 files, explored 6 files, 2 searches, 1 tool, ran 1 command"
        );
    }
}
