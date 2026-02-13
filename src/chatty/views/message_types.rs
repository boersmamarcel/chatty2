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
        old_state: ToolCallState,
        new_state: ToolCallState,
    },
    /// Tool call received input
    ToolCallInputReceived { tool_id: String },
    /// Tool call received output
    ToolCallOutputReceived { tool_id: String, has_output: bool },
    /// Thinking block state changed
    ThinkingStateChanged {
        old_state: ThinkingState,
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
    #[allow(dead_code)]
    pub fn add_approval(&mut self, approval: ApprovalBlock) {
        self.items.push(TraceItem::ApprovalPrompt(approval));
    }

    /// Update the state of an approval prompt by ID
    #[allow(dead_code)]
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

    pub fn with_trace(text: String, trace: SystemTrace) -> Self {
        Self {
            text,
            system_trace: Some(trace),
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

    /// Create an AssistantMessage with a trace from JSON
    pub fn with_trace_json(text: String, trace_json: &serde_json::Value) -> Option<Self> {
        serde_json::from_value::<SystemTrace>(trace_json.clone())
            .ok()
            .map(|trace| Self::with_trace(text, trace))
    }
}
