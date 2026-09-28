//! `/agent <name> <prompt>`, resolved the same way in both frontends
//! (PL-U5).
//!
//! The first word names an agent when it is a remote A2A agent the user
//! enabled or a spec on the local roster — the same roster `list_agents`
//! lists and the broker serves. Otherwise the whole text is the prompt for
//! the default sub-agent. A WASM plugin is never an agent, so a plugin's
//! name is just a word.

use crate::agent_spec::AgentSpec;
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::tools::LOCAL_AGENT_NAME;

/// What `/agent …` runs.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentCommandTarget {
    /// A remote A2A agent, over the protocol.
    Remote {
        config: A2aAgentConfig,
        prompt: String,
    },
    /// A spec on the local roster, run as a headless sub-agent with
    /// `--agent <name>`.
    Spec { spec: AgentSpec, prompt: String },
    /// The default sub-agent, with the whole text as its prompt.
    Default { prompt: String },
}

/// Resolve `/agent`'s argument against the remote agents and the local
/// roster. A remote agent wins a name both have, as it does in
/// `invoke_agent`. A name with no prompt after it is not a target: the
/// text is the default sub-agent's prompt. `local-agent` is the default
/// sub-agent by its roster name.
pub fn resolve_agent_command(
    text: &str,
    remote_agents: &[A2aAgentConfig],
    roster: &[AgentSpec],
) -> AgentCommandTarget {
    let text = text.trim();
    let (first, rest) = match text.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (text, ""),
    };
    if rest.is_empty() {
        return AgentCommandTarget::Default {
            prompt: text.to_string(),
        };
    }
    if let Some(config) = remote_agents
        .iter()
        .find(|agent| agent.enabled && agent.name == first)
    {
        return AgentCommandTarget::Remote {
            config: config.clone(),
            prompt: rest.to_string(),
        };
    }
    if first == LOCAL_AGENT_NAME {
        return AgentCommandTarget::Default {
            prompt: rest.to_string(),
        };
    }
    if let Some(spec) = roster.iter().find(|spec| spec.agent.name == first) {
        return AgentCommandTarget::Spec {
            spec: spec.clone(),
            prompt: rest.to_string(),
        };
    }
    AgentCommandTarget::Default {
        prompt: text.to_string(),
    }
}

/// The arguments a headless sub-agent for `spec` is started with, ahead of
/// `--headless --message <prompt>`: `--agent <name>`, and `--model` only
/// when the spec leaves the model to its host, since an explicit flag beats
/// the spec's own.
pub fn spec_sub_agent_args(spec: Option<&AgentSpec>, host_model: &str) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(spec) = spec {
        args.push("--agent".to_string());
        args.push(spec.agent.name.clone());
    }
    let spec_has_model = spec.is_some_and(|spec| spec.agent.model.is_some());
    if !spec_has_model && !host_model.is_empty() {
        args.push("--model".to_string());
        args.push(host_model.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::load_roster_from;

    fn remote(name: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: format!("https://example.com/{name}"),
            api_key: None,
            enabled,
            skills: Vec::new(),
        }
    }

    /// The issue's "Verify": `/agent benford-analyst <question>` runs the
    /// preset spec, resolved from the same roster the broker serves.
    #[test]
    fn agent_command_resolves_against_the_roster() {
        let roster = load_roster_from(&[], None, None).unwrap();
        let target = resolve_agent_command(
            "benford-analyst  Audit 120, 245, 1300",
            &[remote("voucher", true)],
            &roster,
        );
        let AgentCommandTarget::Spec { spec, prompt } = target else {
            panic!("benford-analyst is a spec on the roster, got {target:?}");
        };
        assert_eq!(spec.agent.name, "benford-analyst");
        assert_eq!(prompt, "Audit 120, 245, 1300");

        assert_eq!(
            resolve_agent_command(
                "voucher translate this",
                &[remote("voucher", true)],
                &roster
            ),
            AgentCommandTarget::Remote {
                config: remote("voucher", true),
                prompt: "translate this".to_string(),
            }
        );
    }

    #[test]
    fn a_word_that_names_no_agent_is_part_of_the_prompt() {
        let roster = load_roster_from(&[], None, None).unwrap();
        for text in [
            "summarize this file",
            // A plugin's name is not an agent's (PL-U5).
            "benford 1, 2, 3",
            // A disabled remote is not a target.
            "off do it",
            // A name alone has no prompt to send.
            "benford-analyst",
        ] {
            assert_eq!(
                resolve_agent_command(text, &[remote("off", false)], &roster),
                AgentCommandTarget::Default {
                    prompt: text.to_string()
                },
                "{text}"
            );
        }
        assert_eq!(
            resolve_agent_command("local-agent do it", &[], &roster),
            AgentCommandTarget::Default {
                prompt: "do it".to_string()
            }
        );
    }

    #[test]
    fn a_remote_agent_wins_a_name_a_spec_also_has() {
        let roster = load_roster_from(&[], None, None).unwrap();
        assert!(matches!(
            resolve_agent_command("local-coder go", &[remote("local-coder", true)], &roster),
            AgentCommandTarget::Remote { .. }
        ));
    }

    #[test]
    fn a_spec_s_own_model_is_not_overridden() {
        let mut spec = AgentSpec::named("analyst");
        assert_eq!(
            spec_sub_agent_args(Some(&spec), "gpt-x"),
            ["--agent", "analyst", "--model", "gpt-x"]
        );
        spec.agent.model = Some("qwen3:4b".to_string());
        assert_eq!(
            spec_sub_agent_args(Some(&spec), "gpt-x"),
            ["--agent", "analyst"]
        );
        assert_eq!(spec_sub_agent_args(None, "gpt-x"), ["--model", "gpt-x"]);
        assert!(spec_sub_agent_args(None, "").is_empty());
    }
}
