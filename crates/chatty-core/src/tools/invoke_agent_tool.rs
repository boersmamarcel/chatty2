use parking_lot::Mutex;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::models::clarification_store::{PendingClarifications, request_clarification};
use crate::models::message_types::ToolSource;
use crate::services::a2a_client::{A2aClarificationRequest, A2aClient, A2aStreamEvent};
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::tools::agent_origin::AgentOrigin;
use crate::tools::list_agents_tool::LocalModuleAgentSummary;

/// The agent name the broker publishes for "a chatty agent in its own
/// process" (ADR-0011 C2).
///
/// Defined here rather than in the gateway because both ends need it and the
/// gateway cannot depend on this crate; the wiring that starts the broker
/// passes this constant to `LocalRunner::with_agent_name`, so the two cannot
/// drift.
pub const LOCAL_AGENT_NAME: &str = "local-agent";

/// Progress events emitted by the invoke_agent tool during streaming execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InvokeAgentProgress {
    /// Agent invocation started.
    Started {
        agent_name: String,
        prompt: String,
        source: ToolSource,
    },
    /// A text chunk from the agent's response.
    Text(String),
    /// Agent invocation finished.
    Finished {
        success: bool,
        result: Option<String>,
    },
}

/// Shared slot for sending progress events from the tool to the stream loop.
///
/// The stream loop installs a fresh sender before each LLM stream. The tool
/// holds a reference to the slot and sends progress events through it.
pub type InvokeAgentProgressSlot = Arc<Mutex<Option<UnboundedSender<InvokeAgentProgress>>>>;

/// Arguments for the invoke_agent tool
#[derive(Deserialize, Serialize)]
pub struct InvokeAgentArgs {
    /// Name of the agent to invoke (must match a known remote or local agent).
    pub agent: String,
    /// The prompt or task to send to the agent.
    pub prompt: String,
}

/// Output from the invoke_agent tool
#[derive(Debug, Serialize)]
pub struct InvokeAgentOutput {
    /// The agent's response text.
    pub response: String,
    /// Name of the agent that was invoked.
    pub agent: String,
    /// Whether the invocation completed successfully.
    pub success: bool,
}

/// Error type for invoke_agent tool
#[derive(Debug, thiserror::Error)]
pub enum InvokeAgentError {
    #[error("Agent not found: {0}")]
    NotFound(String),
    #[error("Agent disabled: {0}")]
    Disabled(String),
    #[error("Invocation failed: {0}")]
    InvocationFailed(String),
}

/// Tool that invokes a named agent (remote A2A or local WASM module) with a prompt.
///
/// Remote agents are called via `A2aClient::send_message_stream()` for real-time progress.
/// Local WASM module agents are called via the protocol gateway's A2A endpoint.
#[derive(Clone)]
pub struct InvokeAgentTool {
    /// Snapshot of configured remote A2A agents taken at construction time.
    remote_agents: Vec<A2aAgentConfig>,
    module_agents: Vec<LocalModuleAgentSummary>,
    /// Base URL for the protocol gateway (e.g. `http://localhost:8420`),
    /// used to call local WASM module agents via their A2A endpoint.
    gateway_base_url: Option<String>,
    client: A2aClient,
    /// Shared slot for sending progress events to the UI stream loop.
    progress_slot: InvokeAgentProgressSlot,
    /// Name of the broker's local-worker agent, when one is published
    /// (ADR-0011 C2). Resolved after remote agents and before modules.
    local_agent: Option<String>,
    /// Whether to say something before handing a prompt to an agent outside
    /// this user's fleet (ADR-0011 C5).
    warn_outside_fleet: bool,
    /// This agent's own clarification store: where a delegated agent's
    /// question is re-asked (AGE-306). `None` means nobody here can answer,
    /// and a question ends the delegation.
    ///
    /// Escalating to a human is the only policy (ADR-0011 C7). Whether a
    /// leader may instead answer for its worker is an open question on the
    /// ADR; nothing selects such a policy today.
    clarifications: Option<PendingClarifications>,
}

impl InvokeAgentTool {
    pub fn new(
        remote_agents: Vec<A2aAgentConfig>,
        module_agents: Vec<LocalModuleAgentSummary>,
        gateway_port: Option<u16>,
    ) -> Self {
        let gateway_base_url = gateway_port.map(|port| format!("http://localhost:{}", port));
        Self {
            remote_agents,
            module_agents,
            gateway_base_url,
            client: A2aClient::with_timeout(std::time::Duration::from_secs(300)),
            progress_slot: Arc::new(Mutex::new(None)),
            local_agent: None,
            warn_outside_fleet: false,
            clarifications: None,
        }
    }

    /// Re-ask a delegated agent's questions on this agent's own `ask_user`
    /// surface (AGE-306). The same store the agent's `AskUserTool` holds,
    /// so a question from below is indistinguishable, to whoever answers,
    /// from one this agent asked itself.
    pub fn with_clarifications(mut self, pending: PendingClarifications) -> Self {
        self.clarifications = Some(pending);
        self
    }

    /// Offer the broker's local-worker agent (ADR-0011 C2), which spawns a
    /// chatty child per task. Only meaningful when the gateway is running,
    /// since that is what serves it.
    pub fn with_local_agent(mut self, name: impl Into<String>) -> Self {
        self.local_agent = Some(name.into());
        self
    }

    /// Warn before handing a prompt to an agent outside this user's fleet
    /// (ADR-0011 C5), from `execution_settings.warn_on_external_agent`.
    ///
    /// What the warning *does* is deliberately nothing but say so. Refusing,
    /// or remembering an answer, is a trust policy nobody has chosen yet; this
    /// is where it will attach.
    pub fn with_external_agent_warning(mut self, warn: bool) -> Self {
        self.warn_outside_fleet = warn;
        self
    }

    /// Say that a prompt is about to leave this user's machines.
    ///
    /// Goes to the progress channel rather than the log, because the person
    /// who needs to know is the user watching the turn.
    fn warn_if_outside_the_fleet(&self, agent: &str, origin: AgentOrigin, url: &str) {
        if !self.warn_outside_fleet || origin.is_own_fleet() {
            return;
        }
        warn!(
            agent = %agent,
            %origin,
            url = %url,
            "Handing a prompt to an agent outside this user's fleet"
        );
        self.send_progress(InvokeAgentProgress::Text(format!(
            "\u{26a0} '{agent}' runs at {url}, outside your fleet ({origin}):              the prompt and anything quoted in it leave this machine."
        )));
    }

    /// Returns a clone of the progress slot for the stream loop to install a sender.
    pub fn progress_slot(&self) -> InvokeAgentProgressSlot {
        self.progress_slot.clone()
    }

    /// Send a progress event through the slot (if a sender is installed).
    fn send_progress(&self, event: InvokeAgentProgress) {
        let guard = self.progress_slot.lock();
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(event);
        }
    }
}

impl Tool for InvokeAgentTool {
    const NAME: &'static str = "invoke_agent";
    type Error = InvokeAgentError;
    type Args = InvokeAgentArgs;
    type Output = InvokeAgentOutput;

    fn description(&self) -> String {
        "Invoke a named agent (remote A2A or local WASM module) with a prompt \
                          and return its response. Use `list_agents` first to discover available \
                          agents. Remote A2A agents are called over HTTP. Local module agents are \
                          called via the protocol gateway. The agent runs autonomously and returns \
                          its final response."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "The name of the agent to invoke. Must match a name from \
                                   `list_agents` output (e.g. \"benford-agent\")."
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt or task to send to the agent."
                }
            },
            "required": ["agent", "prompt"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let agent_name = args.agent.trim().to_string();
        let prompt = args.prompt.trim().to_string();

        if agent_name.is_empty() {
            return Err(InvokeAgentError::NotFound(
                "Agent name cannot be empty".to_string(),
            ));
        }
        if prompt.is_empty() {
            return Err(InvokeAgentError::InvocationFailed(
                "Prompt cannot be empty".to_string(),
            ));
        }

        // 1. Check remote A2A agents first (they take precedence)
        if let Some(config) = self.remote_agents.iter().find(|a| a.name == agent_name) {
            if !config.enabled {
                return Err(InvokeAgentError::Disabled(format!(
                    "Remote agent '{}' is disabled. Enable it in Settings → A2A Agents.",
                    agent_name
                )));
            }

            info!(agent = %agent_name, url = %config.url, "Invoking remote A2A agent");
            self.warn_if_outside_the_fleet(&agent_name, AgentOrigin::RemoteConfigured, &config.url);
            self.send_progress(InvokeAgentProgress::Started {
                agent_name: agent_name.clone(),
                prompt: prompt.clone(),
                source: ToolSource::ExternalService {
                    name: agent_name.clone(),
                },
            });
            return self.call_streaming(config, &prompt).await;
        }

        // 2. The broker's local worker: a chatty child in its own process.
        //    Ahead of modules, so a module cannot claim its reserved name
        //    by accident.
        if let Some(local) = self
            .local_agent
            .as_deref()
            .filter(|local| **local == agent_name)
        {
            let Some(ref base_url) = self.gateway_base_url else {
                return Err(InvokeAgentError::InvocationFailed(format!(
                    "Agent '{local}' needs the protocol gateway. \
                         Enable it in Settings \u{2192} Modules."
                )));
            };

            info!(agent = %local, "Delegating to a local worker through the broker");
            let config = A2aAgentConfig {
                name: local.to_string(),
                url: format!("{}/a2a/{}", base_url, local),
                api_key: None,
                enabled: true,
                skills: vec!["delegate".to_string()],
            };
            self.send_progress(InvokeAgentProgress::Started {
                agent_name: local.to_string(),
                prompt: prompt.clone(),
                source: ToolSource::Local,
            });
            return self.call_streaming(&config, &prompt).await;
        }

        // 3. Check local WASM module agents
        if let Some(module) = self.module_agents.iter().find(|m| m.name == agent_name) {
            if !module.supports_a2a {
                return Err(InvokeAgentError::InvocationFailed(format!(
                    "Local module '{}' does not support the A2A protocol.",
                    agent_name
                )));
            }

            let Some(ref base_url) = self.gateway_base_url else {
                return Err(InvokeAgentError::InvocationFailed(format!(
                    "Local module '{}' found but the protocol gateway is not running. \
                         Enable it in Settings → Modules.",
                    agent_name
                )));
            };

            info!(agent = %agent_name, "Invoking local module agent via protocol gateway");
            let config = A2aAgentConfig {
                name: agent_name.clone(),
                url: format!("{}/a2a/{}", base_url, agent_name),
                api_key: None,
                enabled: true,
                skills: module.tools.clone(),
            };
            self.send_progress(InvokeAgentProgress::Started {
                agent_name: agent_name.clone(),
                prompt: prompt.clone(),
                source: if matches!(module.execution_mode.as_str(), "remote" | "remote_only") {
                    ToolSource::HiveCloud
                } else {
                    ToolSource::Local
                },
            });
            return self.call_streaming(&config, &prompt).await;
        }

        // 4. Not found
        let available: Vec<String> = self
            .remote_agents
            .iter()
            .map(|a| a.name.clone())
            .chain(self.local_agent.clone())
            .chain(self.module_agents.iter().map(|m| m.name.clone()))
            .collect();

        Err(InvokeAgentError::NotFound(format!(
            "No agent named '{}'. Available agents: {}",
            agent_name,
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        )))
    }
}

impl InvokeAgentTool {
    /// Call an agent via streaming A2A protocol, forwarding progress events.
    async fn call_streaming(
        &self,
        config: &A2aAgentConfig,
        prompt: &str,
    ) -> Result<InvokeAgentOutput, InvokeAgentError> {
        use futures::StreamExt;

        let mut stream = self
            .client
            .send_message_stream(config, prompt)
            .await
            .map_err(|e| {
                let err_text = format!("⚠️ Failed to invoke agent '{}': {}", config.name, e);
                self.send_progress(InvokeAgentProgress::Finished {
                    success: false,
                    result: Some(err_text),
                });
                InvokeAgentError::InvocationFailed(format!(
                    "Failed to invoke agent '{}': {}",
                    config.name, e
                ))
            })?;

        let mut response = String::new();
        let mut success = true;
        let mut error_msg = None;

        while let Some(event) = stream.next().await {
            match event {
                Ok(A2aStreamEvent::StatusUpdate {
                    task_id,
                    state,
                    message,
                    metadata,
                    ..
                }) => {
                    if state == "failed" {
                        success = false;
                        error_msg = message.clone();
                    } else if state == "working"
                        && let Some(ref msg) = message
                    {
                        self.send_progress(InvokeAgentProgress::Text(msg.clone()));
                    } else if state == "input-required"
                        && let Some(request) =
                            A2aClarificationRequest::from_status_metadata(metadata.as_ref())
                        && let Err(e) = self.answer_input_required(config, &task_id, request).await
                    {
                        // Dropping the stream on the way out cancels the
                        // parked task, which reaps the worker.
                        success = false;
                        error_msg = Some(e);
                        break;
                    }
                    // "completed" — just let the stream end naturally. An
                    // "input-required" without a request is an approval
                    // the worker is waiting on, which the worker settles
                    // itself; nothing to do here.
                }
                Ok(A2aStreamEvent::ArtifactUpdate { text, .. }) => {
                    if !text.is_empty() {
                        self.send_progress(InvokeAgentProgress::Text(text.clone()));
                        response.push_str(&text);
                    }
                }
                Err(e) => {
                    warn!(agent = %config.name, error = %e, "Stream error");
                    success = false;
                    error_msg = Some(e.to_string());
                    break;
                }
            }
        }

        let response = response.trim().to_string();

        if !success {
            let err_text = error_msg
                .as_ref()
                .map(|m| format!("⚠️ {m}"))
                .unwrap_or_else(|| "⚠️ Agent failed".to_string());
            self.send_progress(InvokeAgentProgress::Finished {
                success: false,
                result: Some(err_text),
            });
            return Err(InvokeAgentError::InvocationFailed(format!(
                "Agent '{}' reported failure{}",
                config.name,
                error_msg.map(|m| format!(": {}", m)).unwrap_or_default()
            )));
        }

        // Emit Finished with the full result so the sub-agent trace block
        // shows the response (identical to /agent visualisation).
        self.send_progress(InvokeAgentProgress::Finished {
            success: true,
            result: if response.is_empty() {
                None
            } else {
                Some(response.clone())
            },
        });

        debug!(agent = %config.name, response_len = response.len(), "Agent responded");

        // Return the actual agent output to the parent model as well. This keeps
        // the sub-agent trace useful for transparency while still giving the
        // calling model the concrete result it needs to answer correctly.
        Ok(InvokeAgentOutput {
            agent: config.name.clone(),
            response: if response.is_empty() {
                format!("Agent '{}' completed successfully.", config.name)
            } else {
                response
            },
            success: true,
        })
    }
}

impl InvokeAgentTool {
    /// The delegated task asked a question: get it answered and send the
    /// answer back down on the same task.
    ///
    /// `Err` is the reason the delegation cannot continue, worded for the
    /// model.
    async fn answer_input_required(
        &self,
        config: &A2aAgentConfig,
        task_id: &str,
        request: A2aClarificationRequest,
    ) -> Result<(), String> {
        let Some(pending) = self.clarifications.as_ref() else {
            return Err(format!(
                "Agent '{}' asked a question and nobody here can answer it: {}",
                config.name,
                request
                    .questions
                    .first()
                    .map(|q| q.question.as_str())
                    .unwrap_or("(no question text)")
            ));
        };

        info!(
            agent = %config.name,
            task = %task_id,
            request = %request.id,
            questions = request.questions.len(),
            "Delegated agent asked a question; escalating"
        );
        let answers = request_clarification(pending, request.questions)
            .await
            .map_err(|e| {
                format!(
                    "Agent '{}' asked a question that went unanswered: {e}",
                    config.name
                )
            })?;

        self.client
            .send_task_input(config, task_id, &request.id, &answers)
            .await
            .map_err(|e| {
                format!(
                    "Agent '{}' asked a question, but the answer could not be delivered: {e:#}",
                    config.name
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::a2a_store::A2aAgentConfig;

    fn make_agent(name: &str, url: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: url.to_string(),
            api_key: None,
            enabled,
            skills: vec![],
        }
    }

    fn make_module(name: &str, supports_a2a: bool) -> LocalModuleAgentSummary {
        LocalModuleAgentSummary {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: format!("{} module", name),
            tools: vec!["tool_a".to_string()],
            supports_a2a,
            execution_mode: "local".to_string(),
        }
    }

    /// Collect the progress the tool reports, as the frontend's channel does.
    fn watch_progress(tool: &InvokeAgentTool) -> std::sync::mpsc::Receiver<InvokeAgentProgress> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel();
        *tool.progress_slot().lock() = Some(async_tx);
        tokio::spawn(async move {
            while let Some(event) = async_rx.recv().await {
                let _ = tx.send(event);
            }
        });
        rx
    }

    fn warnings(rx: &std::sync::mpsc::Receiver<InvokeAgentProgress>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|event| match event {
                InvokeAgentProgress::Text(text) if text.contains("outside your fleet") => {
                    Some(text)
                }
                _ => None,
            })
            .collect()
    }

    /// ADR-0011 C5: with the flag on, a prompt leaving this user's machines is
    /// said out loud before it goes. The call itself then fails — there is no
    /// server at that URL — which is fine: the warning has to come first.
    #[tokio::test]
    async fn a_third_party_agent_is_announced_before_the_prompt_goes() {
        let tool = InvokeAgentTool::new(
            vec![make_agent("voucher", "http://127.0.0.1:1/a2a", true)],
            vec![],
            None,
        )
        .with_external_agent_warning(true);
        let progress = watch_progress(&tool);

        let _ = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "voucher".to_string(),
                    prompt: "summarise the contract".to_string(),
                },
            )
            .await;

        // Give the forwarding task a moment to drain the channel.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let said = warnings(&progress);
        assert_eq!(said.len(), 1, "expected exactly one warning, got {said:?}");
        assert!(said[0].contains("voucher"), "{}", said[0]);
        assert!(said[0].contains("127.0.0.1:1"), "{}", said[0]);
        assert!(
            said[0].contains("remote_configured"),
            "the warning names the origin: {}",
            said[0]
        );
    }

    /// The flag is off by default, and the hook is silent until someone turns
    /// it on. Which trust policy it should carry is not decided here.
    #[tokio::test]
    async fn nothing_is_said_when_the_flag_is_off() {
        let tool = InvokeAgentTool::new(
            vec![make_agent("voucher", "http://127.0.0.1:1/a2a", true)],
            vec![],
            None,
        );
        let progress = watch_progress(&tool);

        let _ = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "voucher".to_string(),
                    prompt: "summarise the contract".to_string(),
                },
            )
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(warnings(&progress).is_empty());
    }

    #[tokio::test]
    async fn test_invoke_not_found() {
        let tool = InvokeAgentTool::new(vec![], vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "nonexistent".to_string(),
                    prompt: "hello".to_string(),
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, InvokeAgentError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_invoke_disabled_remote_agent() {
        let agent = make_agent("my-agent", "https://example.com/a2a", false);
        let tool = InvokeAgentTool::new(vec![agent], vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "my-agent".to_string(),
                    prompt: "hello".to_string(),
                },
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::Disabled(_)));
    }

    #[tokio::test]
    async fn test_invoke_local_module_no_gateway() {
        let module = make_module("benford-agent", true);
        let tool = InvokeAgentTool::new(vec![], vec![module], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "benford-agent".to_string(),
                    prompt: "analyze data".to_string(),
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, InvokeAgentError::InvocationFailed(_)));
        assert!(err.to_string().contains("gateway is not running"));
    }

    #[tokio::test]
    async fn test_invoke_local_module_no_a2a_support() {
        let module = make_module("basic-module", false);
        let tool = InvokeAgentTool::new(vec![], vec![module], Some(8420));

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "basic-module".to_string(),
                    prompt: "hello".to_string(),
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, InvokeAgentError::InvocationFailed(_)));
        assert!(
            err.to_string()
                .contains("does not support the A2A protocol")
        );
    }

    #[tokio::test]
    async fn test_invoke_empty_agent_name() {
        let tool = InvokeAgentTool::new(vec![], vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "  ".to_string(),
                    prompt: "hello".to_string(),
                },
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_invoke_empty_prompt() {
        let tool = InvokeAgentTool::new(vec![], vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "some-agent".to_string(),
                    prompt: "".to_string(),
                },
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            InvokeAgentError::InvocationFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_remote_agent_takes_precedence() {
        // When both remote and local share a name, remote should win
        let remote = make_agent("shared-name", "https://example.com/a2a", false);
        let module = make_module("shared-name", true);
        let tool = InvokeAgentTool::new(vec![remote], vec![module], Some(8420));

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "shared-name".to_string(),
                    prompt: "hello".to_string(),
                },
            )
            .await;

        // Should hit the remote disabled check, not the local module path
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::Disabled(_)));
    }
}
