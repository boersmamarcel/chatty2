//! AGE-376's verification: a `--broker` leader can delegate to `local-agent`
//! and get the worker's answer back — what `chatty-tui --headless --broker
//! -m "delegate the task 'say hello' to local-agent and repeat its answer"`
//! needs to do.
//!
//! `Broker::start` (`participant/broker.rs`) is the production code under
//! test: it is exactly what `main.rs` runs when `--broker` is passed —
//! before this issue, nothing in chatty-tui ever called it, so a headless
//! leader had no `local-agent` to offer (the issue's "Why"). The worker is
//! scripted the same way `equivalence.rs` scripts one, over the same real
//! gateway and socket a `--broker` leader actually serves; a real LLM
//! answering "say hello" is not what this pins, the wiring from a leader's
//! `invoke_agent` tool through its own broker to `local-agent` and back is.
//!
//! `invoke_agent`'s progress channel is what a real turn's `SessionEvent::
//! Delegation` events come from (`session/handler.rs`); this asserts
//! directly on those `InvokeAgentProgress` events and on the response text
//! `invoke_agent` hands back to the model — the "worker's answer" the
//! issue's acceptance criterion asks for.

use chatty_core::services::{StreamSurface, install_progress_channel, scenarios};
use chatty_core::session::{SessionEvent, TurnPolicy, replay_scenario};
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::tools::LOCAL_AGENT_NAME;
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentProgress, InvokeAgentTool,
};
use chatty_core::tools::list_agents_tool::{ListAgentsTool, ListAgentsToolArgs};
use chatty_protocol_gateway::participant::{
    AgentOrigin, BrokerFrame, ParticipantCard, ParticipantRegistry,
};
use chatty_protocol_gateway::worker::TaskMapper;
use rig_agent::tool::{Tool, ToolContext};
use tokio::sync::mpsc;

use super::broker::Broker;

/// The policy a scripted worker's turn "runs" under, matching
/// `equivalence.rs`'s.
fn policy() -> TurnPolicy {
    TurnPolicy {
        surface: StreamSurface::Headless,
        max_agent_turns: 10,
        loop_guard: false,
        already_asked_to_retry: false,
    }
}

/// Register a scripted `local-agent` participant that answers its one task
/// by replaying `events` through the real broker↔A2A mapping — identical to
/// `equivalence.rs`'s `spawn_scripted_worker`, since it is the same
/// worker-side contract both tests pin.
fn spawn_scripted_worker(registry: &ParticipantRegistry, events: Vec<SessionEvent>) {
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<BrokerFrame>();
    registry
        .register(
            ParticipantCard {
                name: LOCAL_AGENT_NAME.to_string(),
                description: "a scripted worker".to_string(),
                ..Default::default()
            },
            AgentOrigin::Local,
            outbound_tx,
        )
        .expect("the scripted worker registers");

    let registry = registry.clone();
    tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            let BrokerFrame::Task { task_id, .. } = frame else {
                continue;
            };
            let mut mapper = TaskMapper::new(task_id);
            for event in &events {
                if let Some(frame) = mapper.map(event) {
                    registry.on_frame(LOCAL_AGENT_NAME, frame);
                }
            }
            registry.on_frame(LOCAL_AGENT_NAME, mapper.terminal());
            return;
        }
    });
}

/// The issue's "Why": today a headless leader's `list_agents` has no
/// `local-agent` to offer. Once `Broker::start` has run (what `--broker`
/// does), it does — built exactly as `agent_factory::mod.rs` builds it when
/// a gateway is running.
#[tokio::test]
async fn list_agents_offers_local_agent_once_the_broker_is_started() {
    let module_settings = ModuleSettingsModel::default();
    let dir = tempfile::tempdir().expect("a temp dir for the socket");
    let broker = Broker::start_at(
        dir.path().join("participants.sock"),
        &[],
        &[],
        &module_settings,
        None,
        false,
    )
    .await
    .expect("the broker starts");

    let tool = ListAgentsTool::new(vec![])
        .with_local_worker(LOCAL_AGENT_NAME)
        .with_gateway_port(broker.port);
    let output = tool
        .call(&mut ToolContext::new(), ListAgentsToolArgs {})
        .await
        .expect("list_agents succeeds");

    assert!(
        output.agents.iter().any(|a| a.name == LOCAL_AGENT_NAME),
        "local-agent is not listed: {:?}",
        output.agents
    );

    broker.shutdown();
}

/// The issue's "Verify": a leader's real `invoke_agent` call, over the exact
/// gateway `--broker` starts, reaches `local-agent` and returns the
/// worker's answer.
#[tokio::test]
async fn invoke_agent_delegates_to_local_agent_and_returns_its_answer() {
    let module_settings = ModuleSettingsModel::default();
    let dir = tempfile::tempdir().expect("a temp dir for the socket");
    let broker = Broker::start_at(
        dir.path().join("participants.sock"),
        &[],
        &[],
        &module_settings,
        None,
        false,
    )
    .await
    .expect("the broker starts");

    let scenario = scenarios()
        .into_iter()
        .find(|s| s.name == "tool_call_then_result")
        .expect("the scenario exists");
    let events = replay_scenario(scenario, policy()).await;
    let expected_answer: String = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(!expected_answer.is_empty(), "the scenario has an answer");

    spawn_scripted_worker(&broker.participants(), events);

    // Built exactly as `agent_factory::mod.rs` builds it when `--broker` has
    // set `gateway_port = Some(broker.port)`.
    let tool =
        InvokeAgentTool::new(vec![], vec![], Some(broker.port)).with_local_agent(LOCAL_AGENT_NAME);
    let mut progress_rx = install_progress_channel(&tool.progress_slot());

    let result = tool
        .call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: LOCAL_AGENT_NAME.to_string(),
                prompt: "say hello".to_string(),
            },
        )
        .await
        .expect("the delegation to local-agent succeeds");

    assert_eq!(
        result.response.trim(),
        expected_answer.trim(),
        "invoke_agent's response is not the worker's answer"
    );

    let mut started_for_local_agent = false;
    let mut finished = false;
    while let Ok(event) = progress_rx.try_recv() {
        match event {
            InvokeAgentProgress::Started { agent_name, .. } if agent_name == LOCAL_AGENT_NAME => {
                started_for_local_agent = true;
            }
            InvokeAgentProgress::Finished { success: true, .. } => finished = true,
            _ => {}
        }
    }
    assert!(
        started_for_local_agent,
        "no delegation-started progress named local-agent"
    );
    assert!(finished, "no successful delegation-finished progress");

    broker.shutdown();
}
