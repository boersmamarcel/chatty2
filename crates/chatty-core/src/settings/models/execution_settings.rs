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

/// How an agent's tools are offered to the model.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolLoading {
    /// Every tool the settings allow, on every request.
    #[default]
    All,
    /// A small core of tools, plus groups the model loads with `load_tools`
    /// when the task needs them (`agent_factory::tool_loading`).
    Dynamic,
}

impl ToolLoading {
    fn is_all(&self) -> bool {
        *self == Self::All
    }
}

impl std::str::FromStr for ToolLoading {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "dynamic" => Ok(Self::Dynamic),
            other => Err(format!(
                "unknown tool loading `{other}` (valid: all, dynamic)"
            )),
        }
    }
}

/// Settings for code execution tool
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Expose the built-in browser tools (navigate, snapshot, screenshot,
    /// console, network, resize) to the model.
    ///
    /// Requires workspace_dir to be set — screenshots and console dumps are
    /// written there. Opt-in: the first use downloads a pinned Chrome build if
    /// no suitable system Chrome is installed.
    #[serde(default)]
    pub browser_enabled: bool,
    /// When the browser's open-web policy is active (internet access on),
    /// also allow navigation to private/internal IPs (RFC-1918, etc.) on the
    /// user's own network. Off by default: an SSRF guard refuses these the
    /// same as any other private target. The link-local/cloud-metadata range
    /// (169.254.0.0/16) stays refused regardless of this setting (AGE-459).
    #[serde(default)]
    pub allow_private_network_access: bool,
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
    /// Maximum number of agentic turns (tool-call rounds) per response;
    /// `0` (the default) is no cap. An interactive user has Stop; headless
    /// runs get a wall-clock budget instead (`chatty-tui --max-duration`).
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
    /// Say so before `invoke_agent` hands a prompt to an agent outside this
    /// user's fleet — a configured third-party URL, or a card learned from
    /// one (ADR-0011 C5).
    ///
    /// Off by default, and deliberately only a warning: whether an external
    /// agent should need an allowlist, a one-time confirmation, or nothing at
    /// all is a product decision that has not been made. This is the hook it
    /// will hang from.
    #[serde(default)]
    pub warn_on_external_agent: bool,
    /// Provider to use for computing embeddings.
    /// Independent of the chat model provider — allows e.g. Anthropic users
    /// to use OpenAI for embeddings while chatting with Claude.
    #[serde(default)]
    pub embedding_provider: Option<ProviderType>,
    /// Embedding model identifier (e.g., "text-embedding-3-small").
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Offer the per-conversation move between this machine and a hosted
    /// server (AGE-308). Developer-only until online mode is account-scoped:
    /// a move carries the transcript and nothing else today — no memory, no
    /// MCP, no skills — so the default build does not offer it at all.
    /// Conversations already marked hosted still load and run; this gates
    /// only the move UI.
    #[serde(default)]
    pub hosted_conversations_enabled: bool,
    /// How the agent's tools are offered to the model: all of them on
    /// every request, or a core plus groups loaded on demand. Absent from
    /// the file while it is the default, so existing settings (and their
    /// snapshots) do not change.
    #[serde(default, skip_serializing_if = "ToolLoading::is_all")]
    pub tool_loading: ToolLoading,
    /// Offer the `ask_user` tool (the input-required chain, ADR-0011 C7) to
    /// the model. On by default; an unattended run (e.g. a benchmark
    /// harness with no one to answer) sets this off via `chatty-tui
    /// --disable ask-user` so a stray call fails fast instead of blocking.
    /// Skipped when on, like `tool_loading`, so persisted settings files and
    /// snapshots stay byte-for-byte what they were until a run turns it off.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub ask_user_enabled: bool,
    /// Offer `terminal_read`, a read-only view of the user's own terminals
    /// (their tmux panes), to the model (AGE-577). Off by default: a
    /// terminal can show anything, secrets included. The tool is registered
    /// only when this is on and a terminal source has something to read.
    /// Skipped when off, like `tool_loading`, so persisted settings files and
    /// snapshots stay byte-for-byte what they were.
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminal_access: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

fn default_true() -> bool {
    true
}

/// No cap: a cap of 10 ended real tasks mid-way — the same local model
/// through Codex CLI (which has none) needed more than 50 commands for 5 of
/// its 8 SWE-bench wins.
fn default_max_agent_turns() -> u32 {
    0
}

impl Default for ExecutionSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in by default for security
            approval_mode: ApprovalMode::AutoApproveSandboxed,
            workspace_dir: None,
            filesystem_read_enabled: true, // Enabled by default when workspace is set
            filesystem_write_enabled: true, // Enabled by default when workspace is set
            fetch_enabled: true,           // Enabled by default for zero-config web access
            git_enabled: false,            // Opt-in: requires workspace with git repo
            browser_enabled: false,        // Opt-in: may download a Chrome build on first use
            allow_private_network_access: false, // Opt-in: SSRF guard blocks private IPs by default
            execute_code_enabled: false,   // Opt-in: exposes execute_code to the model
            docker_code_execution_enabled: false, // Opt-in: requires Docker
            docker_host: None,
            timeout_seconds: 30,
            max_output_bytes: 51200, // 50KB
            network_isolation: false,
            max_agent_turns: default_max_agent_turns(),
            memory_enabled: true, // Enabled by default for cross-conversation recall
            warn_on_external_agent: false,
            embedding_enabled: false, // Opt-in: requires embedding provider
            embedding_provider: None,
            embedding_model: None,
            hosted_conversations_enabled: false, // Developer-only until online mode is account-scoped
            tool_loading: ToolLoading::All,
            ask_user_enabled: true,
            terminal_access: false, // Opt-in: the terminal may show secrets
        }
    }
}

#[cfg(test)]
mod ask_user_serde_tests {
    use super::ExecutionSettingsModel;

    /// A settings file or snapshot written before `ask_user_enabled` existed
    /// must serialize to the same bytes, and a missing key reads as on.
    #[test]
    fn ask_user_enabled_is_absent_when_on_and_defaults_on() {
        let json = serde_json::to_value(ExecutionSettingsModel::default()).unwrap();
        assert!(json.get("ask_user_enabled").is_none());

        let back: ExecutionSettingsModel = serde_json::from_value(json).unwrap();
        assert!(back.ask_user_enabled);

        let off = ExecutionSettingsModel {
            ask_user_enabled: false,
            ..Default::default()
        };
        let json = serde_json::to_value(&off).unwrap();
        assert_eq!(json["ask_user_enabled"], false);
        let back: ExecutionSettingsModel = serde_json::from_value(json).unwrap();
        assert!(!back.ask_user_enabled);
    }

    /// `terminal_access` is off by default and, like `ask_user_enabled`,
    /// absent from the serialized settings until it is turned on.
    #[test]
    fn terminal_access_is_off_by_default_and_absent_when_off() {
        let json = serde_json::to_value(ExecutionSettingsModel::default()).unwrap();
        assert!(json.get("terminal_access").is_none());
        let back: ExecutionSettingsModel = serde_json::from_value(json).unwrap();
        assert!(!back.terminal_access);

        let on = ExecutionSettingsModel {
            terminal_access: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&on).unwrap();
        assert_eq!(json["terminal_access"], true);
        let back: ExecutionSettingsModel = serde_json::from_value(json).unwrap();
        assert!(back.terminal_access);
    }
}
