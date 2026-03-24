use serde::{Deserialize, Serialize};
use std::time::Duration;

/// User message content
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessage {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

/// Attachment types for messages
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessageAttachment {
    Image { path: String, media_type: String },
    Document { path: String, file_type: String },
}

/// Assistant message with optional system trace
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// The final response text shown to the user
    pub text: String,
    /// System trace containing reasoning and tool calls (if any)
    pub system_trace: Option<SystemTrace>,
    /// Whether this message is currently streaming
    pub is_streaming: bool,
}

/// System trace represents the "thinking" and "tool execution" layer
/// This is the container for all internal processing steps
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemTrace {
    /// Sequential list of trace items (thinking blocks and tool calls)
    pub items: Vec<TraceItem>,
    /// Total processing time for all trace items
    pub total_duration: Option<Duration>,
    /// Track which tool is currently executing (by index)
    pub active_tool_index: Option<usize>,
}

/// Individual items in the system trace
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TraceItem {
    /// A reasoning/thinking block
    Thinking(ThinkingBlock),
    /// A tool call execution
    ToolCall(ToolCallBlock),
    /// An execution approval prompt
    ApprovalPrompt(ApprovalBlock),
}

/// Events emitted by SystemTraceView when trace state changes
#[derive(Clone, Debug)]
pub enum TraceEvent {
    /// Tool call state changed (Running → Success/Error)
    ToolCallStateChanged {
        tool_id: String,
        #[allow(dead_code)]
        old_state: ToolCallState,
        #[allow(dead_code)]
        new_state: ToolCallState,
    },
    /// Tool call received input
    #[allow(dead_code)]
    ToolCallInputReceived { tool_id: String },
    /// Tool call received output
    ToolCallOutputReceived {
        tool_id: String,
        #[allow(dead_code)]
        has_output: bool,
    },
    /// Thinking block state changed
    ThinkingStateChanged {
        #[allow(dead_code)]
        old_state: ThinkingState,
        #[allow(dead_code)]
        new_state: ThinkingState,
    },
}

/// Represents a "thinking" or "reasoning" session
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// The actual reasoning content (chain of thought)
    pub content: String,
    /// One-line summary for collapsed view
    pub summary: String,
    /// Time spent on this thinking session
    pub duration: Option<Duration>,
    /// Current state of the thinking block
    pub state: ThinkingState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ThinkingState {
    /// Currently processing
    Processing,
    /// Completed successfully
    Completed,
}

/// Represents a single tool call and its execution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallBlock {
    /// Unique identifier for this tool call
    #[serde(default)]
    pub id: String,
    /// Name of the tool being called (e.g., "google_search", "execute_python")
    pub tool_name: String,
    /// Display-friendly name for the UI
    pub display_name: String,
    /// The input parameters sent to the tool (JSON or formatted text)
    pub input: String,
    /// The raw output from the tool
    pub output: Option<String>,
    /// Formatted/preview version of output for UI
    pub output_preview: Option<String>,
    /// Current execution state
    pub state: ToolCallState,
    /// Execution duration
    pub duration: Option<Duration>,
    /// Text content that appeared before this tool call (for interleaved rendering)
    #[serde(default)]
    pub text_before: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ToolCallState {
    /// Tool is currently executing
    Running,
    /// Tool completed successfully
    Success,
    /// Tool execution failed
    Error(String),
}

/// Represents an execution approval request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalBlock {
    /// Unique ID for tracking this approval
    pub id: String,
    /// Command to be executed
    pub command: String,
    /// Whether execution will be sandboxed
    pub is_sandboxed: bool,
    /// Current approval state
    pub state: ApprovalState,
    /// When the approval was requested
    pub created_at: std::time::SystemTime,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ApprovalState {
    /// Awaiting user decision
    Pending,
    /// User approved execution
    Approved,
    /// User denied execution
    Denied,
}

impl ThinkingState {
    pub fn is_processing(&self) -> bool {
        matches!(self, ThinkingState::Processing)
    }
}

impl SystemTrace {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            total_duration: None,
            active_tool_index: None,
        }
    }

    pub fn add_tool_call(&mut self, tool_call: ToolCallBlock) {
        self.items.push(TraceItem::ToolCall(tool_call));
    }

    #[allow(dead_code)]
    pub fn add_thinking(&mut self, thinking: ThinkingBlock) {
        self.items.push(TraceItem::Thinking(thinking));
    }

    pub fn has_items(&self) -> bool {
        !self.items.is_empty()
    }

    /// Mark a tool as currently executing
    pub fn set_active_tool(&mut self, index: usize) {
        self.active_tool_index = Some(index);
    }

    /// Clear active tool when it completes
    pub fn clear_active_tool(&mut self) {
        self.active_tool_index = None;
    }

    /// Add an approval prompt to the trace
    pub fn add_approval(&mut self, approval: ApprovalBlock) {
        self.items.push(TraceItem::ApprovalPrompt(approval));
    }

    /// Update the state of an approval prompt by ID
    pub fn update_approval_state(&mut self, id: &str, state: ApprovalState) {
        for item in &mut self.items {
            if let TraceItem::ApprovalPrompt(approval) = item
                && approval.id == id
            {
                approval.state = state;
                break;
            }
        }
    }

    /// Transition all Running tool calls to Error("Cancelled").
    /// Called when a stream is cancelled to prevent tool calls from staying stuck
    /// in the Running state permanently.
    pub fn cancel_running_tool_calls(&mut self) {
        for item in &mut self.items {
            if let TraceItem::ToolCall(tc) = item
                && matches!(tc.state, ToolCallState::Running)
            {
                tc.state = ToolCallState::Error("Cancelled".to_string());
            }
        }
    }

    /// Update a tool call by ID.
    ///
    /// Pass 1 (forward): find the FIRST entry with matching ID in Running state (FIFO).
    /// Pass 2 (fallback, reverse): find the LAST entry with matching ID regardless of state.
    ///
    /// FIFO order ensures that when results arrive for duplicate tool-call IDs,
    /// they match the oldest pending call first.
    ///
    /// Returns true if a matching tool call was found and updated.
    pub fn update_tool_call<F>(&mut self, tool_id: &str, updater: F) -> bool
    where
        F: FnOnce(&mut ToolCallBlock),
    {
        // Pass 1: find first Running entry with matching ID (FIFO order)
        for item in self.items.iter_mut() {
            if let TraceItem::ToolCall(tc) = item
                && tc.id == tool_id
                && matches!(tc.state, ToolCallState::Running)
            {
                updater(tc);
                return true;
            }
        }

        // Pass 2 (fallback): find last entry with matching ID regardless of state
        for item in self.items.iter_mut().rev() {
            if let TraceItem::ToolCall(tc) = item
                && tc.id == tool_id
            {
                updater(tc);
                return true;
            }
        }

        false
    }
}

/// Returns true if a tool result string indicates the action was denied by the user.
pub fn is_denial_result(result: &str) -> bool {
    let lower = result.to_lowercase();
    lower.contains("denied by user") || lower.contains("execution denied")
}

/// Map raw tool names to user-friendly display names
pub fn friendly_tool_name(name: &str) -> String {
    match name {
        "read_file" => "Reading file".to_string(),
        "read_binary" => "Reading binary file".to_string(),
        "list_directory" => "Listing directory".to_string(),
        "glob_search" => "Searching files".to_string(),
        "write_file" => "Writing file".to_string(),
        "create_directory" => "Creating directory".to_string(),
        "delete_file" => "Deleting file".to_string(),
        "move_file" => "Moving file".to_string(),
        "apply_diff" => "Applying diff".to_string(),
        "shell_execute" => "Running command".to_string(),
        "create_chart" => "Creating chart".to_string(),
        "search_memory" => "Searching memory".to_string(),
        "remember" => "Remembering".to_string(),
        "search_web" => "Searching online".to_string(),
        "fetch" => "Fetching".to_string(),
        "daytona_run" => "Running in sandbox".to_string(),
        "browser_use" => "Browsing web".to_string(),
        other => other.to_string(),
    }
}

impl Default for SystemTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantMessage {
    pub fn new(text: String) -> Self {
        Self {
            text,
            system_trace: None,
            is_streaming: false,
        }
    }
}

impl UserMessage {
    pub fn new(text: String) -> Self {
        Self {
            text,
            attachments: Vec::new(),
        }
    }

    /// Convert from rig UserContent to UserMessage
    pub fn from_rig_content(content: &rig::OneOrMany<rig::message::UserContent>) -> Self {
        let text: String = content
            .iter()
            .filter_map(|uc| match uc {
                rig::message::UserContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        Self::new(text)
    }
}

impl AssistantMessage {
    /// Convert from rig AssistantContent to AssistantMessage
    pub fn from_rig_content(
        content: &rig::OneOrMany<rig::completion::message::AssistantContent>,
    ) -> Self {
        let text: String = content
            .iter()
            .filter_map(|ac| match ac {
                rig::completion::message::AssistantContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        Self::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool_call(id: &str, name: &str, state: ToolCallState) -> ToolCallBlock {
        ToolCallBlock {
            id: id.to_string(),
            tool_name: name.to_string(),
            display_name: name.to_string(),
            input: String::new(),
            output: None,
            output_preview: None,
            state,
            duration: None,
            text_before: String::new(),
        }
    }

    #[test]
    fn update_tool_call_fifo_with_duplicate_ids() {
        // Simulates the bug: multiple sub_agent calls all sharing the same ID
        let mut trace = SystemTrace::new();
        trace.add_tool_call(make_tool_call(
            "sub_agent",
            "sub_agent",
            ToolCallState::Running,
        ));
        trace.add_tool_call(make_tool_call(
            "sub_agent",
            "sub_agent",
            ToolCallState::Running,
        ));
        trace.add_tool_call(make_tool_call(
            "sub_agent",
            "sub_agent",
            ToolCallState::Running,
        ));

        // First result should update the FIRST Running entry (FIFO)
        assert!(trace.update_tool_call("sub_agent", |tc| {
            tc.output = Some("joke1".to_string());
            tc.state = ToolCallState::Success;
        }));

        // Second result should update the SECOND Running entry
        assert!(trace.update_tool_call("sub_agent", |tc| {
            tc.output = Some("joke2".to_string());
            tc.state = ToolCallState::Success;
        }));

        // Third result should update the THIRD Running entry
        assert!(trace.update_tool_call("sub_agent", |tc| {
            tc.output = Some("joke3".to_string());
            tc.state = ToolCallState::Success;
        }));

        // Verify all three got their correct results
        let tool_calls: Vec<_> = trace
            .items
            .iter()
            .filter_map(|item| {
                if let TraceItem::ToolCall(tc) = item {
                    Some(tc)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(tool_calls[0].output.as_deref(), Some("joke1"));
        assert_eq!(tool_calls[1].output.as_deref(), Some("joke2"));
        assert_eq!(tool_calls[2].output.as_deref(), Some("joke3"));
    }

    #[test]
    fn update_tool_call_with_unique_ids() {
        let mut trace = SystemTrace::new();
        trace.add_tool_call(make_tool_call(
            "call_1",
            "sub_agent",
            ToolCallState::Running,
        ));
        trace.add_tool_call(make_tool_call(
            "call_2",
            "sub_agent",
            ToolCallState::Running,
        ));

        // Results can arrive in any order with unique IDs
        assert!(trace.update_tool_call("call_2", |tc| {
            tc.output = Some("result2".to_string());
            tc.state = ToolCallState::Success;
        }));
        assert!(trace.update_tool_call("call_1", |tc| {
            tc.output = Some("result1".to_string());
            tc.state = ToolCallState::Success;
        }));

        let tool_calls: Vec<_> = trace
            .items
            .iter()
            .filter_map(|item| {
                if let TraceItem::ToolCall(tc) = item {
                    Some(tc)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(tool_calls[0].output.as_deref(), Some("result1"));
        assert_eq!(tool_calls[1].output.as_deref(), Some("result2"));
    }

    #[test]
    fn update_tool_call_fallback_to_any_state() {
        let mut trace = SystemTrace::new();
        trace.add_tool_call(make_tool_call("call_1", "fetch", ToolCallState::Success));

        // Pass 2 fallback: match by ID regardless of state
        assert!(trace.update_tool_call("call_1", |tc| {
            tc.output = Some("updated".to_string());
        }));

        let tc = match &trace.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };
        assert_eq!(tc.output.as_deref(), Some("updated"));
    }

    #[test]
    fn update_tool_call_not_found() {
        let mut trace = SystemTrace::new();
        trace.add_tool_call(make_tool_call("call_1", "fetch", ToolCallState::Running));

        assert!(!trace.update_tool_call("nonexistent", |_tc| {}));
    }

    #[test]
    fn cancel_running_tool_calls_transitions_running_to_cancelled() {
        let mut trace = SystemTrace::new();
        trace.add_tool_call(make_tool_call(
            "call_1",
            "sub_agent",
            ToolCallState::Running,
        ));
        trace.add_tool_call(make_tool_call(
            "call_2",
            "sub_agent",
            ToolCallState::Success,
        ));
        trace.add_tool_call(make_tool_call(
            "call_3",
            "sub_agent",
            ToolCallState::Running,
        ));

        trace.cancel_running_tool_calls();

        let tool_calls: Vec<_> = trace
            .items
            .iter()
            .filter_map(|item| {
                if let TraceItem::ToolCall(tc) = item {
                    Some(tc)
                } else {
                    None
                }
            })
            .collect();

        // Running calls should be cancelled
        assert!(matches!(
            &tool_calls[0].state,
            ToolCallState::Error(msg) if msg == "Cancelled"
        ));
        // Already-completed call should be unchanged
        assert!(matches!(&tool_calls[1].state, ToolCallState::Success));
        // Running calls should be cancelled
        assert!(matches!(
            &tool_calls[2].state,
            ToolCallState::Error(msg) if msg == "Cancelled"
        ));
    }

    #[test]
    fn cancel_running_tool_calls_noop_when_no_running() {
        let mut trace = SystemTrace::new();
        trace.add_tool_call(make_tool_call("call_1", "fetch", ToolCallState::Success));

        trace.cancel_running_tool_calls();

        let tc = match &trace.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };
        assert!(matches!(&tc.state, ToolCallState::Success));
    }
}
