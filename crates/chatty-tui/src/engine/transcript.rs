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

use super::{DisplayMessage, MessageRole, ToolCallInfo, ToolCallState};

#[derive(Debug, Default)]
pub struct Transcript {
    pub messages: Vec<DisplayMessage>,
    /// Index into `messages` of the system message showing sub-agent progress.
    /// `None` when no sub-agent is running.
    pub sub_agent_msg_idx: Option<usize>,
    /// Tracks `invoke_agent` / `sub_agent` tool call IDs to suppress their
    /// ToolCallBlock rendering (progress goes through the sub-agent channel).
    active_invoke_agent_ids: HashSet<String>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.sub_agent_msg_idx = None;
        self.active_invoke_agent_ids.clear();
    }

    pub fn push_user(&mut self, text: String) {
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
    pub fn mark_last_as_sub_agent_row(&mut self) {
        self.sub_agent_msg_idx = self.messages.len().checked_sub(1);
    }

    pub fn reset_sub_agent_row(&mut self) {
        self.sub_agent_msg_idx = None;
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
        let idx = streaming_assistant_index(&self.messages, self.sub_agent_msg_idx)?;
        self.messages.get_mut(idx)
    }

    /// The streaming row, opening a continuation row after a sub-agent's
    /// progress row if the turn carries on past it.
    fn streaming_assistant_or_continuation(&mut self) -> Option<&mut DisplayMessage> {
        if let Some(idx) = streaming_assistant_index(&self.messages, self.sub_agent_msg_idx) {
            return self.messages.get_mut(idx);
        }
        if self.sub_agent_msg_idx.is_some() {
            self.start_assistant();
            return self.messages.last_mut();
        }
        None
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(msg) = self.streaming_assistant_or_continuation() {
            msg.push_text(text);
        }
    }

    pub fn tool_started(&mut self, id: String, name: String) {
        if name == "invoke_agent" || name == "sub_agent" {
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
            // invoke_agent / sub_agent result — sub-agent progress already handled
            return;
        }
        if let Some(last) = self.streaming_assistant_mut()
            && let Some(tc) = last.tool_call_mut(id)
        {
            tc.execution_engine = detect_execution_engine(&tc.name, &result);
            tc.output = Some(result);
            tc.state = ToolCallState::Success;
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
    pub fn sub_agent_progress(&mut self, line: String) {
        if self.sub_agent_msg_idx.is_none() {
            self.seal_parent_before_sub_agent_progress();
            self.add_system(line);
            self.mark_last_as_sub_agent_row();
        } else if let Some(idx) = self.sub_agent_msg_idx
            && let Some(msg) = self.messages.get_mut(idx)
        {
            msg.push_text("\n");
            msg.push_text(&line);
        }
    }

    pub fn sub_agent_finished(&mut self, message: String) {
        if let Some(idx) = self.sub_agent_msg_idx
            && let Some(msg) = self.messages.get_mut(idx)
        {
            msg.push_text("\n");
            msg.push_text(&message);
        } else {
            self.add_system(message);
            self.mark_last_as_sub_agent_row();
        }
    }

    /// Close the streaming row: the turn completed.
    pub fn finish_streaming(&mut self) {
        if let Some(last) = self.streaming_assistant_mut() {
            last.is_streaming = false;
        }
    }

    pub fn mark_cancelled(&mut self) {
        if let Some(last) = self.streaming_assistant_mut() {
            last.push_text("\n\n[Cancelled]");
            last.is_streaming = false;
        }
    }

    pub fn mark_error(&mut self, error: &str) {
        if let Some(last) = self.streaming_assistant_mut() {
            let prefix = if last.text().is_empty() { "" } else { "\n\n" };
            last.push_text(&format!("{prefix}[Error: {error}]"));
            last.is_streaming = false;
        }
    }

    fn seal_parent_before_sub_agent_progress(&mut self) {
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

    #[test]
    fn invoke_agent_rows_are_suppressed_in_favour_of_the_progress_row() {
        let mut transcript = Transcript::new();
        transcript.start_assistant();
        transcript.tool_started("c1".into(), "sub_agent".into());
        transcript.sub_agent_progress("[local agent] task".into());
        transcript.sub_agent_progress("reading".into());
        transcript.tool_result("c1", "answer".into());
        transcript.sub_agent_finished("answer".into());

        assert!(transcript.tool_call("c1").is_none());
        let row = &transcript.messages[transcript.sub_agent_msg_idx.unwrap()];
        assert_eq!(row.text(), "[local agent] task\nreading\nanswer");
    }
}
