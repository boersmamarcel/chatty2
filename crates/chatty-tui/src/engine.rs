use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

use anyhow::{Context, Result, bail};
use chatty_core::factories::AgentClient;
use chatty_core::models::Conversation;
use chatty_core::models::execution_approval_store::{
    ApprovalDecision, ApprovalNotification, ApprovalResolution, ExecutionApprovalStore,
};
use chatty_core::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
use chatty_core::services::{McpService, MemoryService, StreamChunk, stream_prompt};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// /model [query] — switch model or list models if query is None
    Model(Option<String>),
    /// /tools [name] — open tool picker or toggle by name
    Tools(Option<String>),
    /// /add-dir <directory> — expand file-access workspace to include a directory
    AddDir(Option<String>),
    /// /agent [prompt] — launch a sub-agent in headless mode
    Agent(Option<String>),
    /// /clear, /new — clear conversation and start fresh
    Clear,
    /// /compact — summarize older conversation turns
    Compact,
    /// /context — show context usage stats
    Context,
    /// /copy — copy latest assistant response to clipboard
    Copy,
    /// /cwd, /cd [directory] — show or change working directory
    Cwd(Option<String>),
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
    pub memory_service: Option<MemoryService>,
    pub search_settings:
        Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<chatty_core::services::EmbeddingService>,
    pub skill_service: chatty_core::services::SkillService,
    pub execution_approval_store: ExecutionApprovalStore,
    pub write_approval_store: WriteApprovalStore,
    pub user_secrets: Vec<(String, String)>,
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

    event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Monotonically increasing counter to discard stale background init results.
    init_generation: u64,
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
        memory_service: Option<MemoryService>,
        search_settings: Option<
            chatty_core::settings::models::search_settings::SearchSettingsModel,
        >,
        embedding_service: Option<chatty_core::services::EmbeddingService>,
        user_secrets: Vec<(String, String)>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        is_sub_agent: bool,
    ) -> Self {
        let skill_service = chatty_core::services::SkillService::new(embedding_service.clone());
        Self {
            conversation: None,
            model_config,
            provider_config,
            execution_settings,
            models,
            providers,
            mcp_service,
            memory_service,
            search_settings,
            embedding_service,
            skill_service,
            execution_approval_store: ExecutionApprovalStore::new(),
            write_approval_store: WriteApprovalStore::new(),
            user_secrets,
            is_sub_agent,
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
            event_tx,
            init_generation: 0,
        }
    }

    /// Initialize the conversation (async — creates agent with tools)
    pub async fn init_conversation(&mut self) -> Result<()> {
        // Bump generation so any in-flight background init is ignored
        self.init_generation += 1;

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
            self.memory_service.clone(),
            self.search_settings.clone(),
            self.embedding_service.clone(),
            !self.is_sub_agent,
            Vec::new(), // no WASM module discovery in TUI/headless mode
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
        let memory_service = self.memory_service.clone();
        let search_settings = self.search_settings.clone();
        let embedding_service = self.embedding_service.clone();
        let event_tx = self.event_tx.clone();
        let is_sub_agent = self.is_sub_agent;

        tokio::spawn(async move {
            // Gather MCP tools
            let mcp_tools = if let Some(ref mcp_service) = mcp_service {
                match mcp_service.get_all_tools_with_sinks().await {
                    Ok(tools) if !tools.is_empty() => {
                        info!(count = tools.len(), "MCP tools loaded (background)");
                        Some(tools)
                    }
                    Ok(_) => None,
                    Err(e) => {
                        warn!(error = ?e, "Failed to load MCP tools (background)");
                        None
                    }
                }
            } else {
                None
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
                mcp_tools,
                exec_settings,
                Some(pending_approvals),
                Some(pending_write_approvals),
                user_secrets,
                None, // no theme colors in TUI
                memory_service,
                search_settings,
                embedding_service,
                !is_sub_agent,
                Vec::new(), // no WASM module discovery in TUI/headless mode
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
        let query_text = message.clone();
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
        let workspace_dir = self.execution_settings.workspace_dir.clone();
        let memory_service = self.memory_service.clone();
        let embedding_service = self.embedding_service.clone();
        let skill_service = self.skill_service.clone();

        tokio::spawn(async move {
            // Auto-retrieve relevant memories and inject as context
            let contents = if let Some(ref mem_svc) = memory_service {
                if !query_text.is_empty() {
                    let workspace_skills_dir = workspace_dir
                        .as_deref()
                        .map(|d| std::path::Path::new(d).join(".claude").join("skills"));
                    if let Some(context_block) = chatty_core::services::load_auto_context_block(
                        chatty_core::services::AutoContextRequest {
                            memory_service: mem_svc,
                            embedding_service: embedding_service.as_ref(),
                            skill_service: &skill_service,
                            query_text: &query_text,
                            fallback_query_text: None,
                            workspace_skills_dir: workspace_skills_dir.as_deref(),
                        },
                    )
                    .await
                    {
                        let mut augmented = vec![UserContent::text(context_block)];
                        augmented.extend(contents);
                        info!("Injected memory context into user message");
                        augmented
                    } else {
                        contents
                    }
                } else {
                    contents
                }
            } else {
                contents
            };

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
                if let Some(idx) = self.sub_agent_msg_idx
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

    /// Try to handle a slash command. Returns true if the input was a command.
    pub fn try_handle_command(&self, input: &str) -> Option<Command> {
        Self::parse_command(input)
    }

    fn parse_command(input: &str) -> Option<Command> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        let arg = parts
            .get(1)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        match parts[0] {
            "/model" => Some(Command::Model(arg)),
            "/tools" => Some(Command::Tools(arg)),
            "/add-dir" => Some(Command::AddDir(arg)),
            "/agent" => Some(Command::Agent(arg)),
            "/clear" | "/new" => Some(Command::Clear),
            "/compact" => Some(Command::Compact),
            "/context" => Some(Command::Context),
            "/copy" => Some(Command::Copy),
            "/cwd" | "/cd" => Some(Command::Cwd(arg)),
            _ => None,
        }
    }

    /// Clear all display and conversation state so a fresh conversation can be initialized.
    pub fn clear_conversation(&mut self) {
        self.messages.clear();
        self.title = "New Chat".to_string();
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.scroll_offset = 0;
        self.pending_approval = None;
        self.model_picker = None;
        self.tool_picker = None;
        self.conversation = None;
        self.is_ready = false;
        self.add_system_message("Started a new conversation.".to_string());
    }

    /// Show current context usage and working directory.
    pub fn context_summary(&self) -> String {
        let used_tokens = self
            .total_input_tokens
            .saturating_add(self.total_output_tokens);
        let workspace = self.current_working_directory();
        if let Some(max_context) = self.model_config.max_context_window
            && max_context > 0
        {
            let max_context_u32 = max_context as u32;
            let pct = (used_tokens as f64 / max_context_u32 as f64 * 100.0).clamp(0.0, 100.0);
            let filled = ((pct / 100.0) * 20.0).round() as usize;
            let bar = format!(
                "[{}{}]",
                "█".repeat(filled.min(20)),
                "░".repeat(20usize.saturating_sub(filled.min(20)))
            );
            format!(
                "Context usage: {} / {} tokens ({:.1}%) {}\nInput: {} tokens, Output: {} tokens\nWorking directory: {}",
                used_tokens,
                max_context_u32,
                pct,
                bar,
                self.total_input_tokens,
                self.total_output_tokens,
                workspace,
            )
        } else {
            format!(
                "Context usage: {} tokens (model max context window unknown)\nInput: {} tokens, Output: {} tokens\nWorking directory: {}",
                used_tokens, self.total_input_tokens, self.total_output_tokens, workspace
            )
        }
    }

    /// Return the active working directory for tool execution.
    pub fn current_working_directory(&self) -> String {
        if let Some(dir) = &self.execution_settings.workspace_dir {
            return dir.clone();
        }
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .to_string_lossy()
            .to_string()
    }

    /// Return a reference to the current execution settings.
    pub fn execution_settings(&self) -> &ExecutionSettingsModel {
        &self.execution_settings
    }

    /// Return a reference to the skill service.
    pub fn skill_service(&self) -> &chatty_core::services::SkillService {
        &self.skill_service
    }

    /// Change workspace directory and reset conversation so tools are reinitialized.
    pub fn set_working_directory(&mut self, directory: &str) -> Result<String> {
        let canonical = self.resolve_directory(directory)?;
        let canonical_str = canonical.to_string_lossy().to_string();
        self.execution_settings.workspace_dir = Some(canonical_str.clone());
        self.conversation = None;
        self.is_ready = false;
        self.add_system_message(format!(
            "Working directory changed to '{}'. Conversation context was reset.",
            canonical_str
        ));
        Ok(canonical_str)
    }

    /// Expand workspace access to include a directory by broadening to a common ancestor.
    pub fn add_allowed_directory(&mut self, directory: &str) -> Result<String> {
        let added_dir = self.resolve_directory(directory)?;
        let new_workspace = match self.execution_settings.workspace_dir.as_deref() {
            Some(current) => {
                let current = std::fs::canonicalize(current)
                    .with_context(|| format!("Current workspace '{}' no longer exists", current))?;
                if added_dir.starts_with(&current) {
                    current
                } else if current.starts_with(&added_dir) {
                    added_dir.clone()
                } else {
                    common_ancestor(&current, &added_dir).unwrap_or(added_dir.clone())
                }
            }
            None => added_dir.clone(),
        };

        let workspace_str = new_workspace.to_string_lossy().to_string();
        self.execution_settings.workspace_dir = Some(workspace_str.clone());
        self.conversation = None;
        self.is_ready = false;
        self.add_system_message(format!(
            "Added directory '{}'. Workspace expanded to '{}'. Conversation context was reset.",
            added_dir.to_string_lossy(),
            workspace_str
        ));
        Ok(workspace_str)
    }

    /// Summarize older conversation history to reduce context usage.
    pub async fn compact_conversation(&mut self) -> Result<()> {
        let (agent, history) = match self.conversation.as_ref() {
            Some(conv) => (conv.agent().clone(), conv.history().to_vec()),
            None => {
                self.add_system_message("No active conversation to compact.".to_string());
                return Ok(());
            }
        };

        if history.len() < 4 {
            self.add_system_message(
                "Conversation is too short to compact (need at least 4 messages).".to_string(),
            );
            return Ok(());
        }

        let midpoint = history.len() / 2;
        let result = chatty_core::token_budget::summarize_oldest_half(&agent, &history)
            .await
            .context("Failed to summarize conversation")?;

        if let Some(conv) = self.conversation.as_mut() {
            conv.replace_history(result.new_history, midpoint);
        }

        self.add_system_message(format!(
            "Compacted conversation: summarized {} messages (estimated {} tokens freed).",
            result.messages_summarized, result.estimated_tokens_freed
        ));
        Ok(())
    }

    /// Copy the latest assistant message to the system clipboard.
    pub fn copy_last_response_to_clipboard(&mut self) -> Result<()> {
        let Some(message) = self
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::Assistant) && !m.text.trim().is_empty())
        else {
            bail!("No assistant response available to copy");
        };
        copy_text_to_clipboard(&message.text)?;
        self.add_system_message("Copied latest assistant response to clipboard.".to_string());
        Ok(())
    }

    /// Launch a sub-agent by invoking chatty-tui in headless mode.
    /// This is intentionally non-blocking: completion is delivered via `AppEvent::SubAgentFinished`.
    pub fn launch_sub_agent(&mut self, prompt: &str) -> Result<()> {
        if self.is_sub_agent {
            bail!("Sub-agents cannot spawn further sub-agents");
        }

        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("Usage: /agent <prompt>");
        }

        let executable = std::env::current_exe().context("Failed to resolve chatty-tui binary")?;
        let model_id = self.model_config.id.clone();
        let prompt_owned = prompt.to_string();
        let auto_approve = matches!(
            self.execution_settings.approval_mode,
            chatty_core::settings::models::execution_settings::ApprovalMode::AutoApproveAll
        );
        let event_tx = self.event_tx.clone();

        self.add_system_message("Launching sub-agent...".to_string());
        self.sub_agent_msg_idx = Some(self.messages.len() - 1);

        tokio::task::spawn_blocking(move || {
            let message = match run_sub_agent_process(
                executable,
                model_id,
                prompt_owned,
                auto_approve,
                event_tx.clone(),
            ) {
                Ok(stdout) => {
                    let stdout = stdout.trim().to_string();
                    if stdout.is_empty() {
                        "Sub-agent completed with no output.".to_string()
                    } else {
                        format!("Sub-agent response:\n{}", stdout)
                    }
                }
                Err(e) => format!("Sub-agent failed: {}", e),
            };

            if let Err(e) = event_tx.send(AppEvent::SubAgentFinished(message)) {
                warn!(error = ?e, "Failed to deliver sub-agent completion event");
            }
        });

        Ok(())
    }

    fn resolve_directory(&self, directory: &str) -> Result<PathBuf> {
        let candidate = Path::new(directory);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            let base = self
                .execution_settings
                .workspace_dir
                .as_ref()
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            base.join(candidate)
        };

        let canonical = std::fs::canonicalize(&resolved)
            .with_context(|| format!("Directory '{}' does not exist", resolved.display()))?;
        if !canonical.is_dir() {
            bail!("'{}' is not a directory", canonical.display());
        }
        Ok(canonical)
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
    let (mut stream, _user_message) = stream_prompt(
        &agent,
        &history,
        contents,
        Some(approval_rx),
        Some(resolution_rx),
        max_agent_turns,
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

fn common_ancestor(left: &Path, right: &Path) -> Option<PathBuf> {
    let mut ancestor = PathBuf::new();
    for (l, r) in left.components().zip(right.components()) {
        if l == r {
            match l {
                Component::RootDir => ancestor.push(Path::new("/")),
                _ => ancestor.push(l.as_os_str()),
            }
        } else {
            break;
        }
    }
    if ancestor.as_os_str().is_empty() {
        None
    } else {
        Some(ancestor)
    }
}

fn copy_text_to_clipboard(text: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return copy_via_command("pbcopy", &[], text);
    }
    #[cfg(target_os = "windows")]
    {
        return copy_via_command("clip", &[], text);
    }
    #[cfg(target_os = "linux")]
    {
        if copy_via_command("wl-copy", &[], text).is_ok() {
            return Ok(());
        }
        if copy_via_command("xclip", &["-selection", "clipboard"], text).is_ok() {
            return Ok(());
        }
        bail!("No clipboard utility found. Install wl-clipboard or xclip.")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = text;
        bail!("Clipboard copy is not supported on this platform")
    }
}

fn copy_via_command(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = ProcessCommand::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to launch '{}'", program))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .context("Failed to write clipboard contents")?;
    }

    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        bail!("'{}' returned non-zero exit status", program)
    }
}

fn run_sub_agent_process(
    executable: PathBuf,
    model_id: String,
    prompt: String,
    auto_approve: bool,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<String> {
    use std::io::BufRead as _;

    let mut command = ProcessCommand::new(executable);
    command
        .arg("--headless")
        .arg("--model")
        .arg(model_id)
        .arg("--message")
        .arg(prompt)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if auto_approve {
        command.arg("--auto-approve");
    }

    let mut child = command
        .spawn()
        .context("Failed to launch sub-agent process")?;

    // Drain stderr in a background thread, forwarding each line as a progress event.
    let stderr = child.stderr.take();
    let stderr_thread = std::thread::spawn(move || {
        if let Some(stderr) = stderr {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().flatten() {
                let _ = event_tx.send(AppEvent::SubAgentProgress(line));
            }
        }
    });

    // Wait for the process and collect stdout (stderr was already taken above).
    let output = child
        .wait_with_output()
        .context("Failed to wait for sub-agent process")?;

    // Ensure the stderr thread has finished before we return.
    let _ = stderr_thread.join();

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        bail!(
            "exit code {:?}: sub-agent process failed",
            output.status.code()
        )
    }
}

fn sanitize_progress_line(line: &str) -> String {
    let mut cleaned = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('\u{7}') => break,
                            Some('\u{1b}') => {
                                if chars.next_if_eq(&'\\').is_some() {
                                    break;
                                }
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        if ch.is_control() && ch != '\t' {
            continue;
        }

        cleaned.push(ch);
    }

    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{Command, common_ancestor, sanitize_progress_line};
    use std::path::Path;

    #[test]
    fn parses_new_slash_commands() {
        assert_eq!(
            super::ChatEngine::parse_command("/add-dir ./src"),
            Some(Command::AddDir(Some("./src".to_string())))
        );
        assert_eq!(
            super::ChatEngine::parse_command("/agent summarize this"),
            Some(Command::Agent(Some("summarize this".to_string())))
        );
        assert_eq!(
            super::ChatEngine::parse_command("/clear"),
            Some(Command::Clear)
        );
        assert_eq!(
            super::ChatEngine::parse_command("/new"),
            Some(Command::Clear)
        );
        assert_eq!(
            super::ChatEngine::parse_command("/compact"),
            Some(Command::Compact)
        );
        assert_eq!(
            super::ChatEngine::parse_command("/context"),
            Some(Command::Context)
        );
        assert_eq!(
            super::ChatEngine::parse_command("/copy"),
            Some(Command::Copy)
        );
        assert_eq!(
            super::ChatEngine::parse_command("/cwd"),
            Some(Command::Cwd(None))
        );
        assert_eq!(
            super::ChatEngine::parse_command("/cd ../workspace"),
            Some(Command::Cwd(Some("../workspace".to_string())))
        );
    }

    #[test]
    fn computes_common_ancestor_for_paths() {
        let left = Path::new("/home/user/project/src");
        let right = Path::new("/home/user/project/docs");
        let ancestor = common_ancestor(left, right).unwrap();
        assert_eq!(ancestor, Path::new("/home/user/project"));
    }

    #[test]
    fn strips_ansi_and_control_sequences_from_progress_lines() {
        let line = "\u{1b}[2K\r\u{1b}[0;32mResolving dependencies...\u{1b}[0m";
        assert_eq!(sanitize_progress_line(line), "Resolving dependencies...");
    }

    #[test]
    fn keeps_tabs_in_progress_lines() {
        assert_eq!(
            sanitize_progress_line("Step\t1:\tPreparing"),
            "Step\t1:\tPreparing"
        );
    }

    #[test]
    fn strips_osc_sequences_from_progress_lines() {
        let line = "\u{1b}]0;chatty\u{7}Installing tools";
        assert_eq!(sanitize_progress_line(line), "Installing tools");
    }

    #[test]
    fn strips_standalone_escape_characters() {
        let line = "\u{1b}Resolving dependencies";
        assert_eq!(sanitize_progress_line(line), "Resolving dependencies");
    }
}
