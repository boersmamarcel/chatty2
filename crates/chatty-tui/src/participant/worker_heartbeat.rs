//! AGE-932 on the swarm kit: a worker busy in one long model call keeps its
//! root's turn alive with rate-limited heartbeats, and a worker whose model
//! is truly silent fails its own delegation rather than the root's turn.
//!
//! The root is a real `AgentSession` (its stall watchdog is the one under
//! test), the worker a real `chatty-tui` on the fake model. Both run with
//! every watchdog duration divided by [`SCALE`] — the root through
//! `with_stall_scale`, the worker through the kit's `CHATTY_TEST_STALL_SCALE`
//! — so production's ratios hold at a sixtieth of the time: a 3 s stall
//! timeout, a 13 s tool timeout and a 0.5 s heartbeat.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use chatty_core::services::{StreamErrorKind, with_stall_scale};
use chatty_core::session::{SessionEvent, TurnInput};
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use serde_json::json;

use super::slash_agent::{ROOT_MODEL, root_session_with};
use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

/// Every watchdog duration, root's and worker's, is divided by this.
const SCALE: u32 = 60;
const WORKER: &str = "kit-thinker";
const WORKER_MODEL: &str = "kit/thinker";
/// How long the test waits for the root's turn before failing.
const DEADLINE: Duration = Duration::from_secs(120);

/// The root's tool stall timeout under [`SCALE`]: 780 s / 60.
const ROOT_TOOL_TIMEOUT: Duration = Duration::from_secs(13);
/// The heartbeat interval under [`SCALE`]: 30 s / 60.
const HEARTBEAT: Duration = Duration::from_millis(500);

/// The root asks its model, which delegates to [`WORKER`] once and then
/// answers `answer`; every event of the turn.
async fn root_turn(kit: &SwarmKit) -> Vec<SessionEvent> {
    let mut session = root_session_with(kit, &[WORKER]).await;
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text("Ask the thinker."), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the root's turn starts");
    tokio::time::timeout(DEADLINE, with_stall_scale(SCALE, turn))
        .await
        .expect("the root's turn ends before the deadline");
    drop(session);
    Rc::try_unwrap(events).unwrap().into_inner()
}

fn delegate() -> Reply {
    Reply::tool_call(
        "invoke_agent",
        json!({ "agent": WORKER, "prompt": "Think it over." }),
    )
}

fn stalls(events: &[SessionEvent]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter(
            |event| matches!(event, SessionEvent::Error(e) if e.kind == StreamErrorKind::Stalled),
        )
        .collect()
}

fn text(events: &[SessionEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The worker streams nothing but reasoning, a delta every 250 ms, for
/// 16 s — longer than the root's tool stall timeout — and then answers.
/// Before AGE-932 the root heard nothing from it after its first steps and
/// ended its own turn as stalled 13 (scaled) seconds in.
#[tokio::test]
async fn long_worker_call_with_streaming_reasoning_does_not_stall_root() {
    let kit = SwarmKit::start_stall_scaled(
        SCALE,
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new()
            .route(
                ROOT_MODEL,
                [delegate(), Reply::text("The thinker answered.")],
            )
            .route(
                WORKER_MODEL,
                [Reply::reason(64, 250), Reply::text("Forty-two.")],
            ),
        Script::new(),
    )
    .await;

    let events = root_turn(&kit).await;

    assert!(stalls(&events).is_empty(), "the root stalled: {events:#?}");
    let worker = kit.sse.requests_for(WORKER_MODEL);
    assert_eq!(worker.len(), 1, "one long model call");
    let call = worker[0].finished.unwrap() - worker[0].arrived;
    assert!(
        call > ROOT_TOOL_TIMEOUT,
        "the worker's call outlasts the root's tool stall timeout: {call:?}"
    );
    let heartbeats = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Delegation(InvokeAgentProgress::Heartbeat)))
        .count();
    // 16 s at one per 0.5 s, give or take the rate limit's edges; far fewer
    // than the 64 deltas the worker's model streamed.
    assert!(
        (10..=40).contains(&heartbeats),
        "the worker's heartbeats reach the root, rate-limited: {heartbeats}"
    );
    assert_eq!(
        kit.sse.requests_for(ROOT_MODEL).len(),
        2,
        "the root's model read the worker's answer"
    );
    let root_requests = kit.sse.requests_for(ROOT_MODEL);
    let result = String::from_utf8_lossy(&root_requests[1].body);
    assert!(result.contains("Forty-two."), "{result}");
    assert!(
        !result.contains("thinking thinking"),
        "the worker's reasoning is not its answer: {result}"
    );
    assert!(
        text(&events).contains("The thinker answered."),
        "the root's turn completes: {events:#?}"
    );
}

/// The same long call one level down: the root delegates to a sub-leader,
/// which delegates to the thinker. The thinker's heartbeats reach the
/// sub-leader as progress and go on up through its own mapper, rate-limited
/// again, so neither the sub-leader's turn nor the root's stalls.
#[tokio::test]
async fn heartbeats_travel_through_a_sub_leader() {
    const LEAD: &str = "kit-lead";
    const LEAD_MODEL: &str = "kit/lead";
    let kit = SwarmKit::start_stall_scaled(
        SCALE,
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse),
        ],
        Script::new()
            .route(
                ROOT_MODEL,
                [
                    Reply::tool_call(
                        "invoke_agent",
                        json!({ "agent": LEAD, "prompt": "Have the thinker think." }),
                    ),
                    Reply::text("The lead reported back."),
                ],
            )
            .route(
                LEAD_MODEL,
                [delegate(), Reply::text("The thinker said 42.")],
            )
            .route(
                WORKER_MODEL,
                [Reply::reason(64, 250), Reply::text("Forty-two.")],
            ),
        Script::new(),
    )
    .await;
    let mut session = root_session_with(&kit, &[LEAD, WORKER]).await;
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text("Ask the lead."), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the root's turn starts");
    tokio::time::timeout(DEADLINE, with_stall_scale(SCALE, turn))
        .await
        .expect("the root's turn ends before the deadline");
    drop(session);
    let events = Rc::try_unwrap(events).unwrap().into_inner();

    assert!(stalls(&events).is_empty(), "the root stalled: {events:#?}");
    let worker = kit.sse.requests_for(WORKER_MODEL);
    assert_eq!(worker.len(), 1, "one long model call");
    let call = worker[0].finished.unwrap() - worker[0].arrived;
    assert!(call > ROOT_TOOL_TIMEOUT, "{call:?}");
    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead.len(), 2, "the lead's turn did not stall either");
    assert!(String::from_utf8_lossy(&lead[1].body).contains("Forty-two."));
    assert!(
        text(&events).contains("The lead reported back."),
        "the root's turn completes: {events:#?}"
    );
}

/// The root's own model thinks for longer than several heartbeat
/// intervals. Nobody listens to a root's liveness, and a hosted frontend
/// reads any `Delegation` event as a delegation, so the root's turn emits
/// no heartbeat — only a delegated worker's does.
#[tokio::test]
async fn a_root_turn_that_thinks_long_emits_no_heartbeat() {
    let kit = SwarmKit::start_stall_scaled(
        SCALE,
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            ROOT_MODEL,
            [Reply::reason(32, 250), Reply::text("Thought it over.")],
        ),
        Script::new(),
    )
    .await;

    let events = root_turn(&kit).await;

    assert!(stalls(&events).is_empty(), "the root stalled: {events:#?}");
    let root = kit.sse.requests_for(ROOT_MODEL);
    assert_eq!(root.len(), 1);
    let call = root[0].finished.unwrap() - root[0].arrived;
    assert!(
        call > HEARTBEAT * 10,
        "the root thought for many heartbeat intervals: {call:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::Delegation(_))),
        "a root's turn emits no delegation event of its own: {events:#?}"
    );
    assert!(kit.sse.requests_for(WORKER_MODEL).is_empty());
    assert!(text(&events).contains("Thought it over."));
}

/// The worker's model accepts the request and sends nothing. The worker's
/// own watchdog ends its turn (and its headless resumes) and the task
/// fails; the root's model reads a failed delegation and answers. The
/// root's turn is never the one that stalls.
#[tokio::test]
async fn silent_worker_still_fails_its_delegation_not_the_turn() {
    // One silent call per attempt: the first and its two headless resumes
    // (HEADLESS_STALL_RESUME_ATTEMPTS), each well past the worker's 3 s.
    let silent = || [Reply::Delay(20_000), Reply::text("Too late.")];
    let kit = SwarmKit::start_stall_scaled(
        SCALE,
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new()
            .route(
                ROOT_MODEL,
                [delegate(), Reply::text("The thinker went quiet.")],
            )
            .route(
                WORKER_MODEL,
                silent().into_iter().chain(silent()).chain(silent()),
            ),
        Script::new(),
    )
    .await;

    let events = root_turn(&kit).await;

    assert!(stalls(&events).is_empty(), "the root stalled: {events:#?}");
    let failed = events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Delegation(InvokeAgentProgress::Finished { success: false, .. })
        )
    });
    assert!(failed, "the delegation failed: {events:#?}");
    let root_requests = kit.sse.requests_for(ROOT_MODEL);
    assert_eq!(root_requests.len(), 2, "the root's model read the failure");
    let result = String::from_utf8_lossy(&root_requests[1].body);
    assert!(
        result.contains("stopped responding"),
        "the root's model reads the worker's stall: {result}"
    );
    assert!(
        text(&events).contains("The thinker went quiet."),
        "the root's turn completes: {events:#?}"
    );
}
