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
use super::session_changes::{file_change_from_tool, file_changes_from_turn, merge_file_changes};
use super::table::extract_table_preview;
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
            TraceItem::ClarificationPrompt(clarification) => {
                flush_activity(blocks, &mut activity_tools);
                blocks.push(Block::Clarification {
                    id: BlockId::from_parts(namespace, &clarification.id),
                    clarification: clarification.clone(),
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

/// Drop every plan block but the most recent one.
///
/// `adapt_message` emits a plan block per message that called `write_todos`,
/// and `render_typed_block` draws every `Block::Plan` from the same live
/// conversation-level snapshot — so a second block is not a second plan, it is
/// the same panel painted twice. The todo protocol can legitimately re-plan in
/// a follow-up turn (AGE-150), which is what makes the duplicate reachable.
/// The newest block is the one kept, since it belongs to the turn the current
/// snapshot came from.
pub fn retain_last_plan_block(turns: &mut [Turn]) {
    let Some(last) = turns
        .iter()
        .rposition(|turn| turn.blocks.iter().any(|b| matches!(b, Block::Plan { .. })))
    else {
        return;
    };
    for (index, turn) in turns.iter_mut().enumerate() {
        if index != last {
            turn.blocks.retain(|b| !matches!(b, Block::Plan { .. }));
        }
    }
}

pub fn plan_turn_index(turns: &[Turn]) -> Option<usize> {
    turns.iter().position(|turn| {
        turn.blocks
            .iter()
            .any(|block| matches!(block, Block::Plan { .. }))
    })
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
            | Block::Clarification { .. }
            | Block::Plan { .. }
            | Block::Error { .. }
    )
}

/// True when a turn renders the "Worked for …" fold header.
///
/// Shared with `ChatView::render_turn` so the header and the blocks under it
/// can never disagree about whether a turn has a work trace.
pub fn turn_has_work_fold(turn: &Turn) -> bool {
    matches!(turn.role, TurnRole::Assistant)
        && (turn.streaming || turn.blocks.iter().any(is_work_trace_block))
}

/// True when a block renders inside its turn.
///
/// A collapsed turn keeps its receipts (artifacts, tables, approvals, plan,
/// errors) and hides the work trace. `User`/`Text` never render here — the
/// message bubble draws them.
pub fn block_visible_in_turn(turn: &Turn, block: &Block) -> bool {
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

    /// A collapsed receipt turn renders a work-fold header plus the artifact
    /// card, and nothing else — no empty message shell under it.
    #[test]
    fn collapsed_assistant_with_artifact_shows_a_work_fold_and_one_receipt() {
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
        assert!(turn.collapsed);
        assert!(turn_has_work_fold(&turn), "a receipt turn folds its work");

        let visible: Vec<&Block> = turn
            .blocks
            .iter()
            .filter(|block| block_visible_in_turn(&turn, block))
            .collect();
        assert!(
            matches!(visible.as_slice(), [Block::Artifact { .. }]),
            "collapsed receipt renders the card and nothing else, got {visible:?}"
        );
    }

    /// A collapsed turn whose only content is a tool run renders as the fold
    /// header alone: the activity card is hidden and there is no message.
    #[test]
    fn collapsed_work_only_turn_renders_just_its_header() {
        let mut msg = assistant_with_tools(vec![sample_tool("a", "read_file")]);
        msg.content.clear();
        let turn = adapt_message(&msg, 0, true);
        assert!(turn_has_work_fold(&turn));
        assert!(
            !turn
                .blocks
                .iter()
                .any(|block| block_visible_in_turn(&turn, block)),
            "a collapsed work-only turn draws nothing but its header"
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
        assert!(
            turn_has_work_fold(&turns[0]),
            "a streaming turn shows the Working header before its first token"
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
    fn a_replan_in_a_later_turn_leaves_one_plan_block() {
        // Every Block::Plan renders the same live conversation snapshot, so two
        // blocks are the same panel drawn twice. The todo protocol can re-plan
        // in a follow-up turn, which is how a user hit this.
        let first = assistant_with_tools(vec![sample_tool("p1", "write_todos")]);
        let second = assistant_with_tools(vec![sample_tool("p2", "write_todos")]);
        let mut turns = vec![
            adapt_message(&first, 0, false),
            adapt_message(&second, 1, false),
        ];

        let before: usize = turns
            .iter()
            .map(|t| {
                t.blocks
                    .iter()
                    .filter(|b| matches!(b, Block::Plan { .. }))
                    .count()
            })
            .sum();
        assert_eq!(before, 2, "fixture should reproduce the duplicate");

        retain_last_plan_block(&mut turns);

        let plan_turns: Vec<usize> = turns
            .iter()
            .enumerate()
            .filter(|(_, t)| t.blocks.iter().any(|b| matches!(b, Block::Plan { .. })))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            plan_turns,
            vec![1],
            "only the newest turn keeps its plan, got {plan_turns:?}"
        );
    }

    #[test]
    fn retain_last_plan_block_leaves_a_single_plan_alone() {
        let msg = assistant_with_tools(vec![sample_tool("p", "write_todos")]);
        let mut turns = vec![adapt_message(&msg, 0, false)];
        let before = turns[0].blocks.len();
        retain_last_plan_block(&mut turns);
        assert_eq!(turns[0].blocks.len(), before, "single plan must survive");
        assert!(plan_turn_index(&turns).is_some());
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

    /// Never had a `#[test]` attribute: the one above it belonged to the plan
    /// strip test that has since moved to `chat_view::scroll`.
    #[test]
    fn tally_sentence_matches_linear_1a_order() {
        let sentence = RunTally {
            edits: 4,
            explore: 6,
            searches: 2,
            external: 1,
            commands: 1,
            handoffs: 0,
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
