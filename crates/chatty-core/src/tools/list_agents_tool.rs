use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::settings::repositories::A2aRepository;

/// Arguments for listing A2A agents (no arguments needed)
#[derive(Deserialize, Serialize)]
pub struct ListAgentsToolArgs {}

/// Summary of a single configured remote A2A agent, safe for display to the LLM.
#[derive(Debug, Serialize, Clone)]
pub struct A2aAgentSummary {
    pub name: String,
    pub url: String,
    /// `true` if an API key is configured (value is never exposed).
    pub has_api_key: bool,
    pub enabled: bool,
    /// Skills advertised by the agent card (may be empty if not yet fetched).
    pub skills: Vec<String>,
}

/// Summary of a locally installed WASM module agent, safe for display to the LLM.
#[derive(Debug, Serialize, Clone)]
pub struct LocalModuleAgentSummary {
    pub name: String,
    pub version: String,
    pub description: String,
    /// Tools exposed by the module.
    pub tools: Vec<String>,
    /// Whether the module supports the A2A protocol (accessible via the protocol gateway).
    pub supports_a2a: bool,
}

/// Output from the list_agents tool
#[derive(Debug, Serialize)]
pub struct ListAgentsToolOutput {
    /// Remote A2A agents configured via Settings → A2A Agents.
    pub remote_agents: Vec<A2aAgentSummary>,
    /// Locally installed WASM module agents discoverable via the modules directory.
    pub local_agents: Vec<LocalModuleAgentSummary>,
    pub total: usize,
    pub note: String,
}

/// Error type for list_agents tool
#[derive(Debug, thiserror::Error)]
pub enum ListAgentsToolError {
    #[error("Repository error: {0}")]
    RepositoryError(String),
}

/// Tool that lists all available agents: both remotely configured A2A agents and
/// locally installed WASM module agents.
///
/// This gives the LLM visibility into what agents are available, including their
/// names, URLs/types, and skills/tools. Each agent is invokable via the
/// `/agent <name> <prompt>` command.
#[derive(Clone)]
pub struct ListAgentsTool {
    repository: Arc<dyn A2aRepository>,
    /// Locally installed WASM module agents with `agent = true`.
    module_agents: Vec<LocalModuleAgentSummary>,
}

impl ListAgentsTool {
    pub fn new(repository: Arc<dyn A2aRepository>) -> Self {
        Self {
            repository,
            module_agents: Vec::new(),
        }
    }

    /// Create a new `ListAgentsTool` that also reports local WASM module agents.
    pub fn new_with_modules(
        repository: Arc<dyn A2aRepository>,
        module_agents: Vec<LocalModuleAgentSummary>,
    ) -> Self {
        Self {
            repository,
            module_agents,
        }
    }
}

impl Tool for ListAgentsTool {
    const NAME: &'static str = "list_agents";
    type Error = ListAgentsToolError;
    type Args = ListAgentsToolArgs;
    type Output = ListAgentsToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_agents".to_string(),
            description: "List all available agents: remote A2A agents configured via \
                         Settings → A2A Agents, and locally installed WASM module agents. \
                         Returns each agent's name, type, enabled state, and the skills/tools \
                         it provides. \
                         \n\n\
                         Use this to discover what agents are available before deciding whether \
                         to delegate a task. Agents can be invoked with \
                         `/agent <name> <prompt>` — each agent runs its own full agentic \
                         loop and returns a result."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let agents = self.repository.load_all().await.map_err(|e| {
            ListAgentsToolError::RepositoryError(format!("Failed to load agents: {}", e))
        })?;

        tracing::info!(
            remote_agent_count = agents.len(),
            local_agent_count = self.module_agents.len(),
            "list_agents called"
        );

        let remote_summaries: Vec<A2aAgentSummary> = agents
            .iter()
            .map(|a| A2aAgentSummary {
                name: a.name.clone(),
                url: a.url.clone(),
                has_api_key: a.has_api_key(),
                enabled: a.enabled,
                skills: a.skills.clone(),
            })
            .collect();

        let total = remote_summaries.len() + self.module_agents.len();
        let note = if total == 0 {
            "No agents are available. Remote agents can be added via Settings → A2A Agents. \
             Local WASM module agents are installed in the modules directory."
                .to_string()
        } else {
            "To invoke an agent, use the `invoke_agent` tool with the agent's name and a prompt. \
             Only enabled remote agents can be called; local module agents are always available. \
             If a remote agent and a local module share the same name, the remote agent takes \
             precedence."
                .to_string()
        };

        Ok(ListAgentsToolOutput {
            remote_agents: remote_summaries,
            local_agents: self.module_agents.clone(),
            total,
            note,
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

    fn make_module_agent(name: &str) -> LocalModuleAgentSummary {
        LocalModuleAgentSummary {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: format!("{} module agent", name),
            tools: vec!["tool_a".to_string()],
            supports_a2a: true,
        }
    }

    #[tokio::test]
    async fn test_list_empty_repo() {
        let repo = Arc::new(MockA2aRepository::new());
        let tool = ListAgentsTool::new(repo);

        let result = tool.call(ListAgentsToolArgs {}).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.total, 0);
        assert!(output.remote_agents.is_empty());
        assert!(output.local_agents.is_empty());
        assert!(output.note.contains("No agents are available"));
    }

    #[tokio::test]
    async fn test_list_returns_agent_fields() {
        let agent = make_agent("voucher-agent", "https://hive.dev/a2a/voucher", true);
        let repo = Arc::new(MockA2aRepository::with_agents(vec![agent]));
        let tool = ListAgentsTool::new(repo);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        assert_eq!(output.total, 1);
        let a = &output.remote_agents[0];
        assert_eq!(a.name, "voucher-agent");
        assert_eq!(a.url, "https://hive.dev/a2a/voucher");
        assert!(!a.has_api_key);
        assert!(a.enabled);
    }

    #[tokio::test]
    async fn test_list_masks_api_key() {
        let agent = A2aAgentConfig {
            name: "secure-agent".to_string(),
            url: "https://example.com/a2a".to_string(),
            api_key: Some("sk-super-secret".to_string()),
            enabled: true,
            skills: vec![],
        };
        let repo = Arc::new(MockA2aRepository::with_agents(vec![agent]));
        let tool = ListAgentsTool::new(repo);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        let a = &output.remote_agents[0];
        // API key value is never exposed — only whether one is configured
        assert!(a.has_api_key);
    }

    #[tokio::test]
    async fn test_list_disabled_agent() {
        let agent = make_agent("disabled-agent", "https://example.com/a2a", false);
        let repo = Arc::new(MockA2aRepository::with_agents(vec![agent]));
        let tool = ListAgentsTool::new(repo);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        assert!(!output.remote_agents[0].enabled);
    }

    #[tokio::test]
    async fn test_list_includes_skills() {
        let agent = A2aAgentConfig {
            name: "skilled-agent".to_string(),
            url: "https://example.com/a2a".to_string(),
            api_key: None,
            enabled: true,
            skills: vec!["data-analysis".to_string(), "report-writing".to_string()],
        };
        let repo = Arc::new(MockA2aRepository::with_agents(vec![agent]));
        let tool = ListAgentsTool::new(repo);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        let a = &output.remote_agents[0];
        assert_eq!(a.skills.len(), 2);
        assert_eq!(a.skills[0], "data-analysis");
    }

    #[tokio::test]
    async fn test_list_multiple_agents() {
        let agents = vec![
            make_agent("agent-a", "https://a.example.com/a2a", true),
            make_agent("agent-b", "https://b.example.com/a2a", false),
        ];
        let repo = Arc::new(MockA2aRepository::with_agents(agents));
        let tool = ListAgentsTool::new(repo);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        assert_eq!(output.total, 2);
        assert_eq!(output.remote_agents[0].name, "agent-a");
        assert_eq!(output.remote_agents[1].name, "agent-b");
    }

    #[tokio::test]
    async fn test_list_includes_local_module_agents() {
        let repo = Arc::new(MockA2aRepository::new());
        let module = make_module_agent("benford-agent");
        let tool = ListAgentsTool::new_with_modules(repo, vec![module]);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        assert_eq!(output.total, 1);
        assert!(output.remote_agents.is_empty());
        assert_eq!(output.local_agents.len(), 1);
        assert_eq!(output.local_agents[0].name, "benford-agent");
        assert!(output.local_agents[0].supports_a2a);
    }

    #[tokio::test]
    async fn test_list_combines_remote_and_local() {
        let remote = make_agent("remote-agent", "https://example.com/a2a", true);
        let repo = Arc::new(MockA2aRepository::with_agents(vec![remote]));
        let module = make_module_agent("local-agent");
        let tool = ListAgentsTool::new_with_modules(repo, vec![module]);

        let output = tool.call(ListAgentsToolArgs {}).await.unwrap();
        assert_eq!(output.total, 2);
        assert_eq!(output.remote_agents.len(), 1);
        assert_eq!(output.local_agents.len(), 1);
    }

    #[tokio::test]
    async fn test_list_load_error() {
        let repo = Arc::new(MockA2aRepository::with_load_error("disk read failure"));
        let tool = ListAgentsTool::new(repo);

        let result = tool.call(ListAgentsToolArgs {}).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ListAgentsToolError::RepositoryError(_)
        ));
    }
}
