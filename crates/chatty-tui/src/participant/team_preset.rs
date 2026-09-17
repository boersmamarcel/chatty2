//! AGE-407's verification: `chatty-tui --team coder-reviewer --headless`
//! against the scripted provider lists both agents with their profiles and
//! delegates to the coder.
//!
//! What `--team` does in `main.rs` is `load_team` + `Team::apply` on this
//! run's module settings, then the exact `--broker` wiring of before; this
//! runs that same sequence over the real gateway and socket, with the
//! stand-in worker binary `equivalence.rs` uses (it records its argv and
//! waits) and a scripted participant answering the task. The leader's own
//! model is not what this pins — the roster and the argv are.

use chatty_core::services::team::{TeamSource, load_team};
use chatty_core::services::virtual_agents::resolve_virtual_agents;
use chatty_core::services::{StreamSurface, scenarios};
use chatty_core::session::{TurnPolicy, replay_scenario};
use chatty_core::settings::models::module_settings::VirtualAgentConfig;
use chatty_core::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};
use chatty_core::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentTool};
use chatty_core::tools::list_agents_tool::{ListAgentsTool, ListAgentsToolArgs};
use rig_agent::tool::{Tool, ToolContext};

use super::broker::Broker;
use super::equivalence::named_virtual_agents::{
    recorded_argv, spawn_argv_gated_worker, stand_in_binary,
};

const CODER: &str = "local-coder";
const REVIEWER: &str = "local-reviewer";

/// The settings `--team coder-reviewer` leaves a run with, starting from a
/// `module_settings.json` that declared a different team and a persisted
/// 10-turn budget.
fn team_settings(
    workspace: Option<&std::path::Path>,
) -> (ModuleSettingsModel, ExecutionSettingsModel) {
    let team = load_team("coder-reviewer", workspace, None).expect("the team loads");
    let module_settings = ModuleSettingsModel {
        // Two slots, so the second delegation does not wait on the first
        // stand-in child being reaped; queueing is C6's test.
        default_endpoint_budget: 2,
        virtual_agents: vec![VirtualAgentConfig {
            name: "stale-agent".to_string(),
            ..VirtualAgentConfig::default()
        }],
        ..ModuleSettingsModel::default()
    };
    let mut execution_settings = ExecutionSettingsModel::default();
    team.apply_turn_budget(&mut execution_settings);
    (
        team.run_module_settings(&module_settings),
        execution_settings,
    )
}

async fn start_team_broker(dir: &std::path::Path, module_settings: &ModuleSettingsModel) -> Broker {
    let specs = resolve_virtual_agents(&[], &[], module_settings, &["--auto-approve".to_string()]);
    Broker::start_at(
        dir.join("participants.sock"),
        stand_in_binary(dir),
        module_settings.default_endpoint_budget,
        specs,
        None,
    )
    .await
    .expect("the broker starts with the team's two agents")
}

/// The issue's "Verify", first half: `--team coder-reviewer --headless`
/// lists both agents with their profiles, and delegating to the coder
/// spawns a child on the `coder` profile with the team's preamble.
#[tokio::test]
async fn the_preset_team_lists_both_agents_with_their_profiles_and_delegates_to_the_coder() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let (module_settings, execution_settings) = team_settings(None);
    assert_eq!(module_settings.virtual_agent_names(), [CODER, REVIEWER]);
    assert_eq!(
        execution_settings.max_agent_turns, 50,
        "the preset's turn budget replaces the persisted default"
    );
    let broker = start_team_broker(dir.path(), &module_settings).await;

    let output = ListAgentsTool::new(vec![])
        .with_local_workers(module_settings.virtual_agent_names())
        .with_gateway_port(broker.port)
        .call(&mut ToolContext::new(), ListAgentsToolArgs {})
        .await
        .expect("list_agents succeeds");
    let find = |name: &str| {
        output
            .agents
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("{name} is listed, got {:?}", output.agents))
    };
    assert!(
        find(CODER).description.contains("Tool profile: coder."),
        "{}",
        find(CODER).description
    );
    assert!(
        find(REVIEWER)
            .description
            .contains("Tool profile: reviewer."),
        "{}",
        find(REVIEWER).description
    );
    assert!(
        find(REVIEWER)
            .description
            .contains("Role: Verify, do not trust, verdict first."),
        "{}",
        find(REVIEWER).description
    );
    assert!(
        !output.agents.iter().any(|a| a.name == "stale-agent"),
        "the team file replaces module settings' roster for the run"
    );

    let events = replay_scenario(
        scenarios()
            .into_iter()
            .find(|s| s.name == "tool_call_then_result")
            .expect("the scenario exists"),
        TurnPolicy {
            surface: StreamSurface::Headless,
            max_agent_turns: 10,
            loop_guard: false,
            already_asked_to_retry: false,
            think_disabled: false,
        },
    )
    .await;
    spawn_argv_gated_worker(
        &broker.participants(),
        &format!("{CODER}-0"),
        dir.path().join("argv.log"),
        events,
    );
    InvokeAgentTool::new(vec![], vec![], Some(broker.port))
        .with_local_agents([CODER, REVIEWER])
        .call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: CODER.to_string(),
                prompt: "Fix the overdraft bug.".to_string(),
                include_trace: false,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("delegating to the coder succeeds: {e:#}"));

    let argv = recorded_argv(dir.path(), 1).await;
    let coder = argv
        .iter()
        .find(|line| line.contains("--participant-name local-coder-0"))
        .unwrap_or_else(|| panic!("no coder child spawned: {argv:?}"));
    assert!(coder.contains("--tools coder"), "{coder}");
    assert!(
        coder.contains("--preamble You are the coder on this team."),
        "{coder}"
    );
    assert!(
        !coder.contains("--model"),
        "no model in the preset; the child resolves the roster's default: {coder}"
    );

    broker.shutdown();
}

/// The issue's "Verify", second half: a team file in the workspace
/// overrides the preset of the same id.
#[tokio::test]
async fn a_team_file_in_the_workspace_overrides_the_preset() {
    let workspace = tempfile::tempdir().expect("a temp dir");
    let team_dir = workspace.path().join(".chatty/teams/coder-reviewer");
    std::fs::create_dir_all(&team_dir).unwrap();
    std::fs::write(
        team_dir.join("team.json"),
        r#"{
          "agents": [{"name": "ws-coder", "tools": "coder"}],
          "verification": "make test",
          "skill": "coder-reviewer",
          "max_agent_turns": 12
        }"#,
    )
    .unwrap();

    let team = load_team("coder-reviewer", Some(workspace.path()), None).unwrap();
    assert_eq!(team.source, TeamSource::Dir(team_dir));
    let (module_settings, execution_settings) = team_settings(Some(workspace.path()));
    assert_eq!(module_settings.virtual_agent_names(), ["ws-coder"]);
    assert_eq!(
        module_settings.team.verification.as_deref(),
        Some("make test")
    );
    assert_eq!(execution_settings.max_agent_turns, 12);

    let specs = resolve_virtual_agents(&[], &[], &module_settings, &[]);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "ws-coder");
    assert_eq!(
        specs[0].verification.as_deref(),
        Some("make test"),
        "the team file's verification reaches the coder's evidence envelope"
    );
}
