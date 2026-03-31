use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::services::a2a_client::A2aClient;
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::repositories::A2aRepository;
use crate::tools::list_agents_tool::LocalModuleAgentSummary;

/// Progress events emitted by the invoke_agent tool during streaming execution.
#[derive(Debug, Clone)]
pub enum InvokeAgentProgress {
    /// Agent invocation started.
    Started { agent_name: String, prompt: String },
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
    #[error("Repository error: {0}")]
    RepositoryError(String),
}

/// Tool that invokes a named agent (remote A2A or local WASM module) with a prompt.
///
/// Remote agents are called via `A2aClient::send_message_stream()` for real-time progress.
/// Local WASM module agents are called via the protocol gateway's A2A endpoint.
#[derive(Clone)]
pub struct InvokeAgentTool {
    repository: Arc<dyn A2aRepository>,
    module_agents: Vec<LocalModuleAgentSummary>,
    /// Base URL for the protocol gateway (e.g. `http://localhost:8420`),
    /// used to call local WASM module agents via their A2A endpoint.
    gateway_base_url: Option<String>,
    client: A2aClient,
    /// Shared slot for sending progress events to the UI stream loop.
    progress_slot: InvokeAgentProgressSlot,
}

impl InvokeAgentTool {
    pub fn new(
        repository: Arc<dyn A2aRepository>,
        module_agents: Vec<LocalModuleAgentSummary>,
        gateway_port: Option<u16>,
    ) -> Self {
        let gateway_base_url = gateway_port.map(|port| format!("http://localhost:{}", port));
        Self {
            repository,
            module_agents,
            gateway_base_url,
            client: A2aClient::with_timeout(std::time::Duration::from_secs(300)),
            progress_slot: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns a clone of the progress slot for the stream loop to install a sender.
    pub fn progress_slot(&self) -> InvokeAgentProgressSlot {
        self.progress_slot.clone()
    }

    /// Send a progress event through the slot (if a sender is installed).
    fn send_progress(&self, event: InvokeAgentProgress) {
        if let Ok(guard) = self.progress_slot.lock()
            && let Some(tx) = guard.as_ref()
        {
            let _ = tx.send(event);
        }
    }
}

impl Tool for InvokeAgentTool {
    const NAME: &'static str = "invoke_agent";
    type Error = InvokeAgentError;
    type Args = InvokeAgentArgs;
    type Output = InvokeAgentOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "invoke_agent".to_string(),
            description: "Invoke a named agent (remote A2A or local WASM module) with a prompt \
                          and return its response. Use `list_agents` first to discover available \
                          agents. Remote A2A agents are called over HTTP. Local module agents are \
                          called via the protocol gateway. The agent runs autonomously and returns \
                          its final response."
                .to_string(),
            parameters: serde_json::json!({
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
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
        let remote_agents = self
            .repository
            .load_all()
            .await
            .map_err(|e| InvokeAgentError::RepositoryError(e.to_string()))?;

        if let Some(config) = remote_agents.iter().find(|a| a.name == agent_name) {
            if !config.enabled {
                return Err(InvokeAgentError::Disabled(format!(
                    "Remote agent '{}' is disabled. Enable it in Settings → A2A Agents.",
                    agent_name
                )));
            }

            info!(agent = %agent_name, url = %config.url, "Invoking remote A2A agent");
            self.send_progress(InvokeAgentProgress::Started {
                agent_name: agent_name.clone(),
                prompt: prompt.clone(),
            });
            return self.call_streaming(config, &prompt).await;
        }

        // 2. Check local WASM module agents
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
            });
            return self.call_streaming(&config, &prompt).await;
        }

        // 3. Not found
        let available: Vec<String> = remote_agents
            .iter()
            .map(|a| a.name.clone())
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
                self.send_progress(InvokeAgentProgress::Finished {
                    success: false,
                    result: None,
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
                Ok(crate::services::a2a_client::A2aStreamEvent::StatusUpdate {
                    state,
                    message,
                    ..
                }) => {
                    if state == "failed" {
                        success = false;
                        error_msg = message.clone();
                    } else if state == "working" {
                        if let Some(ref msg) = message {
                            self.send_progress(InvokeAgentProgress::Text(msg.clone()));
                        }
                    }
                    // "completed" — just let the stream end naturally
                }
                Ok(crate::services::a2a_client::A2aStreamEvent::ArtifactUpdate {
                    text, ..
                }) => {
                    if !text.is_empty() {
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

        // Return a brief summary to rig-core so the LLM knows the agent
        // finished without echoing the full response (it's already shown
        // in the sub-agent trace block visible to the user).
        Ok(InvokeAgentOutput {
            agent: config.name.clone(),
            response: format!(
                "Agent '{}' completed successfully. The full response ({} chars) \
                 has been displayed to the user in the sub-agent trace.",
                config.name,
                response.len()
            ),
            success: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::a2a_store::A2aAgentConfig;
    use crate::tools::test_helpers::MockA2aRepository;

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
        }
    }

    #[tokio::test]
    async fn test_invoke_not_found() {
        let repo = Arc::new(MockA2aRepository::new());
        let tool = InvokeAgentTool::new(repo, vec![], None);

        let result = tool
            .call(InvokeAgentArgs {
                agent: "nonexistent".to_string(),
                prompt: "hello".to_string(),
            })
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, InvokeAgentError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_invoke_disabled_remote_agent() {
        let agent = make_agent("my-agent", "https://example.com/a2a", false);
        let repo = Arc::new(MockA2aRepository::with_agents(vec![agent]));
        let tool = InvokeAgentTool::new(repo, vec![], None);

        let result = tool
            .call(InvokeAgentArgs {
                agent: "my-agent".to_string(),
                prompt: "hello".to_string(),
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::Disabled(_)));
    }

    #[tokio::test]
    async fn test_invoke_local_module_no_gateway() {
        let repo = Arc::new(MockA2aRepository::new());
        let module = make_module("benford-agent", true);
        let tool = InvokeAgentTool::new(repo, vec![module], None);

        let result = tool
            .call(InvokeAgentArgs {
                agent: "benford-agent".to_string(),
                prompt: "analyze data".to_string(),
            })
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, InvokeAgentError::InvocationFailed(_)));
        assert!(err.to_string().contains("gateway is not running"));
    }

    #[tokio::test]
    async fn test_invoke_local_module_no_a2a_support() {
        let repo = Arc::new(MockA2aRepository::new());
        let module = make_module("basic-module", false);
        let tool = InvokeAgentTool::new(repo, vec![module], Some(8420));

        let result = tool
            .call(InvokeAgentArgs {
                agent: "basic-module".to_string(),
                prompt: "hello".to_string(),
            })
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
        let repo = Arc::new(MockA2aRepository::new());
        let tool = InvokeAgentTool::new(repo, vec![], None);

        let result = tool
            .call(InvokeAgentArgs {
                agent: "  ".to_string(),
                prompt: "hello".to_string(),
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_invoke_empty_prompt() {
        let repo = Arc::new(MockA2aRepository::new());
        let tool = InvokeAgentTool::new(repo, vec![], None);

        let result = tool
            .call(InvokeAgentArgs {
                agent: "some-agent".to_string(),
                prompt: "".to_string(),
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            InvokeAgentError::InvocationFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_invoke_repo_error() {
        let repo = Arc::new(MockA2aRepository::with_load_error("disk failure"));
        let tool = InvokeAgentTool::new(repo, vec![], None);

        let result = tool
            .call(InvokeAgentArgs {
                agent: "any-agent".to_string(),
                prompt: "hello".to_string(),
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            InvokeAgentError::RepositoryError(_)
        ));
    }

    #[tokio::test]
    async fn test_remote_agent_takes_precedence() {
        // When both remote and local share a name, remote should win
        let remote = make_agent("shared-name", "https://example.com/a2a", false);
        let repo = Arc::new(MockA2aRepository::with_agents(vec![remote]));
        let module = make_module("shared-name", true);
        let tool = InvokeAgentTool::new(repo, vec![module], Some(8420));

        let result = tool
            .call(InvokeAgentArgs {
                agent: "shared-name".to_string(),
                prompt: "hello".to_string(),
            })
            .await;

        // Should hit the remote disabled check, not the local module path
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::Disabled(_)));
    }
}
