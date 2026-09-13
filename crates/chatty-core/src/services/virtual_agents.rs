//! The broker's virtual agents, resolved from settings once for both
//! frontends (ADR-0011 C10 / AGE-377).
//!
//! A worker is a `chatty-tui` child, and everything that makes one worker
//! differ from another is its argv: `--model` picks the model, `--disable`
//! trims the tool set, and the leader's own provider flags (`--ollama`,
//! `--openai-compat-url`, `--api-key`) tell a child with no `providers.json`
//! — a Harbor sandbox — where the model server is. So a *role* is a name
//! plus an argv, declared in `module_settings.json` as a
//! [`VirtualAgentConfig`], and never a parameter on `invoke_agent`: the
//! leader's tool schema and prompt prefix stay identical whatever the team.
//!
//! This decides the argv and the endpoint to meter for each declared agent.
//! Wrapping that in a `LocalRunner` is left to the caller, since that type
//! lives in `chatty-protocol-gateway`, which this crate does not depend on
//! (see [`worker_endpoint`](super::worker_endpoint)).

use crate::factories::agent_factory::{tool_profile, tool_profile_names};
use crate::settings::models::ModuleSettingsModel;
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};
use crate::settings::models::module_settings::VirtualAgentConfig;
use crate::settings::models::providers_store::ProviderConfig;
use crate::tools::LOCAL_AGENT_NAME;

use super::worker_endpoint::resolve_worker_endpoint;

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
}

/// Resolve every virtual agent the broker should publish.
///
/// One per `module_settings.virtual_agents` entry, or the single default
/// `local-agent` when none is declared. `common_args` is appended to every
/// agent's argv after its own — `--auto-approve` when the leader runs
/// unattended, and the leader's provider flags when it was configured by
/// flags rather than by a config dir the child would read too.
pub fn resolve_virtual_agents(
    models: &[ModelConfig],
    providers: &[ProviderConfig],
    module_settings: &ModuleSettingsModel,
    common_args: &[String],
) -> Vec<VirtualAgentSpec> {
    let default_agent = [VirtualAgentConfig {
        name: LOCAL_AGENT_NAME.to_string(),
        ..VirtualAgentConfig::default()
    }];
    let declared: &[VirtualAgentConfig] = if module_settings.virtual_agents.is_empty() {
        &default_agent
    } else {
        &module_settings.virtual_agents
    };

    declared
        .iter()
        .map(|agent| {
            let mut args = Vec::new();
            if let Some(model) = agent.model.as_deref() {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            // A profile is an allowlist of tool names and `--disable` a list
            // of groups; when both are declared the profile wins, so the two
            // never fight over the same tool (ADR-0011 C11).
            match agent.tools.as_deref() {
                Some(profile) => {
                    if tool_profile(profile).is_none() {
                        tracing::warn!(
                            agent = %agent.name,
                            profile,
                            valid = ?tool_profile_names(),
                            "Virtual agent names an unknown tool profile; its workers will fail to start"
                        );
                    }
                    args.push("--tools".to_string());
                    args.push(profile.to_string());
                }
                None => {
                    if !agent.disable_tools.is_empty() {
                        args.push("--disable".to_string());
                        args.push(agent.disable_tools.join(","));
                    }
                }
            }
            if let Some(preamble) = agent.preamble.as_deref().map(str::trim)
                && !preamble.is_empty()
            {
                args.push("--preamble".to_string());
                args.push(preamble.to_string());
            }
            args.extend(agent.extra_args.iter().cloned());
            args.extend(common_args.iter().cloned());

            let endpoint = resolve_worker_endpoint(
                models,
                providers,
                module_settings,
                agent.model.as_deref(),
            );
            if endpoint.is_none() {
                tracing::warn!(
                    agent = %agent.name,
                    model = ?agent.model,
                    "Virtual agent's model resolves to no configured provider; its workers are unmetered"
                );
            }

            VirtualAgentSpec {
                name: agent.name.clone(),
                description: describe(agent, models),
                args,
                endpoint,
            }
        })
        .collect()
}

/// The card text: what the agent is, which model it runs, and which tool
/// groups it lacks.
fn describe(agent: &VirtualAgentConfig, models: &[ModelConfig]) -> String {
    let mut text = String::from(
        "A chatty agent in its own process, with its own workspace. \
         Delegate a self-contained task to it and it works autonomously \
         and reports back.",
    );
    match agent.model.as_deref() {
        Some(model) => text.push_str(&format!(" Model: {model}.")),
        None => match resolve_model_query(models, None) {
            Some(model) => text.push_str(&format!(" Model: {} (the default).", model.name)),
            None => text.push_str(" Model: the configured default."),
        },
    }
    match agent.tools.as_deref() {
        Some(profile) => text.push_str(&format!(" Tool profile: {profile}.")),
        None if agent.disable_tools.is_empty() => text.push_str(" Tools: the full set."),
        None => text.push_str(&format!(
            " Tool groups disabled: {}.",
            agent.disable_tools.join(", ")
        )),
    }
    if let Some(sentence) = first_sentence(agent.preamble.as_deref()) {
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

    fn team() -> ModuleSettingsModel {
        ModuleSettingsModel {
            virtual_agents: vec![
                VirtualAgentConfig {
                    name: "local-coder".to_string(),
                    model: Some("qwen".to_string()),
                    ..VirtualAgentConfig::default()
                },
                VirtualAgentConfig {
                    name: "local-reviewer".to_string(),
                    model: Some("gemma".to_string()),
                    disable_tools: vec!["fs-write".into(), "shell".into(), "git".into()],
                    extra_args: vec!["--enable".into(), "fetch".into()],
                    ..VirtualAgentConfig::default()
                },
            ],
            ..ModuleSettingsModel::default()
        }
    }

    /// Nothing declared is exactly the pre-C10 broker: one `local-agent`,
    /// no `--model`, only the common flags.
    #[test]
    fn nothing_declared_is_the_one_default_worker() {
        let models = vec![model("qwen", ProviderType::Ollama)];
        let providers = vec![provider(ProviderType::Ollama, "http://localhost:11434")];
        let specs = resolve_virtual_agents(
            &models,
            &providers,
            &ModuleSettingsModel::default(),
            &["--auto-approve".to_string()],
        );

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, LOCAL_AGENT_NAME);
        assert_eq!(specs[0].args, vec!["--auto-approve".to_string()]);
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

    /// Do item 2: `--model` when set, `--disable` when set, then
    /// `extra_args`, then what every worker gets.
    #[test]
    fn each_declared_agent_gets_its_own_argv() {
        let models = vec![
            model("qwen", ProviderType::Ollama),
            model("gemma", ProviderType::Ollama),
        ];
        let providers = vec![provider(ProviderType::Ollama, "http://localhost:11434")];
        let common = vec![
            "--auto-approve".to_string(),
            "--ollama".to_string(),
            "http://localhost:11434".to_string(),
        ];
        let specs = resolve_virtual_agents(&models, &providers, &team(), &common);

        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "local-coder");
        assert_eq!(
            specs[0].args,
            vec![
                "--model",
                "qwen",
                "--auto-approve",
                "--ollama",
                "http://localhost:11434"
            ]
        );
        assert_eq!(specs[1].name, "local-reviewer");
        assert_eq!(
            specs[1].args,
            vec![
                "--model",
                "gemma",
                "--disable",
                "fs-write,shell,git",
                "--enable",
                "fetch",
                "--auto-approve",
                "--ollama",
                "http://localhost:11434"
            ]
        );
    }

    /// Do item 6: the card says which model and which tool groups are
    /// missing, so the leader can pick a reviewer by reading `list_agents`.
    #[test]
    fn the_description_carries_the_model_and_the_disabled_groups() {
        let specs = resolve_virtual_agents(&[], &[], &team(), &[]);

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

    /// Do item 3, the budget half: two agents on different provider URLs
    /// are metered on different endpoints; two on one URL share its key,
    /// which is what makes them share one budget once the caller wraps it.
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
        let mut settings = team();
        settings.virtual_agents.push(VirtualAgentConfig {
            name: "local-tester".to_string(),
            model: Some("phi".to_string()),
            ..VirtualAgentConfig::default()
        });
        settings
            .endpoint_budgets
            .insert("http://other:8000/v1".to_string(), 3);

        let specs = resolve_virtual_agents(&models, &providers, &settings, &[]);

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

    #[test]
    fn a_model_that_resolves_to_nothing_leaves_the_agent_unmetered() {
        let models = vec![model("qwen", ProviderType::Ollama)];
        let providers = vec![provider(ProviderType::Ollama, "http://localhost:11434")];
        let settings = ModuleSettingsModel {
            virtual_agents: vec![VirtualAgentConfig {
                name: "local-mystery".to_string(),
                model: Some("no-such-model".to_string()),
                ..VirtualAgentConfig::default()
            }],
            ..ModuleSettingsModel::default()
        };

        let specs = resolve_virtual_agents(&models, &providers, &settings, &[]);
        assert_eq!(specs[0].endpoint, None);
        assert_eq!(specs[0].args, vec!["--model", "no-such-model"]);
    }

    /// A role travels as argv (ADR-0011 C11): the profile as `--tools`, the
    /// standing instructions as `--preamble`, both ahead of `extra_args`.
    #[test]
    fn a_declared_role_rides_along_as_tools_and_preamble_flags() {
        let settings = ModuleSettingsModel {
            virtual_agents: vec![VirtualAgentConfig {
                name: "local-reviewer".to_string(),
                model: Some("gemma".to_string()),
                tools: Some("reviewer".to_string()),
                preamble: Some("You are the reviewer. Run the tests.".to_string()),
                ..VirtualAgentConfig::default()
            }],
            ..ModuleSettingsModel::default()
        };

        let specs = resolve_virtual_agents(&[], &[], &settings, &["--auto-approve".to_string()]);

        assert_eq!(
            specs[0].args,
            vec![
                "--model",
                "gemma",
                "--tools",
                "reviewer",
                "--preamble",
                "You are the reviewer. Run the tests.",
                "--auto-approve",
            ]
        );
    }

    /// Do item 1: `tools` wins when both are set, so the two never disagree
    /// about one tool.
    #[test]
    fn a_profile_replaces_the_disabled_groups_rather_than_joining_them() {
        let settings = ModuleSettingsModel {
            virtual_agents: vec![VirtualAgentConfig {
                name: "local-reviewer".to_string(),
                disable_tools: vec!["fs-write".into()],
                tools: Some("reviewer".to_string()),
                ..VirtualAgentConfig::default()
            }],
            ..ModuleSettingsModel::default()
        };

        let specs = resolve_virtual_agents(&[], &[], &settings, &[]);

        assert_eq!(specs[0].args, vec!["--tools", "reviewer"]);
        assert!(
            specs[0].description.contains("Tool profile: reviewer."),
            "{}",
            specs[0].description
        );
        assert!(
            !specs[0].description.contains("Tool groups disabled"),
            "{}",
            specs[0].description
        );
    }

    /// Do item 3: the card carries the profile name and the preamble's first
    /// sentence, so the leader picks a reviewer by reading `list_agents`.
    #[test]
    fn the_card_carries_the_profile_and_the_preambles_first_sentence() {
        let settings = ModuleSettingsModel {
            virtual_agents: vec![VirtualAgentConfig {
                name: "local-reviewer".to_string(),
                tools: Some("reviewer".to_string()),
                preamble: Some(
                    "You review code you did not write. Never edit the tree; \
                     run the tests and report."
                        .to_string(),
                ),
                ..VirtualAgentConfig::default()
            }],
            ..ModuleSettingsModel::default()
        };

        let description = resolve_virtual_agents(&[], &[], &settings, &[])
            .swap_remove(0)
            .description;

        assert!(
            description.contains("Tool profile: reviewer."),
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
