//! ADR-0092 (AGE-1677) at the wire: the runs of one root form a tree, and
//! a cancel or a deadline stops a task's whole subtree, deepest runs first,
//! before the task reports its end.
//!
//! The workers speak raw frames over the connection the broker made for
//! them, as in `swarm_forwarding_tests`. A worker's handle stands in for its
//! process: dropping it (the broker reaping the worker) records the reap and
//! closes the worker's socket, which is what a killed process does. These
//! are the test plan's T2.2 ("cancel cascades") and T2.7 ("diamond").

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

use super::*;
use crate::participant::{LocalConnection, WorkerFuture, WorkerHandle, open_connection};
use chatty_fabric::Remaining;
use serde_json::{Value, json};

/// What happened, in order: `reaped <node>` when a worker's handle is
/// dropped, `task <spec> <text>` when a worker gets its task.
type Log = Arc<Mutex<Vec<String>>>;

/// What a scripted worker does with its one task.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Call the agents of each step at once, wait for all their results,
    /// then the next step, whose prompt carries every response so far.
    /// Then answer `done <taskId>` and complete.
    Steps(&'static [&'static [&'static str]]),
    /// Like `Steps`, but never complete: wait until reaped.
    StepsThenHang(&'static [&'static [&'static str]]),
    /// Answer `branch <taskId>` and complete.
    Answer,
    /// Never answer: a model call that takes forever.
    Hang,
}

fn line(value: Value) -> String {
    let mut value = value;
    value["v"] = json!(3);
    format!("{value}\n")
}

async fn serve(stream: UnixStream, spec: String, behaviour: Behaviour, log: Log) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({"id": 1, "method": "session.hello",
        "params": {"card": {}, "schema": chatty_fabric::wire::schema::hash()}});
    write.write_all(line(hello).as_bytes()).await.unwrap();
    while let Ok(Some(text)) = lines.next_line().await {
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["method"] != "task.run" {
            continue;
        }
        let run = frame["id"].clone();
        let task_id = frame["params"]["taskId"].as_str().unwrap().to_string();
        let prompt = frame["params"]["text"].as_str().unwrap_or_default();
        log.lock().unwrap().push(format!("task {spec} {prompt}"));
        let (steps, hang) = match behaviour {
            Behaviour::Steps(steps) => (steps, false),
            Behaviour::StepsThenHang(steps) => (steps, true),
            Behaviour::Answer => (&[][..], false),
            Behaviour::Hang => (&[][..], true),
        };
        let mut responses = Vec::new();
        let mut next_id = 2u64;
        for step in steps {
            let mut waiting = Vec::new();
            for agent in *step {
                let call = json!({"id": next_id, "method": "agent.invoke",
                    "params": {"agent": agent, "prompt": format!("go {}", responses.join(" + ")),
                               "run": task_id}});
                write.write_all(line(call).as_bytes()).await.unwrap();
                waiting.push(next_id);
                next_id += 1;
            }
            while !waiting.is_empty() {
                let Ok(Some(text)) = lines.next_line().await else {
                    return;
                };
                let reply: Value = serde_json::from_str(&text).unwrap();
                let Some(id) = reply["id"].as_u64() else {
                    continue;
                };
                if reply.get("method").is_none() && waiting.contains(&id) {
                    waiting.retain(|w| *w != id);
                    responses.push(
                        reply["result"]["response"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                    );
                }
            }
        }
        if !hang {
            let answer = match behaviour {
                Behaviour::Answer => format!("branch {task_id}"),
                _ => format!("done {task_id}"),
            };
            let chunk = json!({"method": "task.event", "params": {"kind": "artifact",
                "id": run, "text": answer, "lastChunk": true}});
            write.write_all(line(chunk).as_bytes()).await.unwrap();
            let done = json!({"id": run, "result": {"state": "completed"}});
            write.write_all(line(done).as_bytes()).await.unwrap();
        }
        // Hold the connection until the worker is reaped.
        while let Ok(Some(_)) = lines.next_line().await {}
        return;
    }
}

struct Scripted {
    name: String,
    registry: ParticipantRegistry,
    behaviour: Behaviour,
    log: Log,
}

/// A worker as its runner holds it: dropping it reaps the "process".
struct Reaped {
    name: String,
    task_id: String,
    serving: JoinHandle<()>,
    log: Log,
}

impl WorkerHandle for Reaped {
    fn name(&self) -> &str {
        &self.name
    }

    fn task_id(&self) -> Option<&str> {
        Some(&self.task_id)
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&chatty_fabric::wire::TaskMetadata>) {}
}

impl Drop for Reaped {
    fn drop(&mut self) {
        self.log
            .lock()
            .unwrap()
            .push(format!("reaped {}", self.name));
        self.serving.abort();
    }
}

impl VirtualAgent for Scripted {
    fn agent_name(&self) -> &str {
        &self.name
    }

    fn agent_card(&self) -> super::super::protocol::ParticipantCard {
        super::super::protocol::ParticipantCard {
            name: self.name.clone(),
            ..Default::default()
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        Box::pin(async move {
            let spawner = task.call.as_ref().and_then(|call| call.caller.as_deref());
            let LocalConnection { name, worker_end } =
                open_connection(&self.registry, &self.name, spawner)?;
            worker_end.set_nonblocking(true)?;
            let stream = UnixStream::from_std(worker_end)?;
            let serving = tokio::spawn(serve(
                stream,
                self.name.clone(),
                self.behaviour,
                self.log.clone(),
            ));
            let registered = tokio::time::Instant::now();
            while !self.registry.is_registered(&name) {
                assert!(
                    registered.elapsed() < Duration::from_secs(10),
                    "never said hello"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let (task_id, updates) = self
                .registry
                .submit_task(&name, task)
                .await
                .ok_or_else(|| anyhow::anyhow!("gone before its task"))?;
            let handle: Box<dyn WorkerHandle> = Box::new(Reaped {
                name,
                task_id,
                serving,
                log: self.log.clone(),
            });
            Ok((handle, updates))
        })
    }
}

fn broker(
    agents: &[(&str, Behaviour)],
    edges: Option<Arc<Mutex<EdgeLog>>>,
) -> (Arc<BrokerCalls>, ParticipantRegistry, Log) {
    let registry = ParticipantRegistry::new();
    let log = Log::default();
    let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = agents
        .iter()
        .map(|(name, behaviour)| {
            let runner: Arc<dyn VirtualAgent> = Arc::new(Scripted {
                name: name.to_string(),
                registry: registry.clone(),
                behaviour: *behaviour,
                log: log.clone(),
            });
            (name.to_string(), runner)
        })
        .collect();
    let calls = Arc::new(BrokerCalls::new(registry.clone(), Arc::new(runners), edges));
    registry.install_calls(&calls);
    (calls, registry, log)
}

fn invoke(agent: &str, remaining: Remaining) -> CallRequest {
    CallRequest::InvokeAgent(InvokeAgentParams {
        agent: agent.into(),
        prompt: "start".into(),
        handle: None,
        include_trace: false,
        files: None,
        spawn_context: None,
        remaining,
        run: None,
    })
}

/// Wait until `registry` has `n` runs open.
async fn until_open(registry: &ParticipantRegistry, n: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while registry.open_runs() < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{n} runs never opened: {}", registry.open_runs()));
}

/// Wait until no worker is registered and no run is open.
async fn until_gone(registry: &ParticipantRegistry) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !registry.names().is_empty() || registry.open_runs() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("left running: {:?}", registry.names()));
}

/// The call's result, reading everything before it.
async fn result_of(stream: &mut CallStream) -> InvokeAgentOutcome {
    while let Some(event) = stream.next().await {
        if let Ok(CallEvent::Result(CallResult::Invoked(outcome))) = event {
            return outcome;
        }
    }
    panic!("no result");
}

/// Where `entry` is in the log.
fn at(log: &Log, entry: &str) -> usize {
    let log = log.lock().unwrap();
    log.iter()
        .position(|e| e == entry)
        .unwrap_or_else(|| panic!("no `{entry}` in {log:?}"))
}

const DEPTH_3: &[(&str, Behaviour)] = &[
    ("research", Behaviour::StepsThenHang(&[&["coding"]])),
    ("coding", Behaviour::StepsThenHang(&[&["coder"]])),
    ("coder", Behaviour::Hang),
];

/// T2.2: the user stops the root's callee while the coder two levels down
/// is mid-turn. The tree is research → coding → coder; the coder is reaped
/// first, then coding, then research, and only then does the root's call
/// end — cancelled, with nothing left registered or open, well within
/// `deadline_grace`.
#[tokio::test]
async fn cancel_cascades_bottom_up_through_a_depth_3_tree() {
    let (calls, registry, log) = broker(DEPTH_3, None);
    let mut stream = calls.call(Peer::Root, invoke("research", Remaining::default()));
    let mut until_result = tokio::spawn(async move { result_of(&mut stream).await });
    until_open(&registry, 3).await;
    assert_eq!(
        registry.run_tree(),
        [
            ("research-0".to_string(), None),
            ("coding-0".to_string(), Some("research-0".to_string())),
            ("coder-0".to_string(), Some("coding-0".to_string())),
        ],
        "each run records the run it was started from"
    );

    let cancelled = tokio::time::Instant::now();
    calls.cancel("research-0").unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), &mut until_result)
        .await
        .expect("the root's call ends")
        .unwrap();
    let took = cancelled.elapsed();

    assert!(outcome.cancelled_by_user, "{outcome:?}");
    assert!(
        took < deadline_grace(Duration::ZERO),
        "every task ended within the grace, in {took:?}"
    );
    assert!(at(&log, "reaped coder-0") < at(&log, "reaped coding-0"));
    assert!(at(&log, "reaped coding-0") < at(&log, "reaped research-0"));
    assert!(
        registry.names().is_empty(),
        "no worker is left by the time the root hears: {:?}",
        registry.names()
    );
    assert_eq!(registry.open_runs(), 0, "every run closed");
}

/// T2.2 for a deadline: the root's callee runs out of time while its
/// subtree hangs. The subtree goes first, bottom-up, then the callee, and
/// the root's call ends with the deadline's failure and nothing left.
#[tokio::test]
async fn a_deadline_stops_the_subtree_bottom_up() {
    let (calls, registry, log) = broker(DEPTH_3, None);
    let mut stream = calls.call(
        Peer::Root,
        invoke(
            "research",
            Remaining {
                // Enough for every level's own call to have a whole
                // second left: a call with less is refused before it
                // starts (DP-3).
                seconds: Some(3),
                ..Remaining::default()
            },
        ),
    );
    let outcome = tokio::time::timeout(Duration::from_secs(30), result_of(&mut stream))
        .await
        .expect("the deadline ends the call");

    assert!(!outcome.success, "{outcome:?}");
    // The coder and coding each hit their own deadline (theirs is the
    // same instant, narrowed down the chain) or the cascade from above,
    // whichever is first; either way bottom-up.
    assert!(at(&log, "reaped coder-0") < at(&log, "reaped research-0"));
    assert!(at(&log, "reaped coding-0") < at(&log, "reaped research-0"));
    assert!(registry.names().is_empty(), "{:?}", registry.names());
    assert_eq!(registry.open_runs(), 0);
}

/// A caller that hangs up — the root drops its call — cannot wait for the
/// subtree, but it still goes: stops go out deepest first, and nothing is
/// left once the workers have gone.
#[tokio::test]
async fn a_dropped_root_call_still_stops_the_whole_tree() {
    let (calls, registry, log) = broker(DEPTH_3, None);
    let stream = calls.call(Peer::Root, invoke("research", Remaining::default()));
    let polling = tokio::spawn(async move {
        let mut stream = stream;
        while stream.next().await.is_some() {}
    });
    until_open(&registry, 3).await;
    polling.abort();
    let _ = polling.await;

    until_gone(&registry).await;
    for node in ["research-0", "coding-0", "coder-0"] {
        at(&log, &format!("reaped {node}"));
    }
}

/// T2.7, the diamond: research calls two coding teams at once, then a
/// reviewer team with both their results. The work is a diamond, the calls
/// a tree: A, B and the reviewer were all called by research, and the
/// reviewer's task carries both branches.
#[tokio::test]
async fn a_diamond_of_work_is_a_tree_of_calls() {
    let data = tempfile::tempdir().unwrap();
    let edges = EdgeLog::open(data.path()).unwrap();
    let path = edges.path();
    let (calls, registry, log) = broker(
        &[
            (
                "research",
                Behaviour::Steps(&[&["coding", "coding"], &["reviewer"]]),
            ),
            ("coding", Behaviour::Steps(&[&["coder"]])),
            ("coder", Behaviour::Answer),
            ("reviewer", Behaviour::Answer),
        ],
        Some(Arc::new(Mutex::new(edges))),
    );
    let mut stream = calls.call(Peer::Root, invoke("research", Remaining::default()));
    let outcome = tokio::time::timeout(Duration::from_secs(30), result_of(&mut stream))
        .await
        .expect("the run completes");
    assert!(outcome.success, "{outcome:?}");

    let rows: Vec<(String, String)> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<EdgeRow>(line).unwrap())
        .filter(|row| row.kind == EdgeKind::Task)
        .map(|row| (row.from, row.to))
        .collect();
    let mut called_by_research: Vec<_> = rows
        .iter()
        .filter(|(from, _)| from == "research-0")
        .map(|(_, to)| to.as_str())
        .collect();
    called_by_research.sort();
    assert_eq!(
        called_by_research,
        ["coding-0", "coding-1", "reviewer-0"],
        "{rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|(_, to)| to == "reviewer-0").count(),
        1,
        "the reviewer ran once"
    );

    let review = log
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.starts_with("task reviewer "))
        .cloned()
        .unwrap();
    assert_eq!(
        review.matches("done task-").count(),
        2,
        "the reviewer gets both coding teams' results: {review}"
    );
    // A completed call reaps its worker as it lets go of the stream.
    drop(stream);
    until_gone(&registry).await;
}

/// T2.7, cancelled during the parallel step: both coding teams and their
/// coders stop, every coder before every coding team and both before
/// research; the reviewer is never called.
#[tokio::test]
async fn cancelling_the_diamond_mid_step_stops_both_branches() {
    let (calls, registry, log) = broker(
        &[
            (
                "research",
                Behaviour::Steps(&[&["coding", "coding"], &["reviewer"]]),
            ),
            ("coding", Behaviour::StepsThenHang(&[&["coder"]])),
            ("coder", Behaviour::Hang),
            ("reviewer", Behaviour::Answer),
        ],
        None,
    );
    let mut stream = calls.call(Peer::Root, invoke("research", Remaining::default()));
    let mut until_result = tokio::spawn(async move { result_of(&mut stream).await });
    until_open(&registry, 5).await;
    let tree = registry.run_tree();
    let parent = |node: &str| {
        tree.iter()
            .find(|(n, _)| n == node)
            .unwrap_or_else(|| panic!("{node} is not in {tree:?}"))
            .1
            .clone()
    };
    assert_eq!(parent("research-0"), None);
    for coding in ["coding-0", "coding-1"] {
        assert_eq!(parent(coding).as_deref(), Some("research-0"), "{tree:?}");
    }
    let mut coders_under: Vec<_> = ["coder-0", "coder-1"].map(|c| parent(c).unwrap()).into();
    coders_under.sort();
    assert_eq!(
        coders_under,
        ["coding-0", "coding-1"],
        "one coder under each: {tree:?}"
    );

    calls.cancel("research-0").unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), &mut until_result)
        .await
        .expect("the root's call ends")
        .unwrap();
    assert!(outcome.cancelled_by_user, "{outcome:?}");

    for coder in ["coder-0", "coder-1"] {
        for coding in ["coding-0", "coding-1"] {
            assert!(
                at(&log, &format!("reaped {coder}")) < at(&log, &format!("reaped {coding}")),
                "{coder} before {coding}: {:?}",
                log.lock().unwrap()
            );
        }
    }
    for coding in ["coding-0", "coding-1"] {
        assert!(at(&log, &format!("reaped {coding}")) < at(&log, "reaped research-0"));
    }
    assert!(
        !log.lock()
            .unwrap()
            .iter()
            .any(|e| e.starts_with("task reviewer")),
        "the reviewer was never called"
    );
    assert!(registry.names().is_empty(), "{:?}", registry.names());
    assert_eq!(registry.open_runs(), 0);
}

/// Stopping one branch of the tree leaves the rest running: the user stops
/// coding-0 mid-step; its coder goes before it, and research and the other
/// branch carry on.
#[tokio::test]
async fn stopping_one_branch_leaves_its_siblings_running() {
    let (calls, registry, log) = broker(
        &[
            (
                "research",
                Behaviour::StepsThenHang(&[&["coding", "coding"]]),
            ),
            ("coding", Behaviour::StepsThenHang(&[&["coder"]])),
            ("coder", Behaviour::Hang),
        ],
        None,
    );
    let mut stream = calls.call(Peer::Root, invoke("research", Remaining::default()));
    let until_result = tokio::spawn(async move { result_of(&mut stream).await });
    until_open(&registry, 5).await;
    let coder_of_0 = registry
        .run_tree()
        .into_iter()
        .find(|(_, parent)| parent.as_deref() == Some("coding-0"))
        .map(|(node, _)| node)
        .unwrap();

    calls.cancel("coding-0").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while registry.is_registered("coding-0") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("coding-0 is stopped");
    assert!(at(&log, &format!("reaped {coder_of_0}")) < at(&log, "reaped coding-0"));
    assert!(!registry.is_registered(&coder_of_0));
    assert_eq!(registry.open_runs(), 3, "research, coding-1 and its coder");
    assert!(registry.is_registered("coding-1"));
    assert!(registry.is_registered("research-0"));
    until_result.abort();
}
