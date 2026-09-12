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
            if !agent.disable_tools.is_empty() {
                args.push("--disable".to_string());
                args.push(agent.disable_tools.join(","));
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
    if agent.disable_tools.is_empty() {
        text.push_str(" Tools: the full set.");
    } else {
        text.push_str(&format!(
            " Tool groups disabled: {}.",
            agent.disable_tools.join(", ")
        ));
    }
    text
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
}
