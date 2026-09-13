//! What building an agent needs, and the one place a host's settings and
//! services are turned into it.
//!
//! [`AgentBuildContext`] has two halves. The *services* half — execution
//! settings, secrets, the memory/skill/embedding services, the agents the
//! agent may delegate to — is gathered by whichever host is building the
//! agent (chatty-gpui, chatty-tui, and `chatty-server` in the `hive` repo)
//! and is the same everywhere; it is described by [`AgentServices`] and
//! mapped onto the context by [`AgentBuildContext::from_services`]. The
//! other half — the per-conversation stores, the MCP tool list, the desktop's
//! theme palette — is host- or conversation-specific and is written by the
//! caller on top of that base.
//!
//! Adding a field to `AgentBuildContext` therefore fails to compile in
//! `from_services` and nowhere else, which is the point: a host cannot
//! silently drop it.

use super::tool_profile::ToolProfile;
use crate::services::embedding_service::EmbeddingService;
use crate::services::memory_service::MemoryService;
use crate::services::shell_service::ShellSession;
use crate::services::skill_service::SkillService;
use crate::settings::models::ExecutionSettingsModel;
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::models::search_settings::SearchSettingsModel;
use crate::tools::{LocalModuleAgentSummary, PendingArtifacts};

/// Contextual dependencies for building an agent.
///
/// Groups the many optional services and settings needed by
/// `AgentClient::from_model_config_with_tools()` and `Conversation::new/from_data()`.
pub struct AgentBuildContext {
    pub mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
    pub exec_settings: Option<ExecutionSettingsModel>,
    pub pending_approvals: Option<crate::models::execution_approval_store::PendingApprovals>,
    pub pending_clarifications: Option<crate::models::clarification_store::PendingClarifications>,
    pub pending_write_approvals: Option<crate::models::write_approval_store::PendingWriteApprovals>,
    /// Sink for artifacts (e.g. attachments) queued mid-stream by tools like
    /// `AddAttachmentTool`, so the desktop transcript can mint a card for
    /// them. Always `None` from chatty-tui, which has no artifact viewport.
    pub pending_artifacts: Option<PendingArtifacts>,
    pub shell_session: Option<std::sync::Arc<ShellSession>>,
    pub user_secrets: Vec<(String, String)>,
    /// Theme palette handed to `CreateChartTool` so generated charts match
    /// the desktop app's theme. Always `None` from chatty-tui, which has no
    /// themed chart rendering.
    pub theme_colors: Option<[String; 5]>,
    pub memory_service: Option<MemoryService>,
    pub skill_service: Option<SkillService>,
    pub search_settings: Option<SearchSettingsModel>,
    pub embedding_service: Option<EmbeddingService>,
    pub module_agents: Vec<LocalModuleAgentSummary>,
    pub gateway_port: Option<u16>,
    /// The broker's virtual agents by name — `local-agent`, or what
    /// `module_settings.virtual_agents` declares (ADR-0011 C10). Only
    /// addressable while `gateway_port` is set.
    pub local_agents: Vec<String>,
    pub remote_agents: Vec<A2aAgentConfig>,
    /// Conversation this turn belongs to. Only consulted when the `browser`
    /// feature is on, to register the built `BrowserManager` where the
    /// artifact viewport (AGE-155) can find it — `None` is fine anywhere
    /// else (e.g. chatty-tui, which doesn't enable that feature).
    pub conversation_id: Option<String>,
    /// The role this agent runs as (ADR-0011 C11). Empty for an ordinary
    /// chat agent; a declared virtual agent's worker carries the role its
    /// `VirtualAgentConfig` names, via `chatty-tui`'s `--preamble` /
    /// `--tools` flags.
    ///
    /// It sits here rather than in [`AgentServices`] because only a worker
    /// has one: every other host would have to write `AgentRole::default()`
    /// for a field it never sets.
    pub role: AgentRole,
}

/// What makes one worker a reviewer and another a coder (ADR-0011 C11):
/// standing instructions, and a named set of tools.
///
/// Both halves are optional and independent — a role may be only a preamble,
/// only a profile, or both. `Default` is "no role", which is every agent that
/// is not a declared virtual agent's worker.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentRole {
    /// Appended to the system prompt right after the base preamble, before
    /// the tool summary, so the worker knows what it is before it reads what
    /// it can do.
    pub preamble: Option<String>,
    /// The tool allowlist the agent is built with. `None` leaves it every
    /// tool the execution settings allow.
    pub profile: Option<&'static ToolProfile>,
}

/// The services half of an [`AgentBuildContext`]: what a host gathers from
/// its own settings before building an agent.
///
/// Every field is the value the host has already decided on — `exec_settings`
/// in particular, since the hosts disagree about the gate (see
/// [`gated_exec_settings`]). `Default` gives the no-services agent that tests
/// and the headless paths want.
#[derive(Default)]
pub struct AgentServices {
    pub exec_settings: Option<ExecutionSettingsModel>,
    pub user_secrets: Vec<(String, String)>,
    pub memory_service: Option<MemoryService>,
    pub skill_service: Option<SkillService>,
    pub search_settings: Option<SearchSettingsModel>,
    pub embedding_service: Option<EmbeddingService>,
    pub module_agents: Vec<LocalModuleAgentSummary>,
    pub gateway_port: Option<u16>,
    /// `ModuleSettingsModel::virtual_agent_names()` on the host's settings.
    pub local_agents: Vec<String>,
    pub remote_agents: Vec<A2aAgentConfig>,
}

/// The execution settings an agent should be built with: `Some` when any
/// execution-related setting is on, `None` otherwise. This is the gate that
/// decides whether the agent gets execution tools at all.
///
/// Hosts that build an agent once and keep it — chatty-tui, and
/// `chatty-server` in the `hive` repo — apply it. chatty-gpui does not: it
/// passes its settings through unconditionally and rebuilds the agent when
/// they change.
pub fn gated_exec_settings(settings: &ExecutionSettingsModel) -> Option<ExecutionSettingsModel> {
    let any_tool_enabled = settings.enabled
        || settings.filesystem_read_enabled
        || settings.filesystem_write_enabled
        || settings.fetch_enabled
        || settings.git_enabled
        || settings.execute_code_enabled;
    any_tool_enabled.then(|| settings.clone())
}

impl AgentBuildContext {
    /// The context a host's services imply, with everything conversation- or
    /// host-specific left unset. Callers add what they have on top:
    ///
    /// ```ignore
    /// AgentBuildContext {
    ///     mcp_tools,
    ///     conversation_id: Some(conv_id.clone()),
    ///     ..AgentBuildContext::from_services(services)
    /// }
    /// ```
    pub fn from_services(services: AgentServices) -> Self {
        let AgentServices {
            exec_settings,
            user_secrets,
            memory_service,
            skill_service,
            search_settings,
            embedding_service,
            module_agents,
            gateway_port,
            local_agents,
            remote_agents,
        } = services;
        Self {
            // Gathering the MCP tool list is async; every host does it
            // separately and sets this itself.
            mcp_tools: None,
            exec_settings,
            // The conversation's session owns these (AGE-272); it fills them
            // in via `AgentSession::build_context`.
            pending_approvals: None,
            pending_clarifications: None,
            pending_write_approvals: None,
            pending_artifacts: None,
            // Created inside the factory when execution is enabled, unless
            // the caller has one to reuse.
            shell_session: None,
            user_secrets,
            theme_colors: None,
            memory_service,
            skill_service,
            search_settings,
            embedding_service,
            module_agents,
            gateway_port,
            local_agents,
            remote_agents,
            conversation_id: None,
            // Only a declared virtual agent's worker has a role, and it
            // arrives on the process's argv rather than from the host's
            // services (see `AgentBuildContext::role`).
            role: AgentRole::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_off() -> ExecutionSettingsModel {
        ExecutionSettingsModel {
            enabled: false,
            filesystem_read_enabled: false,
            filesystem_write_enabled: false,
            fetch_enabled: false,
            git_enabled: false,
            execute_code_enabled: false,
            ..ExecutionSettingsModel::default()
        }
    }

    #[test]
    fn execution_settings_are_withheld_when_every_execution_setting_is_off() {
        assert!(gated_exec_settings(&all_off()).is_none());
    }

    /// One flag at a time, so a gate that forgets a flag fails here rather
    /// than silently building an agent without the tool the user turned on.
    #[test]
    fn any_single_execution_setting_lets_the_settings_through() {
        let opens_the_gate = |turn_on: fn(&mut ExecutionSettingsModel)| {
            let mut settings = all_off();
            turn_on(&mut settings);
            gated_exec_settings(&settings).is_some()
        };
        assert!(opens_the_gate(|s| s.enabled = true), "enabled");
        assert!(
            opens_the_gate(|s| s.filesystem_read_enabled = true),
            "filesystem_read_enabled"
        );
        assert!(
            opens_the_gate(|s| s.filesystem_write_enabled = true),
            "filesystem_write_enabled"
        );
        assert!(opens_the_gate(|s| s.fetch_enabled = true), "fetch_enabled");
        assert!(opens_the_gate(|s| s.git_enabled = true), "git_enabled");
        assert!(
            opens_the_gate(|s| s.execute_code_enabled = true),
            "execute_code_enabled"
        );
    }

    /// Pins that each service lands in its own field. Several fields are
    /// `Option<..>` of unrelated services and two are `Vec`s, so a
    /// transposition is invisible to the type system — which is exactly what
    /// a hand-copied assembly gets wrong.
    #[test]
    fn each_service_lands_in_its_own_field() {
        let exec = ExecutionSettingsModel {
            enabled: true,
            workspace_dir: Some("/tmp/ws".to_string()),
            ..ExecutionSettingsModel::default()
        };
        let search = SearchSettingsModel {
            max_results: 42,
            ..SearchSettingsModel::default()
        };

        let ctx = AgentBuildContext::from_services(AgentServices {
            exec_settings: Some(exec),
            user_secrets: vec![("KEY".to_string(), "value".to_string())],
            memory_service: None,
            skill_service: None,
            search_settings: Some(search),
            embedding_service: None,
            module_agents: vec![LocalModuleAgentSummary {
                name: "echo".to_string(),
                version: "0.1.0".to_string(),
                description: "echoes".to_string(),
                tools: Vec::new(),
                supports_a2a: false,
                execution_mode: "local".to_string(),
            }],
            gateway_port: Some(4242),
            local_agents: vec!["local-coder".to_string()],
            remote_agents: vec![A2aAgentConfig {
                name: "remote".to_string(),
                url: "http://127.0.0.1:9000".to_string(),
                api_key: None,
                enabled: true,
                skills: Vec::new(),
            }],
        });

        assert_eq!(
            ctx.exec_settings.as_ref().unwrap().workspace_dir.as_deref(),
            Some("/tmp/ws")
        );
        assert_eq!(
            ctx.user_secrets,
            vec![("KEY".to_string(), "value".to_string())]
        );
        assert_eq!(ctx.search_settings.as_ref().unwrap().max_results, 42);
        assert_eq!(ctx.module_agents.len(), 1);
        assert_eq!(ctx.module_agents[0].name, "echo");
        assert_eq!(ctx.gateway_port, Some(4242));
        assert_eq!(ctx.local_agents, vec!["local-coder".to_string()]);
        assert_eq!(ctx.remote_agents.len(), 1);
        assert_eq!(ctx.remote_agents[0].name, "remote");

        // The conversation's half stays unset for the caller to fill in.
        assert!(ctx.mcp_tools.is_none());
        assert!(ctx.pending_approvals.is_none());
        assert!(ctx.pending_clarifications.is_none());
        assert!(ctx.pending_write_approvals.is_none());
        assert!(ctx.pending_artifacts.is_none());
        assert!(ctx.shell_session.is_none());
        assert!(ctx.theme_colors.is_none());
        assert!(ctx.conversation_id.is_none());
        assert_eq!(ctx.role, AgentRole::default());
    }
}
