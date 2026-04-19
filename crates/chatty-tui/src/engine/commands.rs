//! Slash command parsing and handling for the TUI chat engine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tracing::{info, warn};

use super::{ChatEngine, MessageRole, ModelPicker, ModelPickerItem, ToolPicker, ToolPickerItem};
use crate::events::AppEvent;

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
            _ => None,
        }
    }

    /// Clear all display and conversation state so a fresh conversation can be initialized.
    pub fn clear_conversation(&mut self) {
        self.messages.clear();
        self.title = "New Chat".to_string();
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.pin_to_bottom();
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
                    super::helpers::common_ancestor(&current, &added_dir)
                        .unwrap_or(added_dir.clone())
                }
            }
            None => added_dir.clone(),
        };

        let workspace_str = new_workspace.to_string_lossy().to_string();
        self.execution_settings.workspace_dir = Some(workspace_str.clone());
        self.refresh_workspace_context();
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
        super::helpers::copy_text_to_clipboard(&message.text)?;
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

        let label = format!("[Agent: {}] {}", config.name, prompt);
        self.add_system_message(label);
        self.sub_agent_msg_idx = Some(self.messages.len() - 1);

        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            let client = chatty_core::services::A2aClient::new();
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
                                    let _ = event_tx.send(AppEvent::SubAgentProgress(msg.clone()));
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

            if let Err(e) = event_tx.send(AppEvent::SubAgentFinished(message)) {
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

        self.add_system_message("Launching sub-agent...".to_string());
        self.sub_agent_msg_idx = Some(self.messages.len() - 1);

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
            "docker-exec" => {
                self.execution_settings.docker_code_execution_enabled =
                    !self.execution_settings.docker_code_execution_enabled
            }
            _ => {
                self.add_system_message(format!(
                    "Unknown tool '{}'. Valid: shell, fs-read, fs-write, fetch, git, docker-exec",
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
            "docker-exec" => self.execution_settings.docker_code_execution_enabled,
            _ => false,
        };
        let state = if enabled { "enabled" } else { "disabled" };
        self.add_system_message(format!("Tool '{}' {}. Reinitializing...", name, state));
        self.conversation = None;
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

            self.conversation = None;
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
        format!(
            "Modules settings:\n- Runtime enabled: {}\n- Module directory: {}\n- Gateway port: {}\n\nCommands:\n/modules show\n/modules enable|disable|on|off\n/modules dir <directory>\n/modules port <1-65535>",
            self.module_settings.enabled,
            self.module_settings.module_dir,
            self.module_settings.gateway_port
        )
    }
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
