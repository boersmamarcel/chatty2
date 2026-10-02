//! TB-1 (AGE-663) at the wire: a root call hears the runs nested under it,
//! tagged by the broker and batched, from workers that speak raw frames.
//!
//! The swarm kit (`chatty-tui`) runs real workers, which only ever send
//! what their mapper builds. These workers write their frames by hand, so
//! one can put a tag in its event payload, and one can burst hundreds of
//! frames at once.

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::*;
use crate::participant::{LocalConnection, WorkerFuture, WorkerHandle, open_connection};
use serde_json::{Value, json};

/// Whether a scripted worker's task asked for `swarm` task events, once it came.
type Asked = Arc<Mutex<Option<bool>>>;

/// What a scripted worker does with its one task.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Call `leaf` over its connection, wait for the result, complete.
    Delegate,
    /// Send one tool event carrying a forged tag, which does not decode:
    /// the broker closes the connection.
    Forge,
    /// Send `n` tool starts and `n` answer chunks back to back, then
    /// complete.
    Burst(usize),
}

/// A worker's frame, as it writes it.
fn line(value: Value) -> String {
    let mut value = value;
    value["v"] = json!(3);
    format!("{value}\n")
}

/// Answer the one task that arrives on `stream` as `behaviour` says, and
/// record whether the task asked for `swarm` task events.
async fn serve(stream: UnixStream, behaviour: Behaviour, asked: Asked) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({"id": 1, "method": "session.hello", "params": {"card": {}}});
    write.write_all(line(hello).as_bytes()).await.unwrap();
    while let Ok(Some(text)) = lines.next_line().await {
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["method"] != "task.run" {
            continue;
        }
        *asked.lock().unwrap() = Some(frame["params"]["swarmEvents"] == json!(true));
        // A task's events and its result name its task.run.
        let run = frame["id"].clone();
        match behaviour {
            Behaviour::Delegate => {
                // A call names the run it is made from (GT-0b).
                let call = json!({"id": 2, "method": "agent.invoke",
                                  "params": {"agent": "leaf", "prompt": "go",
                                             "run": frame["params"]["taskId"]}});
                write.write_all(line(call).as_bytes()).await.unwrap();
                while let Ok(Some(text)) = lines.next_line().await {
                    let reply: Value = serde_json::from_str(&text).unwrap();
                    if reply["id"] == 2 && reply.get("method").is_none() {
                        break;
                    }
                }
            }
            Behaviour::Forge => {
                let forged = json!({"method": "task.event", "params": {"kind": "swarm", "id": run,
                    "root_task_id": "forged", "node": "root",
                    "event": {"kind": "tool_call_started", "id": "c1", "name": "shell",
                              "root_task_id": "forged", "node": "root",
                              "chain": {"root_task_id": "forged", "chain": ["root"], "depth": 0}}}});
                write.write_all(line(forged).as_bytes()).await.unwrap();
                // The broker hangs up; there is no result to send.
                while let Ok(Some(_)) = lines.next_line().await {}
                return;
            }
            Behaviour::Burst(n) => {
                let mut out = String::new();
                for i in 0..n {
                    out.push_str(&line(json!({"method": "task.event", "params": {"kind": "swarm",
                        "id": run, "event": {"kind": "tool_call_started", "id": format!("c{i}"), "name": "shell"}}})));
                    out.push_str(&line(
                        json!({"method": "task.event", "params": {"kind": "artifact",
                        "id": run, "text": "chunk", "lastChunk": false}}),
                    ));
                }
                write.write_all(out.as_bytes()).await.unwrap();
            }
        }
        let done = json!({"id": run, "result": {"state": "completed"}});
        write.write_all(line(done).as_bytes()).await.unwrap();
        // Hold the connection until the broker lets go of the task.
        while let Ok(Some(_)) = lines.next_line().await {}
        return;
    }
}

struct Scripted {
    name: String,
    registry: ParticipantRegistry,
    behaviour: Behaviour,
    asked: Asked,
}

struct Handle {
    name: String,
    task_id: String,
}

impl WorkerHandle for Handle {
    fn name(&self) -> &str {
        &self.name
    }

    fn task_id(&self) -> Option<&str> {
        Some(&self.task_id)
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&chatty_fabric::wire::TaskMetadata>) {}
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

    /// What the local runner does, with the child a task on this runtime.
    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        Box::pin(async move {
            let spawner = task.call.as_ref().and_then(|call| call.caller.as_deref());
            let LocalConnection { name, worker_end } =
                open_connection(&self.registry, &self.name, spawner)?;
            worker_end.set_nonblocking(true)?;
            let stream = UnixStream::from_std(worker_end)?;
            tokio::spawn(serve(stream, self.behaviour, self.asked.clone()));
            let registered = Instant::now();
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
            let handle: Box<dyn WorkerHandle> = Box::new(Handle { name, task_id });
            Ok((handle, updates))
        })
    }
}

/// A broker whose `mid` delegates to a `leaf` that does `leaf`, and whether
/// each was asked for `swarm` task events.
fn broker(leaf: Behaviour) -> (Arc<BrokerCalls>, [Asked; 2]) {
    let registry = ParticipantRegistry::new();
    let asked: [Asked; 2] = [Arc::default(), Arc::default()];
    let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = [
        ("mid", Behaviour::Delegate, asked[0].clone()),
        ("leaf", leaf, asked[1].clone()),
    ]
    .into_iter()
    .map(|(name, behaviour, asked)| {
        let runner: Arc<dyn VirtualAgent> = Arc::new(Scripted {
            name: name.to_string(),
            registry: registry.clone(),
            behaviour,
            asked,
        });
        (name.to_string(), runner)
    })
    .collect();
    let calls = Arc::new(BrokerCalls::new(registry.clone(), Arc::new(runners), None));
    registry.install_calls(&calls);
    (calls, asked)
}

/// The root delegates to `mid`: every swarm batch its call received, and
/// how long the call took.
async fn root_call(calls: &BrokerCalls) -> (Vec<chatty_fabric::SwarmEvent>, Duration) {
    let started = Instant::now();
    let mut stream = calls.call(
        Peer::Root,
        CallRequest::InvokeAgent(InvokeAgentParams {
            agent: "mid".into(),
            prompt: "delegate".into(),
            handle: None,
            include_trace: false,
            spawn_context: None,
            remaining: Default::default(),
            run: None,
        }),
    );
    let mut batches = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = stream.next().await {
            match event {
                Ok(CallEvent::Swarm(batch)) => batches.push(batch),
                Ok(CallEvent::Result(CallResult::Invoked(result))) => return result,
                Ok(_) => {}
                Err(e) => panic!("the call failed: {e}"),
            }
        }
        panic!("no result");
    })
    .await
    .expect("the root call ends");
    assert!(result.success, "{result:?}");
    (batches, started.elapsed())
}

/// Every item the root heard, in order.
fn items(batches: &[chatty_fabric::SwarmEvent]) -> Vec<SwarmItem> {
    batches.iter().flat_map(|b| b.inner.clone()).collect()
}

/// A worker that sends a tag in its event payload is cut off (EN-3a): the
/// frame does not decode, the broker closes its connection, and the root
/// hears only that the run ended, under the node and chain the broker
/// stamped. Nothing forged reaches it. (That an item which is not the
/// worker's to report, such as usage, does not decode either is the codec's
/// `a_swarm_event_with_a_forged_tag_does_not_decode`.)
#[tokio::test]
async fn worker_cannot_forge_tags() {
    let (calls, [mid_asked, leaf_asked]) = broker(Behaviour::Forge);
    let (batches, _) = root_call(&calls).await;

    assert_eq!(
        *mid_asked.lock().unwrap(),
        Some(false),
        "the root's own callee reports to the root directly"
    );
    assert_eq!(
        *leaf_asked.lock().unwrap(),
        Some(true),
        "a nested run is asked for its events"
    );
    assert!(!batches.is_empty(), "the nested run reached the root");
    for batch in &batches {
        assert_eq!(batch.node, "leaf-0");
        assert_ne!(batch.root_task_id, "forged");
        assert_eq!(batch.root_task_id, batch.chain.root_task_id);
        assert_eq!(batch.chain.chain, ["root", "mid", "leaf"]);
        assert_eq!(batch.chain.depth, 2);
    }
    assert_eq!(
        items(&batches),
        [SwarmItem::Ended {
            state: "failed".into()
        }],
        "the forged event was refused and its run ended with the connection"
    );
    let json = serde_json::to_string(&batches).unwrap();
    assert!(!json.contains("forged"), "{json}");
}

/// A burst of frames from one node reaches the root as at most one batch
/// per interval, with nothing lost and no text: 400 frames, counted
/// against however long the call took.
#[tokio::test]
async fn a_burst_is_one_batch_per_interval() {
    const N: usize = 200;
    let (calls, _) = broker(Behaviour::Burst(N));
    let (batches, took) = root_call(&calls).await;

    let intervals = took.as_secs_f64() / FORWARD_INTERVAL.as_secs_f64();
    assert!(
        batches.len() as f64 <= intervals.floor(),
        "{} batches in {intervals:.1} intervals",
        batches.len()
    );
    let items = items(&batches);
    let started = items
        .iter()
        .filter(|i| matches!(i, SwarmItem::ToolCallStarted { .. }))
        .count();
    assert_eq!(started, N, "every tool event arrived");
    let bytes: u64 = items
        .iter()
        .map(|i| match i {
            SwarmItem::Text { bytes } => *bytes,
            _ => 0,
        })
        .sum();
    assert_eq!(bytes, (N * "chunk".len()) as u64, "the text, as its length");
    assert!(!serde_json::to_string(&batches).unwrap().contains("chunk"));
}
