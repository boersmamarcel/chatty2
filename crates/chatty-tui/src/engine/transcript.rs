//! The terminal transcript: what the TUI and the headless runner show for a
//! conversation, kept apart from the turn (which is the session's) and from
//! the terminal (which is `ui/`).
//!
//! `DisplayMessage` and `ToolCallInfo` are plain data; this type is the
//! bookkeeping around them — which assistant row is streaming, where a
//! sub-agent's progress goes, which tool rows belong to `invoke_agent` and
//! are rendered from the sub-agent row instead. Both the interactive engine
//! and the headless runner apply the same stream events to it (AGE-196),
//! so a tool row is shaped the same way whichever mode is running.

use std::collections::HashSet;

use chatty_core::models::message_types::{
    classify_initial_execution_engine, classify_tool_source, detect_execution_engine,
    predict_execution_engine,
};
use chatty_core::services::{AgentTaskSnapshot, is_agent_todo_tool, snapshot_from_tool_output};

use super::{
    ApprovalInfo, DiffStat, DisplayMessage, MessageBlock, MessageRole, ToolCallInfo, ToolCallState,
};
use crate::ui::verb;

#[derive(Debug, Default)]
pub struct Transcript {
    pub messages: Vec<DisplayMessage>,
    /// Index into `messages` of the system message showing sub-agent progress.
    /// `None` when no sub-agent is running.
    pub delegation_msg_idx: Option<usize>,
    /// Tracks `invoke_agent` tool call IDs to suppress their
    /// ToolCallBlock rendering (progress goes through the sub-agent channel).
    active_invoke_agent_ids: HashSet<String>,
    /// Latest plan snapshot, for the status bar's live position. Cleared when
    /// a new user turn starts, which is when `AgentTaskController` resets the
    /// plan it mirrors (AGE-342).
    pub plan: Option<AgentTaskSnapshot>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.delegation_msg_idx = None;
        self.active_invoke_agent_ids.clear();
        self.plan = None;
    }

    pub fn push_user(&mut self, text: String) {
        self.plan = None;
        self.messages
            .push(DisplayMessage::with_text(MessageRole::User, text));
    }

    /// Open the streaming assistant row a turn writes into.
    pub fn start_assistant(&mut self) {
        self.messages
            .push(DisplayMessage::new(MessageRole::Assistant, true));
    }

    pub fn add_system(&mut self, text: String) {
        self.messages
            .push(DisplayMessage::with_text(MessageRole::System, text));
    }

    /// Route sub-agent progress into the last row from now on.
    pub fn mark_last_as_delegation_row(&mut self) {
        self.delegation_msg_idx = self.messages.len().checked_sub(1);
    }

    pub fn reset_delegation_row(&mut self) {
        self.delegation_msg_idx = None;
    }

    /// Number of assistant rows, streaming or not.
    pub fn assistant_turns(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
            .count()
    }

    /// The most recent tool call with this id, if any row has it.
    pub fn tool_call(&self, id: &str) -> Option<&ToolCallInfo> {
        self.messages
            .iter()
            .rev()
            .flat_map(|m| m.tool_calls())
            .find(|tc| tc.id == id)
    }

    /// The most recent tool call with this name, if any row has it.
    pub fn tool_call_named(&self, name: &str) -> Option<&ToolCallInfo> {
        self.messages
            .iter()
            .rev()
            .flat_map(|m| m.tool_calls())
            .find(|tc| tc.name == name)
    }

    fn streaming_assistant_mut(&mut self) -> Option<&mut DisplayMessage> {
        let idx = streaming_assistant_index(&self.messages, self.delegation_msg_idx)?;
        self.messages.get_mut(idx)
    }

    /// The streaming row, opening a continuation row after a sub-agent's
    /// progress row if the turn carries on past it.
    fn streaming_assistant_or_continuation(&mut self) -> Option<&mut DisplayMessage> {
        if let Some(idx) = streaming_assistant_index(&self.messages, self.delegation_msg_idx) {
            return self.messages.get_mut(idx);
        }
        if self.delegation_msg_idx.is_some() {
            self.start_assistant();
            return self.messages.last_mut();
        }
        None
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(msg) = self.streaming_assistant_or_continuation() {
            // The turn moved on from a run of tools: fold what settled.
            if !matches!(msg.blocks.last(), Some(MessageBlock::Text(_))) {
                msg.fold_settled_tools();
            }
            msg.push_text(text);
        }
    }

    /// The approval prompt, inline where the stream asked for it.
    pub fn approval_requested(&mut self, id: String, command: String, is_sandboxed: bool) {
        if let Some(msg) = self.streaming_assistant_or_continuation() {
            msg.blocks.push(MessageBlock::Approval(ApprovalInfo {
                id,
                command,
                is_sandboxed,
                decision: None,
            }));
        }
    }

    pub fn approval_resolved(&mut self, id: &str, approved: bool) {
        if let Some(msg) = self.streaming_assistant_mut()
            && let Some(approval) = msg.approval_mut(id)
        {
            approval.decision = Some(approved);
        }
    }

    pub fn tool_started(&mut self, id: String, name: String) {
        if name == "invoke_agent" {
            self.active_invoke_agent_ids.insert(id);
            return;
        }
        let source = classify_tool_source(&name);
        let execution_engine = classify_initial_execution_engine(&name);
        let info = ToolCallInfo {
            id,
            name,
            input: String::new(),
            output: None,
            state: ToolCallState::Running,
            source,
            execution_engine,
        };
        if let Some(msg) = self.streaming_assistant_or_continuation() {
            msg.push_tool_call(info);
        }
    }

    pub fn tool_input(&mut self, id: &str, arguments: &str) {
        if self.active_invoke_agent_ids.contains(id) {
            return;
        }
        if let Some(last) = self.streaming_assistant_mut()
            && let Some(tc) = last.tool_call_mut(id)
        {
            tc.execution_engine =
                predict_execution_engine(&tc.name, arguments).or(tc.execution_engine);
            tc.input.push_str(arguments);
        }
    }

    pub fn tool_result(&mut self, id: &str, result: String) {
        if self.active_invoke_agent_ids.remove(id) {
            // invoke_agent result — the delegation progress already handled
            return;
        }
        let mut plan = None;
        let mut diff = None;
        if let Some(last) = self.streaming_assistant_mut()
            && let Some(tc) = last.tool_call_mut(id)
        {
            tc.execution_engine = detect_execution_engine(&tc.name, &result);
            if is_agent_todo_tool(&tc.name) {
                plan = snapshot_from_tool_output(&result);
            }
            diff = verb::diff_stats(&tc.name, &tc.input, Some(&result)).map(|(added, removed)| {
                DiffStat {
                    path: verb::subject_for(&tc.input),
                    added,
                    removed,
                }
            });
            tc.output = Some(result);
            tc.state = ToolCallState::Success;
        }
        if let Some(snapshot) = plan {
            if let Some(last) = self.streaming_assistant_mut() {
                last.set_plan(snapshot.clone());
            }
            self.plan = Some(snapshot);
        }
        if let Some(diff) = diff
            && let Some(last) = self.streaming_assistant_mut()
        {
            last.blocks.push(MessageBlock::Diff(diff));
        }
    }

    pub fn tool_error(&mut self, id: &str, error: String) {
        if self.active_invoke_agent_ids.remove(id) {
            return;
        }
        if let Some(last) = self.streaming_assistant_mut()
            && let Some(tc) = last.tool_call_mut(id)
        {
            tc.output = Some(error);
            tc.state = ToolCallState::Error;
        }
    }

    /// A (sanitized, non-empty) line of sub-agent progress: the first opens
    /// the sub-agent row, later ones append to it.
    pub fn delegation_progress(&mut self, line: String) {
        if self.delegation_msg_idx.is_none() {
            self.seal_parent_before_delegation_progress();
            self.add_system(line);
            self.mark_last_as_delegation_row();
        } else if let Some(idx) = self.delegation_msg_idx
            && let Some(msg) = self.messages.get_mut(idx)
        {
            msg.push_text("\n");
            msg.push_text(&line);
        }
    }

    pub fn delegation_finished(&mut self, message: String) {
        if let Some(idx) = self.delegation_msg_idx
            && let Some(msg) = self.messages.get_mut(idx)
        {
            msg.push_text("\n");
            msg.push_text(&message);
        } else {
            self.add_system(message);
            self.mark_last_as_delegation_row();
        }
    }

    /// Close the streaming row: the turn completed.
    pub fn finish_streaming(&mut self) {
        if let Some(last) = self.streaming_assistant_mut() {
            last.fold_settled_tools();
            last.is_streaming = false;
        }
    }

    pub fn mark_cancelled(&mut self) {
        if let Some(last) = self.streaming_assistant_mut() {
            last.fold_settled_tools();
            last.push_text("\n\n[Cancelled]");
            last.is_streaming = false;
        }
    }

    /// The error as its own block, after whatever the turn produced.
    pub fn mark_error(&mut self, error: &str) {
        if let Some(last) = self.streaming_assistant_mut() {
            last.fold_settled_tools();
            last.blocks.push(MessageBlock::Error(error.to_string()));
            last.is_streaming = false;
        }
    }

    fn seal_parent_before_delegation_progress(&mut self) {
        let Some(idx) = streaming_assistant_index(&self.messages, None) else {
            return;
        };
        let empty = self.messages[idx].text().is_empty()
            && self.messages[idx].tool_calls().next().is_none();
        if empty {
            self.messages.remove(idx);
        } else {
            self.messages[idx].is_streaming = false;
        }
    }
}

fn streaming_assistant_index(messages: &[DisplayMessage], after: Option<usize>) -> Option<usize> {
    messages.iter().enumerate().rev().find_map(|(i, m)| {
        if after.is_some_and(|a| i <= a) {
            return None;
        }
        (matches!(m.role, MessageRole::Assistant) && m.is_streaming).then_some(i)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_assistant_before_progress_is_parent() {
        let messages = vec![
            DisplayMessage::new(MessageRole::Assistant, true),
            DisplayMessage::with_text(MessageRole::System, "progress".into()),
        ];
        assert_eq!(streaming_assistant_index(&messages, None), Some(0));
    }

    #[test]
    fn streaming_assistant_after_progress_is_continuation() {
        let messages = vec![
            DisplayMessage::new(MessageRole::Assistant, false),
            DisplayMessage::with_text(MessageRole::System, "progress".into()),
            DisplayMessage::new(MessageRole::Assistant, true),
        ];
        assert_eq!(streaming_assistant_index(&messages, Some(1)), Some(2));
    }

    #[test]
    fn streaming_assistant_none_when_only_system_messages() {
        let messages = vec![DisplayMessage::with_text(
            MessageRole::System,
            "progress".into(),
        )];
        assert_eq!(streaming_assistant_index(&messages, None), None);
    }

    #[test]
    fn a_tool_round_trip_lands_on_the_streaming_row() {
        let mut transcript = Transcript::new();
        transcript.push_user("hi".into());
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "read_file".into());
        transcript.tool_input("c1", r#"{"path":"a"}"#);
        transcript.tool_result("c1", "contents".into());
        transcript.push_text("done");
        transcript.finish_streaming();

        let tc = transcript.tool_call("c1").expect("tool row exists");
        assert!(matches!(tc.state, ToolCallState::Success));
        assert_eq!(tc.output.as_deref(), Some("contents"));
        let last = transcript.messages.last().unwrap();
        assert_eq!(last.text(), "done");
        assert!(!last.is_streaming);
    }

    fn todo_output(current: &str) -> String {
        format!(
            r#"{{"message":"ok","snapshot":{{"goal":"ship","todos":[{{"id":"t1","title":"{current}","description":"","status":"in_progress"}}],"write_todos_called":true,"verified":false,"evidence":[]}}}}"#
        )
    }

    fn kinds(msg: &DisplayMessage) -> Vec<&'static str> {
        msg.blocks
            .iter()
            .map(|b| match b {
                MessageBlock::Text(_) => "text",
                MessageBlock::ToolCall(_) => "tool",
                MessageBlock::Activity(_) => "activity",
                MessageBlock::Approval(_) => "approval",
                MessageBlock::Plan(_) => "plan",
                MessageBlock::Error(_) => "error",
                MessageBlock::Diff(_) => "diff",
            })
            .collect()
    }

    /// AGE-136: the approval is a block in the stream, not only an input
    /// slot, and it records the decision where it was asked.
    #[test]
    fn an_approval_is_a_block_in_the_stream_with_its_decision() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "run_shell".into());
        transcript.approval_requested("a1".into(), "rm -rf build".into(), false);
        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["tool", "approval"]);
        assert!(matches!(
            &msg.blocks[1],
            MessageBlock::Approval(a) if a.decision.is_none() && a.command == "rm -rf build"
        ));

        transcript.approval_resolved("a1", true);
        let msg = transcript.messages.last().unwrap();
        assert!(matches!(
            &msg.blocks[1],
            MessageBlock::Approval(a) if a.decision == Some(true)
        ));
    }

    /// AGE-136: a todo tool's result opens one plan block, and the next todo
    /// call rewrites it in place instead of adding another.
    #[test]
    fn todo_results_become_one_plan_block_rewritten_in_place() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "write_todos".into());
        transcript.tool_result("c1", todo_output("first"));
        transcript.tool_started("c2".into(), "update_todo".into());
        transcript.tool_result("c2", todo_output("second"));
        transcript.finish_streaming();

        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["tool", "plan", "tool"]);
        assert!(matches!(
            &msg.blocks[1],
            MessageBlock::Plan(p) if p.todos[0].title == "second"
        ));
        assert_eq!(transcript.plan.as_ref().unwrap().todos[0].title, "second");
    }

    /// AGE-136: consecutive settled tools collapse to one activity block once
    /// the turn moves on; a failed call stays visible on its own.
    #[test]
    fn consecutive_settled_tools_fold_into_an_activity_block() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        for (id, name) in [
            ("c1", "read_file"),
            ("c2", "read_file"),
            ("c3", "shell_execute"),
        ] {
            transcript.tool_started(id.into(), name.into());
            transcript.tool_result(id, "ok".into());
        }
        transcript.push_text("Now the failure.");
        transcript.tool_started("c4".into(), "apply_diff".into());
        transcript.tool_error("c4", "boom".into());
        transcript.tool_started("c5".into(), "read_file".into());
        transcript.tool_result("c5", "ok".into());
        transcript.finish_streaming();

        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["activity", "text", "tool", "tool"]);
        assert!(matches!(&msg.blocks[0], MessageBlock::Activity(tools) if tools.len() == 3));
        // Folded calls are still reachable by id.
        assert_eq!(
            transcript.tool_call("c2").map(|tc| tc.name.as_str()),
            Some("read_file")
        );
    }

    /// AGE-136: an edit's stat row follows the edit as its own block.
    #[test]
    fn an_edit_result_adds_a_diff_stat_block() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "apply_diff".into());
        transcript.tool_input("c1", r#"{"path":"src/lib.rs"}"#);
        transcript.tool_result("c1", r#"{"insertions":3,"deletions":1}"#.into());
        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["tool", "diff"]);
        assert!(matches!(
            &msg.blocks[1],
            MessageBlock::Diff(d) if d.path == "src/lib.rs" && d.added == 3 && d.removed == 1
        ));
    }

    /// AGE-136: two approvals in one turn resolve by id, an unknown id
    /// touches nothing, and settled tools do not fold across the approvals.
    #[test]
    fn approvals_resolve_by_id_and_do_not_fold_across() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "shell_execute".into());
        transcript.approval_requested("a1".into(), "rm -rf build".into(), false);
        transcript.approval_resolved("a1", false);
        transcript.tool_error("c1", "denied".into());
        transcript.tool_started("c2".into(), "shell_execute".into());
        transcript.approval_requested("a2".into(), "cargo build".into(), true);
        transcript.approval_resolved("nope", true);
        transcript.approval_resolved("a2", true);
        transcript.tool_result("c2", "ok".into());
        transcript.finish_streaming();

        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["tool", "approval", "tool", "approval"]);
        assert!(
            matches!(&msg.blocks[1], MessageBlock::Approval(a) if a.id == "a1" && a.decision == Some(false))
        );
        assert!(
            matches!(&msg.blocks[3], MessageBlock::Approval(a) if a.id == "a2" && a.decision == Some(true))
        );
    }

    /// AGE-136: a cancelled turn folds what settled and leaves the undecided
    /// approval on a closed row, which the view shows as cancelled.
    #[test]
    fn a_cancelled_turn_folds_and_closes_the_row_over_an_open_approval() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "read_file".into());
        transcript.tool_result("c1", "ok".into());
        transcript.tool_started("c2".into(), "read_file".into());
        transcript.tool_result("c2", "ok".into());
        transcript.tool_started("c3".into(), "shell_execute".into());
        transcript.approval_requested("a1".into(), "rm -rf build".into(), false);
        transcript.mark_cancelled();

        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["activity", "tool", "approval", "text"]);
        assert!(!msg.is_streaming);
        assert!(matches!(&msg.blocks[2], MessageBlock::Approval(a) if a.decision.is_none()));
    }

    #[test]
    fn a_single_settled_tool_is_not_folded() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "read_file".into());
        transcript.tool_result("c1", "ok".into());
        transcript.finish_streaming();
        assert_eq!(kinds(transcript.messages.last().unwrap()), vec!["tool"]);
    }

    /// AGE-136: a stream error is its own block, after the partial text.
    #[test]
    fn a_stream_error_is_an_error_block() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.push_text("partial");
        transcript.mark_error("provider went away");
        let msg = transcript.messages.last().unwrap();
        assert_eq!(kinds(msg), vec!["text", "error"]);
        assert_eq!(msg.text(), "partial");
        assert!(!msg.is_streaming);
    }

    #[test]
    fn invoke_agent_rows_are_suppressed_in_favour_of_the_progress_row() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "invoke_agent".into());
        transcript.delegation_progress("[local agent] task".into());
        transcript.delegation_progress("reading".into());
        transcript.tool_result("c1", "answer".into());
        transcript.delegation_finished("answer".into());

        assert!(transcript.tool_call("c1").is_none());
        let row = &transcript.messages[transcript.delegation_msg_idx.unwrap()];
        assert_eq!(row.text(), "[local agent] task\nreading\nanswer");
    }
}
