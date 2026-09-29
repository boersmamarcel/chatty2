//! TB-7 (AGE-749): stop one agent — a node and everything under it — while
//! the rest of the swarm keeps running.
//!
//! Every hop is real: the root is the kit's in-process root (or its leader
//! tool), every worker a `chatty-tui` process behind the kit's broker, and
//! the stop is the root's direct handle's `cancel`, which is what the
//! desktop's Stop and the TUI's `/stop` call. Nothing here waits on the
//! clock except the deadlines only a failure reaches.

use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_fabric::{CANCELLED_BY_USER, SwarmItem};

use super::swarm_kit::{AgentDef, Endpoint, KitRoot, ROOT_MODEL, SwarmKit};

const LEAD: &str = "kit-lead";
const LEAD_MODEL: &str = "kit/lead";
const STUCK: &str = "kit-stuck";
const STUCK_MODEL: &str = "kit/stuck";
const LEAF: &str = "kit-leaf";
const LEAF_MODEL: &str = "kit/leaf";
const HELPER: &str = "kit-helper";
const HELPER_MODEL: &str = "kit/helper";
const CODER: &str = "kit-coder";
const CODER_MODEL: &str = "kit/coder";

/// Generous for CI: only a failure waits this long.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// A model call that does not come back while the test runs: the worker
/// making it is busy until someone stops it.
const HANG_MS: u64 = 120_000;

fn invoke(agent: &str, prompt: &str) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        serde_json::json!({ "agent": agent, "prompt": prompt }),
    )
}

/// The last message of a request: the tool result the model reads next.
fn last_message(request: &RecordedRequest) -> String {
    request.json()["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .map(|message| message.to_string())
        .unwrap_or_default()
}

/// Wait, up to the deadline, until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(DEADLINE, async {
        while !done() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} before the deadline"));
}

/// The name the broker admitted the running node of `spec` under.
fn node_of(kit: &SwarmKit, spec: &str) -> String {
    let participants = kit.participants();
    participants
        .admitted()
        .into_iter()
        .find(|name| name.starts_with(&format!("{spec}-")) && participants.is_registered(name))
        .unwrap_or_else(|| panic!("a running {spec} node"))
}

/// Every worker has gone away: no run open, no participant connected.
async fn all_reaped(kit: &SwarmKit) {
    let participants = kit.participants();
    until("every worker goes away", || {
        participants.open_runs() == 0
            && !participants
                .admitted()
                .iter()
                .any(|name| participants.is_registered(name))
    })
    .await;
}

/// The broker's edge log as `(from, to, outcome)` task rows.
fn task_rows(kit: &SwarmKit) -> Vec<(String, String, String)> {
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    log.lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("a JSON row"))
        .filter(|row| row["kind"] == "task")
        .map(|row| {
            let field = |key: &str| row[key].as_str().unwrap_or_default().to_string();
            (field("from"), field("to"), field("outcome"))
        })
        .collect()
}

/// root → lead → { stuck → leaf, then helper }. Stopping `stuck` while its
/// leaf is busy takes both away; the lead reads `cancelled_by_user`, calls
/// its other child, which finishes normally, and the root's call ends as
/// if nothing happened.
#[tokio::test]
async fn stop_one_node_cancels_its_subtree_only() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(STUCK, STUCK_MODEL, Endpoint::Ndjson).sub_leader(),
            AgentDef::new(LEAF, LEAF_MODEL, Endpoint::Ndjson),
            AgentDef::new(HELPER, HELPER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                invoke(STUCK, "Dig."),
                invoke(HELPER, "Help."),
                Reply::text("Lead done."),
            ],
        ),
        Script::new()
            .route(
                STUCK_MODEL,
                [invoke(LEAF, "Deeper."), Reply::text("Stuck done.")],
            )
            .route(
                LEAF_MODEL,
                [Reply::Delay(HANG_MS), Reply::text("Leaf done.")],
            )
            .route(HELPER_MODEL, [Reply::text("Helper done.")]),
    )
    .await;

    let stop = async {
        // The leaf is inside its model call: the stuck node's call is
        // running, and so is the one below it.
        until("the leaf is busy", || {
            kit.ndjson.requests_for(LEAF_MODEL).len() == 1
        })
        .await;
        let stuck = node_of(&kit, STUCK);
        let leaf = node_of(&kit, LEAF);
        kit.broker()
            .transport()
            .cancel(&stuck)
            .expect("the stuck node is stopped");
        (stuck, leaf)
    };
    let (run, (stuck, leaf)) = tokio::join!(kit.run_leader_to(LEAD, "Go."), stop);

    assert_eq!(
        run.output
            .expect("the root's call to the lead succeeds")
            .response,
        "Lead done.",
        "the stopped node's parent carries on to its own answer"
    );
    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead.len(), 3, "the lead is asked again after each child");
    let stopped = last_message(&lead[1]);
    assert!(
        stopped.contains(CANCELLED_BY_USER),
        "the lead reads its stopped child as cancelled_by_user: {stopped}"
    );
    assert!(
        last_message(&lead[2]).contains("Helper done."),
        "the sibling finishes normally"
    );
    assert_eq!(
        kit.ndjson.requests_for(STUCK_MODEL).len(),
        1,
        "the stopped node is never asked again"
    );
    assert_eq!(kit.ndjson.requests_for(HELPER_MODEL).len(), 1);

    // The root's tree hears both ends: the stopped node's and the one its
    // call took with it.
    let ended: Vec<(String, String)> = run
        .progress
        .iter()
        .filter_map(|event| match event {
            InvokeAgentProgress::Swarm(batch) => Some(batch),
            _ => None,
        })
        .flat_map(|batch| {
            batch.inner.iter().filter_map(|item| match item {
                SwarmItem::Ended { state } => Some((batch.node.clone(), state.clone())),
                _ => None,
            })
        })
        .collect();
    for node in [&stuck, &leaf] {
        assert!(
            ended.contains(&(node.clone(), "canceled".to_string())),
            "{node} ends canceled in the root's tree: {ended:?}"
        );
    }

    all_reaped(&kit).await;
    let rows = task_rows(&kit);
    assert!(
        rows.iter().any(|(from, to, outcome)| from.starts_with(LEAD)
            && *to == stuck
            && outcome == "cancelled"),
        "the stopped call has a cancelled row: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(from, to, outcome)| *from == stuck && *to == leaf && outcome == "canceled"),
        "the stopped node's own call went with it: {rows:?}"
    );
    assert!(
        rows.iter().any(|(from, to, outcome)| from.starts_with(LEAD)
            && to.starts_with(HELPER)
            && outcome == "completed"),
        "the sibling's row is completed: {rows:?}"
    );
}

/// The root's own callee, stopped by the spec the root's tree knows it by:
/// the root's model reads a typed `cancelled_by_user` tool error and
/// answers without it.
#[tokio::test]
async fn caller_sees_cancelled_by_user() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(STUCK, STUCK_MODEL, Endpoint::Ndjson)],
        Script::new().route(
            ROOT_MODEL,
            [invoke(STUCK, "Dig."), Reply::text("Root done.")],
        ),
        Script::new().route(
            STUCK_MODEL,
            [Reply::Delay(HANG_MS), Reply::text("Stuck done.")],
        ),
    )
    .await;
    let root = KitRoot::build(&kit).await;
    let run = root.run("Start.");

    until("the callee is busy", || {
        kit.ndjson.requests_for(STUCK_MODEL).len() == 1
    })
    .await;
    kit.broker()
        .transport()
        .cancel(STUCK)
        .expect("the root's callee is stopped by its spec");

    let answer = tokio::time::timeout(DEADLINE, run)
        .await
        .expect("the root's run ends before the deadline")
        .expect("the root's run does not panic");
    assert_eq!(answer, "Root done.", "the root carries on without it");
    let requests = kit.sse.requests_for(ROOT_MODEL);
    assert_eq!(requests.len(), 2);
    let result = last_message(&requests[1]);
    assert!(
        result.contains("cancelled_by_user: the user stopped 'kit-stuck'"),
        "the root's model reads the typed result: {result}"
    );
    all_reaped(&kit).await;
    assert!(
        kit.broker().transport().cancel(STUCK).is_err(),
        "nothing is left to stop"
    );
}

/// root → lead → coder, each asking before a command. The coder's command
/// waits on the root's human; stopping the coder withdraws that card hop by
/// hop while the lead is still running — its next model call does not come
/// back — and the command never runs. Stopping the lead then ends the
/// root's call.
#[tokio::test]
async fn parked_approval_withdrawn_on_stop() {
    let kit = SwarmKit::start_asking(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Ndjson),
        ],
        Script::new()
            .route(
                ROOT_MODEL,
                [invoke(LEAD, "Run it."), Reply::text("Root done.")],
            )
            .route(
                LEAD_MODEL,
                [
                    invoke(CODER, "Say hi."),
                    Reply::Delay(HANG_MS),
                    Reply::text("Lead done."),
                ],
            ),
        Script::new().route(
            CODER_MODEL,
            [
                Reply::tool_call("shell_execute", serde_json::json!({ "command": "echo hi" })),
                Reply::text("Coder done."),
            ],
        ),
    )
    .await;
    let mut root = KitRoot::build(&kit).await;
    let run = root.run("Start.");
    let approval = root.next_approval().await;
    assert!(approval.command.ends_with("echo hi"), "{approval:?}");
    assert_eq!(
        root.approvals.pending_ids(),
        std::slice::from_ref(&approval.id)
    );

    let coder = node_of(&kit, CODER);
    kit.broker()
        .transport()
        .cancel(&coder)
        .expect("the parked coder is stopped");

    let withdrawn = tokio::time::timeout(DEADLINE, root.resolved.recv())
        .await
        .expect("the root's card is withdrawn before the deadline")
        .expect("the resolution channel is open");
    assert_eq!(withdrawn.id, approval.id);
    assert!(!withdrawn.approved, "withdrawn, not granted");
    assert!(
        root.approvals.pending_ids().is_empty(),
        "nothing is left waiting on the human"
    );

    // The lead is still running: it has read the stop and is in its next
    // model call.
    until("the lead reads the stop", || {
        kit.sse.requests_for(LEAD_MODEL).len() == 2
    })
    .await;
    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert!(
        last_message(&lead[1]).contains(CANCELLED_BY_USER),
        "the lead reads its stopped child as cancelled_by_user"
    );
    assert!(kit.participants().is_registered(&node_of(&kit, LEAD)));

    kit.broker()
        .transport()
        .cancel(LEAD)
        .expect("the lead is stopped by its spec");
    let answer = tokio::time::timeout(DEADLINE, run)
        .await
        .expect("the root's run ends before the deadline")
        .expect("the root's run does not panic");
    assert_eq!(answer, "Root done.");
    assert_eq!(
        kit.ndjson.requests_for(CODER_MODEL).len(),
        1,
        "the coder never ran the command it was waiting to run"
    );
    all_reaped(&kit).await;
}
