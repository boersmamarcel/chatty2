//! EN-0b (AGE-766) at the wire: what one worker connection may cost the
//! broker, and that only a peer's own frame closes its connection.
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
        };
        raw.send(json!({"type": "hello", "card": {}})).await;
        assert_eq!(raw.next().await.expect("a welcome")["type"], "welcome");
        wait_for(|| registry.is_registered(&raw.name), "registered").await;
        raw
    }

    async fn send(&mut self, mut frame: Value) {
        frame["v"] = json!(2);
        let line = format!("{frame}\n");
        self.write.write_all(line.as_bytes()).await.unwrap();
    }

    /// Call `id`'s `call_result` or `call_error`, past its progress.
    async fn answer(&mut self, id: u64) -> Value {
        loop {
            let frame = self.next().await.expect("an answer before the close");
            assert_eq!(frame["id"], id, "{}", frame["type"]);
            if frame["type"] != "call_progress" {
                return frame;
            }
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

fn invoke(id: u64, agent: &str, prompt: &str) -> Value {
    json!({"type": "call", "id": id, "method": "invoke_agent",
           "params": {"agent": agent, "prompt": prompt}})
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

/// A line over the cap closes the connection it came on, with an `error`
/// frame first, and nobody else's.
#[tokio::test]
async fn oversized_line_closes_only_sender() {
    let registry = ParticipantRegistry::new();
    let mut sender = Raw::connect(&registry, "sender").await;
    let mut other = Raw::connect(&registry, "other").await;

    let mut line = vec![b'x'; MAX_FRAME_BYTES + 1];
    line.push(b'\n');
    // The broker stops reading at the cap, so the rest may not be taken.
    let _ = sender.write.write_all(&line).await;

    let error = sender
        .next()
        .await
        .expect("an error frame before the close");
    assert_eq!(error["type"], "error");
    assert!(
        error["reason"].as_str().unwrap().contains("frame cap"),
        "{error}"
    );
    assert_eq!(sender.next().await, None, "then the connection closes");
    wait_for(|| !registry.is_registered(&sender.name), "deregistered").await;

    assert!(registry.is_registered(&other.name), "the other one stays");
    let (task_id, _updates) = registry
        .submit_task(&other.name, DelegatedTask::new("still here"))
        .await
        .expect("the other connection takes work");
    let task = other.next().await.expect("its task");
    assert_eq!(task["type"], "task");
    assert_eq!(task["taskId"], task_id);

    // A line that is not a frame at all closes its sender the same way.
    other.send(json!({"type": "no-such-frame"})).await;
    assert_eq!(other.next().await.expect("an error frame")["type"], "error");
    assert_eq!(other.next().await, None);
}

/// A callee's result too large for the caller's cap fails that one call
/// with a `call_error`; the caller's connection and its next call are fine.
#[tokio::test]
async fn relayed_result_over_receiver_cap_fails_only_that_request() {
    let (registry, _calls) = broker();
    let mut caller = Raw::connect(&registry, "caller").await;

    // The callee's answer is exactly the cap; wrapped in the caller's
    // `call_result` frame, it is over it.
    caller
        .send(invoke(1, "answering", &MAX_FRAME_BYTES.to_string()))
        .await;
    let failed = caller.answer(1).await;
    assert_eq!(failed["type"], "call_error", "{}", failed["type"]);
    assert_eq!(failed["id"], 1);
    assert!(
        failed.to_string().contains("frame cap"),
        "names the cap: {}",
        failed["error"]
    );

    caller.send(invoke(2, "answering", "small")).await;
    let ok = caller.answer(2).await;
    assert_eq!(ok["type"], "call_result", "{ok}");
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
                    .submit_task(&name, DelegatedTask::new(text.clone()))
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
            slow.next().await.expect("every task arrives")["type"],
            "task"
        );
    }
    let streams = producer.await.unwrap();
    assert_eq!(streams.len(), total);
    assert!(registry.is_registered(&slow.name));
}

/// Past [`MAX_IN_FLIGHT_CALLS`] running calls, the next one is refused with
/// a `call_error`; the calls in flight and the connection go on.
#[tokio::test]
async fn in_flight_cap_refuses_extra_call() {
    let (registry, _calls) = broker();
    let mut caller = Raw::connect(&registry, "caller").await;

    let extra = MAX_IN_FLIGHT_CALLS as u64 + 1;
    for id in 1..=extra {
        caller.send(invoke(id, "answering", "hang")).await;
    }
    let refused = caller.next().await.expect("a refusal");
    assert_eq!(refused["type"], "call_error", "{refused}");
    assert_eq!(refused["id"], extra, "only the extra call: {refused}");
    assert!(
        refused["error"].to_string().contains("in flight"),
        "{refused}"
    );
    assert!(registry.is_registered(&caller.name));
}
