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

/// Whether a scripted worker's task asked for `event` frames, once it came.
type Asked = Arc<Mutex<Option<bool>>>;

/// What a scripted worker does with its one task.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Call `leaf` over its connection, wait for the result, complete.
    Delegate,
    /// Send one tool event carrying a forged tag, then complete.
    Forge,
    /// Send `n` tool starts and `n` answer chunks back to back, then
    /// complete.
    Burst(usize),
}

/// A worker's frame, as it writes it.
fn line(value: Value) -> String {
    let mut value = value;
    value["v"] = json!(2);
    format!("{value}\n")
}

/// Answer the one task that arrives on `stream` as `behaviour` says, and
/// record whether the task asked for `event` frames.
async fn serve(stream: UnixStream, behaviour: Behaviour, asked: Asked) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({"type": "hello", "card": {}});
    write.write_all(line(hello).as_bytes()).await.unwrap();
    while let Ok(Some(text)) = lines.next_line().await {
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["type"] != "task" {
            continue;
        }
        *asked.lock().unwrap() = Some(frame["swarmEvents"] == json!(true));
        let task = frame["taskId"].clone();
        match behaviour {
            Behaviour::Delegate => {
                let call = json!({"type": "call", "id": 1, "method": "invoke_agent",
                                  "params": {"agent": "leaf", "prompt": "go"}});
                write.write_all(line(call).as_bytes()).await.unwrap();
                while let Ok(Some(text)) = lines.next_line().await {
                    let reply: Value = serde_json::from_str(&text).unwrap();
                    if reply["type"] == "call_result" || reply["type"] == "call_error" {
                        break;
                    }
                }
            }
            Behaviour::Forge => {
                let forged = json!({"type": "event", "taskId": task,
                    "root_task_id": "forged", "node": "root",
                    "event": {"kind": "tool_call_started", "id": "c1", "name": "shell",
                              "root_task_id": "forged", "node": "root",
                              "chain": {"root_task_id": "forged", "chain": ["root"], "depth": 0}}});
                // Not the worker's to report: the broker reads usage off
                // the terminal status.
                let usage = json!({"type": "event", "taskId": task,
                    "event": {"kind": "usage", "usage": {"inputTokens": 1_000_000}}});
                for frame in [forged, usage] {
                    write.write_all(line(frame).as_bytes()).await.unwrap();
                }
            }
            Behaviour::Burst(n) => {
                let mut out = String::new();
                for i in 0..n {
                    out.push_str(&line(json!({"type": "event", "taskId": task,
                        "event": {"kind": "tool_call_started", "id": format!("c{i}"), "name": "shell"}})));
                    out.push_str(&line(json!({"type": "artifact", "taskId": task,
                        "text": "chunk", "lastChunk": false})));
                }
                write.write_all(out.as_bytes()).await.unwrap();
            }
        }
        let done = json!({"type": "status", "taskId": task, "state": "completed"});
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
    _run: Option<super::super::registry::RunGuard>,
}

impl WorkerHandle for Handle {
    fn name(&self) -> &str {
        &self.name
    }

    fn task_id(&self) -> Option<&str> {
        Some(&self.task_id)
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&Value>) {}
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
            let run = task.call.as_ref().and_then(|call| {
                self.registry
                    .open_run(&name, call.caller.as_deref(), call.chain.clone())
            });
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
            let handle: Box<dyn WorkerHandle> = Box::new(Handle {
                name,
                task_id,
                _run: run,
            });
            Ok((handle, updates))
        })
    }
}

/// A broker whose `mid` delegates to a `leaf` that does `leaf`, and whether
/// each was asked for `event` frames.
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
        Caller::Root,
        CallRequest::InvokeAgent(InvokeAgentParams {
            agent: "mid".into(),
            prompt: "delegate".into(),
            handle: None,
            include_trace: false,
            spawn_context: None,
            remaining: Default::default(),
        }),
    );
    let mut batches = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = stream.next().await {
            match event {
                Ok(CallEvent::Swarm(batch)) => batches.push(batch),
                Ok(CallEvent::Result(result)) => return result,
                Ok(_) => {}
                Err(e) => panic!("the call failed: {e}"),
            }
        }
        panic!("no result");
    })
    .await
    .expect("the root call ends");
    assert_eq!(result["success"], true, "{result}");
    (batches, started.elapsed())
}

/// Every item the root heard, in order.
fn items(batches: &[chatty_fabric::SwarmEvent]) -> Vec<SwarmItem> {
    batches.iter().flat_map(|b| b.inner.clone()).collect()
}

/// A worker that sends a tag in its event payload gets it overwritten: the
/// root hears the event under the node and chain the broker stamped, and
/// an item that is not the worker's to report is dropped.
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
        [
            SwarmItem::ToolCallStarted {
                id: "c1".into(),
                name: "shell".into()
            },
            SwarmItem::Ended {
                state: "completed".into()
            },
        ],
        "the forged usage item was dropped"
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
