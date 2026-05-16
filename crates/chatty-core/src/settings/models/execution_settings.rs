use crate::settings::models::providers_store::ProviderType;
use serde::{Deserialize, Serialize};

/// Approval mode for code execution requests
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum ApprovalMode {
    /// Require approval for all commands (default, most secure)
    #[default]
    AlwaysAsk,
    /// Auto-approve sandboxed commands, ask for unsandboxed
    AutoApproveSandboxed,
    /// Auto-approve all commands (dangerous, opt-in only)
    AutoApproveAll,
}

/// Settings for code execution tool
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionSettingsModel {
    /// Master toggle for code execution feature
    pub enabled: bool,
    /// Approval behavior for command execution
    pub approval_mode: ApprovalMode,
    /// Working directory for commands (None = current directory)
    pub workspace_dir: Option<String>,
    /// Enable filesystem read tools (requires workspace_dir to be set)
    #[serde(default = "default_true")]
    pub filesystem_read_enabled: bool,
    /// Enable filesystem write tools (requires workspace_dir to be set)
    #[serde(default = "default_true")]
    pub filesystem_write_enabled: bool,
    /// Enable the built-in fetch tool, which allows the LLM to make read-only HTTP GET requests.
    /// Zero-configuration web access without requiring an MCP fetch server.
    #[serde(default = "default_true")]
    pub fetch_enabled: bool,
    /// Enable git integration tools (status, diff, log, branch, commit).
    /// Requires workspace_dir to be set and the workspace to be a git repository.
    #[serde(default)]
    pub git_enabled: bool,
    /// Expose the execute_code tool to the model.
    /// Python may run via Monty; other languages require Docker fallback.
    #[serde(default)]
    pub execute_code_enabled: bool,
    /// Enable Docker-based code execution sandbox.
    /// When disabled, execute_code runs in Monty-only mode and only supports
    /// stdlib Python snippets that Monty can handle.
    #[serde(default)]
    pub docker_code_execution_enabled: bool,
    /// Custom Docker host URI or socket path (e.g., "/run/user/1000/docker.sock"
    /// or "unix:///path/to/docker.sock"). When None, the app tries common default locations.
    #[serde(default)]
    pub docker_host: Option<String>,
    /// Maximum execution time in seconds
    pub timeout_seconds: u32,
    /// Maximum output size in bytes (prevents memory exhaustion)
    pub max_output_bytes: usize,
    /// Enable network isolation in sandbox (when available)
    pub network_isolation: bool,
    /// Maximum number of agentic turns (tool-call rounds) per response
    #[serde(default = "default_max_agent_turns")]
    pub max_agent_turns: u32,
    /// Enable persistent agent memory (remember/search_memory tools).
    /// When enabled, the agent can store and recall information across conversations.
    #[serde(default = "default_true")]
    pub memory_enabled: bool,
    /// Enable semantic (vector) search for memory.
    /// Requires an embedding provider and model to be configured.
    #[serde(default)]
    pub embedding_enabled: bool,
    /// Provider to use for computing embeddings.
    /// Independent of the chat model provider — allows e.g. Anthropic users
    /// to use OpenAI for embeddings while chatting with Claude.
    #[serde(default)]
    pub embedding_provider: Option<ProviderType>,
    /// Embedding model identifier (e.g., "text-embedding-3-small").
    #[serde(default)]
    pub embedding_model: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_max_agent_turns() -> u32 {
    10
}

impl Default for ExecutionSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in by default for security
            approval_mode: ApprovalMode::AlwaysAsk,
            workspace_dir: None,
            filesystem_read_enabled: true, // Enabled by default when workspace is set
            filesystem_write_enabled: true, // Enabled by default when workspace is set
            fetch_enabled: true,           // Enabled by default for zero-config web access
            git_enabled: false,            // Opt-in: requires workspace with git repo
            execute_code_enabled: false,   // Opt-in: exposes execute_code to the model
            docker_code_execution_enabled: false, // Opt-in: requires Docker
            docker_host: None,
            timeout_seconds: 30,
            max_output_bytes: 51200, // 50KB
            network_isolation: false,
            max_agent_turns: default_max_agent_turns(),
            memory_enabled: true, // Enabled by default for cross-conversation recall
            embedding_enabled: false, // Opt-in: requires embedding provider
            embedding_provider: None,
            embedding_model: None,
        }
    }
}
