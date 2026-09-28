//! PL-U5's verification over the real broker: with nothing declared, the
//! roster is every exposed spec, so the `benford-analyst` preset is listed
//! by `list_agents` and reached by a coordinator's `invoke_agent` exactly
//! as any other local agent — and the `benford` plugin it runs with is not
//! an agent at all.
//!
//! The broker and the stand-in worker are `team_preset.rs`'s: the child
//! records its argv and answers on the connection the runner made for it.

use chatty_core::agent_spec::load_roster_from;
use chatty_core::services::virtual_agents::resolve_virtual_agents;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::tools::LOCAL_AGENT_NAME;
use chatty_core::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentTool};
use chatty_core::tools::list_agents_tool::{ListAgentsTool, ListAgentsToolArgs};
use chatty_fabric::AgentOrigin;
use rig_agent::tool::{Tool, ToolContext};

use super::broker::Broker;
use super::equivalence::named_virtual_agents::{child_argv, completed_turn};
use super::stand_in::{recorded_argv, scripted_worker_binary};

const BENFORD: &str = "benford-analyst";

#[tokio::test]
async fn benford_analyst_is_listed_and_reached_like_any_local_agent() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let workspace = tempfile::tempdir().expect("a temp workspace");
    // Nothing declared, no spec files: the presets and the default worker.
    let roster = load_roster_from(&[], Some(workspace.path()), None).expect("the roster loads");
    let names: Vec<String> = roster.iter().map(|spec| spec.agent.name.clone()).collect();
    assert_eq!(names[0], LOCAL_AGENT_NAME);
    assert!(names.iter().any(|name| name == BENFORD), "{names:?}");

    let module_settings = ModuleSettingsModel {
        default_endpoint_budget: 2,
        ..ModuleSettingsModel::default()
    };
    let specs = resolve_virtual_agents(
        &[],
        &[],
        &module_settings,
        &roster,
        &["--auto-approve".to_string()],
    );
    let broker = Broker::start_at(
        dir.path().join("participants.sock"),
        scripted_worker_binary(dir.path(), &completed_turn().await),
        module_settings.default_endpoint_budget,
        specs,
        None,
    )
    .await
    .expect("the broker starts with the whole roster");

    let output = ListAgentsTool::new(vec![])
        .with_local_workers(names.clone())
        .with_gateway_port(broker.port)
        .call(&mut ToolContext::new(), ListAgentsToolArgs {})
        .await
        .expect("list_agents succeeds");
    let benford = output
        .agents
        .iter()
        .find(|agent| agent.name == BENFORD)
        .unwrap_or_else(|| panic!("{BENFORD} is listed, got {:?}", output.agents));
    assert_eq!(benford.origin, AgentOrigin::Local);
    assert!(
        benford.description.contains("Tool profile: reviewer."),
        "the broker's card says what it runs: {}",
        benford.description
    );
    for name in &names {
        assert!(
            output.agents.iter().any(|agent| &agent.name == name),
            "{name} is listed: {:?}",
            output.agents
        );
    }
    assert!(
        !output.agents.iter().any(|agent| agent.name == "benford"),
        "a plugin is never an agent: {:?}",
        output.agents
    );

    InvokeAgentTool::new(vec![], Some(broker.port))
        .with_local_agents(names)
        .call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: BENFORD.to_string(),
                prompt: "Audit 120, 245, 1300, 1450, 2100.".to_string(),
                include_trace: false,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("a coordinator's invoke_agent reaches {BENFORD}: {e:#}"));

    let argv = recorded_argv(dir.path(), 1).await;
    let child = child_argv(&argv, BENFORD);
    assert!(child.contains(r#""module":"benford""#), "{child}");
    assert!(child.contains(r#""profile":"reviewer""#), "{child}");

    broker.shutdown();
}
