use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use chatty_core::factories::agent_factory::AgentBuildContext;
use chatty_core::models::ClarificationStore;
use chatty_core::models::Conversation;
use chatty_core::models::TurnOutcome;
use chatty_core::models::clarification_store::{
    ClarificationAnswer, ClarificationNotification, ClarifyingQuestion,
};
use chatty_core::models::execution_approval_store::{
    ApprovalDecision, ApprovalNotification, ApprovalResolution, ExecutionApprovalStore,
};
use chatty_core::models::message_types::{
    ExecutionEngine, ToolSource, classify_initial_execution_engine, classify_tool_source,
    detect_execution_engine, predict_execution_engine,
};
use chatty_core::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
use chatty_core::services::github_pr_service::{PullRequestSummary, resolve_pull_request};
use chatty_core::services::{ContextShaperSettings, McpService, MemoryService, shape_context};
use chatty_core::settings::models::a2a_store::A2aAgentConfig;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::module_settings::ModuleSettingsModel;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::settings::models::{ExecutionSettingsModel, ModelsModel};
use chatty_core::tools::LocalModuleAgentSummary;

use rig_core::message::UserContent;
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
    pub source: ToolSource,
    pub execution_engine: Option<ExecutionEngine>,
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

/// Clarifying questions the agent is blocked on, answered one at a time.
#[derive(Debug, Clone)]
pub struct PendingClarification {
    pub id: String,
    pub questions: Vec<ClarifyingQuestion>,
    /// Which question the user is answering now.
    pub current: usize,
    /// Answers gathered so far, in question order.
    pub answers: Vec<ClarificationAnswer>,
    /// Buffer for a typed answer; `None` unless the user chose to type one.
    pub custom: Option<String>,
}

impl PendingClarification {
    pub fn current_question(&self) -> Option<&ClarifyingQuestion> {
        self.questions.get(self.current)
    }
}

/// A message for display in the TUI
#[derive(Debug, Clone)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// A single block within an assistant/user/system message. Blocks appear in
/// the order they were produced — text arrives, then a tool call fires, then
/// more text, etc. — so the UI can render the timeline accurately.
#[derive(Debug, Clone)]
pub enum MessageBlock {
    Text(String),
    ToolCall(ToolCallInfo),
}

#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: MessageRole,
    pub blocks: Vec<MessageBlock>,
    pub is_streaming: bool,
}

impl DisplayMessage {
    pub fn new(role: MessageRole, is_streaming: bool) -> Self {
        Self {
            role,
            blocks: Vec::new(),
            is_streaming,
        }
    }

    pub fn with_text(role: MessageRole, text: String) -> Self {
        let mut msg = Self::new(role, false);
        if !text.is_empty() {
            msg.blocks.push(MessageBlock::Text(text));
        }
        msg
    }

    /// Concatenated text across all Text blocks (tool calls excluded).
    /// Used by `/copy` and headless stdout output.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            if let MessageBlock::Text(t) = block {
                out.push_str(t);
            }
        }
        out
    }

    /// Append to the trailing Text block, or create a new one if the last block
    /// is a tool call (so the tool's output stays above the subsequent text).
    pub fn push_text(&mut self, text: &str) {
        if let Some(MessageBlock::Text(last)) = self.blocks.last_mut() {
            last.push_str(text);
        } else {
            self.blocks.push(MessageBlock::Text(text.to_string()));
        }
    }

    pub fn push_tool_call(&mut self, info: ToolCallInfo) {
        self.blocks.push(MessageBlock::ToolCall(info));
    }

    pub fn tool_call_mut(&mut self, id: &str) -> Option<&mut ToolCallInfo> {
        self.blocks.iter_mut().find_map(|b| match b {
            MessageBlock::ToolCall(tc) if tc.id == id => Some(tc),
            _ => None,
        })
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCallInfo> {
        self.blocks.iter().filter_map(|b| match b {
            MessageBlock::ToolCall(tc) => Some(tc),
            _ => None,
        })
    }
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
    pub module_settings: ModuleSettingsModel,
    pub models: ModelsModel,
    pub providers: Vec<ProviderConfig>,
    pub mcp_service: Option<McpService>,
    pub memory_service: Option<MemoryService>,
    pub search_settings:
        Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<chatty_core::services::EmbeddingService>,
    pub skill_service: chatty_core::services::SkillService,
    pub execution_approval_store: ExecutionApprovalStore,
    pub clarification_store: ClarificationStore,
    pub write_approval_store: WriteApprovalStore,
    pub user_secrets: Vec<(String, String)>,
    /// Configured remote A2A agents available for `invoke_agent` and `/agent`.
    pub remote_agents: Vec<A2aAgentConfig>,
    pub module_agents: Vec<LocalModuleAgentSummary>,
    /// When `true`, this engine is running as a sub-agent and must not expose
    /// the sub_agent tool (preventing recursive sub-agent spawning).
    pub is_sub_agent: bool,

    // Display state
    pub messages: Vec<DisplayMessage>,
    pub is_streaming: bool,
    pub cancel_flag: Option<Arc<AtomicBool>>,
    pub pending_approval: Option<PendingApproval>,
    pub pending_clarification: Option<PendingClarification>,
    /// Set when `finalize_turn` rolled back the pending user message
    /// (`TurnOutcome::DroppedAndRolledBack`); the caller restores this into
    /// the input so the user doesn't lose what they typed (AGE-243).
    pub pending_restore_text: Option<String>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cache_read_tokens: u32,
    pub total_cache_write_tokens: u32,
    /// Per-request usage records for the turn currently streaming, folded
    /// into `last_turn_usage` once the turn's aggregate arrives.
    current_turn_calls: Vec<chatty_core::models::token_usage::ApiCallUsage>,
    /// The most recently completed turn's per-request usage. Its last call's
    /// prompt size is the model's actual current context fill — summing every
    /// request in the turn over-states it by the tool-call count (AGE-223).
    pub last_turn_usage: Option<chatty_core::models::token_usage::TokenUsage>,
    pub title: String,
    pub is_ready: bool,
    /// Whether deferred background services (MCP, memory, embedding, etc.) have
    /// finished loading. `false` during the brief window after the TUI appears but
    /// before `ServicesReady` is received.
    pub services_loaded: bool,
    pub git_branch: Option<String>,
    /// GitHub pull request for the workspace branch, when one exists.
    pub pull_request: Option<PullRequestSummary>,
    pub model_picker: Option<ModelPicker>,
    pub tool_picker: Option<ToolPicker>,
    /// Lines scrolled up from the bottom of the chat transcript.
    /// `0` + `pinned_to_bottom` means the viewport follows new content.
    pub scroll_offset: u16,
    /// When true, incoming content keeps the view pinned to the bottom.
    /// Flipped to `false` whenever the user scrolls up.
    pub pinned_to_bottom: bool,
    /// Total wrapped-line count recorded on the last render. Used to preserve
    /// the user's visible window when new lines are appended while unpinned.
    pub last_content_height: u16,
    /// Bounding rectangle of the chat transcript as of the last render.
    /// Used to route mouse wheel events only when the pointer is over the chat area.
    pub last_chat_area: ratatui::layout::Rect,
    /// Index into `messages` of the system message showing sub-agent progress.
    /// `None` when no sub-agent is running.
    pub sub_agent_msg_idx: Option<usize>,
    /// Tracks `invoke_agent` / `sub_agent` tool call IDs to suppress their
    /// ToolCallBlock rendering (progress goes through the sub-agent channel).
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
    pub module_settings: ModuleSettingsModel,
    pub models: ModelsModel,
    pub providers: Vec<ProviderConfig>,
    pub mcp_service: Option<McpService>,
    pub memory_service: Option<MemoryService>,
    pub search_settings:
        Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<chatty_core::services::EmbeddingService>,
    pub user_secrets: Vec<(String, String)>,
    pub remote_agents: Vec<A2aAgentConfig>,
    pub module_agents: Vec<LocalModuleAgentSummary>,
    pub is_sub_agent: bool,
    /// Set to `true` when all services were loaded eagerly (headless mode).
    /// Set to `false` when services are deferred to background (interactive mode).
    pub services_loaded: bool,
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
            module_settings: config.module_settings,
            models: config.models,
            providers: config.providers,
            mcp_service: config.mcp_service,
            memory_service: config.memory_service,
            search_settings: config.search_settings,
            embedding_service: config.embedding_service,
            skill_service,
            execution_approval_store: ExecutionApprovalStore::new(),
            clarification_store: ClarificationStore::new(),
            pending_clarification: None,
            write_approval_store: WriteApprovalStore::new(),
            user_secrets: config.user_secrets,
            remote_agents: config.remote_agents,
            module_agents: config.module_agents,
            is_sub_agent: config.is_sub_agent,
            messages: Vec::new(),
            is_streaming: false,
            cancel_flag: None,
            pending_approval: None,
            pending_restore_text: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            current_turn_calls: Vec::new(),
            last_turn_usage: None,
            title: "New Chat".to_string(),
            is_ready: false,
            services_loaded: config.services_loaded,
            git_branch: None,
            pull_request: None,
            model_picker: None,
            tool_picker: None,
            scroll_offset: 0,
            pinned_to_bottom: true,
            last_content_height: 0,
            last_chat_area: ratatui::layout::Rect::default(),
            sub_agent_msg_idx: None,
            active_invoke_agent_ids: HashSet::new(),
            event_tx,
            init_generation: 0,
        }
    }

    pub fn refresh_workspace_context(&mut self) {
        let workspace_dir = self.execution_settings.workspace_dir.clone();
        let event_tx = self.event_tx.clone();
        tokio::task::spawn_blocking({
            let workspace_dir = workspace_dir.clone();
            let event_tx = event_tx.clone();
            move || {
                let branch = detect_git_branch(workspace_dir.as_deref());
                let _ = event_tx.send(AppEvent::GitBranchDetected(branch));
            }
        });

        // The pull-request lookup shells out to `gh` / the GitHub API, so it
        // runs on its own and reports separately; the branch must never wait
        // on it.
        if !self.execution_settings.git_enabled {
            let _ = event_tx.send(AppEvent::PullRequestDetected(None));
            return;
        }
        tokio::spawn(async move {
            let Some(workspace) = workspace_dir
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
            else {
                return;
            };
            let summary = resolve_pull_request(&workspace).await;
            let _ = event_tx.send(AppEvent::PullRequestDetected(summary.map(Box::new)));
        });
    }

    /// Pin the chat viewport to the bottom so new content is auto-followed.
    pub fn pin_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.pinned_to_bottom = true;
    }

    /// Scroll the chat up by `lines`. Unpins auto-follow so the view stays put
    /// when new streaming content arrives.
    pub fn scroll_up(&mut self, lines: u16) {
        if lines == 0 {
            return;
        }
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
        self.pinned_to_bottom = false;
    }

    /// Scroll the chat down by `lines`. Re-pins to the bottom once we land at 0.
    pub fn scroll_down(&mut self, lines: u16) {
        if lines == 0 {
            return;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.pinned_to_bottom = true;
        }
    }

    fn available_model_ids(&self) -> Vec<String> {
        self.models.models().iter().map(|m| m.id.clone()).collect()
    }

    /// Whether any execution-related setting is enabled — gates whether the
    /// built agent gets `exec_settings` (and thus execution tools) at all.
    fn any_tool_enabled(es: &ExecutionSettingsModel) -> bool {
        es.enabled
            || es.filesystem_read_enabled
            || es.filesystem_write_enabled
            || es.fetch_enabled
            || es.git_enabled
            || es.execute_code_enabled
    }

    /// Build the `AgentBuildContext` shared by `init_conversation` and
    /// `spawn_init_conversation`. `mcp_tools` is left `None`; both callers set
    /// it themselves after gathering it, since that gathering is async and,
    /// for the background path, must happen inside the spawned task rather
    /// than while still borrowing `&self` (AGE-224).
    fn build_agent_context(&self) -> AgentBuildContext {
        let exec_settings = if Self::any_tool_enabled(&self.execution_settings) {
            Some(self.execution_settings.clone())
        } else {
            None
        };
        AgentBuildContext {
            mcp_tools: None,
            exec_settings,
            pending_approvals: Some(self.execution_approval_store.get_pending_approvals()),
            pending_clarifications: Some(self.clarification_store.get_pending_clarifications()),
            pending_write_approvals: Some(self.write_approval_store.get_pending_approvals()),
            pending_artifacts: None,
            shell_session: None,
            user_secrets: self.user_secrets.clone(),
            theme_colors: None, // no theme colors in TUI
            memory_service: self.memory_service.clone(),
            skill_service: Some(self.skill_service.clone()),
            search_settings: self.search_settings.clone(),
            embedding_service: self.embedding_service.clone(),
            allow_sub_agent: !self.is_sub_agent,
            module_agents: self.module_agents.clone(),
            gateway_port: self
                .module_settings
                .enabled
                .then_some(self.module_settings.gateway_port),
            remote_agents: self.remote_agents.clone(),
            available_model_ids: self.available_model_ids(),
            conversation_id: None, // browser feature isn't enabled in the TUI
        }
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

        let mut ctx = self.build_agent_context();
        ctx.mcp_tools = mcp_tools;

        let conversation = Conversation::new(
            id,
            "New Chat".to_string(),
            &self.model_config,
            &self.provider_config,
            ctx,
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
        let mut ctx = self.build_agent_context();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            // Gather MCP tools
            ctx.mcp_tools = match mcp_service {
                Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
                None => None,
            };

            let result = Conversation::new(
                id,
                "New Chat".to_string(),
                &model_config,
                &provider_config,
                ctx,
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

        // Injected protocol follow-ups re-enter here; only a real human turn
        // resets the todo protocol state (AGE-150).
        let reset_agent_task = !chatty_core::services::is_protocol_follow_up_text(&message);

        // Reset scroll to bottom when sending
        self.pin_to_bottom();
        self.sub_agent_msg_idx = None;

        let conversation = match self.conversation.as_mut() {
            Some(c) => c,
            None => return,
        };

        // Add user message to display
        self.messages.push(DisplayMessage::with_text(
            MessageRole::User,
            message.clone(),
        ));

        let (raw_history, contents) = prepare_user_turn(conversation, message);

        // Start assistant placeholder
        self.messages
            .push(DisplayMessage::new(MessageRole::Assistant, true));
        self.is_streaming = true;

        // Set up approval channels
        let (approval_tx, approval_rx) = mpsc::unbounded_channel::<ApprovalNotification>();
        let (resolution_tx, resolution_rx) = mpsc::unbounded_channel::<ApprovalResolution>();
        chatty_core::models::execution_approval_store::set_global_approval_notifier(
            approval_tx.clone(),
        );
        self.execution_approval_store
            .set_notifiers(approval_tx, resolution_tx);

        let (clarification_tx, clarification_rx) =
            mpsc::unbounded_channel::<ClarificationNotification>();
        chatty_core::models::clarification_store::set_global_clarification_notifier(
            clarification_tx,
        );

        // Spawn stream task
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Some(cancel_flag.clone());

        let agent = conversation.agent().clone();
        let invoke_agent_progress_slot = conversation.invoke_agent_progress_slot();
        let event_tx = self.event_tx.clone();
        let max_agent_turns = self.execution_settings.max_agent_turns as usize;

        tokio::spawn(async move {
            // Apply context shaping before every LLM call (stages 1-3 are free;
            // stages 4-5 need an LLM call so we pass None here to cap at stage 3).
            let shaper_settings = ContextShaperSettings::default();
            let shaped = shape_context(raw_history, &shaper_settings, None).await;
            if let Some(stage) = shaped.stage_applied {
                tracing::debug!(
                    stage = ?stage,
                    chars_freed = shaped.chars_freed,
                    "context shaper applied before stream"
                );
            }
            let history = shaped.messages;

            let result = streaming::run_stream(streaming::StreamParams {
                agent,
                history,
                contents,
                cancel_flag,
                event_tx: event_tx.clone(),
                approval_rx,
                clarification_rx,
                resolution_rx,
                max_agent_turns,
                invoke_agent_progress_slot,
                reset_agent_task,
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
                self.pin_to_bottom();
                EngineAction::Redraw
            }
            AppEvent::TextChunk(text) => {
                // Append to conversation streaming state
                if let Some(conv) = self.conversation.as_mut() {
                    conv.append_streaming_content(&text);
                }
                if let Some(msg) = self.streaming_assistant_mut() {
                    msg.push_text(&text);
                } else if self.sub_agent_msg_idx.is_some() {
                    self.messages
                        .push(DisplayMessage::new(MessageRole::Assistant, true));
                    if let Some(msg) = self.messages.last_mut() {
                        msg.push_text(&text);
                    }
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallStarted { id, name } => {
                if name == "invoke_agent" || name == "sub_agent" {
                    self.active_invoke_agent_ids.insert(id);
                } else {
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
                    if let Some(last) = self.streaming_assistant_mut() {
                        last.push_tool_call(info);
                    } else if self.sub_agent_msg_idx.is_some() {
                        self.messages
                            .push(DisplayMessage::new(MessageRole::Assistant, true));
                        if let Some(last) = self.messages.last_mut() {
                            last.push_tool_call(info);
                        }
                    }
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallInput { id, arguments } => {
                if !self.active_invoke_agent_ids.contains(&id)
                    && let Some(last) = self.streaming_assistant_mut()
                    && let Some(tc) = last.tool_call_mut(&id)
                {
                    tc.execution_engine =
                        predict_execution_engine(&tc.name, &arguments).or(tc.execution_engine);
                    tc.input.push_str(&arguments);
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallResult { id, result } => {
                if self.active_invoke_agent_ids.remove(&id) {
                    // invoke_agent / sub_agent result — sub-agent progress already handled
                } else if let Some(last) = self.streaming_assistant_mut()
                    && let Some(tc) = last.tool_call_mut(&id)
                {
                    tc.execution_engine = detect_execution_engine(&tc.name, &result);
                    tc.output = Some(result);
                    tc.state = ToolCallState::Success;
                }
                EngineAction::Redraw
            }
            AppEvent::ToolCallError { id, error } => {
                if self.active_invoke_agent_ids.remove(&id) {
                    // invoke_agent / sub_agent error — sub-agent progress already handled
                } else if let Some(last) = self.streaming_assistant_mut()
                    && let Some(tc) = last.tool_call_mut(&id)
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
            AppEvent::ClarificationRequested { id, questions } => {
                self.pending_clarification = Some(PendingClarification {
                    id,
                    questions,
                    current: 0,
                    answers: Vec::new(),
                    custom: None,
                });
                EngineAction::Redraw
            }
            AppEvent::ApiCallUsage(call) => {
                self.current_turn_calls.push(call);
                EngineAction::None
            }
            AppEvent::TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                self.total_input_tokens = self.total_input_tokens.saturating_add(input_tokens);
                self.total_output_tokens = self.total_output_tokens.saturating_add(output_tokens);
                self.total_cache_read_tokens = self
                    .total_cache_read_tokens
                    .saturating_add(cache_read_tokens);
                self.total_cache_write_tokens = self
                    .total_cache_write_tokens
                    .saturating_add(cache_write_tokens);
                // The per-request records are the source of truth; the
                // provider's aggregate only stands in when none arrived
                // (mirrors StreamManager on the desktop).
                self.last_turn_usage = Some(if self.current_turn_calls.is_empty() {
                    let mut usage = chatty_core::models::token_usage::TokenUsage::new(
                        input_tokens,
                        output_tokens,
                    );
                    usage.cache_read_tokens = cache_read_tokens;
                    usage.cache_write_tokens = cache_write_tokens;
                    usage
                } else {
                    chatty_core::models::token_usage::TokenUsage::from_calls(std::mem::take(
                        &mut self.current_turn_calls,
                    ))
                });
                EngineAction::Redraw
            }
            AppEvent::StreamCompleted => {
                self.finalize_stream();
                EngineAction::Redraw
            }
            AppEvent::StreamError(error) => {
                error!(error = %error, "Stream error");
                if let Some(last) = self.streaming_assistant_mut() {
                    let prefix = if last.text().is_empty() { "" } else { "\n\n" };
                    last.push_text(&format!("{}[Error: {}]", prefix, error));
                    last.is_streaming = false;
                }
                self.finalize_partial_response();
                self.reset_stream_state();
                EngineAction::Redraw
            }
            AppEvent::AgentProtocolFollowUp(prompt) => {
                self.add_system_message(format!("Agent protocol follow-up: {}", prompt));
                if !self.is_streaming {
                    self.send_message(prompt);
                }
                EngineAction::Redraw
            }
            AppEvent::StreamCancelled => {
                if let Some(last) = self.streaming_assistant_mut() {
                    last.push_text("\n\n[Cancelled]");
                    last.is_streaming = false;
                }
                self.finalize_partial_response();
                self.reset_stream_state();
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
            AppEvent::ServicesReady(services) => {
                info!("Deferred services loaded, patching engine state");
                self.user_secrets = services.user_secrets;
                self.mcp_service = services.mcp_service;
                self.memory_service = services.memory_service;
                self.search_settings = services.search_settings;
                self.skill_service =
                    chatty_core::services::SkillService::new(services.embedding_service.clone());
                self.embedding_service = services.embedding_service;
                self.services_loaded = true;
                // Re-initialize conversation only if the user hasn't sent any messages yet.
                // This gives the agent access to MCP tools, memory, etc. without losing context.
                if self.messages.is_empty() && !self.is_streaming {
                    self.spawn_init_conversation();
                }
                EngineAction::Redraw
            }
            AppEvent::GitBranchDetected(branch) => {
                self.git_branch = branch;
                EngineAction::Redraw
            }
            AppEvent::PullRequestDetected(pull_request) => {
                self.pull_request = pull_request.map(|pr| *pr);
                EngineAction::Redraw
            }
            AppEvent::SubAgentProgress(line) => {
                let line = sanitize_progress_line(&line);
                if line.is_empty() {
                    return EngineAction::None;
                }
                if self.sub_agent_msg_idx.is_none() {
                    self.seal_parent_before_sub_agent_progress();
                    self.add_system_message(line);
                    self.sub_agent_msg_idx = Some(self.messages.len() - 1);
                } else if let Some(idx) = self.sub_agent_msg_idx
                    && let Some(msg) = self.messages.get_mut(idx)
                {
                    msg.push_text("\n");
                    msg.push_text(&line);
                }
                EngineAction::Redraw
            }
            AppEvent::SubAgentFinished(message) => {
                if let Some(idx) = self.sub_agent_msg_idx
                    && let Some(msg) = self.messages.get_mut(idx)
                {
                    msg.push_text("\n");
                    msg.push_text(&message);
                } else {
                    self.add_system_message(message);
                    self.sub_agent_msg_idx = Some(self.messages.len() - 1);
                }
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
        // A blocked `ask_user` call never reaches the stream loop's cancel-flag
        // check, so drop the pending request too. Without this, stopping does
        // nothing visible until the tool's five-minute timeout expires.
        self.pending_clarification = None;
        self.clarification_store.cancel_all();
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

    /// Record the user's pick for the current clarifying question and move on.
    pub fn answer_clarification_option(&mut self, option_ix: usize) {
        let Some(pending) = self.pending_clarification.as_mut() else {
            return;
        };
        let Some(question) = pending.questions.get(pending.current) else {
            return;
        };
        let Some(option) = question.options.get(option_ix) else {
            return;
        };

        pending.answers.push(ClarificationAnswer {
            id: question.id.clone(),
            answer: option.clone(),
            custom: false,
        });
        pending.custom = None;
        pending.current += 1;
        self.finish_clarification_if_complete();
    }

    /// Start typing a free-text answer to the current question.
    pub fn start_clarification_custom(&mut self) {
        if let Some(pending) = self.pending_clarification.as_mut() {
            pending.custom = Some(String::new());
        }
    }

    /// Abandon the free-text answer and go back to the options.
    pub fn cancel_clarification_custom(&mut self) {
        if let Some(pending) = self.pending_clarification.as_mut() {
            pending.custom = None;
        }
    }

    pub fn push_clarification_char(&mut self, c: char) {
        if let Some(pending) = self.pending_clarification.as_mut()
            && let Some(buf) = pending.custom.as_mut()
        {
            buf.push(c);
        }
    }

    pub fn pop_clarification_char(&mut self) {
        if let Some(pending) = self.pending_clarification.as_mut()
            && let Some(buf) = pending.custom.as_mut()
        {
            buf.pop();
        }
    }

    /// Commit the typed answer for the current question and move on.
    pub fn commit_clarification_custom(&mut self) {
        let Some(pending) = self.pending_clarification.as_mut() else {
            return;
        };
        let typed = pending
            .custom
            .as_ref()
            .map(|b| b.trim().to_string())
            .unwrap_or_default();
        if typed.is_empty() {
            return;
        }
        let Some(question) = pending.questions.get(pending.current) else {
            return;
        };

        pending.answers.push(ClarificationAnswer {
            id: question.id.clone(),
            answer: typed,
            custom: true,
        });
        pending.custom = None;
        pending.current += 1;
        self.finish_clarification_if_complete();
    }

    /// Send the answers back once every question has one.
    fn finish_clarification_if_complete(&mut self) {
        let done = self
            .pending_clarification
            .as_ref()
            .is_some_and(|p| p.current >= p.questions.len());
        if !done {
            return;
        }
        if let Some(pending) = self.pending_clarification.take() {
            self.clarification_store
                .resolve(&pending.id, pending.answers);
        }
    }

    /// Add a system message to the display
    pub fn add_system_message(&mut self, text: String) {
        self.messages
            .push(DisplayMessage::with_text(MessageRole::System, text));
    }

    fn streaming_assistant_index(&self) -> Option<usize> {
        streaming_assistant_index(&self.messages, self.sub_agent_msg_idx)
    }

    fn streaming_assistant_mut(&mut self) -> Option<&mut DisplayMessage> {
        let idx = self.streaming_assistant_index()?;
        self.messages.get_mut(idx)
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

    /// Whether the first exchange just completed and a title should be
    /// generated. Counts conversation history, not display messages: a
    /// system line (slash-command notice, protocol follow-up) inflates
    /// `self.messages.len()` without adding a real exchange, which used to
    /// defeat this check (AGE-223).
    fn should_generate_title(&self) -> bool {
        self.title == "New Chat"
            && self
                .conversation
                .as_ref()
                .is_some_and(|conv| conv.message_count() == 2)
    }

    fn finalize_stream(&mut self) {
        // Mark display message as done
        if let Some(last) = self.streaming_assistant_mut() {
            last.is_streaming = false;
        }

        self.finalize_partial_response();
        self.reset_stream_state();

        // Generate title after first exchange.
        if self.should_generate_title()
            && let Some(conv) = &self.conversation
        {
            let event_tx = self.event_tx.clone();
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

    /// Commit the streamed response to conversation history using the one
    /// shared empty-turn rule (AGE-243 / D4): persist when there is response
    /// text, otherwise the turn is empty — committing it would write an empty
    /// assistant message that goes back to the provider on the next request
    /// (AGE-222/AGE-151). The pending user message that triggered it is
    /// rolled back so history is left exactly as it was before the send, and
    /// its text is queued in `pending_restore_text` so the caller can put it
    /// back into the input.
    fn finalize_partial_response(&mut self) {
        if let Some(conv) = self.conversation.as_mut() {
            let response = conv.streaming_message().cloned().unwrap_or_default();
            if let TurnOutcome::DroppedAndRolledBack(text) =
                conv.finalize_turn(response, vec![], None)
            {
                self.pending_restore_text = Some(text);
            }
            conv.set_streaming_message(None);
        }
    }

    fn reset_stream_state(&mut self) {
        self.is_streaming = false;
        self.cancel_flag = None;
        self.pending_approval = None;
        // Drop the popover and unblock any `ask_user` call still waiting, so a
        // cancelled stream cannot leave a tool parked until its timeout.
        self.pending_clarification = None;
        self.clarification_store.cancel_all();
    }
}

pub fn detect_git_branch(workspace_dir: Option<&str>) -> Option<String> {
    let working_dir = workspace_dir
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;

    let output = ProcessCommand::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&working_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !branch.is_empty() {
        return Some(branch);
    }

    let detached_head = ProcessCommand::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&working_dir)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty());

    detached_head
        .map(|sha| format!("HEAD ({sha})"))
        .or_else(|| Some("HEAD (detached)".to_string()))
}

fn streaming_assistant_index(messages: &[DisplayMessage], after: Option<usize>) -> Option<usize> {
    messages.iter().enumerate().rev().find_map(|(i, m)| {
        if after.is_some_and(|a| i <= a) {
            return None;
        }
        (matches!(m.role, MessageRole::Assistant) && m.is_streaming).then_some(i)
    })
}

/// Snapshot the conversation history for the outgoing request, then commit the
/// new user message to the conversation.
///
/// The returned `history` is captured BEFORE the message is added: rig's
/// `stream_prompt` appends `contents` after the caller-supplied `history` with
/// no de-duplication, so sending the same text in both would carry the user's
/// message twice on every request (AGE-221).
fn prepare_user_turn(
    conversation: &mut Conversation,
    message: String,
) -> (Vec<rig_core::completion::Message>, Vec<UserContent>) {
    let user_content = UserContent::text(message);
    let contents = vec![user_content.clone()];
    let raw_history = conversation.messages();
    let user_msg = rig_core::completion::Message::User {
        content: vec![user_content],
    };
    conversation.add_user_message_with_attachments(user_msg, vec![]);
    (raw_history, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_assistant_before_progress_is_parent() {
        let messages = vec![
            DisplayMessage::new(MessageRole::Assistant, true),
            DisplayMessage::with_text(MessageRole::System, "⟳ list_directory".to_string()),
        ];
        assert_eq!(streaming_assistant_index(&messages, None), Some(0));
        assert_eq!(streaming_assistant_index(&messages, Some(1)), None);
    }

    #[test]
    fn streaming_assistant_after_progress_is_continuation() {
        let messages = vec![
            DisplayMessage::with_text(MessageRole::Assistant, "pre-tool".to_string()),
            DisplayMessage::with_text(MessageRole::System, "done".to_string()),
            DisplayMessage::new(MessageRole::Assistant, true),
        ];
        assert_eq!(streaming_assistant_index(&messages, Some(1)), Some(2));
    }

    #[test]
    fn streaming_assistant_none_when_only_system_messages() {
        let messages = vec![DisplayMessage::with_text(
            MessageRole::System,
            "done".to_string(),
        )];
        assert_eq!(streaming_assistant_index(&messages, None), None);
    }

    /// Builds a real `Conversation` with no tools enabled. Ollama client
    /// construction is purely local (no network access), so this is safe to
    /// run in unit tests.
    async fn test_conversation() -> Conversation {
        use chatty_core::settings::models::providers_store::ProviderType;

        // Agent construction unconditionally resolves the MCP repository (for
        // the always-on list_mcp tool), which panics unless the process-global
        // registry has been set up. `init_repositories()` only resolves the
        // config directory path here — it does not touch disk — and repeat
        // calls are a harmless no-op (`OnceLock::set` after the first).
        let _ = chatty_core::init_repositories();

        let model_config = ModelConfig::new(
            "m1".to_string(),
            "Test Model".to_string(),
            ProviderType::Ollama,
            "llama3.2".to_string(),
        );
        let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
        Conversation::new(
            "c1".to_string(),
            "Test".to_string(),
            &model_config,
            &provider_config,
            AgentBuildContext {
                mcp_tools: None,
                exec_settings: None,
                pending_approvals: None,
                pending_clarifications: None,
                pending_write_approvals: None,
                pending_artifacts: None,
                shell_session: None,
                user_secrets: Vec::new(),
                theme_colors: None,
                memory_service: None,
                skill_service: None,
                search_settings: None,
                embedding_service: None,
                allow_sub_agent: false,
                module_agents: Vec::new(),
                gateway_port: None,
                remote_agents: Vec::new(),
                available_model_ids: Vec::new(),
                conversation_id: None,
            },
        )
        .await
        .expect("conversation should build without network access")
    }

    #[tokio::test]
    async fn prepare_user_turn_snapshots_history_before_the_new_message() {
        let mut conversation = test_conversation().await;

        // Seed one prior exchange so the "before" history is non-trivial.
        conversation.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conversation.finalize_response("hello!".to_string(), vec![], None);
        let before_len = conversation.messages().len();
        assert_eq!(before_len, 2);

        let (history, contents) = prepare_user_turn(&mut conversation, "what's next?".to_string());

        // The history handed to run_stream must have the length the
        // conversation had BEFORE this send...
        assert_eq!(history.len(), before_len);
        // ...and must not already end with the new prompt (AGE-221: rig
        // appends `contents` after `history` with no de-duplication).
        let new_user_message = rig_core::completion::Message::User { content: contents };
        assert_ne!(history.last(), Some(&new_user_message));

        // The new message is still committed to the conversation itself.
        assert_eq!(conversation.messages().len(), before_len + 1);
        assert_eq!(conversation.messages().last(), Some(&new_user_message));
    }

    /// A `ChatEngine` wrapping a real (network-free) `Conversation`, for tests
    /// that exercise conversation-history side effects of engine methods.
    async fn test_engine() -> ChatEngine {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut engine = ChatEngine::new(
            ChatEngineConfig {
                model_config: ModelConfig::new(
                    "m1".to_string(),
                    "Test Model".to_string(),
                    chatty_core::settings::models::providers_store::ProviderType::Ollama,
                    "llama3.2".to_string(),
                ),
                provider_config: ProviderConfig::new(
                    "Ollama".to_string(),
                    chatty_core::settings::models::providers_store::ProviderType::Ollama,
                ),
                execution_settings: ExecutionSettingsModel::default(),
                module_settings: ModuleSettingsModel::default(),
                models: ModelsModel::default(),
                providers: Vec::new(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: Vec::new(),
                remote_agents: Vec::new(),
                module_agents: Vec::new(),
                is_sub_agent: false,
                services_loaded: true,
            },
            event_tx,
        );
        engine.conversation = Some(test_conversation().await);
        engine
    }

    /// AGE-222: a cancelled turn that produced no text must leave history
    /// exactly as it was before the send — the user message that triggered it
    /// is rolled back, not left dangling with no reply.
    #[tokio::test]
    async fn cancelled_turn_with_no_text_rolls_back_the_user_message() {
        let mut engine = test_engine().await;
        let conv = engine.conversation.as_mut().unwrap();
        let before_send = conv.messages();

        conv.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conv.set_streaming_message(Some(String::new()));

        engine.finalize_partial_response();

        assert_eq!(engine.conversation.unwrap().messages(), before_send);
    }

    /// AGE-243 / D4: the one shared empty-turn rule applies regardless of why
    /// the turn ended — a *completed* turn with no text and no trace is
    /// rolled back too (previously only the cancelled path did this), and the
    /// dropped user text is queued for restoring into the input.
    #[tokio::test]
    async fn completed_turn_with_no_text_rolls_back_the_user_message_too() {
        let mut engine = test_engine().await;
        let conv = engine.conversation.as_mut().unwrap();
        let before_send = conv.messages();

        conv.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conv.set_streaming_message(Some(String::new()));

        engine.finalize_partial_response();

        assert_eq!(engine.conversation.unwrap().messages(), before_send);
        assert_eq!(engine.pending_restore_text.as_deref(), Some("hi"));
    }

    /// AGE-222: an errored turn that produced some text still persists that
    /// text as the assistant's reply.
    #[tokio::test]
    async fn errored_turn_with_text_persists_the_partial_response() {
        let mut engine = test_engine().await;
        let conv = engine.conversation.as_mut().unwrap();

        conv.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conv.set_streaming_message(Some("partial answer".to_string()));

        engine.finalize_partial_response();

        let messages = engine.conversation.unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages.last(),
            Some(&rig_core::completion::Message::Assistant {
                id: None,
                content: vec![rig_core::completion::message::AssistantContent::text(
                    "partial answer"
                )],
            })
        );
    }

    /// AGE-222: a normal completion with text is unaffected by the empty-turn
    /// guard — behaviour is unchanged from before the fix.
    #[tokio::test]
    async fn normal_completion_with_text_is_unchanged() {
        let mut engine = test_engine().await;
        let conv = engine.conversation.as_mut().unwrap();

        conv.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conv.set_streaming_message(Some("full answer".to_string()));

        engine.finalize_partial_response();

        let messages = engine.conversation.unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages.last(),
            Some(&rig_core::completion::Message::Assistant {
                id: None,
                content: vec![rig_core::completion::message::AssistantContent::text(
                    "full answer"
                )],
            })
        );
    }

    /// A `ChatEngine` with no conversation, for tests that only exercise
    /// event-handling state (token counters, usage folding) with no need for
    /// a real agent.
    fn bare_engine() -> ChatEngine {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        ChatEngine::new(
            ChatEngineConfig {
                model_config: ModelConfig::new(
                    "m1".to_string(),
                    "Test Model".to_string(),
                    chatty_core::settings::models::providers_store::ProviderType::Ollama,
                    "llama3.2".to_string(),
                ),
                provider_config: ProviderConfig::new(
                    "Ollama".to_string(),
                    chatty_core::settings::models::providers_store::ProviderType::Ollama,
                ),
                execution_settings: ExecutionSettingsModel::default(),
                module_settings: ModuleSettingsModel::default(),
                models: ModelsModel::default(),
                providers: Vec::new(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: Vec::new(),
                remote_agents: Vec::new(),
                module_agents: Vec::new(),
                is_sub_agent: false,
                services_loaded: true,
            },
            event_tx,
        )
    }

    /// AGE-223: per-call usage chunks fold into `last_turn_usage` via
    /// `TokenUsage::from_calls` once the turn's aggregate arrives, mirroring
    /// `StreamManager` on the desktop.
    #[test]
    fn api_call_usage_chunks_fold_into_last_turn_usage() {
        let mut engine = bare_engine();

        let call1 = chatty_core::models::token_usage::ApiCallUsage {
            turn: 1,
            input_tokens: 100,
            cache_read_tokens: 0,
            cache_write_tokens: 900,
            output_tokens: 20,
        };
        let call2 = chatty_core::models::token_usage::ApiCallUsage {
            turn: 2,
            input_tokens: 50,
            cache_read_tokens: 900,
            cache_write_tokens: 0,
            output_tokens: 10,
        };

        engine.handle_event(AppEvent::ApiCallUsage(call1));
        engine.handle_event(AppEvent::ApiCallUsage(call2));
        engine.handle_event(AppEvent::TokenUsage {
            input_tokens: 150,
            output_tokens: 30,
            cache_read_tokens: 900,
            cache_write_tokens: 900,
        });

        let usage = engine.last_turn_usage.as_ref().expect("usage recorded");
        assert_eq!(usage.calls, vec![call1, call2]);
        // The last call's prompt, not the turn total, is the real context
        // size — used by `/context` instead of summing every request.
        assert_eq!(usage.last_call(), Some(&call2));
        assert_eq!(usage.last_call().unwrap().prompt_tokens(), 950);

        // Session totals still accumulate as before.
        assert_eq!(engine.total_input_tokens, 150);
        assert_eq!(engine.total_output_tokens, 30);
        assert_eq!(engine.total_cache_read_tokens, 900);
        assert_eq!(engine.total_cache_write_tokens, 900);
    }

    /// AGE-223: with no per-call records (e.g. a provider that doesn't stream
    /// them), `TokenUsage` falls back to the turn's reported aggregate.
    #[test]
    fn token_usage_without_prior_calls_falls_back_to_the_aggregate() {
        let mut engine = bare_engine();

        engine.handle_event(AppEvent::TokenUsage {
            input_tokens: 40,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });

        let usage = engine.last_turn_usage.as_ref().expect("usage recorded");
        assert!(usage.calls.is_empty());
        assert_eq!(usage.input_tokens, 40);
        assert_eq!(usage.output_tokens, 5);
    }

    /// AGE-223: the running totals saturate instead of wrapping on overflow.
    #[test]
    fn token_usage_counters_saturate_instead_of_overflowing() {
        let mut engine = bare_engine();
        engine.total_input_tokens = u32::MAX;

        engine.handle_event(AppEvent::TokenUsage {
            input_tokens: 10,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });

        assert_eq!(engine.total_input_tokens, u32::MAX);
    }

    /// AGE-223: the title-generation trigger counts conversation history, not
    /// display messages — a system line padding `self.messages` must not
    /// defeat it.
    #[tokio::test]
    async fn should_generate_title_counts_conversation_history_not_display_messages() {
        let mut engine = test_engine().await;
        engine.add_system_message("Agent protocol follow-up: ...".to_string());
        engine
            .messages
            .push(DisplayMessage::with_text(MessageRole::User, "hi".into()));
        engine.messages.push(DisplayMessage::with_text(
            MessageRole::Assistant,
            "hello".into(),
        ));
        assert_eq!(engine.messages.len(), 3);

        let conv = engine.conversation.as_mut().unwrap();
        conv.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conv.finalize_response("hello".to_string(), vec![], None);
        assert_eq!(conv.message_count(), 2);

        assert!(engine.should_generate_title());
    }

    /// AGE-223: once the conversation has grown past the first exchange, the
    /// trigger no longer fires.
    #[tokio::test]
    async fn should_generate_title_is_false_past_the_first_exchange() {
        let mut engine = test_engine().await;
        let conv = engine.conversation.as_mut().unwrap();
        for _ in 0..2 {
            conv.add_user_message_with_attachments(
                rig_core::completion::Message::User {
                    content: vec![UserContent::text("hi".to_string())],
                },
                vec![],
            );
            conv.finalize_response("hello".to_string(), vec![], None);
        }
        assert_eq!(conv.message_count(), 4);

        assert!(!engine.should_generate_title());
    }

    /// AGE-223: once a title has already been set, the trigger no longer fires.
    #[tokio::test]
    async fn should_generate_title_is_false_once_a_title_is_set() {
        let mut engine = test_engine().await;
        engine.title = "Custom Title".to_string();
        let conv = engine.conversation.as_mut().unwrap();
        conv.add_user_message_with_attachments(
            rig_core::completion::Message::User {
                content: vec![UserContent::text("hi".to_string())],
            },
            vec![],
        );
        conv.finalize_response("hello".to_string(), vec![], None);

        assert!(!engine.should_generate_title());
    }
}
