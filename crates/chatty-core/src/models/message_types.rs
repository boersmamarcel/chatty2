use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use std::time::SystemTime;

use crate::sandbox::MontySandbox;

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

/// Where a tool call executes — used to render data-egress badges in the UI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum ToolSource {
    /// Executes locally; no data leaves Chatty.
    #[default]
    Local,
    /// Remote WASM module executed on the Hive cloud runner.
    HiveCloud,
    /// Built-in internet-facing tool (web fetch, web search, cloud sandbox, browser automation).
    Internet {
        /// Short human-readable label, e.g. "web search", "cloud sandbox".
        label: String,
    },
    /// External service: remote A2A agent or user-configured external MCP server.
    ExternalService {
        /// Display name of the service, e.g. an A2A agent name or MCP server name.
        name: String,
    },
}

/// Which engine actually executed a runnable tool call.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEngine {
    Shell,
    Monty,
    Docker,
    Daytona,
}

impl ExecutionEngine {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Monty => "monty",
            Self::Docker => "docker",
            Self::Daytona => "daytona",
        }
    }
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
    /// Where this tool call executes — used to render data-egress badges.
    #[serde(default)]
    pub source: ToolSource,
    /// Which runtime actually executed this tool call, when known.
    #[serde(default)]
    pub execution_engine: Option<ExecutionEngine>,
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

    pub fn new_sub_agent(prompt: &str, source: ToolSource) -> Self {
        let tool_call = ToolCallBlock {
            id: format!(
                "sub-agent-{}",
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ),
            tool_name: "sub_agent".to_string(),
            display_name: "Sub-agent".to_string(),
            input: json!({ "task": prompt }).to_string(),
            output: None,
            output_preview: None,
            state: ToolCallState::Running,
            duration: None,
            text_before: String::new(),
            source,
            execution_engine: None,
        };

        let mut trace = Self::new();
        trace.add_tool_call(tool_call);
        trace.set_active_tool(0);
        trace
    }

    pub fn is_running_sub_agent(&self) -> bool {
        self.active_tool_index.is_some_and(|idx| {
            matches!(
                self.items.get(idx),
                Some(TraceItem::ToolCall(tc))
                    if tc.tool_name == "sub_agent" && matches!(tc.state, ToolCallState::Running)
            )
        })
    }

    pub fn append_sub_agent_progress(&mut self, line: &str) {
        for item in self.items.iter_mut() {
            if let TraceItem::ToolCall(tc) = item
                && tc.tool_name == "sub_agent"
            {
                let new_output = if let Some(ref existing) = tc.output {
                    format!("{existing}\n{line}")
                } else {
                    line.to_string()
                };
                tc.output = Some(new_output);
                break;
            }
        }
    }

    pub fn finalize_sub_agent_progress(&mut self, success: bool, result: Option<String>) {
        for item in self.items.iter_mut() {
            if let TraceItem::ToolCall(tc) = item
                && tc.tool_name == "sub_agent"
            {
                let error_text = (!success).then(|| {
                    result
                        .as_ref()
                        .filter(|text| !text.trim().is_empty())
                        .cloned()
                        .unwrap_or_else(|| "Sub-agent failed".to_string())
                });
                tc.state = if success {
                    ToolCallState::Success
                } else {
                    ToolCallState::Error(
                        error_text
                            .clone()
                            .unwrap_or_else(|| "Sub-agent failed".to_string()),
                    )
                };

                if let Some(text) = result.or(error_text) {
                    tc.output = Some(match tc.output.take() {
                        Some(existing) if !existing.is_empty() => {
                            format!("{existing}\n\n---\n\n{text}")
                        }
                        _ => text,
                    });
                }
                break;
            }
        }

        self.clear_active_tool();
    }
}

/// Returns true if a tool result string indicates the action was denied by the user.
pub fn is_denial_result(result: &str) -> bool {
    let lower = result.to_lowercase();
    lower.contains("denied by user") || lower.contains("execution denied")
}

/// Classify a built-in tool call by name into a [`ToolSource`] for source badges.
pub fn classify_tool_source(tool_name: &str) -> ToolSource {
    match tool_name {
        "fetch" | "fetch_url" => ToolSource::Internet {
            label: "web fetch".to_string(),
        },
        name if name.starts_with("web_search") || name.starts_with("brave_search") => {
            ToolSource::Internet {
                label: "web search".to_string(),
            }
        }
        "daytona_run" => ToolSource::Internet {
            label: "cloud sandbox".to_string(),
        },
        "browser_use" => ToolSource::Internet {
            label: "browser-use.com".to_string(),
        },
        _ => ToolSource::Local,
    }
}

/// Return the execution engine that is known immediately when a tool starts.
pub fn classify_initial_execution_engine(tool_name: &str) -> Option<ExecutionEngine> {
    match tool_name {
        "shell_execute" => Some(ExecutionEngine::Shell),
        "daytona_run" => Some(ExecutionEngine::Daytona),
        _ => None,
    }
}

/// Predict the execution engine from a tool's input while it is still running.
pub fn predict_execution_engine(tool_name: &str, input: &str) -> Option<ExecutionEngine> {
    match tool_name {
        "execute_code" => {
            let json = serde_json::from_str::<serde_json::Value>(input).ok()?;
            let language = json
                .get("language")
                .and_then(|v| v.as_str())
                .unwrap_or("python");
            if language != "python" {
                return Some(ExecutionEngine::Docker);
            }

            if json.get("expose_port").and_then(|v| v.as_u64()).is_some() {
                return Some(ExecutionEngine::Docker);
            }

            let code = json.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if MontySandbox::can_handle(code) {
                Some(ExecutionEngine::Monty)
            } else {
                Some(ExecutionEngine::Docker)
            }
        }
        _ => classify_initial_execution_engine(tool_name),
    }
}

/// Detect the execution engine for a completed tool call from its output payload.
pub fn detect_execution_engine(tool_name: &str, output: &str) -> Option<ExecutionEngine> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output)
        && let Some(engine) = json.get("execution_engine").and_then(|v| v.as_str())
    {
        return match engine {
            "shell" => Some(ExecutionEngine::Shell),
            "monty" => Some(ExecutionEngine::Monty),
            "docker" => Some(ExecutionEngine::Docker),
            "daytona" => Some(ExecutionEngine::Daytona),
            _ => None,
        };
    }

    classify_initial_execution_engine(tool_name)
}

/// Map raw tool names to user-friendly display names
pub fn friendly_tool_name(name: &str) -> String {
    match name {
        // Filesystem — read
        "read_file" => "Reading file".to_string(),
        "read_binary" => "Reading binary file".to_string(),
        "list_directory" => "Listing directory".to_string(),
        "glob_search" => "Searching files".to_string(),
        // Filesystem — write
        "write_file" => "Writing file".to_string(),
        "create_directory" => "Creating directory".to_string(),
        "delete_file" => "Deleting file".to_string(),
        "move_file" => "Moving file".to_string(),
        "apply_diff" => "Applying changes".to_string(),
        // Shell
        "shell_execute" => "Running command".to_string(),
        "shell_cd" => "Changing directory".to_string(),
        "shell_set_env" => "Setting environment".to_string(),
        "shell_status" => "Checking shell".to_string(),
        // Code search
        "search_code" => "Searching code".to_string(),
        "find_files" => "Finding files".to_string(),
        "find_definition" => "Looking up definition".to_string(),
        // Git
        "git_status" => "Checking git status".to_string(),
        "git_diff" => "Viewing diff".to_string(),
        "git_log" => "Viewing git log".to_string(),
        "git_add" => "Staging changes".to_string(),
        "git_commit" => "Committing changes".to_string(),
        "git_create_branch" => "Creating branch".to_string(),
        "git_switch_branch" => "Switching branch".to_string(),
        // Web
        "search_web" => "Searching the web".to_string(),
        "fetch" => "Fetching page".to_string(),
        // Media & documents
        "add_attachment" => "Attaching file".to_string(),
        "create_chart" => "Creating chart".to_string(),
        "compile_typst" => "Generating PDF".to_string(),
        // Excel
        "read_excel" => "Reading spreadsheet".to_string(),
        "write_excel" => "Writing spreadsheet".to_string(),
        "edit_excel" => "Editing spreadsheet".to_string(),
        // PDF
        "pdf_info" => "Inspecting PDF".to_string(),
        "pdf_extract_text" => "Extracting PDF text".to_string(),
        "pdf_to_image" => "Rendering PDF page".to_string(),
        // Data
        "query_data" => "Querying data".to_string(),
        "describe_data" => "Inspecting schema".to_string(),
        // Code execution & sandboxes
        "execute_code" => "Executing code".to_string(),
        "daytona_run" => "Executing code".to_string(),
        // Memory
        "search_memory" => "Searching memory".to_string(),
        "remember" => "Saving to memory".to_string(),
        "save_skill" => "Saving skill".to_string(),
        // Agents
        "list_agents" => "Listing agents".to_string(),
        "invoke_agent" => "Calling agent".to_string(),
        "sub_agent" => "Delegating to sub-agent".to_string(),
        // Browser
        "browser_use" => "Browsing web".to_string(),
        // MCP & modules
        "list_mcp_services" => "Listing MCP services".to_string(),
        "publish_wasm_module" => "Publishing module".to_string(),
        // Meta
        "list_tools" => "Listing tools".to_string(),
        "read_skill" => "Loading skill".to_string(),
        // Unknown tool — show raw name
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
            source: ToolSource::Local,
            execution_engine: None,
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

    #[test]
    fn sub_agent_helpers_preserve_source_progress_and_result() {
        let mut trace = SystemTrace::new_sub_agent("audit these values", ToolSource::HiveCloud);

        trace.append_sub_agent_progress("Preparing remote module...");
        trace.append_sub_agent_progress("Running analysis...");
        trace.finalize_sub_agent_progress(true, Some("Analysis complete".to_string()));

        let tc = match &trace.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };

        assert_eq!(tc.tool_name, "sub_agent");
        assert_eq!(tc.source, ToolSource::HiveCloud);
        assert!(matches!(&tc.state, ToolCallState::Success));

        let output = tc.output.as_deref().unwrap_or_default();
        assert!(output.contains("Preparing remote module..."));
        assert!(output.contains("Running analysis..."));
        assert!(output.contains("Analysis complete"));
    }

    #[test]
    fn running_sub_agent_detection_uses_active_trace_item() {
        let mut trace = SystemTrace::new_sub_agent("audit these values", ToolSource::Local);
        assert!(trace.is_running_sub_agent());

        trace.finalize_sub_agent_progress(true, None);
        assert!(!trace.is_running_sub_agent());
    }

    #[test]
    fn sub_agent_trace_roundtrip_preserves_source_and_running_state() {
        let mut trace = SystemTrace::new_sub_agent(
            "audit these values",
            ToolSource::ExternalService {
                name: "team-a2a".to_string(),
            },
        );

        trace.append_sub_agent_progress("Checking inputs...");

        let json = serde_json::to_string(&trace).unwrap();
        let restored: SystemTrace = serde_json::from_str(&json).unwrap();

        let tc = match &restored.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };

        assert_eq!(
            tc.source,
            ToolSource::ExternalService {
                name: "team-a2a".to_string()
            }
        );
        assert!(restored.is_running_sub_agent());
        assert_eq!(tc.output.as_deref(), Some("Checking inputs..."));
    }

    #[test]
    fn detects_execution_engine_from_result_json() {
        let output = r#"{"stdout":"42\n","stderr":"","exit_code":0,"timed_out":false,"port_mappings":{},"execution_engine":"monty"}"#;
        assert_eq!(
            detect_execution_engine("execute_code", output),
            Some(ExecutionEngine::Monty)
        );
    }

    #[test]
    fn falls_back_to_known_start_engine_when_result_is_not_json() {
        assert_eq!(
            detect_execution_engine("shell_execute", "plain text output"),
            Some(ExecutionEngine::Shell)
        );
        assert_eq!(
            detect_execution_engine("execute_code", "plain text output"),
            None
        );
    }

    #[test]
    fn predicts_execute_code_engine_from_input() {
        let monty = r#"{"language":"python","code":"print(1 + 1)"}"#;
        let docker = r#"{"language":"python","code":"import requests\nprint('hi')"}"#;
        let typescript = r#"{"language":"typescript","code":"console.log('hi')"}"#;

        assert_eq!(
            predict_execution_engine("execute_code", monty),
            Some(ExecutionEngine::Monty)
        );
        assert_eq!(
            predict_execution_engine("execute_code", docker),
            Some(ExecutionEngine::Docker)
        );
        assert_eq!(
            predict_execution_engine("execute_code", typescript),
            Some(ExecutionEngine::Docker)
        );
    }
}
