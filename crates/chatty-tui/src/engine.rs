use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use chatty_core::factories::AgentClient;
use chatty_core::models::Conversation;
use chatty_core::models::execution_approval_store::{
    ApprovalDecision, ApprovalNotification, ApprovalResolution, ExecutionApprovalStore,
};
use chatty_core::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
use chatty_core::services::{McpService, StreamChunk, stream_prompt};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::settings::models::{ExecutionSettingsModel, ModelsModel};

use futures::StreamExt;
use rig::message::UserContent;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::events::AppEvent;

/// Tool call status tracked during streaming
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub input: String,
    pub output: Option<String>,
    pub state: ToolCallState,
}

#[derive(Debug, Clone)]
pub enum ToolCallState {
    Running,
    Success,
    Error(#[allow(dead_code)] String),
}

/// Pending approval waiting for user decision
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: String,
    pub command: String,
    pub is_sandboxed: bool,
}

/// A message for display in the TUI
#[derive(Debug, Clone)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: MessageRole,
    pub text: String,
    pub tool_calls: Vec<ToolCallInfo>,
    pub is_streaming: bool,
}

/// Parsed slash command from user input
pub enum Command {
    /// /model [query] — switch model or list models if query is None
    Model(Option<String>),
    /// /tools [name] — open tool picker or toggle by name
    Tools(Option<String>),
}

/// Interactive model picker state
pub struct ModelPicker {
    pub items: Vec<ModelPickerItem>,
    pub selected: usize,
}

pub struct ModelPickerItem {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub is_active: bool,
}

impl ModelPicker {
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.items.get(self.selected).map(|i| i.id.as_str())
    }
}

/// Interactive tool picker state
pub struct ToolPicker {
    pub items: Vec<ToolPickerItem>,
    pub selected: usize,
}

pub struct ToolPickerItem {
    pub key: String,
    pub label: String,
    pub enabled: bool,
}

impl ToolPicker {
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    pub fn toggle_selected(&mut self) {
        if let Some(item) = self.items.get_mut(self.selected) {
            item.enabled = !item.enabled;
        }
    }
}

/// The result of handling an event — tells the main loop what to do next.
#[allow(dead_code)] // Quit reserved for future use
pub enum EngineAction {
    None,
    Redraw,
    Quit,
}

/// UI-agnostic chat engine that manages a single conversation.
/// Can be used by the TUI or by a headless runner for sub-agents.
pub struct ChatEngine {
    pub conversation: Option<Conversation>,
    pub model_config: ModelConfig,
    pub provider_config: ProviderConfig,
    pub execution_settings: ExecutionSettingsModel,
    pub models: ModelsModel,
    pub providers: Vec<ProviderConfig>,
    pub mcp_service: Option<McpService>,
    pub execution_approval_store: ExecutionApprovalStore,
    pub write_approval_store: WriteApprovalStore,
    pub user_secrets: Vec<(String, String)>,

    // Display state
    pub messages: Vec<DisplayMessage>,
    pub is_streaming: bool,
    pub cancel_flag: Option<Arc<AtomicBool>>,
    pub pending_approval: Option<PendingApproval>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub title: String,
    pub is_ready: bool,
    pub model_picker: Option<ModelPicker>,
    pub tool_picker: Option<ToolPicker>,
    pub scroll_offset: u16,

    event_tx: mpsc::UnboundedSender<AppEvent>,
}

impl ChatEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model_config: ModelConfig,
        provider_config: ProviderConfig,
        execution_settings: ExecutionSettingsModel,
        models: ModelsModel,
        providers: Vec<ProviderConfig>,
        mcp_service: Option<McpService>,
        user_secrets: Vec<(String, String)>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            conversation: None,
            model_config,
            provider_config,
            execution_settings,
            models,
            providers,
            mcp_service,
            execution_approval_store: ExecutionApprovalStore::new(),
            write_approval_store: WriteApprovalStore::new(),
            user_secrets,
            messages: Vec::new(),
            is_streaming: false,
            cancel_flag: None,
            pending_approval: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            title: "New Chat".to_string(),
            is_ready: false,
            model_picker: None,
            tool_picker: None,
            scroll_offset: 0,
            event_tx,
        }
    }

    /// Initialize the conversation (async — creates agent with tools)
    pub async fn init_conversation(&mut self) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();

        // Gather MCP tools if service is available
        let mcp_tools = if let Some(ref mcp_service) = self.mcp_service {
            match mcp_service.get_all_tools_with_sinks().await {
                Ok(tools) if !tools.is_empty() => {
                    info!(count = tools.len(), "MCP tools loaded");
                    Some(tools)
                }
                Ok(_) => None,
                Err(e) => {
                    warn!(error = ?e, "Failed to load MCP tools");
                    None
                }
            }
        } else {
            None
        };

        let es = &self.execution_settings;
        let any_tool_enabled = es.enabled
            || es.filesystem_read_enabled
            || es.filesystem_write_enabled
            || es.fetch_enabled
            || es.git_enabled
            || es.mcp_service_tool_enabled
            || es.docker_code_execution_enabled;
        let exec_settings = if any_tool_enabled {
            Some(self.execution_settings.clone())
        } else {
            None
        };

        let pending_approvals = self.execution_approval_store.get_pending_approvals();
        let pending_write_approvals = self.write_approval_store.get_pending_approvals();

        let conversation = Conversation::new(
            id,
            "New Chat".to_string(),
            &self.model_config,
            &self.provider_config,
            mcp_tools,
            exec_settings,
            Some(pending_approvals),
            Some(pending_write_approvals),
            self.user_secrets.clone(),
            None, // no theme colors in TUI
        )
        .await
        .context("Failed to create conversation")?;

        self.conversation = Some(conversation);
        self.is_ready = true;
        Ok(())
    }

    /// Send a message and start streaming the response
    pub fn send_message(&mut self, message: String) {
        if !self.is_ready || self.is_streaming {
            return;
        }

        // Reset scroll to bottom when sending
        self.scroll_offset = 0;

        let conversation = match self.conversation.as_mut() {
            Some(c) => c,
            None => return,
        };

        // Add user message to display
        self.messages.push(DisplayMessage {
            role: MessageRole::User,
            text: message.clone(),
            tool_calls: Vec::new(),
            is_streaming: false,
        });

        // Build user content
        let contents = vec![UserContent::text(message.clone())];

        // Add user message to conversation history
        let user_msg = rig::completion::Message::User {
            content: rig::OneOrMany::one(UserContent::text(message)),
        };
        conversation.add_user_message_with_attachments(user_msg, vec![]);

        // Start assistant placeholder
        self.messages.push(DisplayMessage {
            role: MessageRole::Assistant,
            text: String::new(),
            tool_calls: Vec::new(),
            is_streaming: true,
        });
        self.is_streaming = true;

        // Set up approval channels
        let (approval_tx, approval_rx) = mpsc::unbounded_channel::<ApprovalNotification>();
        let (resolution_tx, resolution_rx) = mpsc::unbounded_channel::<ApprovalResolution>();
        chatty_core::models::execution_approval_store::set_global_approval_notifier(
            approval_tx.clone(),
        );
        self.execution_approval_store
            .set_notifiers(approval_tx, resolution_tx);

        // Spawn stream task
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Some(cancel_flag.clone());

        let agent = conversation.agent().clone();
        let history = conversation.history().to_vec();
        let event_tx = self.event_tx.clone();
        let max_agent_turns = self.execution_settings.max_agent_turns as usize;

        tokio::spawn(async move {
            let result = run_stream(StreamParams {
                agent,
                history,
                contents,
                cancel_flag,
                event_tx: event_tx.clone(),
                approval_rx,
                resolution_rx,
                max_agent_turns,
            })
            .await;

            if let Err(e) = result {
                let _ = event_tx.send(AppEvent::StreamError(e.to_string()));
            }
        });
    }

    /// Process an AppEvent and return what the main loop should do
    pub fn handle_event(&mut self, event: AppEvent) -> EngineAction {
        match event {
            AppEvent::StreamStarted => {
                self.is_streaming = true;
                self.scroll_offset = 0;
                EngineAction::Redraw
            }
            AppEvent::TextChunk(text) => {
                // Append to conversation streaming state
                if let Some(conv) = self.conversation.as_mut() {
                    conv.append_streaming_content(&text);
                }
                // Append to display
                if let Some(last) = self.messages.last_mut() {
                    last.text.push_str(&text);
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallStarted { id, name } => {
                let info = ToolCallInfo {
                    id,
                    name,
                    input: String::new(),
                    output: None,
                    state: ToolCallState::Running,
                };
                if let Some(last) = self.messages.last_mut() {
                    last.tool_calls.push(info);
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallInput { id, arguments } => {
                if let Some(last) = self.messages.last_mut()
                    && let Some(tc) = last.tool_calls.iter_mut().find(|t| t.id == id)
                {
                    tc.input.push_str(&arguments);
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallResult { id, result } => {
                if let Some(last) = self.messages.last_mut()
                    && let Some(tc) = last.tool_calls.iter_mut().find(|t| t.id == id)
                {
                    tc.output = Some(result);
                    tc.state = ToolCallState::Success;
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallError { id, error } => {
                if let Some(last) = self.messages.last_mut()
                    && let Some(tc) = last.tool_calls.iter_mut().find(|t| t.id == id)
                {
                    tc.output = Some(error.clone());
                    tc.state = ToolCallState::Error(error);
                }
                EngineAction::Redraw
            }
            AppEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => {
                self.pending_approval = Some(PendingApproval {
                    id,
                    command,
                    is_sandboxed,
                });
                EngineAction::Redraw
            }
            AppEvent::ApprovalResolved { id: _, approved: _ } => {
                self.pending_approval = None;
                EngineAction::Redraw
            }
            AppEvent::TokenUsage {
                input_tokens,
                output_tokens,
            } => {
                self.total_input_tokens += input_tokens;
                self.total_output_tokens += output_tokens;
                EngineAction::Redraw
            }
            AppEvent::StreamCompleted => {
                self.finalize_stream();
                EngineAction::Redraw
            }
            AppEvent::StreamError(error) => {
                error!(error = %error, "Stream error");
                if let Some(last) = self.messages.last_mut() {
                    if last.text.is_empty() {
                        last.text = format!("[Error: {}]", error);
                    } else {
                        last.text.push_str(&format!("\n\n[Error: {}]", error));
                    }
                    last.is_streaming = false;
                }
                self.is_streaming = false;
                self.cancel_flag = None;
                self.pending_approval = None;
                EngineAction::Redraw
            }
            AppEvent::StreamCancelled => {
                if let Some(last) = self.messages.last_mut() {
                    last.text.push_str("\n\n[Cancelled]");
                    last.is_streaming = false;
                }
                self.is_streaming = false;
                self.cancel_flag = None;
                self.pending_approval = None;
                // Finalize whatever we got
                if let Some(conv) = self.conversation.as_mut() {
                    let response = conv.streaming_message().cloned().unwrap_or_default();
                    conv.finalize_response(response, vec![], None);
                    conv.set_streaming_message(None);
                }
                EngineAction::Redraw
            }
            AppEvent::TitleGenerated(title) => {
                self.title = title;
                EngineAction::Redraw
            }
            AppEvent::ConversationReady => {
                self.is_ready = true;
                EngineAction::Redraw
            }
            AppEvent::TerminalInput(_) | AppEvent::Tick => {
                // Handled by app.rs, not the engine
                EngineAction::None
            }
        }
    }

    /// Stop the active stream
    pub fn stop_stream(&mut self) {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Approve a pending tool execution (checks both execution and write stores)
    pub fn approve(&mut self) {
        if let Some(approval) = self.pending_approval.take()
            && !self
                .execution_approval_store
                .resolve(&approval.id, ApprovalDecision::Approved)
        {
            self.write_approval_store
                .resolve(&approval.id, WriteApprovalDecision::Approved);
        }
    }

    /// Deny a pending tool execution (checks both execution and write stores)
    pub fn deny(&mut self) {
        if let Some(approval) = self.pending_approval.take()
            && !self
                .execution_approval_store
                .resolve(&approval.id, ApprovalDecision::Denied)
        {
            self.write_approval_store
                .resolve(&approval.id, WriteApprovalDecision::Denied);
        }
    }

    /// Try to handle a slash command. Returns true if the input was a command.
    pub fn try_handle_command(&self, input: &str) -> Option<Command> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        match parts[0] {
            "/model" => {
                let query = parts.get(1).map(|s| s.trim().to_string());
                Some(Command::Model(query))
            }
            "/tools" => {
                let name = parts.get(1).map(|s| s.trim().to_string());
                Some(Command::Tools(name))
            }
            _ => None,
        }
    }

    /// Prepare to switch models: resolve the model, update config, mark not ready.
    /// Call `init_conversation()` after this to complete the switch.
    pub fn prepare_model_switch(&mut self, query: &str) -> Result<()> {
        let all_models = self.models.models();

        // Try exact match on id first
        let new_model = all_models
            .iter()
            .find(|m| m.id == query)
            // Then case-insensitive name match
            .or_else(|| {
                all_models
                    .iter()
                    .find(|m| m.name.to_lowercase() == query.to_lowercase())
            })
            // Then partial match on model identifier
            .or_else(|| {
                all_models
                    .iter()
                    .find(|m| m.model_identifier.contains(query))
            })
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Model '{}' not found. Type /model to see available models.",
                    query
                )
            })?;

        let new_provider = self
            .providers
            .iter()
            .find(|p| p.provider_type == new_model.provider_type)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("No provider configured for {:?}", new_model.provider_type)
            })?;

        let model_name = new_model.name.clone();
        self.model_config = new_model;
        self.provider_config = new_provider;
        self.conversation = None;
        self.is_ready = false;

        self.add_system_message(format!(
            "Switched to {}. Conversation context was reset.",
            model_name,
        ));

        info!(model = %model_name, "Switched model");
        Ok(())
    }

    /// Open the interactive model picker
    pub fn open_model_picker(&mut self) {
        let all_models = self.models.models();
        let active_id = &self.model_config.id;
        let mut selected = 0;

        let items: Vec<ModelPickerItem> = all_models
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let is_active = m.id == *active_id;
                if is_active {
                    selected = i;
                }
                ModelPickerItem {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    provider: format!("{:?}", m.provider_type),
                    is_active,
                }
            })
            .collect();

        if items.is_empty() {
            self.add_system_message("No models configured.".to_string());
            return;
        }

        self.model_picker = Some(ModelPicker { items, selected });
    }

    /// Close the model picker without selecting
    pub fn close_model_picker(&mut self) {
        self.model_picker = None;
    }

    /// Open the interactive tool picker
    pub fn open_tool_picker(&mut self) {
        let es = &self.execution_settings;
        let items = vec![
            ToolPickerItem {
                key: "shell".to_string(),
                label: "Shell Execution".to_string(),
                enabled: es.enabled,
            },
            ToolPickerItem {
                key: "fs-read".to_string(),
                label: "Filesystem Read".to_string(),
                enabled: es.filesystem_read_enabled,
            },
            ToolPickerItem {
                key: "fs-write".to_string(),
                label: "Filesystem Write".to_string(),
                enabled: es.filesystem_write_enabled,
            },
            ToolPickerItem {
                key: "fetch".to_string(),
                label: "Fetch".to_string(),
                enabled: es.fetch_enabled,
            },
            ToolPickerItem {
                key: "git".to_string(),
                label: "Git".to_string(),
                enabled: es.git_enabled,
            },
            ToolPickerItem {
                key: "mcp-manage".to_string(),
                label: "MCP Management".to_string(),
                enabled: es.mcp_service_tool_enabled,
            },
            ToolPickerItem {
                key: "docker-exec".to_string(),
                label: "Docker Execution".to_string(),
                enabled: es.docker_code_execution_enabled,
            },
        ];

        self.tool_picker = Some(ToolPicker { items, selected: 0 });
    }

    /// Close the tool picker without applying changes
    pub fn close_tool_picker(&mut self) {
        self.tool_picker = None;
    }

    /// Apply tool picker changes: update execution_settings, clear conversation for reinit
    pub fn apply_tool_picker(&mut self) {
        let picker = match self.tool_picker.take() {
            Some(p) => p,
            None => return,
        };

        for item in &picker.items {
            match item.key.as_str() {
                "shell" => self.execution_settings.enabled = item.enabled,
                "fs-read" => self.execution_settings.filesystem_read_enabled = item.enabled,
                "fs-write" => self.execution_settings.filesystem_write_enabled = item.enabled,
                "fetch" => self.execution_settings.fetch_enabled = item.enabled,
                "git" => self.execution_settings.git_enabled = item.enabled,
                "mcp-manage" => self.execution_settings.mcp_service_tool_enabled = item.enabled,
                "docker-exec" => {
                    self.execution_settings.docker_code_execution_enabled = item.enabled
                }
                _ => {}
            }
        }

        self.conversation = None;
        self.is_ready = false;
        self.add_system_message(
            "Tool settings updated. Conversation context was reset.".to_string(),
        );
    }

    /// Toggle a tool by name directly (for `/tools <name>`)
    pub fn toggle_tool_by_name(&mut self, name: &str) -> bool {
        match name {
            "shell" => self.execution_settings.enabled = !self.execution_settings.enabled,
            "fs-read" => {
                self.execution_settings.filesystem_read_enabled =
                    !self.execution_settings.filesystem_read_enabled
            }
            "fs-write" => {
                self.execution_settings.filesystem_write_enabled =
                    !self.execution_settings.filesystem_write_enabled
            }
            "fetch" => {
                self.execution_settings.fetch_enabled = !self.execution_settings.fetch_enabled
            }
            "git" => self.execution_settings.git_enabled = !self.execution_settings.git_enabled,
            "mcp-manage" => {
                self.execution_settings.mcp_service_tool_enabled =
                    !self.execution_settings.mcp_service_tool_enabled
            }
            "docker-exec" => {
                self.execution_settings.docker_code_execution_enabled =
                    !self.execution_settings.docker_code_execution_enabled
            }
            _ => {
                self.add_system_message(format!(
                    "Unknown tool '{}'. Valid: shell, fs-read, fs-write, fetch, git, mcp-manage, docker-exec",
                    name
                ));
                return false;
            }
        }

        let enabled = match name {
            "shell" => self.execution_settings.enabled,
            "fs-read" => self.execution_settings.filesystem_read_enabled,
            "fs-write" => self.execution_settings.filesystem_write_enabled,
            "fetch" => self.execution_settings.fetch_enabled,
            "git" => self.execution_settings.git_enabled,
            "mcp-manage" => self.execution_settings.mcp_service_tool_enabled,
            "docker-exec" => self.execution_settings.docker_code_execution_enabled,
            _ => false,
        };
        let state = if enabled { "enabled" } else { "disabled" };
        self.add_system_message(format!("Tool '{}' {}. Reinitializing...", name, state));
        self.conversation = None;
        self.is_ready = false;
        true
    }

    /// Add a system message to the display
    pub fn add_system_message(&mut self, text: String) {
        self.messages.push(DisplayMessage {
            role: MessageRole::System,
            text,
            tool_calls: Vec::new(),
            is_streaming: false,
        });
    }

    fn finalize_stream(&mut self) {
        // Mark display message as done
        if let Some(last) = self.messages.last_mut() {
            last.is_streaming = false;
        }

        // Finalize conversation
        if let Some(conv) = self.conversation.as_mut() {
            let response = conv.streaming_message().cloned().unwrap_or_default();
            conv.finalize_response(response, vec![], None);
            conv.set_streaming_message(None);
        }

        self.is_streaming = false;
        self.cancel_flag = None;
        self.pending_approval = None;

        // Generate title after first exchange
        if self.messages.len() == 2 && self.title == "New Chat" {
            let event_tx = self.event_tx.clone();
            if let Some(conv) = &self.conversation {
                let agent = conv.agent().clone();
                let history = conv.history().to_vec();
                tokio::spawn(async move {
                    match chatty_core::services::generate_title(&agent, &history).await {
                        Ok(title) => {
                            let _ = event_tx.send(AppEvent::TitleGenerated(title));
                        }
                        Err(e) => {
                            warn!(error = ?e, "Failed to generate title");
                        }
                    }
                });
            }
        }
    }
}

struct StreamParams {
    agent: AgentClient,
    history: Vec<rig::completion::Message>,
    contents: Vec<UserContent>,
    cancel_flag: Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    approval_rx: mpsc::UnboundedReceiver<ApprovalNotification>,
    resolution_rx: mpsc::UnboundedReceiver<ApprovalResolution>,
    max_agent_turns: usize,
}

/// Run the LLM stream in a background task, sending AppEvents for each chunk.
async fn run_stream(params: StreamParams) -> Result<()> {
    let StreamParams {
        agent,
        history,
        contents,
        cancel_flag,
        event_tx,
        approval_rx,
        resolution_rx,
        max_agent_turns,
    } = params;
    let tool_concurrency: usize = 4;
    let (mut stream, _user_message) = stream_prompt(
        &agent,
        &history,
        contents,
        Some(approval_rx),
        Some(resolution_rx),
        max_agent_turns,
        tool_concurrency,
    )
    .await
    .context("Failed to start stream")?;

    let _ = event_tx.send(AppEvent::StreamStarted);

    while let Some(chunk_result) = stream.next().await {
        if cancel_flag.load(Ordering::Relaxed) {
            let _ = event_tx.send(AppEvent::StreamCancelled);
            return Ok(());
        }

        match chunk_result? {
            StreamChunk::Text(text) => {
                let _ = event_tx.send(AppEvent::TextChunk(text));
            }
            StreamChunk::ToolCallStarted { id, name } => {
                let _ = event_tx.send(AppEvent::ToolCallStarted { id, name });
            }
            StreamChunk::ToolCallInput { id, arguments } => {
                let _ = event_tx.send(AppEvent::ToolCallInput { id, arguments });
            }
            StreamChunk::ToolCallResult { id, result } => {
                let _ = event_tx.send(AppEvent::ToolCallResult { id, result });
            }
            StreamChunk::ToolCallError { id, error } => {
                let _ = event_tx.send(AppEvent::ToolCallError { id, error });
            }
            StreamChunk::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => {
                let _ = event_tx.send(AppEvent::ApprovalRequested {
                    id,
                    command,
                    is_sandboxed,
                });
            }
            StreamChunk::ApprovalResolved { id, approved } => {
                let _ = event_tx.send(AppEvent::ApprovalResolved { id, approved });
            }
            StreamChunk::TokenUsage {
                input_tokens,
                output_tokens,
            } => {
                let _ = event_tx.send(AppEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                });
            }
            StreamChunk::Done => break,
            StreamChunk::Error(e) => {
                let _ = event_tx.send(AppEvent::StreamError(e));
                return Ok(());
            }
        }
    }

    let _ = event_tx.send(AppEvent::StreamCompleted);
    Ok(())
}
