//! Slash command parsing and handling for the TUI chat engine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use rig_core::completion::Message;
use rig_core::completion::message::AssistantContent;
use tracing::{info, warn};

use chatty_core::models::conversation::ConversationMode;
use chatty_core::session::{
    BRING_BACK_SUMMARY, HostedSession, MoveSummary, TAKE_ONLINE_SUMMARY, fetch_hosted,
    refuse_reason, take_online,
};

use super::{ChatEngine, MessageRole, ModelPicker, ModelPickerItem, ToolPicker, ToolPickerItem};
use crate::events::AppEvent;

/// Render a move's "what travels and what does not" table for a terminal.
///
/// The strings come from chatty-core, so the TUI prompt and the desktop
/// dialog say the same thing and neither can drift from what the code does.
fn push_move_summary(out: &mut String, summary: &MoveSummary) {
    out.push_str("Moves:\n");
    for item in summary.moves {
        out.push_str(&format!("  + {item}\n"));
    }
    out.push_str("Does not move:\n");
    for (what, why) in summary.does_not_move {
        out.push_str(&format!("  - {what} ({why})\n"));
    }
}

/// Parsed slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// /model [query] — switch model or list models if query is None
    Model(Option<String>),
    /// /tools [name] — open tool picker or toggle by name
    Tools(Option<String>),
    /// /modules [show|enable|disable|dir <path>|port <1-65535>] — manage module runtime settings
    Modules(Option<String>),
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
    /// /update — trigger CLI auto-update if an installed CLI exists
    Update,
    /// /cwd, /cd [directory] — show or change working directory
    Cwd(Option<String>),
    /// /online [url|off] — show where this conversation runs, take it online,
    /// or bring it back (AGE-298)
    Online(Option<String>),
    /// /verbose — toggle between folded tool-call summaries and full payloads
    Verbose,
    /// /paste [n] — print the full text of an elided paste
    Paste(Option<String>),
    /// /quit, /exit — quit the application
    Quit,
}

impl ChatEngine {
    pub fn try_handle_command(&self, input: &str) -> Option<Command> {
        Self::parse_command(input)
    }

    pub(super) fn parse_command(input: &str) -> Option<Command> {
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
            "/modules" => Some(Command::Modules(arg)),
            "/add-dir" => Some(Command::AddDir(arg)),
            "/agent" => Some(Command::Agent(arg)),
            "/clear" | "/new" => Some(Command::Clear),
            "/compact" => Some(Command::Compact),
            "/context" => Some(Command::Context),
            "/copy" => Some(Command::Copy),
            "/update" => Some(Command::Update),
            "/cwd" | "/cd" => Some(Command::Cwd(arg)),
            "/online" => Some(Command::Online(arg)),
            "/verbose" => Some(Command::Verbose),
            "/paste" => Some(Command::Paste(arg)),
            "/quit" | "/exit" => Some(Command::Quit),
            _ => None,
        }
    }

    /// Clear all display and conversation state so a fresh conversation can be initialized.
    pub fn clear_conversation(&mut self) {
        self.transcript.clear();
        self.title = "New Chat".to_string();
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cache_read_tokens = 0;
        self.total_cache_write_tokens = 0;
        self.last_turn_usage = None;
        self.pin_to_bottom();
        self.pending_approval = None;
        self.pending_clarification = None;
        self.session.clarifications().cancel_all();
        self.model_picker = None;
        self.tool_picker = None;
        self.session.set_conversation(None);
        self.is_ready = false;
        self.add_system_message("Started a new conversation.".to_string());
    }

    /// Show current context usage and working directory.
    pub fn context_summary(&self) -> String {
        // The last request's prompt is the actual context size; summing every
        // request's input tokens over the whole session over-states it (AGE-223).
        let used_tokens = self
            .last_turn_usage
            .as_ref()
            .and_then(|usage| usage.last_call())
            .map(|call| call.prompt_tokens())
            .unwrap_or_else(|| {
                self.total_input_tokens
                    .saturating_add(self.total_output_tokens)
            });
        let workspace = self.current_working_directory();
        let cache_line = if self.total_cache_read_tokens > 0 || self.total_cache_write_tokens > 0 {
            format!(
                "\nCached: {} tokens read, {} tokens written",
                self.total_cache_read_tokens, self.total_cache_write_tokens
            )
        } else {
            String::new()
        };
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
                "Context usage: {} / {} tokens ({:.1}%) {}\nInput: {} tokens, Output: {} tokens{}\nWorking directory: {}",
                used_tokens,
                max_context_u32,
                pct,
                bar,
                self.total_input_tokens,
                self.total_output_tokens,
                cache_line,
                workspace,
            )
        } else {
            format!(
                "Context usage: {} tokens (model max context window unknown)\nInput: {} tokens, Output: {} tokens{}\nWorking directory: {}",
                used_tokens,
                self.total_input_tokens,
                self.total_output_tokens,
                cache_line,
                workspace
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
    pub fn execution_settings(&self) -> &chatty_core::settings::models::ExecutionSettingsModel {
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
        self.refresh_workspace_context();
        self.session.set_conversation(None);
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
                    super::helpers::common_ancestor(&current, &added_dir)
                        .unwrap_or(added_dir.clone())
                }
            }
            None => added_dir.clone(),
        };

        let workspace_str = new_workspace.to_string_lossy().to_string();
        self.execution_settings.workspace_dir = Some(workspace_str.clone());
        self.refresh_workspace_context();
        self.session.set_conversation(None);
        self.is_ready = false;
        self.add_system_message(format!(
            "Added directory '{}'. Workspace expanded to '{}'. Conversation context was reset.",
            added_dir.to_string_lossy(),
            workspace_str
        ));
        Ok(workspace_str)
    }

    /// `/online` with no argument: where this conversation runs, and what a
    /// move would and would not carry.
    ///
    /// The table is printed *before* anything moves, because taking a
    /// conversation online is a data egress and AGE-298 asks that the user see
    /// what leaves this machine before they agree to it. `/online <url>` is
    /// the confirmation — there is no second prompt, because typing the URL is
    /// already a deliberate act and the terminal has just shown the table.
    pub fn online_status(&self) -> String {
        let mut out = String::new();
        match self.conversation_mode() {
            ConversationMode::Local => {
                out.push_str("This conversation runs locally.\n");
                out.push_str("  /online <server-url>  take it online\n\n");
                push_move_summary(&mut out, &TAKE_ONLINE_SUMMARY);
            }
            ConversationMode::Hosted {
                server_url,
                remote_id,
            } => {
                out.push_str(&format!(
                    "This conversation runs on {server_url} (as {remote_id}).\n"
                ));
                out.push_str("  /online off  bring it back to this machine\n\n");
                push_move_summary(&mut out, &BRING_BACK_SUMMARY);
            }
        }
        out
    }

    /// Where this conversation's turns run.
    pub fn conversation_mode(&self) -> ConversationMode {
        self.session
            .conversation()
            .map(|conv| conv.mode().clone())
            .unwrap_or_default()
    }

    /// `/online <url>` — upload this conversation's history and continue it
    /// there. `/online off` brings it back.
    ///
    /// Ordering is the safety property: the history goes up first, and only a
    /// server that has accepted it and named it causes anything local to
    /// change. A client killed part-way through leaves the local conversation
    /// exactly as it was, and the hosted conversation it never learned the id
    /// of is unreachable rather than half-adopted.
    pub async fn set_online(&mut self, target: Option<String>) -> Result<()> {
        let going_online = target.is_some();
        let mode = self.conversation_mode();
        if let Some(reason) = refuse_reason(self.is_streaming, &mode, going_online) {
            self.add_system_message(reason.to_string());
            return Ok(());
        }
        let Some(conversation) = self.session.conversation() else {
            self.add_system_message("No active conversation to move.".to_string());
            return Ok(());
        };

        match target {
            Some(server_url) => {
                let new_mode =
                    take_online(&server_url, conversation.title(), &conversation.messages())
                        .await
                        .context("Failed to take the conversation online")?;
                let (url, remote_id) = new_mode
                    .hosted_on()
                    .map(|(url, id)| (url.to_string(), id.to_string()))
                    .expect("take_online returns a hosted mode");

                self.hosted = Some(HostedSession::new(&url, &remote_id));
                if let Some(conv) = self.session.conversation_mut() {
                    conv.set_mode(new_mode);
                }
                self.add_system_message(format!(
                    "This conversation now runs on {url}. Its history stays on this machine too — /online off brings it back."
                ));
            }
            None => {
                let (server_url, remote_id) = mode
                    .hosted_on()
                    .map(|(url, id)| (url.to_string(), id.to_string()))
                    .expect("refuse_reason rejected a local conversation already");
                let remote = fetch_hosted(&server_url, &remote_id)
                    .await
                    .context("Failed to read the hosted conversation back")?;

                // This client applied every event of every hosted turn it was
                // present for, so its local history is usually already
                // complete *and richer* — it has the per-message traces the
                // wire does not carry. Adopting the server's copy wholesale
                // would throw those away. So the server's history is only
                // taken when it is longer, which is exactly the case it exists
                // to cover: turns that happened while this client was closed.
                let local_len = self
                    .session
                    .conversation()
                    .map(|conv| conv.messages().len())
                    .unwrap_or(0);
                let missed = remote.messages.len().saturating_sub(local_len);
                if missed > 0 {
                    for message in remote.messages.iter().skip(local_len) {
                        self.append_missed_message(message);
                    }
                    if let Some(conv) = self.session.conversation_mut() {
                        conv.import_history(remote.messages);
                    }
                }
                if let Some(conv) = self.session.conversation_mut() {
                    conv.set_mode(ConversationMode::Local);
                }
                self.hosted = None;
                self.add_system_message(match missed {
                    0 => "This conversation runs locally again.".to_string(),
                    1 => "This conversation runs locally again, with 1 turn it ran without you."
                        .to_string(),
                    n => format!(
                        "This conversation runs locally again, with {n} messages it ran without you."
                    ),
                });
            }
        }
        Ok(())
    }

    /// Put a message the hosted run recorded while this client was away into
    /// the transcript, so bringing the conversation back shows what happened
    /// rather than jumping silently forward.
    ///
    /// Tool round-trips are skipped for the same reason every other reader
    /// skips them: they are history for the model, not lines for a person.
    fn append_missed_message(&mut self, message: &Message) {
        if chatty_core::services::is_tool_message(message) {
            return;
        }
        match message {
            Message::User { content } => {
                let text = chatty_core::services::extract_user_text(content);
                if !text.trim().is_empty() {
                    self.transcript.push_user(text);
                }
            }
            Message::Assistant { content, .. } => {
                let text = content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.trim().is_empty() {
                    self.transcript.start_assistant();
                    self.transcript.push_text(&text);
                    self.transcript.finish_streaming();
                }
            }
            // A system message is context the agent was given, not a line
            // anyone said; the transcript has never shown one.
            Message::System { .. } => {}
        }
    }

    /// Summarize older conversation history to reduce context usage.
    pub async fn compact_conversation(&mut self) -> Result<()> {
        let (agent, history) = match self.session.conversation() {
            Some(conv) => (conv.agent().clone(), conv.messages()),
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

        if let Some(conv) = self.session.conversation_mut() {
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
        let text = self
            .transcript
            .messages
            .iter()
            .rev()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
            .map(|m| m.text())
            .find(|t| !t.trim().is_empty());
        let Some(text) = text else {
            bail!("No assistant response available to copy");
        };
        super::helpers::copy_text_to_clipboard(&text)?;
        self.add_system_message("Copied latest assistant response to clipboard.".to_string());
        Ok(())
    }

    /// Trigger CLI auto-update behavior.
    pub async fn update_cli_if_installed(&mut self) {
        match do_update_cli_if_installed().await {
            Ok(Some(message)) => self.add_system_message(message),
            Ok(None) => self.add_system_message(
                "CLI auto-update is not required on this platform.".to_string(),
            ),
            Err(error) => self.add_system_message(format!("CLI update failed: {}", error)),
        }
    }

    /// Launch a sub-agent. If the first word matches a registered A2A agent,
    /// dispatches via the A2A protocol with SSE streaming. Otherwise falls back
    /// to invoking chatty-tui in headless mode.
    pub fn launch_sub_agent(&mut self, prompt: &str) -> Result<()> {
        if self.is_sub_agent {
            bail!("Sub-agents cannot spawn further sub-agents");
        }

        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("Usage: /agent <prompt>");
        }

        // Check if first word is an A2A agent name
        let (first_word, rest_of_prompt) = {
            let mut words = prompt.splitn(2, char::is_whitespace);
            let first = words.next().unwrap_or("").to_string();
            let tail = words.next().unwrap_or("").trim().to_string();
            (first, tail)
        };

        let a2a_match = if !rest_of_prompt.is_empty() {
            self.remote_agents
                .iter()
                .find(|a| a.enabled && a.name == first_word)
                .cloned()
        } else {
            None
        };

        if let Some(config) = a2a_match {
            return self.launch_a2a_agent(config, rest_of_prompt);
        }

        // Fall back to headless subprocess
        self.launch_subprocess_agent(prompt)
    }

    /// Dispatch a task to a remote A2A agent via SSE streaming.
    fn launch_a2a_agent(
        &mut self,
        config: chatty_core::settings::models::a2a_store::A2aAgentConfig,
        prompt: String,
    ) -> Result<()> {
        info!(agent = %config.name, prompt = %prompt, "Dispatching task to remote A2A agent");

        let label = format!("[remote agent: {}] {}", config.name, prompt);
        self.add_system_message(label);
        self.transcript.mark_last_as_delegation_row();

        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            // The delegation client: a remote agent's answer takes as long as
            // it takes, and only silence is a failure (AGE-319).
            let client = chatty_core::services::A2aClient::for_delegation();
            let stream_result = client.send_message_stream(&config, &prompt).await;

            let message = match stream_result {
                Ok(mut stream) => {
                    let mut response = String::new();
                    let mut success = true;

                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(
                                chatty_core::services::a2a_client::A2aStreamEvent::StatusUpdate {
                                    state,
                                    message,
                                    ..
                                },
                            ) => {
                                if state == "failed" {
                                    success = false;
                                    if let Some(msg) = message {
                                        response = format!("\u{26a0}\u{fe0f} {msg}");
                                    }
                                } else if state == "working"
                                    && let Some(ref msg) = message
                                {
                                    let _ =
                                        event_tx.send(AppEvent::DelegationProgress(msg.clone()));
                                }
                            }
                            Ok(
                                chatty_core::services::a2a_client::A2aStreamEvent::ArtifactUpdate {
                                    text,
                                    ..
                                },
                            ) => {
                                response.push_str(&text);
                            }
                            Err(e) => {
                                success = false;
                                response = format!("\u{26a0}\u{fe0f} A2A error: {e:#}");
                                break;
                            }
                        }
                    }

                    if success {
                        if response.is_empty() {
                            "Agent completed with no output.".to_string()
                        } else {
                            format!("Agent response:\n{response}")
                        }
                    } else {
                        response
                    }
                }
                Err(e) => format!("\u{26a0}\u{fe0f} A2A error: {e:#}"),
            };

            if let Err(e) = event_tx.send(AppEvent::DelegationFinished(message)) {
                warn!(error = ?e, "Failed to deliver A2A agent completion event");
            }
        });

        Ok(())
    }

    /// Launch a sub-agent by invoking chatty-tui in headless mode (subprocess fallback).
    fn launch_subprocess_agent(&mut self, prompt: &str) -> Result<()> {
        let executable = std::env::current_exe().context("Failed to resolve chatty-tui binary")?;
        let model_id = self.model_config.id.clone();
        let prompt_owned = prompt.to_string();
        let auto_approve = matches!(
            self.execution_settings.approval_mode,
            chatty_core::settings::models::execution_settings::ApprovalMode::AutoApproveAll
        );
        let event_tx = self.event_tx.clone();

        self.add_system_message("Launching local sub-agent...".to_string());
        self.transcript.mark_last_as_delegation_row();

        tokio::task::spawn_blocking(move || {
            let message = match super::helpers::run_sub_agent_process(
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

            if let Err(e) = event_tx.send(AppEvent::DelegationFinished(message)) {
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
        self.session.set_conversation(None);
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
                key: "code-exec".to_string(),
                label: "Code Execution".to_string(),
                enabled: es.execute_code_enabled,
            },
            ToolPickerItem {
                key: "docker-exec".to_string(),
                label: "Docker Fallback".to_string(),
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
                "code-exec" => self.execution_settings.execute_code_enabled = item.enabled,
                "docker-exec" => {
                    self.execution_settings.docker_code_execution_enabled = item.enabled
                }
                _ => {}
            }
        }
        if self.execution_settings.docker_code_execution_enabled {
            self.execution_settings.execute_code_enabled = true;
        }

        self.session.set_conversation(None);
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
            "code-exec" => {
                self.execution_settings.execute_code_enabled =
                    !self.execution_settings.execute_code_enabled
            }
            "docker-exec" => {
                self.execution_settings.docker_code_execution_enabled =
                    !self.execution_settings.docker_code_execution_enabled;
                if self.execution_settings.docker_code_execution_enabled {
                    self.execution_settings.execute_code_enabled = true;
                }
            }
            _ => {
                self.add_system_message(format!(
                    "Unknown tool '{}'. Valid: shell, fs-read, fs-write, fetch, git, code-exec, docker-exec",
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
            "code-exec" => self.execution_settings.execute_code_enabled,
            "docker-exec" => self.execution_settings.docker_code_execution_enabled,
            _ => false,
        };
        let state = if enabled { "enabled" } else { "disabled" };
        self.add_system_message(format!("Tool '{}' {}. Reinitializing...", name, state));
        self.session.set_conversation(None);
        self.is_ready = false;
        true
    }

    /// Handle `/modules` command variants and persist changes asynchronously.
    ///
    /// Returns `Ok(true)` when settings changed and the conversation should be
    /// reinitialized, `Ok(false)` for read-only actions, or an error for invalid
    /// input.
    pub fn handle_modules_command(&mut self, arg: Option<&str>) -> Result<bool> {
        let Some(raw) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
            self.add_system_message(self.module_settings_summary());
            return Ok(false);
        };

        let mut changed = false;
        let mut parts = raw.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or_default().to_ascii_lowercase();
        let rest = parts.next().map(str::trim).unwrap_or_default();

        match cmd.as_str() {
            "show" => {
                self.add_system_message(self.module_settings_summary());
            }
            "enable" | "on" => {
                if !self.module_settings.enabled {
                    self.module_settings.enabled = true;
                    changed = true;
                }
            }
            "disable" | "off" => {
                if self.module_settings.enabled {
                    self.module_settings.enabled = false;
                    changed = true;
                }
            }
            "dir" => {
                if rest.is_empty() {
                    bail!("Usage: /modules dir <directory>");
                }
                let path = Path::new(rest);
                let resolved = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("."))
                        .join(path)
                };
                self.module_settings.module_dir = resolved.to_string_lossy().to_string();
                changed = true;
            }
            "port" => {
                if rest.is_empty() {
                    bail!("Usage: /modules port <1-65535>");
                }
                let port = rest
                    .parse()
                    .context("Port must be a number between 1 and 65535")?;
                let port =
                    std::num::NonZeroU16::new(port).context("Port must be between 1 and 65535")?;
                self.module_settings.gateway_port = port.get();
                changed = true;
            }
            _ => {
                bail!(
                    "Unknown /modules command '{}'. Valid: show, enable, disable, dir, port",
                    cmd
                );
            }
        }

        if changed {
            let settings = self.module_settings.clone();
            tokio::spawn(async move {
                if let Err(e) = chatty_core::module_settings_repository()
                    .save(settings)
                    .await
                {
                    warn!(error = ?e, "Failed to persist module settings");
                }
            });

            self.session.set_conversation(None);
            self.is_ready = false;
            self.add_system_message(format!(
                "Modules settings updated: enabled={}, dir={}, port={}. Conversation context was reset.",
                self.module_settings.enabled,
                self.module_settings.module_dir,
                self.module_settings.gateway_port
            ));
        }

        Ok(changed)
    }

    pub fn module_settings_summary(&self) -> String {
        let local_agents = self
            .module_agents
            .iter()
            .filter(|agent| !matches!(agent.execution_mode.as_str(), "remote" | "remote_only"))
            .count();
        let remote_agents = self
            .module_agents
            .iter()
            .filter(|agent| matches!(agent.execution_mode.as_str(), "remote" | "remote_only"))
            .count();
        let broker_line = match self.broker_port {
            Some(port) => format!("\n- Broker: active on port {port} (not persisted)"),
            None => String::new(),
        };
        format!(
            "Modules settings:\n- Runtime enabled: {}\n- Module directory: {}\n- Gateway port: {}\n- Local module agents: {}\n- Remote module agents: {}{}\n\nCommands:\n/modules show\n/modules enable|disable|on|off\n/modules dir <directory>\n/modules port <1-65535>",
            self.module_settings.enabled,
            self.module_settings.module_dir,
            self.module_settings.gateway_port,
            local_agents,
            remote_agents,
            broker_line
        )
    }
}

#[cfg(target_os = "linux")]
async fn do_update_cli_if_installed() -> Result<Option<String>> {
    let bin_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".local/bin");
    let target = bin_dir.join("chatty-tui");

    if !target.exists() {
        return Ok(Some(format!(
            "CLI auto-update skipped: '{}' is not installed.",
            target.display()
        )));
    }

    let source = std::fs::canonicalize(
        std::env::current_exe().context("Failed to resolve current chatty-tui binary")?,
    )
    .context("Failed to canonicalize current chatty-tui binary path")?;
    let target_canonical = std::fs::canonicalize(&target)
        .with_context(|| format!("Failed to canonicalize '{}'", target.display()))?;

    if source == target_canonical {
        return Ok(Some(
            "CLI already points to the current binary.".to_string(),
        ));
    }

    tokio::fs::copy(&source, &target).await.with_context(|| {
        format!(
            "Failed to copy '{}' to '{}'",
            source.display(),
            target.display()
        )
    })?;

    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
        .await
        .with_context(|| {
            format!(
                "Failed to set executable permissions on '{}'",
                target.display()
            )
        })?;

    Ok(Some(format!(
        "CLI at '{}' updated to the current version.",
        target.display()
    )))
}

#[cfg(not(target_os = "linux"))]
async fn do_update_cli_if_installed() -> Result<Option<String>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{ChatEngine, Command};

    #[test]
    fn parse_modules_command_variants() {
        assert_eq!(
            ChatEngine::parse_command("/modules"),
            Some(Command::Modules(None))
        );
        assert_eq!(
            ChatEngine::parse_command("/modules show"),
            Some(Command::Modules(Some("show".to_string())))
        );
        assert_eq!(
            ChatEngine::parse_command("/modules port 8421"),
            Some(Command::Modules(Some("port 8421".to_string())))
        );
    }

    #[test]
    fn parse_update_command() {
        assert_eq!(ChatEngine::parse_command("/update"), Some(Command::Update));
    }

    /// AGE-382: `--broker` threads its ephemeral port into `broker_port`,
    /// never into `module_settings`. A `/modules` mutation that only touches
    /// `module_dir` must leave `enabled`/`gateway_port` exactly as they were
    /// before the broker started, so the struct `handle_modules_command`
    /// hands to the repository never carries the broker's port to disk.
    /// Revert the fix (route `broker_port` back through `module_settings`)
    /// and this fails.
    #[tokio::test]
    async fn broker_port_does_not_leak_into_module_settings_on_a_modules_save() {
        use crate::engine::ChatEngineConfig;
        use chatty_core::services::StreamSurface;
        use chatty_core::settings::models::models_store::ModelConfig;
        use chatty_core::settings::models::module_settings::ModuleSettingsModel;
        use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
        use chatty_core::settings::models::{ExecutionSettingsModel, ModelsModel};

        let on_disk = ModuleSettingsModel::default();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut engine = ChatEngine::new(
            ChatEngineConfig {
                model_config: ModelConfig::new(
                    "m1".to_string(),
                    "Test Model".to_string(),
                    ProviderType::Ollama,
                    "llama3.2".to_string(),
                ),
                provider_config: ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
                execution_settings: ExecutionSettingsModel::default(),
                module_settings: on_disk.clone(),
                broker_port: Some(54321),
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

        let dir = tempfile::tempdir().expect("a temp dir for module_dir");
        let changed = engine
            .handle_modules_command(Some(&format!("dir {}", dir.path().display())))
            .expect("dir is a valid /modules subcommand");

        assert!(changed, "module_dir changed, so the command reports true");
        assert_eq!(
            engine.module_settings.enabled, on_disk.enabled,
            "the broker must not flip `enabled` on"
        );
        assert_eq!(
            engine.module_settings.gateway_port, on_disk.gateway_port,
            "the broker's ephemeral port must not overwrite the persisted gateway_port"
        );
        assert_eq!(
            engine.module_settings.module_dir,
            dir.path().to_string_lossy()
        );
    }
}
