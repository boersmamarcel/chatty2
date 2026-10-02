//! EN-0b (AGE-766) at the wire: what one worker connection may cost the
//! broker, and that only a peer's own frame closes its connection. EN-1
//! (AGE-769): which v3 lines close a connection, and which are dropped.
//!
//! The workers here write raw frames over the broker-made socket pair, so a
//! test can send a line no real worker would, or stop reading.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::*;
use crate::participant::limits::{MAX_FRAME_BYTES, MAX_IN_FLIGHT_CALLS, OUTBOUND_QUEUE_FRAMES};
use crate::participant::{
    BrokerCalls, DelegatedTask, ParticipantCard, TaskState, TaskUpdate, VirtualAgent, WorkerFuture,
    WorkerHandle,
};

/// A worker end that speaks raw lines.
struct Raw {
    name: String,
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
    /// The task this worker serves, whose run its calls name (GT-0b).
    task: Option<String>,
}

impl Raw {
    /// Open a connection for `spec`, say hello and read the welcome.
    async fn connect(registry: &ParticipantRegistry, spec: &str) -> Self {
        let LocalConnection { name, worker_end } =
            open_connection(registry, spec, None).expect("admitted");
        worker_end.set_nonblocking(true).unwrap();
        let (read, write) = UnixStream::from_std(worker_end).unwrap().into_split();
        let mut raw = Self {
            name,
            lines: BufReader::new(read).lines(),
            write,
            task: None,
        };
        raw.send(json!({"id": 0, "method": "session.hello", "params": {"card": {}}}))
            .await;
        let welcome = raw.next().await.expect("a welcome");
        assert_eq!(welcome["id"], 0, "the hello's result: {welcome}");
        assert_eq!(welcome["result"]["name"], raw.name.as_str());
        wait_for(|| registry.is_registered(&raw.name), "registered").await;
        raw
    }

    /// [`connect`](Self::connect), then take a task from the root: a node
    /// with a run open, whose calls are made from it (ADR-0023 § 1).
    async fn serving(registry: &ParticipantRegistry, spec: &str) -> Self {
        let mut raw = Self::connect(registry, spec).await;
        let (task, _updates) = registry
            .submit_task(&raw.name, DelegatedTask::from_root("serve"))
            .await
            .expect("the connection takes work");
        let run = raw.next().await.expect("its task.run");
        assert_eq!(run["params"]["taskId"], task.as_str());
        raw.task = Some(task);
        raw
    }

    /// An `agent.invoke` of `agent` with `prompt`, as request `id`, from
    /// the run of the task this worker serves.
    fn invoke(&self, id: u64, agent: &str, prompt: &str) -> Value {
        json!({"id": id, "method": "agent.invoke",
               "params": {"agent": agent, "prompt": prompt, "run": self.task}})
    }

    async fn send(&mut self, mut frame: Value) {
        frame["v"] = json!(3);
        let line = format!("{frame}\n");
        self.write.write_all(line.as_bytes()).await.unwrap();
    }

    /// Request `id`'s result or error, past progress on any request.
    async fn answer(&mut self, id: u64) -> Value {
        loop {
            let frame = self.next().await.expect("an answer before the close");
            if frame["method"] == "req.progress" {
                continue;
            }
            assert_eq!(frame["id"], id, "{frame}");
            return frame;
        }
    }

    /// The next frame, `None` once the broker closed the connection.
    async fn next(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(Duration::from_secs(30), self.lines.next_line())
            .await
            .expect("a frame within 30 s")
            .ok()??;
        Some(serde_json::from_str(&line).expect("the broker writes JSON"))
    }
}

async fn wait_for(done: impl Fn() -> bool, what: &str) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "never {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A virtual agent whose worker answers in-process: with `prompt` bytes of
/// `x` when the prompt is a number, with `ok` otherwise, and never when the
/// prompt is `hang`.
struct Answering {
    registry: ParticipantRegistry,
}

struct Worker(String);

impl WorkerHandle for Worker {
    fn name(&self) -> &str {
        "answering-0"
    }

    fn task_id(&self) -> Option<&str> {
        Some(&self.0)
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&Value>) {}
}

impl VirtualAgent for Answering {
    fn agent_name(&self) -> &str {
        "answering"
    }

    fn agent_card(&self) -> ParticipantCard {
        ParticipantCard {
            name: "answering".to_string(),
            ..Default::default()
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let prompt = task.text.trim().to_string();
        if prompt == "hang" {
            // The sender outlives the call: it never ends on its own.
            std::mem::forget(tx);
        } else {
            let text = match prompt.parse::<usize>() {
                Ok(bytes) => "x".repeat(bytes),
                Err(_) => "ok".to_string(),
            };
            let _ = tx.send(TaskUpdate::Artifact {
                text,
                last_chunk: true,
            });
            let _ = tx.send(TaskUpdate::Status {
                state: TaskState::Completed,
                message: None,
                metadata: None,
                input: None,
            });
        }
        Box::pin(async move {
            Ok((
                Box::new(Worker("task-answering".to_string())) as Box<dyn WorkerHandle>,
                rx,
            ))
        })
    }
}

/// A registry with a call path whose one virtual agent is [`Answering`].
fn broker() -> (ParticipantRegistry, Arc<BrokerCalls>) {
    let registry = ParticipantRegistry::new();
    let answering: Arc<dyn VirtualAgent> = Arc::new(Answering {
        registry: registry.clone(),
    });
    let calls = Arc::new(BrokerCalls::new(
        registry.clone(),
        Arc::new(BTreeMap::from([("answering".to_string(), answering)])),
        None,
    ));
    registry.install_calls(&calls);
    (registry, calls)
}

/// A line over the cap closes the connection it came on, without a reply
/// (ADR-0021: an `error` answers only a hello), and nobody else's.
#[tokio::test]
async fn oversized_line_closes_only_sender() {
    let registry = ParticipantRegistry::new();
    let mut sender = Raw::connect(&registry, "sender").await;
    let mut other = Raw::connect(&registry, "other").await;

    let mut line = vec![b'x'; MAX_FRAME_BYTES + 1];
    line.push(b'\n');
    // The broker stops reading at the cap, so the rest may not be taken.
    let _ = sender.write.write_all(&line).await;

    assert_eq!(sender.next().await, None, "the connection closes");
    wait_for(|| !registry.is_registered(&sender.name), "deregistered").await;

    assert!(registry.is_registered(&other.name), "the other one stays");
    let (task_id, _updates) = registry
        .submit_task(&other.name, DelegatedTask::from_root("still here"))
        .await
        .expect("the other connection takes work");
    let task = other.next().await.expect("its task");
    assert_eq!(task["method"], "task.run");
    assert_eq!(task["params"]["taskId"], task_id);

    // A line that is not a message at all closes its sender the same way.
    other.send(json!({"method": "no-such-method"})).await;
    assert_eq!(other.next().await, None);
}

/// A callee's result too large for the caller's cap fails that one call
/// with an `error`; the caller's connection and its next call are fine.
#[tokio::test]
async fn relayed_result_over_receiver_cap_fails_only_that_request() {
    let (registry, _calls) = broker();
    let mut caller = Raw::serving(&registry, "caller").await;

    // The callee's answer is exactly the cap; wrapped in the caller's
    // result envelope, it is over it.
    caller
        .send(caller.invoke(1, "answering", &MAX_FRAME_BYTES.to_string()))
        .await;
    let failed = caller.answer(1).await;
    assert!(failed.get("error").is_some(), "an error: {}", failed["id"]);
    assert_eq!(failed["id"], 1);
    assert!(
        failed.to_string().contains("frame cap"),
        "names the cap: {}",
        failed["error"]
    );

    caller.send(caller.invoke(2, "answering", "small")).await;
    let ok = caller.answer(2).await;
    assert!(ok.get("result").is_some(), "{ok}");
    assert_eq!(ok["id"], 2);
    assert!(registry.is_registered(&caller.name));
}

/// A worker that stops reading makes whoever queues for it wait, and keeps
/// its connection; once it reads again, every frame arrives.
#[tokio::test]
async fn full_outbound_queue_backpressures_and_keeps_connection() {
    let registry = ParticipantRegistry::new();
    let mut slow = Raw::connect(&registry, "slow").await;

    // Enough 64 KiB tasks to fill the socket's buffers and then the queue
    // several times over.
    let total = OUTBOUND_QUEUE_FRAMES * 3;
    let queued = Arc::new(AtomicUsize::new(0));
    let producer = tokio::spawn({
        let registry = registry.clone();
        let name = slow.name.clone();
        let queued = queued.clone();
        async move {
            let text = "t".repeat(64 * 1024);
            let mut streams = Vec::new();
            for _ in 0..total {
                let task = registry
                    .submit_task(&name, DelegatedTask::from_root(text.clone()))
                    .await
                    .expect("a full queue waits, it does not fail");
                streams.push(task);
                queued.fetch_add(1, Ordering::SeqCst);
            }
            streams
        }
    });

    // The producer stalls: the count stops moving short of the total.
    let mut last = usize::MAX;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = queued.load(Ordering::SeqCst);
        if now == last {
            break;
        }
        last = now;
    }
    assert!(
        last < total,
        "the producer waits ({last} of {total} queued)"
    );
    assert!(!producer.is_finished());
    assert!(registry.is_registered(&slow.name), "the connection is kept");

    for _ in 0..total {
        assert_eq!(
            slow.next().await.expect("every task arrives")["method"],
            "task.run"
        );
    }
    let streams = producer.await.unwrap();
    assert_eq!(streams.len(), total);
    assert!(registry.is_registered(&slow.name));
}

/// Past [`MAX_IN_FLIGHT_CALLS`] running calls, the next one is refused with
/// an `error`; the calls in flight and the connection go on.
#[tokio::test]
async fn in_flight_cap_refuses_extra_call() {
    let (registry, _calls) = broker();
    let mut caller = Raw::serving(&registry, "caller").await;

    let extra = MAX_IN_FLIGHT_CALLS as u64 + 1;
    for id in 1..=extra {
        caller.send(caller.invoke(id, "answering", "hang")).await;
    }
    let refused = caller.next().await.expect("a refusal");
    assert!(refused.get("error").is_some(), "{refused}");
    assert_eq!(refused["id"], extra, "only the extra call: {refused}");
    assert!(
        refused["error"].to_string().contains("in flight"),
        "{refused}"
    );
    assert!(registry.is_registered(&caller.name));
}

/// Open a connection for `spec` and send `first` as its first line: what
/// comes back, and whether the connection then closed.
async fn first_line(registry: &ParticipantRegistry, spec: &str, first: Value) -> Raw {
    let LocalConnection { name, worker_end } =
        open_connection(registry, spec, None).expect("admitted");
    worker_end.set_nonblocking(true).unwrap();
    let (read, write) = UnixStream::from_std(worker_end).unwrap().into_split();
    let mut raw = Raw {
        name,
        lines: BufReader::new(read).lines(),
        write,
        task: None,
    };
    raw.send(first).await;
    raw
}

/// EN-1: a second `session.hello` closes the connection, as a second hello
/// always has.
#[tokio::test]
async fn second_hello_closes() {
    let registry = ParticipantRegistry::new();
    let mut worker = Raw::connect(&registry, "twice").await;
    worker
        .send(json!({"id": 1, "method": "session.hello", "params": {"card": {}}}))
        .await;
    assert_eq!(worker.next().await, None, "closed without a reply");
    wait_for(|| !registry.is_registered(&worker.name), "deregistered").await;
}

/// EN-1: a hello the broker refuses gets an `error` for its own id, then the
/// connection closes. The shared socket refuses every hello (ADR-0020).
#[tokio::test]
async fn refused_hello_gets_error_for_its_id() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    // The socket's directory is owner-only (AGE-768).
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("participants.sock");
    let listener = bind(&path).unwrap();
    let server = tokio::spawn(serve(listener));

    let stream = UnixStream::connect(&path).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"{\"v\":3,\"id\":42,\"method\":\"session.hello\",\"params\":{\"card\":{}}}\n")
        .await
        .unwrap();
    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(reply["v"], 3);
    assert_eq!(reply["id"], 42, "the error names the hello: {reply}");
    assert_eq!(reply["error"]["kind"], "refused", "{reply}");
    assert!(lines.next_line().await.unwrap().is_none(), "then closed");

    // A first line that is not a hello is closed without a reply.
    let registry = ParticipantRegistry::new();
    let mut worker = first_line(
        &registry,
        "not-hello",
        json!({"id": 1, "method": "agent.list"}),
    )
    .await;
    assert_eq!(worker.next().await, None);
    assert!(!registry.is_registered(&worker.name));
    server.abort();
}

/// EN-1: a method only the broker may send does not decode from a worker,
/// and the connection closes without a reply; nobody else's does.
#[tokio::test]
async fn wrong_direction_method_closes_connection() {
    let registry = ParticipantRegistry::new();
    let other = Raw::connect(&registry, "other").await;
    for line in [
        json!({"id": 1, "method": "task.run", "params": {"taskId": "t", "text": "x"}}),
        json!({"method": "req.progress", "params": {"id": 1, "event": {}}}),
        json!({"method": "task.input", "params": {"id": 1,
            "input": {"requestId": "r", "answers": []}}}),
    ] {
        let mut worker = Raw::connect(&registry, "wrong").await;
        worker.send(line.clone()).await;
        assert_eq!(worker.next().await, None, "closed: {line}");
        wait_for(|| !registry.is_registered(&worker.name), "deregistered").await;
    }
    assert!(registry.is_registered(&other.name), "the other one stays");
}

/// EN-1: a worker that reuses the id of a request still in flight is
/// closed, and its calls with it.
#[tokio::test]
async fn reused_inflight_id_closes_connection() {
    let (registry, _calls) = broker();
    let mut caller = Raw::serving(&registry, "caller").await;
    caller.send(caller.invoke(7, "answering", "hang")).await;
    // Once a call has answered, its id is no longer in flight.
    caller.send(caller.invoke(8, "answering", "small")).await;
    assert!(caller.answer(8).await.get("result").is_some());
    caller.send(caller.invoke(7, "answering", "small")).await;
    assert_eq!(caller.next().await, None, "closed without a reply");
    wait_for(|| !registry.is_registered(&caller.name), "deregistered").await;
}

/// EN-1: a result, error, event or cancel naming nothing in flight —
/// unknown, duplicate or late — is dropped, and the connection goes on.
#[tokio::test]
async fn late_or_unknown_response_is_dropped_not_fatal() {
    let (registry, _calls) = broker();
    let mut worker = Raw::serving(&registry, "worker").await;
    for line in [
        json!({"id": 99, "result": {"state": "completed"}}),
        json!({"id": 99, "error": {"kind": "failed", "message": "x"}}),
        json!({"method": "task.event", "params": {"kind": "status", "id": 99, "state": "working"}}),
        json!({"method": "req.cancel", "params": {"id": 99}}),
    ] {
        worker.send(line).await;
    }

    // A task's result, then the same result again: the second is late.
    let (_task_id, mut updates) = registry
        .submit_task(&worker.name, DelegatedTask::from_root("do it"))
        .await
        .expect("the connection takes work");
    let run = worker.next().await.expect("its task.run");
    assert_eq!(run["method"], "task.run");
    let done = json!({"id": run["id"], "result": {"state": "completed"}});
    worker.send(done.clone()).await;
    worker.send(done).await;
    let mut states = Vec::new();
    while let Some(update) = updates.recv().await {
        if let TaskUpdate::Status { state, .. } = update {
            states.push(state);
        }
    }
    assert_eq!(states, [TaskState::Completed], "one terminal status");

    // Still connected: the next call is answered.
    worker.send(worker.invoke(3, "answering", "small")).await;
    assert!(worker.answer(3).await.get("result").is_some());
    assert!(registry.is_registered(&worker.name));
}
