//! EN-2b (AGE-771) at the wire: a question is a `human.ask` request that
//! climbs the caller chain with its first stamp.
//!
//! These workers write their frames by hand, so a leaf can prefill an asker
//! and an intermediate can escalate a relayed question or sit on it. The
//! root is the test, holding a root call's stream and answering through the
//! broker.

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::*;
use crate::participant::{LocalConnection, WorkerFuture, WorkerHandle, open_connection};

/// Generous for CI: only a failure waits this long.
const DEADLINE: Duration = Duration::from_secs(30);

/// Every frame each worker received, by the name the broker gave it.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// What a scripted worker does with its one task.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Call `leaf` and wait for its result; answer every question relayed
    /// to it `escalate`.
    Escalate,
    /// Call `leaf` and wait for its result; never answer a question relayed
    /// to it.
    Hold,
    /// Ask a question (`human.ask` request 2), with a forged asker, and
    /// wait for its result. Hang up when the broker cancels the task, as a
    /// reaped worker would.
    Ask,
}

fn line(mut value: Value) -> String {
    value["v"] = json!(3);
    format!("{value}\n")
}

/// The leaf's question, with an asker it has no business setting.
fn forged_question() -> Value {
    json!({
        "questions": [{"id": "q1", "question": "Which database?", "options": ["SQLite"]}],
        "asker": {"agent": "root", "chain": ["root"]},
    })
}

/// Answer the one task that arrives on `stream` as `behaviour` says,
/// recording every frame it receives in `seen`.
async fn serve(stream: UnixStream, behaviour: Behaviour, seen: Seen) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut name = String::new();
    let hello = json!({"id": 1, "method": "session.hello", "params": {"card": {}}});
    write.write_all(line(hello).as_bytes()).await.unwrap();
    let mut run = None;
    while let Ok(Some(text)) = lines.next_line().await {
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["id"] == 1 && frame.get("result").is_some() && run.is_none() {
            name = frame["result"]["name"].as_str().unwrap().to_string();
            continue;
        }
        seen.lock().unwrap().push((name.clone(), frame.clone()));
        match frame["method"].as_str() {
            Some("task.run") => {
                run = Some(frame["id"].clone());
                let first = match behaviour {
                    Behaviour::Escalate | Behaviour::Hold => {
                        json!({"id": 2, "method": "agent.invoke",
                                         "params": {"agent": "leaf", "prompt": "go"}})
                    }
                    Behaviour::Ask => {
                        json!({"id": 2, "method": "human.ask", "params": forged_question()})
                    }
                };
                write.write_all(line(first).as_bytes()).await.unwrap();
            }
            Some("human.ask") => {
                if let Behaviour::Escalate = behaviour {
                    let reply = json!({"id": frame["id"], "result": "escalate"});
                    write.write_all(line(reply).as_bytes()).await.unwrap();
                }
            }
            Some("req.cancel") if Some(&frame["params"]["id"]) == run.as_ref() => {
                if let Behaviour::Ask = behaviour {
                    return;
                }
            }
            Some(_) => {}
            // A result or an error: request 2's is the last thing awaited.
            None if frame["id"] == 2 => {
                let done = json!({"id": run.clone().unwrap(), "result": {"state": "completed"}});
                write.write_all(line(done).as_bytes()).await.unwrap();
            }
            None => {}
        }
    }
}

struct Scripted {
    name: String,
    registry: ParticipantRegistry,
    behaviour: Behaviour,
    seen: Seen,
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
            tokio::spawn(serve(stream, self.behaviour, self.seen.clone()));
            let registered = Instant::now();
            while !self.registry.is_registered(&name) {
                assert!(registered.elapsed() < DEADLINE, "never said hello");
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

/// A broker with a `mid` scripted as given over an asking `leaf`, and what
/// every worker saw.
fn broker(mid: Behaviour) -> (Arc<BrokerCalls>, Seen) {
    let registry = ParticipantRegistry::new();
    let seen = Seen::default();
    let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = [("mid", mid), ("leaf", Behaviour::Ask)]
        .into_iter()
        .map(|(name, behaviour)| {
            let runner: Arc<dyn VirtualAgent> = Arc::new(Scripted {
                name: name.to_string(),
                registry: registry.clone(),
                behaviour,
                seen: seen.clone(),
            });
            (name.to_string(), runner)
        })
        .collect();
    let calls = Arc::new(BrokerCalls::new(registry.clone(), Arc::new(runners), None));
    registry.install_calls(&calls);
    (calls, seen)
}

/// The root's call to `mid`.
fn root_call(calls: &BrokerCalls) -> CallStream {
    calls.call(
        Caller::Root,
        CallRequest::InvokeAgent(InvokeAgentParams {
            agent: "mid".into(),
            prompt: "go".into(),
            handle: None,
            include_trace: false,
            spawn_context: None,
            remaining: Default::default(),
        }),
    )
}

/// The next item on the root's call that is not progress or a swarm batch.
async fn next_event(stream: &mut CallStream) -> CallEvent {
    tokio::time::timeout(DEADLINE, async {
        loop {
            match stream.next().await {
                Some(Ok(CallEvent::Progress(_) | CallEvent::Swarm(_))) => {}
                Some(Ok(event)) => return event,
                Some(Err(e)) => panic!("the root's call failed: {e}"),
                None => panic!("the root's call ended"),
            }
        }
    })
    .await
    .expect("the root hears from its call before the deadline")
}

/// The next question the root's call delivers.
async fn next_question(stream: &mut CallStream) -> (String, AskRequest) {
    match next_event(stream).await {
        CallEvent::Ask { id, request } => (id, request),
        other => panic!("a question, not {other:?}"),
    }
}

/// The first frame `node` received that `pick` matches, waiting for it.
async fn received(seen: &Seen, node: &str, pick: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let found = seen
                .lock()
                .unwrap()
                .iter()
                .find(|(name, frame)| name == node && pick(frame))
                .map(|(_, frame)| frame.clone());
            if let Some(frame) = found {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{node} never received the frame"))
}

fn is_relayed_question(frame: &Value) -> bool {
    frame["method"] == "human.ask"
}

/// The stamp the broker gives the leaf's question.
fn leaf_stamp() -> Value {
    json!({"agent": "leaf-0", "chain": ["root", "mid", "leaf"]})
}

/// EN-2b: root → mid → leaf. The leaf's question reaches the root stamped
/// with the leaf — whatever asker it wrote is overwritten — and the root's
/// answers come back as the result of the leaf's own request.
#[tokio::test]
async fn depth2_question_reaches_root_with_depth2_asker() {
    let (calls, seen) = broker(Behaviour::Escalate);
    let mut root = root_call(&calls);
    let (id, request) = next_question(&mut root).await;
    assert_eq!(json!(request.asker), leaf_stamp(), "the depth-2 asker");
    assert_eq!(request.questions[0].question, "Which database?");
    assert_eq!(request.origin, None);

    let answers = vec![Answer {
        id: "q1".into(),
        answer: "SQLite".into(),
        custom: false,
    }];
    calls.answer_question(&id, answers).unwrap();
    let result = received(&seen, "leaf-0", |frame| {
        frame["id"] == 2 && frame.get("result").is_some()
    })
    .await;
    assert_eq!(
        result["result"],
        json!([{"id": "q1", "answer": "SQLite", "custom": false}])
    );
    assert!(
        calls.answer_question(&id, Vec::new()).is_err(),
        "an answered question is over"
    );
    match next_event(&mut root).await {
        CallEvent::Result(result) => assert_eq!(result["success"], true),
        other => panic!("the call's result, not {other:?}"),
    }
}

/// EN-2b: the intermediate the broker relays the question to sees the
/// leaf's stamp; it escalates, and the root gets the original request, the
/// first stamp intact — never the intermediate's name.
#[tokio::test]
async fn escalated_question_keeps_first_stamp() {
    let (calls, seen) = broker(Behaviour::Escalate);
    let mut root = root_call(&calls);
    let (_, at_root) = next_question(&mut root).await;

    let relayed = received(&seen, "mid-0", is_relayed_question).await;
    assert_eq!(relayed["params"]["request"]["asker"], leaf_stamp());
    assert_eq!(
        relayed["params"]["request"],
        json!(at_root),
        "the root gets the request the intermediate escalated, unchanged"
    );
    assert_eq!(
        relayed["params"]["question"],
        json!(waiting_question(&calls)),
        "one broker id for the question at every hop"
    );
    assert!(
        !json!(at_root).to_string().contains("mid-0"),
        "the relayer is not named: {at_root:?}"
    );
}

/// The broker id of the one question waiting.
fn waiting_question(calls: &BrokerCalls) -> String {
    let questions = lock_questions(&calls.questions);
    let mut ids = questions.keys().cloned().collect::<Vec<_>>();
    assert_eq!(ids.len(), 1, "one question waits: {ids:?}");
    ids.remove(0)
}

/// EN-2b: when the asker's callee is cancelled, the copy relayed to an
/// intermediate is withdrawn with a `req.cancel` naming that `human.ask`,
/// and one waiting on the root leaves the root's popover
/// (`InputWithdrawn`, by the broker's id).
#[tokio::test]
async fn callee_cancel_withdraws_relayed_question() {
    // Relayed to mid, which sits on it.
    let (calls, seen) = broker(Behaviour::Hold);
    let root = tokio::spawn({
        let mut root = root_call(&calls);
        async move { while root.next().await.is_some() {} }
    });
    let relayed = received(&seen, "mid-0", is_relayed_question).await;
    calls.cancel("leaf-0").expect("the leaf is running");
    let cancel = received(&seen, "mid-0", |frame| frame["method"] == "req.cancel").await;
    assert_eq!(cancel["params"]["id"], relayed["id"]);
    root.abort();

    // Escalated to the root.
    let (calls, _seen) = broker(Behaviour::Escalate);
    let mut root = root_call(&calls);
    let (id, _) = next_question(&mut root).await;
    calls.cancel("leaf-0").expect("the leaf is running");
    match next_event(&mut root).await {
        CallEvent::InputWithdrawn { id: withdrawn } => assert_eq!(withdrawn, id),
        other => panic!("the question's withdrawal, not {other:?}"),
    }
    assert!(
        calls.answer_question(&id, Vec::new()).is_err(),
        "a withdrawn question cannot be answered"
    );
}
