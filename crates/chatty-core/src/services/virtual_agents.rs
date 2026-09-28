//! The broker's virtual agents, resolved from agent specs once for both
//! frontends (ADR-0011 C10 / AGE-377, AGE-614).
//!
//! A worker is a `chatty-tui` child, and everything that makes one worker
//! differ from another is its [`AgentSpec`], which travels on the child's
//! argv as `--agent-json <spec>` — the spec's wire form — so the child
//! builds itself through [`AgentBuildContext::from_spec`] exactly as a
//! `--agent` run would. The leader's own provider flags (`--ollama`,
//! `--openai-compat-url`, `--api-key`) tell a child with no `providers.json`
//! — a Harbor sandbox — where the model server is. A *role* is never a
//! parameter on `invoke_agent`: the leader's tool schema and prompt prefix
//! stay identical whatever the team.
//!
//! This decides the argv and the endpoint to meter for each declared agent.
//! Wrapping that in a `LocalRunner` is left to the caller, since that type
//! lives in `chatty-protocol-gateway`, which this crate does not depend on
//! (see [`worker_endpoint`](super::worker_endpoint)).
//!
//! [`AgentBuildContext::from_spec`]: crate::factories::AgentBuildContext::from_spec

use crate::agent_spec::AgentSpec;
use crate::factories::agent_factory::tool_profile;
use crate::settings::models::ModuleSettingsModel;
use crate::settings::models::execution_settings::canonical_tool_group;
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};
use crate::settings::models::providers_store::ProviderConfig;
use crate::tools::LOCAL_AGENT_NAME;

use super::worker_endpoint::resolve_worker_endpoint;

/// The flag a worker's spec rides on.
pub const AGENT_JSON_FLAG: &str = "--agent-json";

/// One virtual agent the broker publishes: the name callers address, the
/// card text that says what it runs, its children's argv, and the model
/// endpoint they are metered on.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualAgentSpec {
    /// The name served at `/a2a/{name}`.
    pub name: String,
    /// The card description: the model and the disabled tool groups, so a
    /// leader reading `list_agents` can choose without guessing.
    pub description: String,
    /// Every child's arguments, ahead of the `--participant-*` pair the
    /// runner adds.
    pub args: Vec<String>,
    /// The base URL of the model server its children talk to and how many
    /// may hold it at once; `None` leaves the agent unmetered.
    pub endpoint: Option<(String, usize)>,
    /// The team's verification command, for this agent (AGE-406). The
    /// team's `verification` when it declared one and this agent's profile
    /// has a shell; `None` otherwise.
    pub verification: Option<String>,
    /// The schema this agent's answers must match, when its team names one
    /// (TD-2, AGE-693). Never set here: a team's `handoffs` are the
    /// broker's to attach, since module settings have none.
    pub handoff: Option<chatty_fabric::HandoffContract>,
}

/// Resolve every virtual agent the broker should publish.
///
/// One per spec in `agents` — the roster `module_settings.virtual_agents`
/// (or a team) names, already loaded — or the single default `local-agent`
/// when it is empty. `module_settings` supplies the endpoint budgets and the
/// team's verification command. `common_args` is appended to every agent's
/// argv after its spec — `--auto-approve` when the leader runs unattended,
/// and the leader's provider flags when it was configured by flags rather
/// than by a config dir the child would read too.
pub fn resolve_virtual_agents(
    models: &[ModelConfig],
    providers: &[ProviderConfig],
    module_settings: &ModuleSettingsModel,
    agents: &[AgentSpec],
    common_args: &[String],
) -> Vec<VirtualAgentSpec> {
    let default_agent = [AgentSpec::named(LOCAL_AGENT_NAME)];
    let declared: &[AgentSpec] = if agents.is_empty() {
        &default_agent
    } else {
        agents
    };

    declared
        .iter()
        .map(|spec| {
            // No `--broker`, even for an agent that delegates in turn: a
            // sub-leader delegates over the connection its broker makes for
            // it, and only a root process starts a broker (BI-5). Which
            // agents it may reach is the spawn context's roster.
            let mut args = vec![
                AGENT_JSON_FLAG.to_string(),
                spec.to_json().expect("an agent spec serializes"),
            ];
            args.extend(common_args.iter().cloned());

            let model = spec.agent.model.as_deref();
            let endpoint = resolve_worker_endpoint(models, providers, module_settings, model);
            if endpoint.is_none() {
                tracing::warn!(
                    agent = %spec.agent.name,
                    model = ?model,
                    "Virtual agent's model resolves to no configured provider; its workers are unmetered"
                );
            }

            VirtualAgentSpec {
                name: spec.agent.name.clone(),
                description: describe(spec, models),
                args,
                endpoint,
                verification: verification_for(spec, module_settings),
                handoff: None,
            }
        })
        .collect()
}

/// The tool group a spec disables when a worker is to run without a shell,
/// and the tool a named profile has to allow for the same thing (AGE-406,
/// "Do not" item 2).
const SHELL_TOOL_GROUP: &str = "shell";
const SHELL_TOOL_NAME: &str = "shell_execute";

/// The team's verification command, unless this agent's profile has no
/// shell.
///
/// A worker that cannot run commands did not produce a build, so running
/// the suite in its tree would report the leader's own state back as the
/// worker's. `tools.profile` and `tools.disable` compose (AGE-452): a named
/// profile has to allow `shell_execute` *and* `disable` has to leave `shell`
/// enabled, so a `reviewer` runs the suite unless it also disables `shell`,
/// and a `coordinator` never does regardless of `disable`.
fn verification_for(spec: &AgentSpec, module_settings: &ModuleSettingsModel) -> Option<String> {
    let profile_has_shell = match spec.tools.profile.as_deref() {
        // An unknown profile name fails the spec's validation, and so the
        // child at start-up, so what this answers for it never matters.
        Some(profile) => tool_profile(profile).is_none_or(|p| p.allows(SHELL_TOOL_NAME)),
        None => true,
    };
    let not_disabled = !spec
        .tools
        .disable
        .iter()
        .any(|group| canonical_tool_group(group) == SHELL_TOOL_GROUP);
    let has_shell = profile_has_shell && not_disabled;
    has_shell
        .then(|| module_settings.team.verification.clone())
        .flatten()
}

/// The card text: what the agent is, which model it runs, and which tool
/// groups it lacks.
fn describe(spec: &AgentSpec, models: &[ModelConfig]) -> String {
    let mut text = String::from(
        "A chatty agent in its own process, with its own workspace. \
         Delegate a self-contained task to it and it works autonomously \
         and reports back.",
    );
    match spec.agent.model.as_deref() {
        Some(model) => text.push_str(&format!(" Model: {model}.")),
        None => match resolve_model_query(models, None) {
            Some(model) => text.push_str(&format!(" Model: {} (the default).", model.name)),
            None => text.push_str(" Model: the configured default."),
        },
    }
    let disabled = &spec.tools.disable;
    if let Some(profile) = spec.tools.profile.as_deref() {
        text.push_str(&format!(" Tool profile: {profile}."));
    } else if disabled.is_empty() {
        text.push_str(" Tools: the full set.");
    }
    if !disabled.is_empty() {
        text.push_str(&format!(" Tool groups disabled: {}.", disabled.join(", ")));
    }
    if let Some(sentence) = first_sentence(spec.agent.preamble.as_deref()) {
        text.push_str(&format!(" Role: {sentence}"));
    }
    text
}

/// The first sentence of a role's preamble, for the card: enough for a leader
/// to pick the right agent, not the whole standing instruction.
fn first_sentence(preamble: Option<&str>) -> Option<String> {
    let preamble = preamble?.trim();
    if preamble.is_empty() {
        return None;
    }
    let end = preamble
        .find(['.', '!', '?', '\n'])
        .map(|i| i + 1)
        .unwrap_or(preamble.len());
    Some(preamble[..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::module_settings::TeamConfig;
    use crate::settings::models::providers_store::ProviderType;

    fn model(id: &str, provider: ProviderType) -> ModelConfig {
        ModelConfig::new(
            id.to_string(),
            id.to_uppercase(),
            provider,
            format!("vendor/{id}"),
        )
    }

    fn provider(kind: ProviderType, url: &str) -> ProviderConfig {
        let mut provider = ProviderConfig::new(url.to_string(), kind);
        provider.base_url = Some(url.to_string());
        provider
    }

    fn agent(name: &str, model: Option<&str>) -> AgentSpec {
        let mut spec = AgentSpec::named(name);
        spec.agent.model = model.map(str::to_string);
        spec
    }

    fn team() -> Vec<AgentSpec> {
        let mut reviewer = agent("local-reviewer", Some("gemma"));
        reviewer.tools.disable = vec!["fs-write".into(), "shell".into(), "git".into()];
        vec![agent("local-coder", Some("qwen")), reviewer]
    }

    fn spec_in(args: &[String]) -> AgentSpec {
        assert_eq!(args[0], AGENT_JSON_FLAG);
        AgentSpec::from_json(&args[1]).expect("the argv carries the spec")
    }

    /// Nothing declared is exactly the pre-C10 broker: one `local-agent`,
    /// the default model, only the common flags besides its bare spec.
    #[test]
    fn nothing_declared_is_the_one_default_worker() {
        let models = vec![model("qwen", ProviderType::Ollama)];
        let providers = vec![provider(ProviderType::Ollama, "http://localhost:11434")];
        let specs = resolve_virtual_agents(
            &models,
            &providers,
            &ModuleSettingsModel::default(),
            &[],
            &["--auto-approve".to_string()],
        );

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, LOCAL_AGENT_NAME);
        assert_eq!(
            specs[0].args,
            vec![
                AGENT_JSON_FLAG.to_string(),
                r#"{"agent":{"name":"local-agent"}}"#.to_string(),
                "--auto-approve".to_string()
            ]
        );
        assert_eq!(
            specs[0].endpoint,
            Some(("http://localhost:11434".to_string(), 1))
        );
        assert!(
            specs[0].description.contains("Model: QWEN (the default)"),
            "{}",
            specs[0].description
        );
    }

    /// Each agent's child gets its whole spec on the argv, then what every
    /// worker gets.
    #[test]
    fn each_declared_agent_gets_its_own_spec_on_the_argv() {
        let common = vec![
            "--auto-approve".to_string(),
            "--ollama".to_string(),
            "http://localhost:11434".to_string(),
        ];
        let roster = team();
        let specs =
            resolve_virtual_agents(&[], &[], &ModuleSettingsModel::default(), &roster, &common);

        assert_eq!(specs.len(), 2);
        for (resolved, declared) in specs.iter().zip(&roster) {
            assert_eq!(resolved.name, declared.agent.name);
            assert_eq!(&spec_in(&resolved.args), declared);
            assert_eq!(resolved.args[2..], common[..]);
        }
    }

    /// PL-U2's desktop path: the desktop runs a spec agent as a
    /// `chatty-tui` worker (`broker_runner::local_runners` over this
    /// function), so a plugin reaches the worker only on its argv — module,
    /// version, grants, config and limits intact — and the worker loads it
    /// as `plugins_headless` shows.
    #[test]
    fn a_workers_plugins_ride_on_its_argv() {
        let mut auditor = agent("auditor", Some("qwen"));
        auditor.plugins = vec![crate::agent_spec::PluginSpec {
            module: "benford-agent".to_string(),
            version: Some("^0.1".to_string()),
            grants: vec![crate::agent_spec::Grant::Llm],
            config: [("threshold".to_string(), "0.05".to_string())].into(),
            limits: crate::agent_spec::PluginLimits {
                max_memory_mb: Some(64),
                max_execution_ms: Some(2_000),
            },
        }];
        let specs = resolve_virtual_agents(
            &[],
            &[],
            &ModuleSettingsModel::default(),
            std::slice::from_ref(&auditor),
            &[],
        );
        assert_eq!(spec_in(&specs[0].args).plugins, auditor.plugins);
    }

    /// BI-5: a spec that delegates in turn is a sub-leader, and its child
    /// starts no broker of its own: it delegates over its connection.
    #[test]
    fn a_sub_leader_is_spawned_without_a_broker_of_its_own() {
        let mut lead = AgentSpec::named("kit-lead");
        lead.swarm.delegates_to = vec!["*".to_string()];
        let specs = resolve_virtual_agents(
            &[],
            &[],
            &ModuleSettingsModel::default(),
            &[lead, AgentSpec::named("kit-worker")],
            &[],
        );
        for spec in &specs {
            assert_eq!(spec.args.len(), 2, "{:?}", spec.args);
            assert!(!spec.args.contains(&"--broker".to_string()));
        }
    }

    /// The card says which model and which tool groups are missing, so the
    /// leader can pick a reviewer by reading `list_agents`.
    #[test]
    fn the_description_carries_the_model_and_the_disabled_groups() {
        let specs = resolve_virtual_agents(&[], &[], &ModuleSettingsModel::default(), &team(), &[]);

        assert!(
            specs[0].description.contains("Model: qwen."),
            "{}",
            specs[0].description
        );
        assert!(
            specs[0].description.contains("Tools: the full set."),
            "{}",
            specs[0].description
        );
        assert!(
            specs[1].description.contains("Model: gemma."),
            "{}",
            specs[1].description
        );
        assert!(
            specs[1]
                .description
                .contains("Tool groups disabled: fs-write, shell, git."),
            "{}",
            specs[1].description
        );
    }

    /// Two agents on different provider URLs are metered on different
    /// endpoints; two on one URL share its key, which is what makes them
    /// share one budget once the caller wraps it.
    #[test]
    fn agents_are_metered_on_their_own_models_endpoint() {
        let models = vec![
            model("qwen", ProviderType::Ollama),
            model("gemma", ProviderType::OpenRouter),
            model("phi", ProviderType::Ollama),
        ];
        let providers = vec![
            provider(ProviderType::Ollama, "http://localhost:11434"),
            provider(ProviderType::OpenRouter, "http://other:8000/v1"),
        ];
        let mut roster = team();
        roster.push(agent("local-tester", Some("phi")));
        let mut settings = ModuleSettingsModel::default();
        settings
            .endpoint_budgets
            .insert("http://other:8000/v1".to_string(), 3);

        let specs = resolve_virtual_agents(&models, &providers, &settings, &roster, &[]);

        assert_eq!(
            specs[0].endpoint,
            Some(("http://localhost:11434".to_string(), 1))
        );
        assert_eq!(
            specs[1].endpoint,
            Some(("http://other:8000/v1".to_string(), 3)),
            "the reviewer is metered on its own server, with that server's limit"
        );
        assert_eq!(
            specs[2].endpoint, specs[0].endpoint,
            "two agents on one server share its endpoint key"
        );
    }

    /// AGE-406: the team's verification command reaches every agent that
    /// could have produced a build, and no agent that could not — read off
    /// `disable` when that is all the agent declares.
    #[test]
    fn the_teams_verification_command_skips_an_agent_that_disables_the_shell_group() {
        let mut settings = ModuleSettingsModel::default();
        settings.team.verification = Some("cargo test".to_string());

        let specs = resolve_virtual_agents(&[], &[], &settings, &team(), &[]);

        assert_eq!(specs[0].verification.as_deref(), Some("cargo test"));
        assert_eq!(
            specs[1].verification, None,
            "the reviewer disables the `shell` group, so the runner must not run commands for it"
        );
    }

    /// AGE-452, applied to AGE-406: `profile` and `disable` compose, so both
    /// have to allow `shell_execute`.
    #[test]
    fn a_named_profile_decides_whether_the_verification_command_runs() {
        let with = |name: &str, profile: &str, disable: &[&str]| {
            let mut spec = AgentSpec::named(name);
            spec.tools.profile = Some(profile.to_string());
            spec.tools.disable = disable.iter().map(|s| s.to_string()).collect();
            spec
        };
        let roster = vec![
            with("local-lead", "coordinator", &[]),
            with("local-reviewer", "reviewer", &["shell"]),
            with("local-coder", "coder", &[]),
        ];
        let settings = ModuleSettingsModel {
            team: TeamConfig {
                verification: Some("cargo test".to_string()),
            },
            ..ModuleSettingsModel::default()
        };

        let specs = resolve_virtual_agents(&[], &[], &settings, &roster, &[]);

        assert_eq!(
            specs[0].verification, None,
            "a coordinator cannot run commands, so its tree was never built"
        );
        assert_eq!(
            specs[1].verification, None,
            "the reviewer profile allows shell_execute, but disable also names \
             shell, and the two compose rather than one winning"
        );
        assert_eq!(specs[2].verification.as_deref(), Some("cargo test"));
    }

    #[test]
    fn no_declared_verification_command_means_none_is_run() {
        let specs = resolve_virtual_agents(&[], &[], &ModuleSettingsModel::default(), &team(), &[]);
        assert!(specs.iter().all(|spec| spec.verification.is_none()));
    }

    #[test]
    fn a_model_that_resolves_to_nothing_leaves_the_agent_unmetered() {
        let models = vec![model("qwen", ProviderType::Ollama)];
        let providers = vec![provider(ProviderType::Ollama, "http://localhost:11434")];
        let specs = resolve_virtual_agents(
            &models,
            &providers,
            &ModuleSettingsModel::default(),
            &[agent("local-mystery", Some("no-such-model"))],
            &[],
        );
        assert_eq!(specs[0].endpoint, None);
    }

    /// The card carries the profile name and the preamble's first sentence,
    /// so the leader picks a reviewer by reading `list_agents`.
    #[test]
    fn the_card_carries_the_profile_and_the_preambles_first_sentence() {
        let mut reviewer = AgentSpec::named("local-reviewer");
        reviewer.tools.profile = Some("reviewer".to_string());
        reviewer.tools.disable = vec!["fs-write".to_string()];
        reviewer.agent.preamble = Some(
            "You review code you did not write. Never edit the tree; \
             run the tests and report."
                .to_string(),
        );

        let description =
            resolve_virtual_agents(&[], &[], &ModuleSettingsModel::default(), &[reviewer], &[])
                .swap_remove(0)
                .description;

        assert!(
            description.contains("Tool profile: reviewer."),
            "{description}"
        );
        assert!(
            description.contains("Tool groups disabled: fs-write."),
            "{description}"
        );
        assert!(
            description.contains("Role: You review code you did not write."),
            "{description}"
        );
        assert!(
            !description.contains("run the tests and report"),
            "only the first sentence, not the whole standing instruction: {description}"
        );
    }

    #[test]
    fn first_sentence_handles_a_preamble_without_one() {
        assert_eq!(first_sentence(None), None);
        assert_eq!(first_sentence(Some("   ")), None);
        assert_eq!(
            first_sentence(Some("Review only")),
            Some("Review only".to_string())
        );
        assert_eq!(
            first_sentence(Some("Review only\nNever edit.")),
            Some("Review only".to_string())
        );
    }
}
