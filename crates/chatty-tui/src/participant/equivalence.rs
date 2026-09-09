//! AGE-301's verification: the parent's trace of a delegated task carries
//! every tool call the child reported, in order.
//!
//! ADR-0011's first kill criterion says A2A's task model must carry a
//! delegated turn at the granularity the parent already renders. "The same"
//! is made precise here: for every scripted scenario both frontends are
//! characterized against, the parent's progress lines are compared against
//! the child's own events put through `progress_text_for_event` — the
//! rendering both ends share.
//!
//! The broker path runs end to end: a real `ProtocolGateway` on a real port,
//! a real `A2aClient` inside a real `InvokeAgentTool`. Only the child's turn
//! is scripted, because that is the input both sides share.
//!
//! # Text is extra
//!
//! The broker also carries the assistant's **text** as artifact chunks, so
//! the parent sees the answer stream in while the turn runs.
//! `progress_text_for_event` says nothing about text, so the comparison
//! subtracts exactly those chunks and asserts the remainder is identical —
//! the broker is a superset of what the child reported, never a subset,
//! which is the direction the kill criterion cares about.

use std::collections::HashMap;
use std::sync::Arc;

use chatty_core::services::{
    Scenario, StreamSurface, clarification_scenario, install_progress_channel, scenarios,
};
use chatty_core::session::{SessionEvent, TurnPolicy, replay_scenario};
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentProgress, InvokeAgentTool,
};
use chatty_core::tools::{LOCAL_AGENT_NAME, progress_text_for_event};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::{
    AgentOrigin, BrokerFrame, ParticipantCard, ParticipantFrame, ParticipantRegistry,
};
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use rig_agent::tool::{Tool, ToolContext};
use tokio::sync::{RwLock, mpsc};

use chatty_protocol_gateway::worker::TaskMapper;

/// The policy a delegated child runs its turn under.
fn policy() -> TurnPolicy {
    TurnPolicy {
        surface: StreamSurface::Headless,
        max_agent_turns: 10,
        loop_guard: false,
        already_asked_to_retry: false,
    }
}

// ---------------------------------------------------------------------------
// The reference rendering: the child's own events, rendered
// ---------------------------------------------------------------------------

/// The progress lines the child's events amount to.
///
/// Every event is offered: `progress_text_for_event` is the filter, and it
/// ignores the ones that say nothing about tool activity (`Text`,
/// `TurnMessages`).
fn reference_trace(events: &[SessionEvent]) -> Vec<String> {
    let mut names = HashMap::new();
    events
        .iter()
        .filter_map(|event| progress_text_for_event(event, &mut names))
        .collect()
}

/// The assistant text of a scripted turn, which the broker streams as
/// artifact chunks.
fn assistant_text(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The broker path: a gateway, a participant, and the real invoke_agent
// ---------------------------------------------------------------------------

struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop".into())
    }
}

/// Start a gateway on an ephemeral port and return its port and registry.
async fn start_gateway() -> (u16, ParticipantRegistry) {
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let modules = Arc::new(RwLock::new(
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
    ));
    let gateway = ProtocolGateway::new(modules, 0);
    let participants = gateway.participants();

    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });

    (port, participants)
}

/// Register a participant that answers its one task by replaying `events`
/// through the mapping under test.
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

/// The parent's progress, and the response `invoke_agent` hands the model.
struct BrokerRun {
    progress: Vec<String>,
    response: String,
    succeeded: bool,
}

/// Delegate one task through the real `invoke_agent` tool and record what the
/// parent saw.
async fn broker_run(events: Vec<SessionEvent>) -> BrokerRun {
    let (port, registry) = start_gateway().await;
    spawn_scripted_worker(&registry, events);

    let tool = InvokeAgentTool::new(vec![], vec![], Some(port)).with_local_agent(LOCAL_AGENT_NAME);
    let mut progress_rx = install_progress_channel(&tool.progress_slot());

    let result = tool
        .call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: LOCAL_AGENT_NAME.to_string(),
                prompt: "do the delegated task".to_string(),
            },
        )
        .await;

    let mut progress = Vec::new();
    while let Ok(event) = progress_rx.try_recv() {
        if let InvokeAgentProgress::Text(text) = event {
            progress.push(text);
        }
    }

    BrokerRun {
        progress,
        response: result
            .as_ref()
            .map(|o| o.response.clone())
            .unwrap_or_default(),
        succeeded: result.is_ok(),
    }
}

/// Remove the artifact chunks — the assistant's text — so what is left is
/// comparable with [`reference_trace`].
fn without_text_chunks(progress: &[String], text: &[String]) -> Vec<String> {
    let mut remaining: Vec<&String> = text.iter().collect();
    progress
        .iter()
        .filter(|line| match remaining.iter().position(|t| *t == *line) {
            Some(at) => {
                remaining.remove(at);
                false
            }
            None => true,
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The issue's "Verify", over every scenario both frontends are pinned to.
#[tokio::test]
async fn a_delegated_task_renders_every_tool_call_the_child_reported() {
    for scenario in scenarios().into_iter().chain([clarification_scenario()]) {
        let name = scenario.name;
        let events = replay_scenario(scenario, policy()).await;

        let expected = reference_trace(&events);
        let run = broker_run(events.clone()).await;
        let over_the_broker = without_text_chunks(&run.progress, &assistant_text(&events));

        assert_eq!(
            expected, over_the_broker,
            "scenario '{name}': the parent's tool-call trace differs from what \
             the child reported.\n  expected: {expected:?}\n  broker: {:?}",
            run.progress
        );
    }
}

/// The answer itself reaches the parent model, not just the progress lines.
#[tokio::test]
async fn the_delegated_answer_reaches_the_parent_model() {
    let scenario = scenarios()
        .into_iter()
        .find(|s: &Scenario| s.name == "tool_call_then_result")
        .expect("the scenario exists");
    let events = replay_scenario(scenario, policy()).await;
    let expected: String = assistant_text(&events).concat();
    assert!(!expected.is_empty(), "the scenario has an answer to carry");

    let run = broker_run(events).await;

    assert!(run.succeeded, "the delegation succeeded");
    assert_eq!(run.response, expected.trim());
}

/// A per-tool trace is the thing the criterion is about, so assert it
/// concretely rather than only against the other path.
#[tokio::test]
async fn the_parent_sees_each_tool_start_and_finish_in_order() {
    let scenario = scenarios()
        .into_iter()
        .find(|s: &Scenario| s.name == "tool_call_then_result")
        .expect("the scenario exists");
    let events = replay_scenario(scenario, policy()).await;
    let run = broker_run(events.clone()).await;
    let trace = without_text_chunks(&run.progress, &assistant_text(&events));

    assert_eq!(
        trace,
        vec!["read_file".to_string(), "\u{2713} read_file".to_string()],
        "the parent renders the tool starting and then finishing"
    );
}

/// The comparison is only worth anything if a changed mapping breaks it.
#[tokio::test]
async fn a_dropped_tool_event_would_be_caught() {
    let events = replay_scenario(
        scenarios()
            .into_iter()
            .find(|s: &Scenario| s.name == "tool_call_then_result")
            .expect("the scenario exists"),
        policy(),
    )
    .await;

    let expected = reference_trace(&events);
    let mut mapper = TaskMapper::new("task-1");
    let mapped: Vec<String> = events
        .iter()
        // Pretend the mapping forgot tool results.
        .filter(|e| !matches!(e, SessionEvent::ToolCallResult { .. }))
        .filter_map(|e| mapper.map(e))
        .filter_map(|frame| match frame {
            ParticipantFrame::Status {
                message: Some(text),
                ..
            } => Some(text),
            _ => None,
        })
        .collect();

    assert_ne!(
        expected, mapped,
        "a mapping that drops tool results must not compare equal"
    );
}
