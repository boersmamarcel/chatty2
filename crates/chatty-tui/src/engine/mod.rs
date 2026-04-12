use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use chatty_core::factories::agent_factory::AgentBuildContext;
use chatty_core::models::Conversation;
use chatty_core::models::execution_approval_store::{
    ApprovalDecision, ApprovalNotification, ApprovalResolution, ExecutionApprovalStore,
};
use chatty_core::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
use chatty_core::services::{McpService, MemoryService};
use chatty_core::settings::models::a2a_store::A2aAgentConfig;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::settings::models::{ExecutionSettingsModel, ModelsModel};

use rig::message::UserContent;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::events::AppEvent;

mod commands;
pub mod helpers;
mod streaming;

pub use commands::Command;
pub(crate) use helpers::sanitize_progress_line;

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
    Error,
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

/// Shared navigation behaviour for picker lists.
pub trait NavigableList {
    fn item_count(&self) -> usize;
    fn selected_mut(&mut self) -> &mut usize;

    fn move_up(&mut self) {
        let sel = self.selected_mut();
        if *sel > 0 {
            *sel -= 1;
        }
    }

    fn move_down(&mut self) {
        let count = self.item_count();
        let sel = self.selected_mut();
        if *sel + 1 < count {
            *sel += 1;
        }
    }
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

impl NavigableList for ModelPicker {
    fn item_count(&self) -> usize {
        self.items.len()
    }
    fn selected_mut(&mut self) -> &mut usize {
        &mut self.selected
    }
}

impl ModelPicker {
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

impl NavigableList for ToolPicker {
    fn item_count(&self) -> usize {
        self.items.len()
    }
    fn selected_mut(&mut self) -> &mut usize {
        &mut self.selected
    }
}

impl ToolPicker {
    pub fn toggle_selected(&mut self) {
        if let Some(item) = self.items.get_mut(self.selected) {
            item.enabled = !item.enabled;
        }
    }
}

/// The result of handling an event — tells the main loop what to do next.
pub enum EngineAction {
    None,
    Redraw,
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
    pub memory_service: Option<MemoryService>,
    pub search_settings:
        Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<chatty_core::services::EmbeddingService>,
    pub skill_service: chatty_core::services::SkillService,
    pub execution_approval_store: ExecutionApprovalStore,
    pub write_approval_store: WriteApprovalStore,
    pub user_secrets: Vec<(String, String)>,
    /// Configured remote A2A agents available for `invoke_agent` and `/agent`.
    pub remote_agents: Vec<A2aAgentConfig>,
    /// When `true`, this engine is running as a sub-agent and must not expose
    /// the sub_agent tool (preventing recursive sub-agent spawning).
    pub is_sub_agent: bool,

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
    /// Index into `messages` of the system message showing sub-agent progress.
    /// `None` when no sub-agent is running.
    pub sub_agent_msg_idx: Option<usize>,
    /// Tracks `invoke_agent` tool call IDs to suppress their ToolCallBlock rendering.
    active_invoke_agent_ids: HashSet<String>,

    event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Monotonically increasing counter to discard stale background init results.
    init_generation: u64,
}

/// Configuration for constructing a new `ChatEngine`.
pub struct ChatEngineConfig {
    pub model_config: ModelConfig,
    pub provider_config: ProviderConfig,
    pub execution_settings: ExecutionSettingsModel,
    pub models: ModelsModel,
    pub providers: Vec<ProviderConfig>,
    pub mcp_service: Option<McpService>,
    pub memory_service: Option<MemoryService>,
    pub search_settings:
        Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<chatty_core::services::EmbeddingService>,
    pub user_secrets: Vec<(String, String)>,
    pub remote_agents: Vec<A2aAgentConfig>,
    pub is_sub_agent: bool,
}

impl ChatEngine {
    pub fn new(config: ChatEngineConfig, event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        let skill_service =
            chatty_core::services::SkillService::new(config.embedding_service.clone());
        Self {
            conversation: None,
            model_config: config.model_config,
            provider_config: config.provider_config,
            execution_settings: config.execution_settings,
            models: config.models,
            providers: config.providers,
            mcp_service: config.mcp_service,
            memory_service: config.memory_service,
            search_settings: config.search_settings,
            embedding_service: config.embedding_service,
            skill_service,
            execution_approval_store: ExecutionApprovalStore::new(),
            write_approval_store: WriteApprovalStore::new(),
            user_secrets: config.user_secrets,
            remote_agents: config.remote_agents,
            is_sub_agent: config.is_sub_agent,
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
            sub_agent_msg_idx: None,
            active_invoke_agent_ids: HashSet::new(),
            event_tx,
            init_generation: 0,
        }
    }

    fn available_model_ids(&self) -> Vec<String> {
        self.models.models().iter().map(|m| m.id.clone()).collect()
    }

    /// Initialize the conversation (async — creates agent with tools)
    pub async fn init_conversation(&mut self) -> Result<()> {
        // Bump generation so any in-flight background init is ignored
        self.init_generation += 1;

        let id = uuid::Uuid::new_v4().to_string();

        // Gather MCP tools if service is available
        let mcp_tools = match self.mcp_service {
            Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
            None => None,
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
            AgentBuildContext {
                mcp_tools,
                exec_settings,
                pending_approvals: Some(pending_approvals),
                pending_write_approvals: Some(pending_write_approvals),
                pending_artifacts: None,
                shell_session: None,
                user_secrets: self.user_secrets.clone(),
                theme_colors: None, // no theme colors in TUI
                memory_service: self.memory_service.clone(),
                search_settings: self.search_settings.clone(),
                embedding_service: self.embedding_service.clone(),
                allow_sub_agent: !self.is_sub_agent,
                module_agents: Vec::new(), // no WASM module discovery in TUI
                gateway_port: None,        // no gateway port in TUI
                remote_agents: self.remote_agents.clone(),
                available_model_ids: self.available_model_ids(),
            },
        )
        .await
        .context("Failed to create conversation")?;

        self.conversation = Some(conversation);
        self.is_ready = true;
        Ok(())
    }

    /// Spawn conversation initialization as a background task.
    ///
    /// The result is delivered via `AppEvent::ConversationInitialized` so the
    /// TUI can render immediately while the agent is being built. Each call
    /// increments an internal generation counter; if `init_conversation()` or
    /// another `spawn_init_conversation()` runs before the background task
    /// finishes, the stale result is silently discarded by the event handler.
    pub fn spawn_init_conversation(&mut self) {
        self.init_generation += 1;
        let generation = self.init_generation;

        let id = uuid::Uuid::new_v4().to_string();
        let model_config = self.model_config.clone();
        let provider_config = self.provider_config.clone();
        let mcp_service = self.mcp_service.clone();
        let execution_settings = self.execution_settings.clone();
        let pending_approvals = self.execution_approval_store.get_pending_approvals();
        let pending_write_approvals = self.write_approval_store.get_pending_approvals();
        let user_secrets = self.user_secrets.clone();
        let remote_agents = self.remote_agents.clone();
        let available_model_ids = self.available_model_ids();
        let memory_service = self.memory_service.clone();
        let search_settings = self.search_settings.clone();
        let embedding_service = self.embedding_service.clone();
        let event_tx = self.event_tx.clone();
        let is_sub_agent = self.is_sub_agent;

        tokio::spawn(async move {
            // Gather MCP tools
            let mcp_tools = match mcp_service {
                Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
                None => None,
            };

            let es = &execution_settings;
            let any_tool_enabled = es.enabled
                || es.filesystem_read_enabled
                || es.filesystem_write_enabled
                || es.fetch_enabled
                || es.git_enabled
                || es.mcp_service_tool_enabled
                || es.docker_code_execution_enabled;
            let exec_settings = if any_tool_enabled {
                Some(execution_settings.clone())
            } else {
                None
            };

            let result = Conversation::new(
                id,
                "New Chat".to_string(),
                &model_config,
                &provider_config,
                AgentBuildContext {
                    mcp_tools,
                    exec_settings,
                    pending_approvals: Some(pending_approvals),
                    pending_write_approvals: Some(pending_write_approvals),
                    pending_artifacts: None,
                    shell_session: None,
                    user_secrets,
                    theme_colors: None, // no theme colors in TUI
                    memory_service,
                    search_settings,
                    embedding_service,
                    allow_sub_agent: !is_sub_agent,
                    module_agents: Vec::new(), // no WASM module discovery in TUI
                    gateway_port: None,        // no gateway port in TUI
                    remote_agents,
                    available_model_ids,
                },
            )
            .await;

            match result {
                Ok(conversation) => {
                    if event_tx
                        .send(AppEvent::ConversationInitialized {
                            conversation: Box::new(conversation),
                            generation,
                        })
                        .is_err()
                    {
                        warn!(
                            generation,
                            "Failed to send ConversationInitialized event (receiver dropped)"
                        );
                    }
                }
                Err(e) => {
                    if event_tx
                        .send(AppEvent::ConversationInitFailed(format!("{:#}", e)))
                        .is_err()
                    {
                        warn!("Failed to send ConversationInitFailed event (receiver dropped)");
                    }
                }
            }
        });
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
        let history = conversation.messages();
        let invoke_agent_progress_slot = conversation.invoke_agent_progress_slot();
        let event_tx = self.event_tx.clone();
        let max_agent_turns = self.execution_settings.max_agent_turns as usize;
        let workspace_dir = self.execution_settings.workspace_dir.clone();
        let memory_service = self.memory_service.clone();
        let embedding_service = self.embedding_service.clone();
        let skill_service = self.skill_service.clone();

        tokio::spawn(async move {
            let contents = chatty_core::services::augment_with_memory(
                contents,
                memory_service.as_ref(),
                embedding_service.as_ref(),
                &skill_service,
                workspace_dir.as_deref(),
            )
            .await;

            let result = streaming::run_stream(streaming::StreamParams {
                agent,
                history,
                contents,
                cancel_flag,
                event_tx: event_tx.clone(),
                approval_rx,
                resolution_rx,
                max_agent_turns,
                invoke_agent_progress_slot,
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
                if name == "invoke_agent" {
                    self.active_invoke_agent_ids.insert(id);
                } else {
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
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallInput { id, arguments } => {
                if !self.active_invoke_agent_ids.contains(&id)
                    && let Some(last) = self.messages.last_mut()
                    && let Some(tc) = last.tool_calls.iter_mut().find(|t| t.id == id)
                {
                    tc.input.push_str(&arguments);
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallResult { id, result } => {
                if self.active_invoke_agent_ids.remove(&id) {
                    // invoke_agent result — sub-agent progress already handled
                } else if let Some(last) = self.messages.last_mut()
                    && let Some(tc) = last.tool_calls.iter_mut().find(|t| t.id == id)
                {
                    tc.output = Some(result);
                    tc.state = ToolCallState::Success;
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallError { id, error } => {
                if self.active_invoke_agent_ids.remove(&id) {
                    // invoke_agent error — sub-agent progress already handled
                } else if let Some(last) = self.messages.last_mut()
                    && let Some(tc) = last.tool_calls.iter_mut().find(|t| t.id == id)
                {
                    tc.output = Some(error.clone());
                    tc.state = ToolCallState::Error;
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
            AppEvent::ConversationInitialized {
                conversation,
                generation,
            } => {
                // Only accept if this is still the latest init generation.
                // A newer init_conversation() or spawn_init_conversation() call
                // may have started since this background task was launched.
                if generation == self.init_generation {
                    self.conversation = Some(*conversation);
                    self.is_ready = true;
                    info!("Background conversation initialization completed");
                }
                EngineAction::Redraw
            }
            AppEvent::ConversationInitFailed(error) => {
                error!(error = %error, "Background conversation initialization failed");
                self.add_system_message(format!("Failed to initialize: {}", error));
                EngineAction::Redraw
            }
            AppEvent::SubAgentProgress(line) => {
                let line = sanitize_progress_line(&line);
                if line.is_empty() {
                    return EngineAction::None;
                }
                if self.sub_agent_msg_idx.is_none() {
                    // Auto-create system message for invoke_agent progress
                    self.add_system_message(line);
                    self.sub_agent_msg_idx = Some(self.messages.len() - 1);
                } else if let Some(idx) = self.sub_agent_msg_idx
                    && let Some(msg) = self.messages.get_mut(idx)
                {
                    msg.text.push('\n');
                    msg.text.push_str(&line);
                }
                EngineAction::Redraw
            }
            AppEvent::SubAgentFinished(message) => {
                self.sub_agent_msg_idx = None;
                self.add_system_message(message);
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
                let history = conv.messages();
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
