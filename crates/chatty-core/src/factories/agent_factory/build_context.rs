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
//!
//! [`AgentBuildContext::from_spec`] is the other way in: an [`AgentSpec`]
//! (PL-D2) laid over the same services, which is how every agent that is
//! declared rather than configured — a `--team` leader, a delegated worker,
//! `chatty-tui --agent` — gets its role, tools and budgets.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::tool_profile::{ToolProfile, tool_profile};
use crate::agent_spec::{AgentSpec, PluginSpec, SpecErrors};
use crate::services::embedding_service::EmbeddingService;
use crate::services::lazy_broker::LazyBroker;
use crate::services::memory_service::MemoryService;
use crate::services::shell_service::ShellSession;
use crate::services::skill_service::SkillService;
use crate::services::spend_gate::{SpendGate, TaskSpendGate};
use crate::services::team::TeamSkill;
use crate::services::terminal::TerminalSource;
use crate::settings::models::ExecutionSettingsModel;
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::models::execution_settings::set_tool_group;
use crate::settings::models::search_settings::SearchSettingsModel;
use crate::tools::plugin_tool::PluginHost;
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
    /// A broker that has not necessarily started yet (BI-2, AGE-634): when
    /// set, `list_agents`/`invoke_agent` start it themselves on first use
    /// instead of expecting `gateway_port` to already be live. Hosts that
    /// still resolve a port eagerly (tests, `chatty-server` in `hive`) leave
    /// this `None`.
    pub lazy_broker: Option<Arc<dyn LazyBroker>>,
    /// The broker's virtual agents by name — `local-agent`, or what
    /// `module_settings.virtual_agents` declares (ADR-0011 C10). Only
    /// addressable while `gateway_port`, `lazy_broker` or `fabric_transport`
    /// is set.
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
    /// The hosted per-user spend cap, asked by `invoke_agent` before it
    /// starts a delegation (AGE-416 / ADR-0010). Only `chatty-server` in
    /// the `hive` repo sets it, on top of [`Self::from_services`]; every
    /// other host leaves it `None`, which means no check at all.
    pub spend_gate: Option<std::sync::Arc<dyn SpendGate>>,
    /// The skill beside the team file a `--team` leader runs under (ADR-0011
    /// C13, AGE-407), served by `read_skill` ahead of the skill directories.
    /// Only chatty-tui's `--team` sets it, on top of [`Self::from_services`];
    /// a worker or an ordinary chat agent has none.
    pub team_skill: Option<TeamSkill>,
    /// Nobody is watching this run (headless, pipe, a delegated worker):
    /// the system prompt says so, so the model finishes the work instead of
    /// offering to. Only chatty-tui's headless runner sets it.
    pub unattended: bool,
    /// Whether this run's task asks for an answer file (`answer.txt`),
    /// when the host knows before the agent exists — chatty-tui's
    /// `--headless` reads it off `--message`. `Some(false)`: `final_answer`
    /// writes nothing, so a coding run cannot leave an answer.txt behind.
    /// `None` (every other host) keeps it writing as before.
    pub answer_file: Option<bool>,
    /// The host's `ExecutionSettingsModel::ask_user_enabled`, carried
    /// separately because a gating host (see [`gated_exec_settings`]) hands
    /// the factory no execution settings at all when every tool group is off
    /// — and a run with every group plus `ask-user` disabled must still
    /// drop `ask_user`. `from_services` sets it `true`; the factory offers
    /// the tool only when this and `exec_settings` (if any) both allow it.
    pub ask_user_enabled: bool,
    /// The directory whose `AGENTS.md` / `CLAUDE.md` apply (AGE-589),
    /// carried separately for the same reason as `ask_user_enabled`: a
    /// gating host hands over no execution settings, and so no workspace,
    /// when every tool group is off, yet the project's instructions still
    /// apply. `None` falls back to `exec_settings.workspace_dir`.
    pub instructions_dir: Option<std::path::PathBuf>,
    /// The desktop's embedded terminal tabs (AGE-583), a live registry the
    /// dock keeps current. When set, `terminal_read` is always offered and
    /// reads the tabs the human shared (per tab, at call time); a tab never
    /// shared is never read. Only chatty-gpui sets it, on top of
    /// [`Self::from_services`]; chatty-tui and hive keep the tmux-only gate.
    pub embedded_terminals: Option<std::sync::Arc<dyn TerminalSource>>,
    /// The connection a delegated worker reaches its local roles and the
    /// broker's directory through (ADR-0020, BI-4): `invoke_agent` and
    /// `list_agents` use it instead of loopback HTTP. Only chatty-tui's
    /// participant mode sets it, on top of [`Self::from_services`], after
    /// the broker's `welcome` and before the agent is built; the in-process
    /// root reaches its broker through [`LazyBroker::transport`] instead.
    pub fabric_transport: Option<Arc<dyn chatty_fabric::Transport>>,
    /// The name the broker's `welcome` gave this worker's owner, set with
    /// [`Self::fabric_transport`]: what the worker's `send_message` names as
    /// its recipient (tree messages, TM-1). `None` beside a transport means
    /// the root owns it ([`chatty_fabric::ROOT_NAME`]).
    pub fabric_owner: Option<String>,
    /// The spec's `[[plugins]]` (PL-U2, AGE-616): loaded by the factory, one
    /// instance per plugin for this agent, their tools registered beside the
    /// native ones. Empty for an agent that is not built from a spec.
    pub plugins: Vec<PluginSpec>,
    /// Where `plugins` are found and what their `llm::complete` may reach.
    pub plugin_host: PluginHost,
}

/// What makes one worker a reviewer and another a coder (ADR-0011 C11):
/// standing instructions, and a named set of tools.
///
/// Both halves are optional and independent — a role may be only a preamble,
/// only a profile, or both. `Default` is "no role", which is every agent that
/// is not built from a spec.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRole {
    /// Appended to the system prompt right after the base preamble, before
    /// the tool summary, so the worker knows what it is before it reads what
    /// it can do.
    pub preamble: Option<String>,
    /// The tool allowlist the agent is built with. `None` leaves it every
    /// tool the execution settings allow.
    pub profile: Option<&'static ToolProfile>,
    /// Whether it is offered `list_agents` and `invoke_agent` (PL-S2 DP-1).
    /// A spec's non-empty `swarm.delegates_to` decides it, never the
    /// profile; an agent with no role (the desktop's, a hosted one) keeps
    /// them.
    pub delegates: bool,
}

impl Default for AgentRole {
    fn default() -> Self {
        Self {
            preamble: None,
            profile: None,
            delegates: true,
        }
    }
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
    /// A broker that has not necessarily started yet (BI-2, AGE-634). See
    /// [`AgentBuildContext::lazy_broker`].
    pub lazy_broker: Option<Arc<dyn LazyBroker>>,
    /// `ModuleSettingsModel::virtual_agent_names()` on the host's settings.
    pub local_agents: Vec<String>,
    pub remote_agents: Vec<A2aAgentConfig>,
    /// Where a spec's plugins are found (PL-U2): the host's module
    /// directory and its configured models. Unused unless the agent is
    /// built from a spec that lists plugins.
    pub plugin_host: PluginHost,
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
        || settings.execute_code_enabled
        || settings.terminal_access;
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
            lazy_broker,
            local_agents,
            remote_agents,
            plugin_host,
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
            lazy_broker,
            local_agents,
            remote_agents,
            conversation_id: None,
            // Only a declared virtual agent's worker has a role, and it
            // arrives on the process's argv rather than from the host's
            // services (see `AgentBuildContext::role`).
            role: AgentRole::default(),
            // Only a hosted leader has a cap to ask about; hive sets it on
            // top of this base.
            spend_gate: None,
            // Only a `--team` leader has one (see `AgentBuildContext::team_skill`).
            team_skill: None,
            unattended: false,
            // Only chatty-tui's `--headless` knows its task up front.
            answer_file: None,
            // chatty-tui overrides this with its own flag; everyone else
            // leaves the decision to `exec_settings`.
            ask_user_enabled: true,
            // chatty-tui sets it from its ungated settings; everyone else
            // leaves it to `exec_settings`.
            instructions_dir: None,
            // Only the desktop has embedded terminals.
            embedded_terminals: None,
            // Only a delegated worker has a connection to its broker.
            fabric_transport: None,
            fabric_owner: None,
            // Only a spec lists plugins (`from_spec`).
            plugins: Vec::new(),
            plugin_host,
        }
    }
}

/// What running as a spec means for a host: the context the agent is built
/// with, plus what a context does not carry because the host decides it
/// outside the agent — which model to run, the run's wall-clock budget, and
/// the handle the host reports the task's spend to.
pub struct SpecBuild {
    pub context: AgentBuildContext,
    /// `agent.model`, for the host to resolve as it resolves `--model`.
    pub model: Option<String>,
    /// `budget.max_duration`: the `Deadline` the host's runner starts.
    pub max_duration: Option<Duration>,
    /// `budget.cap_usd`'s gate, also installed as the context's
    /// `spend_gate`.
    pub task_spend: Option<TaskSpendGate>,
}

impl AgentBuildContext {
    /// The context `spec` implies over a host's services — the one place a
    /// spec becomes an agent (AGE-614).
    ///
    /// `services.exec_settings` is the host's *ungated* settings: the spec's
    /// `budget.max_agent_turns` and `tools.disable` are applied to them and
    /// only then is [`gated_exec_settings`] asked, so a spec that disables
    /// the last group builds an agent with no execution tools, and
    /// `ask_user_enabled` / `instructions_dir` read the narrowed settings as
    /// every host does. `tools.profile` and `agent.preamble` become the
    /// role, `tools.skills` a line of the preamble, `budget.cap_usd` the
    /// spend gate, `plugins` the plugins the factory loads (PL-U2) from
    /// `services.plugin_host`, and a non-empty `swarm.delegates_to` the
    /// delegation tools (DP-1).
    pub fn from_spec(spec: &AgentSpec, services: AgentServices) -> Result<SpecBuild, SpecErrors> {
        spec.validate(None)?;
        let mut services = services;
        let settings = services.exec_settings.take().map(|mut settings| {
            if let Some(turns) = spec.budget.max_agent_turns {
                settings.max_agent_turns = turns;
            }
            for group in &spec.tools.disable {
                set_tool_group(&mut settings, group, false).expect("validated as a tool group");
            }
            settings
        });
        let task_spend = spec.budget.cap_usd.map(TaskSpendGate::new);
        let context = Self {
            role: AgentRole {
                preamble: role_preamble(spec),
                profile: spec.tools.profile.as_deref().and_then(tool_profile),
                delegates: !spec.swarm.delegates_to.is_empty(),
            },
            spend_gate: task_spend
                .clone()
                .map(|gate| Arc::new(gate) as Arc<dyn SpendGate>),
            ask_user_enabled: settings.as_ref().is_none_or(|s| s.ask_user_enabled),
            instructions_dir: settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
                .map(PathBuf::from),
            plugins: spec.plugins.clone(),
            ..Self::from_services(AgentServices {
                exec_settings: settings.as_ref().and_then(gated_exec_settings),
                ..services
            })
        };
        Ok(SpecBuild {
            context,
            model: spec.agent.model.clone(),
            max_duration: spec.max_duration(),
            task_spend,
        })
    }
}

/// The role's preamble: the spec's own, then the skills it names.
fn role_preamble(spec: &AgentSpec) -> Option<String> {
    if spec.tools.skills.is_empty() {
        return spec.agent.preamble.clone();
    }
    let skills = format!(
        "Before you start, read these skills with read_skill and follow them: {}.",
        spec.tools.skills.join(", ")
    );
    Some(match spec.agent.preamble.as_deref() {
        Some(preamble) => format!("{preamble}\n\n{skills}"),
        None => skills,
    })
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
        assert!(
            opens_the_gate(|s| s.terminal_access = true),
            "terminal_access"
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
            lazy_broker: None,
            local_agents: vec!["local-coder".to_string()],
            remote_agents: vec![A2aAgentConfig {
                name: "remote".to_string(),
                url: "http://127.0.0.1:9000".to_string(),
                api_key: None,
                enabled: true,
                skills: Vec::new(),
            }],
            plugin_host: PluginHost {
                module_roots: vec![PathBuf::from("/modules")],
                ..PluginHost::default()
            },
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
        assert_eq!(
            ctx.plugin_host.module_roots,
            vec![PathBuf::from("/modules")]
        );
        assert!(ctx.plugins.is_empty(), "only a spec lists plugins");

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
        assert!(ctx.spend_gate.is_none());
    }

    #[test]
    fn a_spec_narrows_the_hosts_settings_and_names_the_role() {
        let spec = AgentSpec::from_toml(
            r#"
[agent]
name = "local-reviewer"
model = "qwen3:4b"
preamble = "Review."

[tools]
profile = "reviewer"
disable = ["fs_write", "ask-user"]
skills = ["coder-reviewer"]

[[plugins]]
module = "echo-agent"

[budget]
max_agent_turns = 30
max_duration = "1h"
cap_usd = 2.5
"#,
        )
        .unwrap();
        let host = ExecutionSettingsModel {
            filesystem_write_enabled: true,
            workspace_dir: Some("/ws".to_string()),
            ..ExecutionSettingsModel::default()
        };
        let built = AgentBuildContext::from_spec(
            &spec,
            AgentServices {
                exec_settings: Some(host),
                local_agents: vec!["local-agent".to_string()],
                ..AgentServices::default()
            },
        )
        .unwrap();
        let ctx = &built.context;
        let settings = ctx
            .exec_settings
            .as_ref()
            .expect("fs-read keeps the gate open");
        assert!(!settings.filesystem_write_enabled);
        assert_eq!(settings.max_agent_turns, 30);
        assert!(!ctx.ask_user_enabled);
        assert_eq!(ctx.instructions_dir, Some(PathBuf::from("/ws")));
        assert_eq!(ctx.role.profile.map(|p| p.name()), Some("reviewer"));
        assert_eq!(
            ctx.role.preamble.as_deref(),
            Some(
                "Review.\n\nBefore you start, read these skills with read_skill and follow them: \
                 coder-reviewer."
            )
        );
        assert!(ctx.spend_gate.is_some());
        assert_eq!(built.task_spend.unwrap().cap_usd(), 2.5);
        assert_eq!(built.model.as_deref(), Some("qwen3:4b"));
        assert_eq!(built.max_duration, Some(Duration::from_secs(3600)));
        assert_eq!(ctx.local_agents, vec!["local-agent".to_string()]);
        assert_eq!(
            ctx.plugins, spec.plugins,
            "the factory loads the spec's plugins"
        );
    }

    /// The tool names `spec` is built with: the tools of the first request
    /// its agent sends to a fake model.
    async fn tools_of(spec: &AgentSpec) -> Vec<String> {
        use crate::settings::models::models_store::ModelConfig;
        use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
        use crate::testing::fake_model::{FakeDaemon, Reply, Script};
        use rig_agent::completion::Prompt;

        let _ = crate::init_repositories();
        let daemon = FakeDaemon::scripted(Script::new().route("dp1-model", [Reply::text("done")]));
        let workspace = tempfile::tempdir().expect("a workspace");
        let built = AgentBuildContext::from_spec(
            spec,
            AgentServices {
                exec_settings: Some(ExecutionSettingsModel {
                    workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
                    fetch_enabled: false,
                    ..ExecutionSettingsModel::default()
                }),
                local_agents: vec!["local-coder".to_string()],
                ..AgentServices::default()
            },
        )
        .expect("the spec builds");
        let model = ModelConfig::new(
            "dp1-model".to_string(),
            "dp1-model".to_string(),
            ProviderType::Ollama,
            "dp1-model".to_string(),
        );
        let provider = ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
            .with_base_url(daemon.base_url());
        let agent = super::super::AgentClient::from_model_config_with_tools(
            &model,
            &provider,
            built.context,
        )
        .await
        .expect("the agent builds");
        // Only the request matters: the fake answers every request as a
        // stream, which a one-shot `prompt` need not parse.
        let _ = agent.client.agent.prompt("go").await;
        let request = daemon.requests().into_iter().next().expect("one request");
        request.json()["tools"]
            .as_array()
            .expect("the request carries tools")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
            .collect()
    }

    /// DP-1: a profile no longer implies delegation. A `coordinator` with an
    /// empty `delegates_to` has neither `invoke_agent` nor `list_agents`;
    /// the same spec listing an agent has both, and so does a `coder` — the
    /// spec decides, whatever the profile.
    #[tokio::test]
    async fn profile_alone_grants_no_delegation() {
        let mut spec = AgentSpec::named("lead");
        spec.tools.profile = Some("coordinator".to_string());
        let tools = tools_of(&spec).await;
        assert!(tools.contains(&"read_file".to_string()), "{tools:?}");
        assert!(!tools.contains(&"invoke_agent".to_string()), "{tools:?}");
        assert!(!tools.contains(&"list_agents".to_string()), "{tools:?}");

        spec.swarm.delegates_to = vec!["local-coder".to_string()];
        let tools = tools_of(&spec).await;
        assert!(tools.contains(&"invoke_agent".to_string()), "{tools:?}");
        assert!(tools.contains(&"list_agents".to_string()), "{tools:?}");

        spec.tools.profile = Some("coder".to_string());
        let tools = tools_of(&spec).await;
        assert!(tools.contains(&"invoke_agent".to_string()), "{tools:?}");
        assert!(
            !tools.contains(&"write_todos".to_string()),
            "the coder profile applies: {tools:?}"
        );
    }

    #[test]
    fn an_invalid_spec_builds_nothing() {
        let mut spec = AgentSpec::named("x");
        spec.tools.profile = Some("wizard".to_string());
        assert!(AgentBuildContext::from_spec(&spec, AgentServices::default()).is_err());
    }
}
