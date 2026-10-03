//! `/agent <name> <prompt>`, resolved the same way in both frontends
//! (PL-U5).
//!
//! The first word names an agent when it is a remote A2A agent the user
//! enabled or a spec on the local roster — the same roster `list_agents`
//! lists and the broker serves. Otherwise the whole text is the prompt for
//! the default sub-agent. A WASM plugin is never an agent, so a plugin's
//! name is just a word.

use std::path::{Path, PathBuf};

use crate::agent_spec::AgentSpec;
use crate::services::worker_start::preflight_card;
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
    /// A spec on the local roster, run as a turn of the conversation handed
    /// to that agent through its own broker (AGE-744/AGE-747).
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

/// What a local `/agent` delegation needs before anything is spawned
/// (AGE-822): a workspace folder that exists, and code execution on — a
/// worker's tools are confined to the workspace, and without code execution
/// it has none to read a file with. `Ok` is the workspace, which the turn
/// shows; `Err` is the card the user is shown instead of starting `agent`.
pub fn preflight(
    agent: &str,
    workspace: Option<&Path>,
    code_execution: bool,
) -> Result<PathBuf, String> {
    let Some(workspace) = workspace else {
        return Err(preflight_card(
            agent,
            "this conversation has no workspace folder, so the agent would have no files to \
             work on.",
            "Pick a folder with the folder chip under the message box, then send the message \
             again.",
        ));
    };
    if !workspace.is_dir() {
        return Err(preflight_card(
            agent,
            &format!(
                "the workspace folder {} does not exist.",
                workspace.display()
            ),
            "Pick an existing folder with the folder chip under the message box, then send \
             the message again.",
        ));
    }
    if !code_execution {
        return Err(preflight_card(
            agent,
            "code execution is off, so the agent cannot read or run anything in the \
             workspace.",
            "Turn on code execution in Settings \u{2192} Execution, then send the message again.",
        ));
    }
    Ok(workspace.to_path_buf())
}

/// The workspace line a local `/agent` turn carries under its command, so
/// the user sees where its agents work before they start (AGE-822).
pub fn workspace_line(workspace: &Path) -> String {
    let shown = match dirs::home_dir()
        .as_deref()
        .and_then(|home| workspace.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => workspace.display().to_string(),
    };
    format!("Workspace: `{shown}`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::load_roster_from;
    use crate::services::worker_start::parse_card;

    /// AGE-822: `/agent` with no workspace, a missing one, or code
    /// execution off is refused with a card before anything is spawned;
    /// with both it goes ahead in that workspace.
    #[test]
    fn agent_command_without_workspace_fails_before_spawn() {
        let refused = preflight("data-lead", None, true).expect_err("no workspace");
        let (title, body) = parse_card(&refused).expect("a card");
        assert_eq!(title, "\u{26d4} Could not start 'data-lead'");
        assert!(body.contains("no workspace folder"), "{body}");
        assert!(body.contains("folder chip"), "{body}");

        let gone = Path::new("/nonexistent/age-822/sales");
        let refused = preflight("data-lead", Some(gone), true).expect_err("missing folder");
        assert!(refused.contains("does not exist"), "{refused}");

        let dir = tempfile::tempdir().expect("a workspace");
        let refused = preflight("data-lead", Some(dir.path()), false).expect_err("no execution");
        assert!(refused.contains("code execution is off"), "{refused}");

        assert_eq!(
            preflight("data-lead", Some(dir.path()), true).expect("ready"),
            dir.path()
        );
        assert!(workspace_line(dir.path()).starts_with("Workspace: `"));
    }

    fn remote(name: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: format!("https://example.com/{name}"),
            api_key: None,
            enabled,
            skills: Vec::new(),
            allow_private_network: false,
        }
    }

    /// The issue's "Verify": `/agent data-analyst <question>` runs the
    /// preset spec, resolved from the same roster the broker serves — one
    /// that names it, as a preset is on no default roster (AGE-760).
    #[test]
    fn agent_command_resolves_against_the_roster() {
        let roster = load_roster_from(&["data-analyst".to_string()], None, None).unwrap();
        let target = resolve_agent_command(
            "data-analyst  Sum the amounts in data.csv",
            &[remote("voucher", true)],
            &roster,
        );
        let AgentCommandTarget::Spec { spec, prompt } = target else {
            panic!("data-analyst is a spec on the roster, got {target:?}");
        };
        assert_eq!(spec.agent.name, "data-analyst");
        assert_eq!(prompt, "Sum the amounts in data.csv");

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
            "data-analyst",
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
}
