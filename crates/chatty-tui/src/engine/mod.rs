use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use chatty_core::factories::agent_factory::AgentBuildContext;
use chatty_core::models::Conversation;
use chatty_core::models::TurnOutcome;
use chatty_core::models::clarification_store::{ClarificationAnswer, ClarifyingQuestion};
use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use chatty_core::services::github_pr_service::{PullRequestSummary, resolve_pull_request};
use chatty_core::services::{McpService, MemoryService, StreamSurface};
use chatty_core::session::{
    AgentSession, AgentSessionConfig, HostedSession, TurnInput, TurnKind, turn_transport,
};
use chatty_core::settings::models::a2a_store::A2aAgentConfig;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::module_settings::ModuleSettingsModel;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::settings::models::{ExecutionSettingsModel, ModelsModel};
use chatty_core::tools::LocalModuleAgentSummary;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::events::AppEvent;

#[cfg(test)]
mod characterization;
mod commands;
pub mod helpers;
mod transcript;

pub use commands::Command;
pub(crate) use helpers::sanitize_progress_line;
pub use transcript::Transcript;

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

/// The TUI's presentation state over one conversation. The conversation
/// itself, the approval stores and the turn live in `session` (AGE-195);
/// this type keeps what a terminal renders and translates the session's
/// events into display updates. Used by the TUI and by the headless runner.
pub struct ChatEngine {
    pub session: AgentSession,
    /// Set when this conversation's turns run on a `chatty-server` (AGE-298).
    /// The session above stays either way: it owns the local conversation,
    /// applies every event to it and finishes the turn, so a hosted
    /// conversation keeps a local row to bring back.
    pub hosted: Option<HostedSession>,
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
    pub user_secrets: Vec<(String, String)>,
    /// Configured remote A2A agents available for `invoke_agent` and `/agent`.
    pub remote_agents: Vec<A2aAgentConfig>,
    pub module_agents: Vec<LocalModuleAgentSummary>,
    /// When `true`, this engine is running as a sub-agent and must not expose
    /// the sub_agent tool (preventing recursive sub-agent spawning).
    pub is_sub_agent: bool,

    // Display state
    pub transcript: Transcript,
    pub is_streaming: bool,
    pub pending_approval: Option<PendingApproval>,
    pub pending_clarification: Option<PendingClarification>,
    /// Set when `finalize_turn` rolled back the pending user message
    /// (`TurnOutcome::DroppedAndRolledBack`); the caller restores this into
    /// the input so the user doesn't lose what they typed (AGE-243).
    pub pending_restore_text: Option<String>,
    /// An agent-protocol follow-up that arrived while a turn was already
    /// streaming. Queued rather than dropped (AGE-242 / D3) and sent once the
    /// in-flight turn ends (`StreamCompleted` / `StreamCancelled`); the first
    /// one queued wins if another arrives before it is sent.
    pending_agent_follow_up: Option<String>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cache_read_tokens: u32,
    pub total_cache_write_tokens: u32,
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
    /// Which recovery table applies to a stream-ending error (AGE-244 / D5).
    pub surface: StreamSurface,
}

impl ChatEngine {
    pub fn new(config: ChatEngineConfig, event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        let skill_service =
            chatty_core::services::SkillService::new(config.embedding_service.clone());
        let session = AgentSession::new(AgentSessionConfig {
            execution_settings: config.execution_settings.clone(),
            surface: config.surface,
            // The interactive TUI never ran the loop guard; headless keeps its
            // own over `AppEvent`s until AGE-196 decides whether it folds in.
            loop_guard: false,
        });
        Self {
            session,
            hosted: None,
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
            pending_clarification: None,
            user_secrets: config.user_secrets,
            remote_agents: config.remote_agents,
            module_agents: config.module_agents,
            is_sub_agent: config.is_sub_agent,
            transcript: Transcript::new(),
            is_streaming: false,
            pending_approval: None,
            pending_restore_text: None,
            pending_agent_follow_up: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
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

    /// Build the `AgentBuildContext` shared by `init_conversation` and
    /// `spawn_init_conversation`. `mcp_tools` is left `None`; both callers set
    /// it themselves after gathering it, since that gathering is async and,
    /// for the background path, must happen inside the spawned task rather
    /// than while still borrowing `&self` (AGE-224).
    fn build_agent_context(&self) -> AgentBuildContext {
        build_agent_context(AgentContextInputs {
            execution_settings: &self.execution_settings,
            module_settings: &self.module_settings,
            models: &self.models,
            user_secrets: &self.user_secrets,
            memory_service: &self.memory_service,
            skill_service: &self.skill_service,
            search_settings: &self.search_settings,
            embedding_service: &self.embedding_service,
            remote_agents: &self.remote_agents,
            module_agents: &self.module_agents,
            is_sub_agent: self.is_sub_agent,
        })
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

        self.session
            .create_conversation(
                id,
                "New Chat".to_string(),
                &self.model_config,
                &self.provider_config,
                ctx,
            )
            .await
            .context("Failed to create conversation")?;

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
        // Built off the session (its stores go into the tools) but outside
        // it: the task cannot hold the engine, so the conversation comes
        // back through `ConversationInitialized`.
        let mut ctx = self.session.build_context(self.build_agent_context());
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
        self.send_message_inner(message, true);
    }

    /// Inject an agent-protocol / loop-guard follow-up without pushing a user
    /// bubble — the earlier system line (`Agent protocol follow-up: …`, or
    /// the loop-guard/deadline `eprintln!` in headless) is the only visible
    /// signal (AGE-242 / D3, mirrors the desktop's `send_protocol_follow_up`).
    pub fn send_protocol_follow_up(&mut self, message: String) {
        self.send_message_inner(message, false);
    }

    fn send_message_inner(&mut self, message: String, show_in_transcript: bool) {
        let Some(input) = self.prepare_send(message, show_in_transcript) else {
            return;
        };
        let event_tx = self.event_tx.clone();
        // The one line that differs between a local and a hosted conversation:
        // who opens the stream. Everything downstream — the events, the
        // display, the finalize — is identical, because the wire is a
        // serialization of `SessionEvent` and nothing else (AGE-298).
        match turn_transport::begin_turn(
            &mut self.session,
            self.hosted.as_mut(),
            input,
            Arc::new(AtomicBool::new(false)),
            move |event| {
                let _ = event_tx.send(AppEvent::from(event));
            },
        ) {
            Ok(turn) => {
                tokio::spawn(turn);
            }
            Err(e) => {
                warn!(error = ?e, "Failed to start the turn");
                self.is_streaming = false;
            }
        }
    }

    /// The display side of a send: the user bubble, the assistant
    /// placeholder, and the `TurnInput` the session gets. `None` when the
    /// engine is not ready or a turn is already streaming.
    fn prepare_send(&mut self, message: String, show_in_transcript: bool) -> Option<TurnInput> {
        if !self.is_ready || self.is_streaming || self.session.conversation().is_none() {
            return None;
        }

        // Injected protocol follow-ups re-enter here; only a real human turn
        // resets the todo protocol state (AGE-150).
        let kind = if chatty_core::services::is_protocol_follow_up_text(&message) {
            TurnKind::ProtocolFollowUp
        } else {
            TurnKind::Human
        };

        // Reset scroll to bottom when sending
        self.pin_to_bottom();
        self.transcript.reset_sub_agent_row();

        // Add user message to display, unless this is a protocol follow-up
        // that already rendered its own system line (AGE-242 / D3).
        if show_in_transcript {
            self.transcript.push_user(message.clone());
        }

        // Start assistant placeholder
        self.transcript.start_assistant();
        self.is_streaming = true;

        // Settings are the engine's (slash commands and headless recovery
        // change them); the session reads its copy at the start of a turn.
        self.session.set_config(AgentSessionConfig {
            execution_settings: self.execution_settings.clone(),
            ..self.session.config().clone()
        });

        Some(TurnInput {
            kind,
            ..TurnInput::text(message)
        })
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
                self.session.append_streaming_text(&text);
                self.transcript.push_text(&text);
                EngineAction::Redraw
            }
            AppEvent::ToolCallStarted { id, name } => {
                self.session.note_tool_started(&id, &name);
                self.transcript.tool_started(id, name);
                EngineAction::Redraw
            }
            AppEvent::ToolCallInput { id, arguments } => {
                self.session.note_tool_input(&id, &arguments);
                self.transcript.tool_input(&id, &arguments);
                EngineAction::Redraw
            }
            AppEvent::ToolCallResult { id, result } => {
                self.session.note_tool_result(&id, &result);
                self.transcript.tool_result(&id, result);
                EngineAction::Redraw
            }
            AppEvent::ToolCallError { id, error } => {
                self.session.note_tool_error(&id, &error);
                self.transcript.tool_error(&id, error);
                EngineAction::Redraw
            }
            AppEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => {
                self.session
                    .note_approval_requested(&id, &command, is_sandboxed);
                self.pending_approval = Some(PendingApproval {
                    id,
                    command,
                    is_sandboxed,
                });
                EngineAction::Redraw
            }
            AppEvent::ApprovalResolved { id, approved } => {
                self.session.note_approval_resolved(&id, approved);
                self.pending_approval = None;
                EngineAction::Redraw
            }
            AppEvent::ClarificationRequested { id, questions } => {
                self.session.note_clarification_requested(&id, &questions);
                self.pending_clarification = Some(PendingClarification {
                    id,
                    questions,
                    current: 0,
                    answers: Vec::new(),
                    custom: None,
                });
                EngineAction::Redraw
            }
            // Per-request records are folded into `TokenUsage` by the session.
            AppEvent::ApiCallUsage(_) => EngineAction::None,
            AppEvent::TokenUsage(usage) => {
                self.total_input_tokens =
                    self.total_input_tokens.saturating_add(usage.input_tokens);
                self.total_output_tokens =
                    self.total_output_tokens.saturating_add(usage.output_tokens);
                self.total_cache_read_tokens = self
                    .total_cache_read_tokens
                    .saturating_add(usage.cache_read_tokens);
                self.total_cache_write_tokens = self
                    .total_cache_write_tokens
                    .saturating_add(usage.cache_write_tokens);
                self.session.record_turn_usage(usage.clone());
                self.last_turn_usage = Some(usage);
                EngineAction::Redraw
            }
            AppEvent::TurnMessages(messages) => {
                self.session.set_turn_messages(messages);
                EngineAction::None
            }
            AppEvent::StreamCompleted => {
                self.finalize_stream();
                self.send_pending_agent_follow_up();
                EngineAction::Redraw
            }
            AppEvent::StreamError(error) => {
                error!(error = %error, "Stream error");
                self.transcript.mark_error(&error.to_string());
                self.finalize_partial_response();
                self.reset_stream_state();
                EngineAction::Redraw
            }
            AppEvent::AgentProtocolFollowUp(prompt) => {
                self.add_system_message(format!("Agent protocol follow-up: {}", prompt));
                if !self.is_streaming {
                    self.send_protocol_follow_up(prompt);
                } else if self.pending_agent_follow_up.is_none() {
                    // Queue rather than drop it (AGE-242 / D3): sent once the
                    // in-flight turn ends.
                    self.pending_agent_follow_up = Some(prompt);
                } else {
                    warn!(
                        "Dropping a later agent protocol follow-up; an earlier one is already queued"
                    );
                }
                EngineAction::Redraw
            }
            AppEvent::StreamCancelled => {
                self.transcript.mark_cancelled();
                self.finalize_partial_response();
                self.reset_stream_state();
                self.send_pending_agent_follow_up();
                EngineAction::Redraw
            }
            AppEvent::TitleGenerated(title) => {
                if let Some(conv) = self.session.conversation_mut() {
                    conv.set_title(title.clone());
                }
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
                    self.session.set_conversation(Some(*conversation));
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
                if self.transcript.messages.is_empty() && !self.is_streaming {
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
                self.transcript.sub_agent_progress(line);
                EngineAction::Redraw
            }
            AppEvent::SubAgent(progress) => {
                self.session.note_sub_agent(&progress);
                let line = helpers::sub_agent_line(&progress);
                if matches!(
                    progress,
                    chatty_core::tools::invoke_agent_tool::InvokeAgentProgress::Finished { .. }
                ) {
                    self.transcript.sub_agent_finished(line);
                } else {
                    let line = sanitize_progress_line(&line);
                    if line.is_empty() {
                        return EngineAction::None;
                    }
                    self.transcript.sub_agent_progress(line);
                }
                EngineAction::Redraw
            }
            AppEvent::SubAgentFinished(message) => {
                self.transcript.sub_agent_finished(message);
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
        // A hosted turn is stopped by a POST, so the cancel has to be spawned;
        // the local flag is already set by the time this returns either way.
        tokio::spawn(turn_transport::cancel(&self.session, self.hosted.as_ref()));
        // A blocked `ask_user` call never reaches the stream loop's cancel-flag
        // check, so drop the pending request too. Without this, stopping does
        // nothing visible until the tool's five-minute timeout expires.
        self.pending_clarification = None;
        self.session.clarifications().cancel_all();
    }

    /// Approve a pending tool execution (checks both execution and write stores)
    pub fn approve(&mut self) {
        self.resolve_pending_approval(true);
    }

    /// Deny a pending tool execution (checks both execution and write stores)
    pub fn deny(&mut self) {
        self.resolve_pending_approval(false);
    }

    /// Both answers take the same route: to this session's stores, or over the
    /// wire to the stores of the server session that raised the request. The
    /// id space is shared between execution and write approvals on both sides.
    fn resolve_pending_approval(&mut self, approved: bool) {
        let Some(approval) = self.pending_approval.take() else {
            return;
        };
        tokio::spawn(turn_transport::resolve_approval(
            &self.session,
            self.hosted.as_ref(),
            &approval.id,
            approved,
        ));
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
            tokio::spawn(turn_transport::resolve_clarification(
                &self.session,
                self.hosted.as_ref(),
                &pending.id,
                pending.answers,
            ));
        }
    }

    /// Add a system message to the display
    pub fn add_system_message(&mut self, text: String) {
        self.transcript.add_system(text);
    }

    /// Whether the first exchange just completed and a title should be
    /// generated. Counts *exchanges* in the conversation history, not display
    /// messages: a system line (slash-command notice, protocol follow-up)
    /// inflates `self.messages.len()` without adding a real exchange, which
    /// used to defeat this check (AGE-223), and a turn with tool calls
    /// persists its tool round-trips too, so the message count is no longer
    /// two after one exchange (AGE-247).
    fn should_generate_title(&self) -> bool {
        self.title == "New Chat" && self.session.should_generate_title()
    }

    fn finalize_stream(&mut self) {
        self.transcript.finish_streaming();

        self.finalize_partial_response();
        self.reset_stream_state();

        // Generate title after the first exchange. Counted on the persisted
        // history, not on display rows: a turn with tool calls persists its
        // tool round-trips too (AGE-247).
        if self.should_generate_title()
            && let Some(conv) = self.session.conversation()
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
        // The TUI has no trace of its own and no artifact queue: the session
        // commits the streamed text as-is. A second call for the same turn
        // (cancelled, then completed) is a no-op inside the session.
        if let Some(TurnOutcome::DroppedAndRolledBack(text)) =
            self.session.finish_turn(None, vec![])
        {
            self.pending_restore_text = Some(text);
        }
    }

    /// Send a follow-up that arrived while a turn was still streaming
    /// (AGE-242 / D3), now that the turn has ended. A no-op when none is
    /// queued.
    fn send_pending_agent_follow_up(&mut self) {
        if let Some(prompt) = self.pending_agent_follow_up.take() {
            self.send_protocol_follow_up(prompt);
        }
    }

    fn reset_stream_state(&mut self) {
        self.is_streaming = false;
        self.pending_approval = None;
        // Drop the popover and unblock any `ask_user` call still waiting, so a
        // cancelled stream cannot leave a tool parked until its timeout.
        self.pending_clarification = None;
        self.session.clarifications().cancel_all();
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

/// What building an agent needs from the owner, borrowed. The interactive
/// engine and the headless runner hold the same services (AGE-196), so the
/// `AgentBuildContext` is assembled in one place from either.
pub(crate) struct AgentContextInputs<'a> {
    pub execution_settings: &'a ExecutionSettingsModel,
    pub module_settings: &'a ModuleSettingsModel,
    pub models: &'a ModelsModel,
    pub user_secrets: &'a [(String, String)],
    pub memory_service: &'a Option<MemoryService>,
    pub skill_service: &'a chatty_core::services::SkillService,
    pub search_settings:
        &'a Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: &'a Option<chatty_core::services::EmbeddingService>,
    pub remote_agents: &'a [A2aAgentConfig],
    pub module_agents: &'a [LocalModuleAgentSummary],
    pub is_sub_agent: bool,
}

/// The services part of the `AgentBuildContext` for a TUI-hosted agent. The
/// session fills in its stores (`AgentSession::build_context`); `mcp_tools`
/// is left `None`, since gathering it is async.
pub(crate) fn build_agent_context(inputs: AgentContextInputs<'_>) -> AgentBuildContext {
    let exec_settings = if any_tool_enabled(inputs.execution_settings) {
        Some(inputs.execution_settings.clone())
    } else {
        None
    };
    AgentBuildContext {
        mcp_tools: None,
        exec_settings,
        pending_approvals: None,
        pending_clarifications: None,
        pending_write_approvals: None,
        pending_artifacts: None,
        shell_session: None,
        user_secrets: inputs.user_secrets.to_vec(),
        theme_colors: None, // no theme colors in TUI
        memory_service: inputs.memory_service.clone(),
        skill_service: Some(inputs.skill_service.clone()),
        search_settings: inputs.search_settings.clone(),
        embedding_service: inputs.embedding_service.clone(),
        allow_sub_agent: !inputs.is_sub_agent,
        module_agents: inputs.module_agents.to_vec(),
        gateway_port: inputs
            .module_settings
            .enabled
            .then_some(inputs.module_settings.gateway_port),
        remote_agents: inputs.remote_agents.to_vec(),
        available_model_ids: inputs
            .models
            .models()
            .iter()
            .map(|m| m.id.clone())
            .collect(),
        conversation_id: None, // browser feature isn't enabled in the TUI
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::UserContent;

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
            "New Chat".to_string(),
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

    /// A `ChatEngine` wrapping a real (network-free) `Conversation`, for tests
    /// that exercise conversation-history side effects of engine methods.
    async fn test_engine() -> (ChatEngine, mpsc::UnboundedReceiver<AppEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
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
                surface: StreamSurface::InteractiveTui,
            },
            event_tx,
        );
        engine
            .session
            .set_conversation(Some(test_conversation().await));
        engine.is_ready = true;
        (engine, event_rx)
    }

    /// Send a message through the engine against a scripted stream, and feed
    /// every resulting event back through `handle_event`, the way the main
    /// loop does. Returns the engine's history afterwards.
    async fn send_scripted(
        engine: &mut ChatEngine,
        event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
        message: &str,
        scenario: chatty_core::services::Scenario,
    ) {
        let input = engine
            .prepare_send(message.to_string(), true)
            .expect("engine is ready and idle");
        let event_tx = engine.event_tx.clone();
        let turn = engine
            .session
            .begin_scripted_turn(input, scenario, move |event| {
                let _ = event_tx.send(AppEvent::from(event));
            })
            .expect("turn starts");
        turn.await;
        while let Ok(event) = event_rx.try_recv() {
            engine.handle_event(event);
        }
    }

    fn scenario(name: &str) -> chatty_core::services::Scenario {
        chatty_core::services::scenarios()
            .into_iter()
            .find(|s| s.name == name)
            .expect("scenario exists")
    }

    /// A completed turn is committed to history and the display closes: the
    /// user bubble, the assistant reply, streaming off, nothing to restore.
    #[tokio::test]
    async fn a_completed_turn_persists_the_reply_and_closes_the_display() {
        let (mut engine, mut event_rx) = test_engine().await;

        send_scripted(&mut engine, &mut event_rx, "hi", scenario("text_only")).await;

        assert!(!engine.is_streaming);
        assert!(engine.pending_restore_text.is_none());
        let messages = engine.session.conversation().unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages.last(),
            Some(&rig_core::completion::Message::Assistant {
                id: None,
                content: vec![rig_core::completion::message::AssistantContent::text(
                    "Hello, world"
                )],
            })
        );
        let last = engine.transcript.messages.last().expect("assistant bubble");
        assert!(matches!(last.role, MessageRole::Assistant));
        assert!(!last.is_streaming);
        assert_eq!(last.text(), "Hello, world");
    }

    /// AGE-243 / D4: a turn that ends with no text — here cancelled before
    /// the first chunk — leaves history exactly as it was before the send,
    /// and the user's text is queued for restoring into the input.
    #[tokio::test]
    async fn a_turn_with_no_text_rolls_back_the_user_message() {
        use chatty_core::services::{Scenario, ScriptedItem, StreamChunk};

        let (mut engine, mut event_rx) = test_engine().await;
        let before = engine.session.conversation().unwrap().messages();

        send_scripted(
            &mut engine,
            &mut event_rx,
            "hi",
            Scenario {
                name: "cancel_before_text",
                progress: Vec::new(),
                // A tool call would put an item in the trace, which counts
                // as content under D4; usage does not.
                items: vec![ScriptedItem::CancelThen(StreamChunk::ApiCallUsage(
                    chatty_core::models::token_usage::ApiCallUsage::default(),
                ))],
            },
        )
        .await;

        assert!(!engine.is_streaming);
        assert_eq!(engine.session.conversation().unwrap().messages(), before);
        assert_eq!(engine.pending_restore_text.as_deref(), Some("hi"));
    }

    /// AGE-222: an errored turn that produced some text still persists that
    /// text as the assistant's reply, and shows the error inline.
    #[tokio::test]
    async fn an_errored_turn_with_text_persists_the_partial_response() {
        let (mut engine, mut event_rx) = test_engine().await;

        send_scripted(
            &mut engine,
            &mut event_rx,
            "hi",
            scenario("provider_error_mid_stream"),
        )
        .await;

        let messages = engine.session.conversation().unwrap().messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages.last(),
            Some(&rig_core::completion::Message::Assistant {
                id: None,
                content: vec![rig_core::completion::message::AssistantContent::text(
                    "Partial "
                )],
            })
        );
        assert!(
            engine
                .transcript
                .messages
                .last()
                .unwrap()
                .text()
                .contains("[Error:")
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
                surface: StreamSurface::InteractiveTui,
            },
            event_tx,
        )
    }

    /// AGE-223: the turn's usage arrives folded from the session; the engine
    /// keeps it for `/context` (the last call's prompt is the real context
    /// size) and accumulates the session totals.
    #[test]
    fn token_usage_is_kept_for_context_and_added_to_the_totals() {
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

        engine.handle_event(AppEvent::TokenUsage(
            chatty_core::models::token_usage::TokenUsage::from_calls(vec![call1, call2]),
        ));

        let usage = engine.last_turn_usage.as_ref().expect("usage recorded");
        assert_eq!(usage.last_call(), Some(&call2));
        assert_eq!(usage.last_call().unwrap().prompt_tokens(), 950);
        assert_eq!(engine.total_input_tokens, 150);
        assert_eq!(engine.total_output_tokens, 30);
        assert_eq!(engine.total_cache_read_tokens, 900);
        assert_eq!(engine.total_cache_write_tokens, 900);
    }

    /// AGE-223: the running totals saturate instead of wrapping on overflow.
    #[test]
    fn token_usage_counters_saturate_instead_of_overflowing() {
        let mut engine = bare_engine();
        engine.total_input_tokens = u32::MAX;

        engine.handle_event(AppEvent::TokenUsage(
            chatty_core::models::token_usage::TokenUsage::new(10, 0),
        ));

        assert_eq!(engine.total_input_tokens, u32::MAX);
    }

    /// AGE-223: the title-generation trigger counts conversation history, not
    /// display messages — a system line padding `self.messages` must not
    /// defeat it.
    #[tokio::test]
    async fn should_generate_title_counts_conversation_history_not_display_messages() {
        let (mut engine, _event_rx) = test_engine().await;
        engine.add_system_message("Agent protocol follow-up: ...".to_string());
        engine.transcript.push_user("hi".into());
        engine.transcript.messages.push(DisplayMessage::with_text(
            MessageRole::Assistant,
            "hello".into(),
        ));
        assert_eq!(engine.transcript.messages.len(), 3);

        let conv = engine.session.conversation_mut().unwrap();
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
        let (mut engine, _event_rx) = test_engine().await;
        let conv = engine.session.conversation_mut().unwrap();
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
        let (mut engine, _event_rx) = test_engine().await;
        engine.title = "Custom Title".to_string();
        let conv = engine.session.conversation_mut().unwrap();
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
