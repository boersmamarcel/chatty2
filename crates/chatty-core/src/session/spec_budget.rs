//! A spec's turn cap holds in a session built from it (AGE-992).
//!
//! hive's hosted worker builds its agent from the lease's spec
//! (`AgentBuildContext::from_spec`) but runs its turn on the user's own
//! execution settings, whose `max_agent_turns` is `0`: no cap. One analyst
//! made ~300 model calls on one delegation that way. The session takes the
//! spec's cap from the context its conversation was built with, so the
//! worker stops at it with a tool-free best-effort answer.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::agent_spec::AgentSpec;
use crate::services::StreamSurface;
use crate::services::turn_budget::FINAL_TURN_NOTE;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
use crate::testing::fake_model::{FakeDaemon, Reply, Script};

const MODEL: &str = "age-992-worker-model";

/// A model that calls a tool whenever it is offered one, and answers only
/// once it is not: the wrap-up request is the one whose last tool result
/// carries [`FINAL_TURN_NOTE`].
fn never_stops() -> Script {
    let calls = (0..50).map(|i| {
        Reply::tool_call(
            "calculate",
            serde_json::json!({ "expression": format!("1 + {i}") }),
        )
    });
    Script::new()
        .route_body("No tool turns left", [Reply::text("Best so far: 42.")])
        .route(MODEL, calls)
}

/// A worker session for `spec` as hive's runs one: the user's settings,
/// capped at `settings_turns` (`0`, none, as a hosted lease's are), on the
/// interactive surface with the loop guard on.
async fn worker(
    spec: &AgentSpec,
    settings_turns: u32,
    daemon: &FakeDaemon,
) -> (AgentSession, tempfile::TempDir) {
    let _ = crate::init_repositories();
    let workspace = tempfile::tempdir().expect("a workspace");
    let settings = ExecutionSettingsModel {
        workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
        fetch_enabled: false,
        max_agent_turns: settings_turns,
        ..ExecutionSettingsModel::default()
    };
    let model = ModelConfig::new(
        MODEL.to_string(),
        MODEL.to_string(),
        ProviderType::OpenRouter,
        MODEL.to_string(),
    );
    let provider = ProviderConfig::new("Hive".to_string(), ProviderType::OpenRouter)
        .with_api_key("lease-token".to_string())
        .with_base_url(daemon.base_url());
    let built = AgentBuildContext::from_spec(
        spec,
        crate::factories::agent_factory::AgentServices {
            exec_settings: Some(settings.clone()),
            ..Default::default()
        },
    )
    .expect("the spec builds");
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings,
        surface: StreamSurface::InteractiveTui,
        loop_guard: true,
    });
    session
        .create_conversation(
            "age-992".to_string(),
            "Delegated task".to_string(),
            &model,
            &provider,
            built.context,
        )
        .await
        .expect("the conversation builds");
    (session, workspace)
}

async fn run_turn(session: &mut AgentSession, text: &str) -> Vec<SessionEvent> {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    session
        .begin_turn(TurnInput::text(text), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the turn starts")
        .await;
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    for event in &events {
        session.apply(event);
    }
    events
}

fn offers_tools(request: &crate::testing::fake_model::RecordedRequest) -> bool {
    request.json()["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty())
}

/// The spec caps a worker at three tool turns while its settings cap
/// nothing: three tool calls, then one tool-free call whose text is the
/// answer, and the last tool result says why (the note the worker's mapper
/// tells its caller from).
#[tokio::test]
async fn a_specs_turn_cap_stops_an_uncapped_session_with_a_best_effort_answer() {
    let daemon = FakeDaemon::scripted(never_stops());
    let mut spec = AgentSpec::named("data-analyst");
    spec.budget.max_agent_turns = Some(3);
    let (mut session, _workspace) = worker(&spec, 0, &daemon).await;

    let events = run_turn(&mut session, "Compute the answer.").await;

    let requests = daemon.requests();
    assert_eq!(requests.len(), 4, "three tool turns and the wrap-up");
    assert!(requests[..3].iter().all(offers_tools));
    assert!(!offers_tools(&requests[3]), "the wrap-up call has no tools");
    assert!(
        !events.iter().any(|e| matches!(e, SessionEvent::Error(_))),
        "the run ends normally: {events:?}"
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Best so far: 42.");
    assert!(
        events.iter().any(|e| matches!(
            e,
            SessionEvent::ToolCallResult { result, .. } if result.contains(FINAL_TURN_NOTE)
        )),
        "the last tool result carries the wrap-up note: {events:?}"
    );
}

/// The settings' own cap still holds where it is the tighter one, and a
/// spec without a cap (absent, or `0`) leaves the settings' as it was.
#[tokio::test]
async fn the_tighter_of_the_settings_and_the_spec_holds() {
    for spec_turns in [None, Some(0), Some(5)] {
        let daemon = FakeDaemon::scripted(never_stops());
        let mut spec = AgentSpec::named("worker");
        spec.budget.max_agent_turns = spec_turns;
        let (mut session, _workspace) = worker(&spec, 2, &daemon).await;

        run_turn(&mut session, "Compute the answer.").await;

        assert_eq!(
            daemon.requests().len(),
            3,
            "two tool turns and the wrap-up under spec cap {spec_turns:?}"
        );
    }
}
