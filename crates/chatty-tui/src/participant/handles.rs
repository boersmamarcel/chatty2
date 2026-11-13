//! Resumable handles on the swarm kit (RC-3, AGE-652): a call that names a
//! handle keeps the callee's conversation in the root broker, and a later
//! call under the same name restores it into a fresh, one-shot worker on
//! the same worktree.

use std::time::Duration;

use chatty_core::testing::append_only::{
    first_rewritten_message, message_elements, without_breakpoints,
};
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentOutput};
use rig_agent::tool::{Tool, ToolContext};

use super::swarm_kit::{
    AgentDef, Endpoint, LeaderRun, SwarmKit, alive, git, subtree, tool_results,
};

const WORKER: &str = "kit-worker";
const WORKER_MODEL: &str = "kit/worker";
const HANDLE: &str = "fixer";

/// The leader's call to the worker, under `handle` when one is given.
fn args(prompt: &str, handle: Option<&str>) -> InvokeAgentArgs {
    InvokeAgentArgs {
        agent: WORKER.to_string(),
        prompt: prompt.to_string(),
        include_trace: false,
        handle: handle.map(str::to_string),
    }
}

async fn call(kit: &SwarmKit, prompt: &str, handle: Option<&str>) -> LeaderRun {
    kit.run_leader_args(kit.leader_tool(), args(prompt, handle))
        .await
}

fn output(run: &LeaderRun) -> &InvokeAgentOutput {
    run.output.as_ref().expect("the delegation succeeded")
}

/// Every worker process the kit started is gone within 5 s.
async fn workers_reaped(kit: &SwarmKit) {
    let since = std::time::Instant::now();
    loop {
        let left: Vec<i32> = subtree(kit).into_iter().filter(|&pid| alive(pid)).collect();
        if left.is_empty() {
            return;
        }
        assert!(
            since.elapsed() < Duration::from_secs(5),
            "worker processes outlived their task: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A repository kit whose worker writes `one.txt` on its first task and,
/// resumed, reads it back and writes `two.txt`.
async fn two_task_kit() -> SwarmKit {
    SwarmKit::start_in_repo(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::tool_call(
                    "write_file",
                    serde_json::json!({ "path": "one.txt", "content": "one\n" }),
                ),
                Reply::text("Wrote one.txt."),
                Reply::tool_call("read_file", serde_json::json!({ "path": "one.txt" })),
                Reply::tool_call(
                    "write_file",
                    serde_json::json!({ "path": "two.txt", "content": "two\n" }),
                ),
                Reply::text("Wrote two.txt."),
            ],
        ),
        Script::new(),
    )
    .await
}

/// Run the two tasks of [`two_task_kit`] under one handle.
async fn two_tasks() -> (SwarmKit, LeaderRun, LeaderRun) {
    let kit = two_task_kit().await;
    let first = call(&kit, "write one.txt", Some(HANDLE)).await;
    let second = call(&kit, "now write two.txt", Some(HANDLE)).await;
    (kit, first, second)
}

/// The fenced evidence block in `response`.
fn evidence(response: &str) -> &str {
    let at = response
        .find("```evidence")
        .unwrap_or_else(|| panic!("no evidence block in {response}"));
    &response[at..]
}

/// The resumed task's first model request starts with everything the
/// handle's first task sent and was answered, verbatim apart from
/// `cache_control` markers: the restored history is an append-only prefix,
/// so the prompt cache holds across the resume.
#[tokio::test]
async fn resume_is_an_append_only_prefix() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
                Reply::text("It is the readme."),
                Reply::text("Still the readme."),
            ],
        ),
        Script::new(),
    )
    .await;
    let first = call(&kit, "read the readme", Some(HANDLE)).await;
    assert_eq!(output(&first).handle.as_deref(), Some(HANDLE));
    let second = call(&kit, "what was it again?", Some(HANDLE)).await;
    assert_eq!(output(&second).response, "Still the readme.");

    let requests = kit.sse.requests_for(WORKER_MODEL);
    assert_eq!(
        requests.len(),
        3,
        "two for the first task, one for the resumed one"
    );
    let earlier = without_breakpoints(&requests[1].body);
    let resumed = without_breakpoints(&requests[2].body);
    if let Some(rewrite) = first_rewritten_message(&earlier, &resumed) {
        panic!(
            "the resumed request rewrote message {} of the first task's:\n{}",
            rewrite.index, rewrite.diff
        );
    }
    // After that prefix: the first task's answer, then the new task.
    let (earlier, resumed) = (message_elements(&earlier), message_elements(&resumed));
    assert_eq!(resumed.len(), earlier.len() + 2, "{resumed:#?}");
    assert!(resumed[earlier.len()].contains("It is the readme."));
    assert!(resumed[earlier.len() + 1].contains("what was it again?"));
}

/// The resumed worker works in the handle's tree: the same directory and
/// branch, with the first task's file and commit in it. No second tree is
/// made.
#[tokio::test]
async fn resume_attaches_same_tree() {
    let (kit, first, second) = two_tasks().await;
    assert!(output(&first).success && output(&second).success);

    let workspace = kit.workspace();
    let trees = workspace.join(".chatty/worktrees");
    let tree = trees.join(format!("{WORKER}-0"));
    assert!(tree.join("one.txt").is_file() && tree.join("two.txt").is_file());
    assert!(
        !trees.join(format!("{WORKER}-1")).exists(),
        "the resumed worker got no tree of its own"
    );
    let branch = format!("sub-agent/{WORKER}-0");
    assert_eq!(
        git(
            &workspace,
            &["rev-list", "--count", &format!("main..{branch}")]
        ),
        "2",
        "both tasks' commits are on the handle's branch"
    );
    // The resumed worker read the first task's file from its tree.
    let requests = kit.sse.requests_for(WORKER_MODEL);
    let body = requests[3].json();
    let read = body["messages"]
        .as_array()
        .expect("a chat request has messages")
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| message["content"].as_str())
        .filter_map(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .find(|result| result["path"] == "one.txt" && result.get("content").is_some())
        .expect("the resumed worker read one.txt");
    assert_eq!(
        read["content"], "one\n",
        "the first task's file was visible"
    );
}

/// Each task's evidence is its own commits: the resumed task's diff names
/// `two.txt` and not `one.txt`, measured from where the task started.
#[tokio::test]
async fn evidence_is_per_task() {
    let (kit, first, second) = two_tasks().await;
    let first_evidence = evidence(&output(&first).response);
    assert!(
        first_evidence.contains("commits: 1") && first_evidence.contains("one.txt"),
        "{first_evidence}"
    );
    let second_evidence = evidence(&output(&second).response);
    let branch = format!("sub-agent/{WORKER}-0");
    let task_base = git(&kit.workspace(), &["rev-parse", &format!("{branch}~1")]);
    assert!(
        second_evidence.contains(&format!("branch: {branch}"))
            && second_evidence.contains(&format!("base: {task_base}"))
            && second_evidence.contains("commits: 1"),
        "{second_evidence}"
    );
    assert!(
        second_evidence.contains("two.txt") && !second_evidence.contains("one.txt"),
        "only this task's commit: {second_evidence}"
    );
}

/// A resumed handle reports how far its tree's branch fell behind the base
/// while it was idle; a plain call and a handle's first call report
/// nothing, and a plain call's JSON is what it was before handles.
#[tokio::test]
async fn behind_by_reported() {
    let kit = two_task_kit().await;
    let first = call(&kit, "write one.txt", Some(HANDLE)).await;
    assert_eq!(output(&first).behind_by, None);

    let workspace = kit.workspace();
    for n in 0..2 {
        std::fs::write(workspace.join(format!("main{n}.txt")), "main\n").unwrap();
        git(&workspace, &["add", "."]);
        git(
            &workspace,
            &["commit", "-q", "-m", &format!("main moves {n}")],
        );
    }
    let second = call(&kit, "now write two.txt", Some(HANDLE)).await;
    assert_eq!(output(&second).handle.as_deref(), Some(HANDLE));
    assert_eq!(output(&second).behind_by, Some(2));
    let model_sees = serde_json::to_value(output(&second)).unwrap();
    assert_eq!(model_sees["handle"], HANDLE);
    assert_eq!(model_sees["behind_by"], 2);
}

/// RO-4: with a real broker, real workers and a real repository,
/// `list_agents` shows the root's handle as `idle` (its reach) and
/// `parked`, with its owner, nothing waiting, and `behind_by: 1` once the
/// leader's branch moves on. Only that row carries the handle's fields.
/// (Only a node may message a handle, so the waiting-mail count is tested
/// at the broker, `handle_rows_count_waiting_mail`, and the whole row in
/// chatty-core, `handle_rows_show_state`.)
#[tokio::test]
async fn handle_rows_show_the_brokers_state() {
    use chatty_core::tools::list_agents_tool::{ListAgentsTool, ListAgentsToolArgs};

    let kit = two_task_kit().await;
    call(&kit, "write one.txt", Some(HANDLE)).await;
    let transport = kit.broker().transport();

    let workspace = kit.workspace();
    std::fs::write(workspace.join("leader.txt"), "leader\n").unwrap();
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-q", "-m", "the leader moves on"]);

    let listed = ListAgentsTool::new(vec![])
        .with_local_workers([WORKER])
        .with_transport(transport)
        .call(&mut ToolContext::new(), ListAgentsToolArgs {})
        .await
        .expect("list_agents succeeds");
    let rows = serde_json::to_value(&listed.agents).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == HANDLE)
        .unwrap_or_else(|| panic!("the handle is listed: {rows}"));
    assert_eq!(row["origin"], "handle");
    assert_eq!(row["reach"], "idle");
    assert_eq!(row["state"], "parked");
    assert_eq!(row["owner"], chatty_fabric::ROOT_NAME);
    assert_eq!(row["pending_messages"], 0);
    assert_eq!(row["behind_by"], 1);
    for other in rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["name"] != HANDLE)
    {
        assert!(
            other.get("owner").is_none() && other.get("pending_messages").is_none(),
            "only handle rows carry handle fields: {other}"
        );
    }
}

/// A call naming no handle keeps nothing, and its JSON carries neither key.
#[tokio::test]
async fn a_plain_call_keeps_no_handle() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(WORKER_MODEL, [Reply::text("Done.")]),
        Script::new(),
    )
    .await;
    let run = call(&kit, "do it", None).await;
    let out = output(&run);
    assert_eq!((out.handle.as_deref(), out.behind_by), (None, None));
    let model_sees = serde_json::to_value(out).unwrap();
    assert!(
        model_sees.get("handle").is_none() && model_sees.get("behind_by").is_none(),
        "{model_sees}"
    );
}

/// Two calls to one handle never run at once, though the endpoint would
/// let them: the second waits for the first, then resumes its
/// conversation.
#[tokio::test]
async fn handle_tasks_are_queued() {
    let kit = SwarmKit::start_with_endpoint_budget(
        2,
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::Delay(1500),
                Reply::text("first done"),
                Reply::text("second done"),
            ],
        ),
        Script::new(),
    )
    .await;
    let tool = kit.leader_tool();
    let first = tokio::spawn(async move {
        tool.call(&mut ToolContext::new(), args("first task", Some(HANDLE)))
            .await
    });
    // The first task's request is in before the second call is made.
    let since = std::time::Instant::now();
    while kit.sse.requests_for(WORKER_MODEL).is_empty() {
        assert!(
            since.elapsed() < Duration::from_secs(60),
            "the first task never asked"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let second = call(&kit, "second task", Some(HANDLE)).await;
    let first = first.await.unwrap().expect("the first task succeeded");
    assert_eq!(first.response, "first done");
    assert_eq!(output(&second).response, "second done");

    assert_eq!(
        kit.sse.max_concurrency(),
        1,
        "one run of the handle at a time"
    );
    let requests = kit.sse.requests_for(WORKER_MODEL);
    let resumed = String::from_utf8_lossy(&requests[1].body);
    assert!(
        resumed.contains("first task") && resumed.contains("first done"),
        "the second task resumed the first's conversation: {resumed}"
    );
}

/// A call waiting in a handle's queue keeps its caller alive: while the
/// call ahead of it runs for longer than the caller's tool stall timeout,
/// the broker heartbeats the queued call, so the root's turn does not end
/// as stalled. Every watchdog duration is divided by 60 (the root's through
/// `with_stall_scale`, which the broker reads when it starts; the workers'
/// through `CHATTY_TEST_STALL_SCALE`): a 13 s tool timeout, a 0.5 s
/// heartbeat.
#[tokio::test]
async fn a_queued_handle_call_does_not_stall_its_caller() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use chatty_core::services::{StreamErrorKind, with_stall_scale};
    use chatty_core::session::{SessionEvent, TurnInput};
    use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;

    use super::slash_agent::{ROOT_MODEL, root_session_with};

    const SCALE: u32 = 60;
    const ROOT_TOOL_TIMEOUT: Duration = Duration::from_secs(13);
    let kit = with_stall_scale(
        SCALE,
        SwarmKit::start_stall_scaled(
            SCALE,
            vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
            Script::new()
                .route(
                    ROOT_MODEL,
                    [
                        Reply::tool_call(
                            "invoke_agent",
                            serde_json::json!({
                                "agent": WORKER, "prompt": "second task", "handle": HANDLE
                            }),
                        ),
                        Reply::text("The queued call came back."),
                    ],
                )
                .route(
                    WORKER_MODEL,
                    [
                        // The first task holds the handle for 20 s.
                        Reply::reason(80, 250),
                        Reply::text("first done"),
                        Reply::text("second done"),
                    ],
                ),
            Script::new(),
        ),
    )
    .await;

    // The first task, under the handle, from the same owner: the root.
    let tool = kit.leader_tool();
    let first = tokio::spawn(async move {
        tool.call(&mut ToolContext::new(), args("first task", Some(HANDLE)))
            .await
    });
    let since = std::time::Instant::now();
    while kit.sse.requests_for(WORKER_MODEL).is_empty() {
        assert!(
            since.elapsed() < Duration::from_secs(60),
            "the first task never asked"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The root's turn calls the same handle and waits behind it.
    let mut session = root_session_with(&kit, &[WORKER]).await;
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let asked = std::time::Instant::now();
    let turn = session
        .begin_turn(TurnInput::text("Run the second task."), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the root's turn starts");
    tokio::time::timeout(Duration::from_secs(120), with_stall_scale(SCALE, turn))
        .await
        .expect("the root's turn ends before the deadline");
    let waited = asked.elapsed();
    drop(session);
    let events = Rc::try_unwrap(events).unwrap().into_inner();

    assert!(first.await.unwrap().is_ok(), "the first task succeeded");
    assert!(
        waited > ROOT_TOOL_TIMEOUT,
        "the queued call outlasted the root's tool stall timeout: {waited:?}"
    );
    let stalls: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Error(e) if e.kind == StreamErrorKind::Stalled))
        .collect();
    assert!(stalls.is_empty(), "the root stalled: {stalls:#?}");
    let heartbeats = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Delegation(InvokeAgentProgress::Heartbeat)))
        .count();
    assert!(
        heartbeats >= 10,
        "the queue heartbeats reached the root: {heartbeats}"
    );
    let root_requests = kit.sse.requests_for(ROOT_MODEL);
    assert_eq!(root_requests.len(), 2, "the root's model read the result");
    let result = String::from_utf8_lossy(&root_requests[1].body);
    assert!(result.contains("second done"), "{result}");
}

/// A resumed worker is as one-shot as any: after each task under the
/// handle, its process is gone.
#[tokio::test]
async fn resumed_worker_is_still_one_shot() {
    let kit = two_task_kit().await;
    let first = call(&kit, "write one.txt", Some(HANDLE)).await;
    assert!(output(&first).success);
    workers_reaped(&kit).await;
    let second = call(&kit, "now write two.txt", Some(HANDLE)).await;
    assert!(output(&second).success);
    workers_reaped(&kit).await;
    assert_eq!(
        kit.participants().admitted(),
        [format!("{WORKER}-0"), format!("{WORKER}-1")],
        "a fresh worker per task"
    );
}

/// A handle holds one agent's conversation: naming it on a call to another
/// agent is refused before anything is spawned.
#[tokio::test]
async fn a_handle_is_bound_to_its_agent() {
    const OTHER: &str = "kit-other";
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse),
            AgentDef::new(OTHER, "kit/other", Endpoint::Sse),
        ],
        Script::new().route(WORKER_MODEL, [Reply::text("Done.")]),
        Script::new(),
    )
    .await;
    assert!(output(&call(&kit, "do it", Some(HANDLE)).await).success);
    let refused = kit
        .run_leader_args(
            kit.leader_tool(),
            InvokeAgentArgs {
                agent: OTHER.to_string(),
                ..args("do it too", Some(HANDLE))
            },
        )
        .await;
    let error = refused
        .output
        .expect_err("another agent's handle is refused");
    assert!(
        error.contains("holds a conversation with 'kit-worker'"),
        "{error}"
    );
    assert!(kit.sse.requests_for("kit/other").is_empty());
}

const LEAD: &str = "kit-lead";
const LEAD_MODEL: &str = "kit/lead";
const SENDER: &str = "kit-sender";
const SENDER_MODEL: &str = "kit/sender";

/// How often `needle` occurs in a model request's body.
fn occurrences(request: &chatty_core::testing::fake_model::RecordedRequest, needle: &str) -> usize {
    String::from_utf8_lossy(&request.body)
        .matches(needle)
        .count()
}

fn invoke_fixer(prompt: &str) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        serde_json::json!({ "agent": WORKER, "prompt": prompt, "handle": HANDLE }),
    )
}

/// Fabric spec 5 invariant 8 (TM-4): an owner's message to its idle
/// handle waits for the handle's next task and opens it — in the handle's
/// first model request of that task, wrapped as untrusted, exactly once:
/// the task after that carries it only as history.
#[tokio::test]
async fn owner_to_handle_delivered_next_run() {
    const TEXT: &str = "use the second approach";
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse),
        ],
        Script::new()
            .route(
                LEAD_MODEL,
                [
                    invoke_fixer("first task"),
                    Reply::tool_call(
                        "send_message",
                        serde_json::json!({ "to": HANDLE, "text": TEXT }),
                    ),
                    invoke_fixer("second task"),
                    invoke_fixer("third task"),
                    Reply::text("Done."),
                ],
            )
            .route(
                WORKER_MODEL,
                [
                    Reply::text("first done"),
                    Reply::text("second done"),
                    Reply::text("third done"),
                ],
            ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader_to(LEAD, "drive the fixer").await;
    assert_eq!(output(&run).response, "Done.");

    // The message was accepted: the lead's request after the send says so.
    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead.len(), 5);
    assert_eq!(
        tool_results(&lead[2]).last(),
        Some(&serde_json::json!({ "status": "pending", "id": "msg-1" })),
        "accepted"
    );

    let wrapped = chatty_fabric::wrap_message(&format!("{LEAD}-0"), TEXT);
    // Request bodies are JSON: the wrapper's quotes are escaped in them.
    let wrapped = wrapped.replace('"', "\\\"");
    let worker = kit.sse.requests_for(WORKER_MODEL);
    assert_eq!(worker.len(), 3);
    assert_eq!(occurrences(&worker[0], TEXT), 0, "not in the task before");
    assert_eq!(
        occurrences(&worker[1], &wrapped),
        1,
        "in the next task's first request, wrapped, once"
    );
    assert_eq!(occurrences(&worker[1], TEXT), 1);
    assert_eq!(
        occurrences(&worker[2], TEXT),
        1,
        "the task after has it as history only, not delivered again"
    );
}

/// TM-4: only a handle's owner may message it. The root owns `fixer`; a
/// worker under the same root that names it is refused as `not_on_tree`,
/// and the handle's next task never carries the text.
#[tokio::test]
async fn non_owner_to_handle_refused() {
    const TEXT: &str = "I am not your owner";
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse),
            AgentDef::new(SENDER, SENDER_MODEL, Endpoint::Sse),
        ],
        Script::new()
            .route(
                WORKER_MODEL,
                [Reply::text("first done"), Reply::text("second done")],
            )
            .route(
                SENDER_MODEL,
                [
                    Reply::tool_call(
                        "send_message",
                        serde_json::json!({ "to": HANDLE, "text": TEXT }),
                    ),
                    Reply::text("Sent."),
                ],
            ),
        Script::new(),
    )
    .await;

    assert!(output(&call(&kit, "first task", Some(HANDLE)).await).success);
    let sender = kit.run_leader_to(SENDER, "message the fixer").await;
    assert_eq!(output(&sender).response, "Sent.");
    let requests = kit.sse.requests_for(SENDER_MODEL);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_results(&requests[1]),
        [serde_json::json!({ "status": "refused", "reason": "not_on_tree" })]
    );

    assert!(output(&call(&kit, "second task", Some(HANDLE)).await).success);
    for request in kit.sse.requests_for(WORKER_MODEL) {
        assert_eq!(occurrences(&request, TEXT), 0);
    }
}

/// TM-4: an owner's ancestor may message the handle too. `boss`, a handle of
/// `kit-lead`, starts the handle `fixer`; `kit-lead` is above `boss` on the
/// tree, so its message to `fixer` is accepted and opens `boss`'s next
/// task's call to `fixer`.
#[tokio::test]
async fn ancestor_to_handle_delivered_next_run() {
    const BOSS: &str = "kit-boss";
    const BOSS_MODEL: &str = "kit/boss";
    const TEXT: &str = "the second approach, from above";
    let invoke_boss = |prompt: &str| {
        Reply::tool_call(
            "invoke_agent",
            serde_json::json!({ "agent": BOSS, "prompt": prompt, "handle": "boss" }),
        )
    };
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(BOSS, BOSS_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse),
        ],
        Script::new()
            .route(
                LEAD_MODEL,
                [
                    invoke_boss("first task"),
                    Reply::tool_call(
                        "send_message",
                        serde_json::json!({ "to": HANDLE, "text": TEXT }),
                    ),
                    invoke_boss("second task"),
                    Reply::text("Done."),
                ],
            )
            .route(
                BOSS_MODEL,
                [
                    invoke_fixer("first task"),
                    Reply::text("boss one"),
                    invoke_fixer("second task"),
                    Reply::text("boss two"),
                ],
            )
            .route(
                WORKER_MODEL,
                [Reply::text("first done"), Reply::text("second done")],
            ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader_to(LEAD, "drive the boss").await;
    assert_eq!(output(&run).response, "Done.");

    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead.len(), 4);
    assert_eq!(
        tool_results(&lead[2]).last(),
        Some(&serde_json::json!({ "status": "pending", "id": "msg-1" })),
        "an ancestor of the owner is accepted"
    );

    let wrapped = chatty_fabric::wrap_message(&format!("{LEAD}-0"), TEXT).replace('"', "\\\"");
    let worker = kit.sse.requests_for(WORKER_MODEL);
    assert_eq!(worker.len(), 2);
    assert_eq!(occurrences(&worker[0], TEXT), 0);
    assert_eq!(occurrences(&worker[1], &wrapped), 1);
}
