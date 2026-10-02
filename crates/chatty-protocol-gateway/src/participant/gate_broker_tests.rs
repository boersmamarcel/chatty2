//! The broker gate in the broker (ADR-0023 §§ 1–4, 7; GT-0, AGE-798;
//! GT-0b, AGE-810): who a request resolves to, the runs tasks open and
//! close, and that a refusal has no effect — over the registry, and at the
//! wire for the timing rules.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::*;
use crate::participant::{
    BrokerFrame, LocalConnection, ParticipantCard, ParticipantFrame, WorkerFuture, WorkerHandle,
    open_connection,
};
use chatty_fabric::SpawnContext;
use serde_json::{Value, json};

/// The stamp each spawn was asked for, in order.
type Stamps = Arc<Mutex<Vec<Option<CallStamp>>>>;

/// A virtual agent that records the stamp of every task it is asked to
/// spawn a worker for, and starts none.
struct Recording {
    name: String,
    registry: ParticipantRegistry,
    stamps: Stamps,
}

impl VirtualAgent for Recording {
    fn agent_name(&self) -> &str {
        &self.name
    }

    fn agent_card(&self) -> ParticipantCard {
        ParticipantCard {
            name: self.name.clone(),
            ..Default::default()
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        self.stamps.lock().unwrap().push(task.call.clone());
        Box::pin(async { Err(anyhow::anyhow!("nothing is spawned here")) })
    }
}

struct Kit {
    calls: Arc<BrokerCalls>,
    registry: ParticipantRegistry,
    stamps: Stamps,
    log: std::path::PathBuf,
    _data: tempfile::TempDir,
}

impl Kit {
    /// A broker with a [`Recording`] virtual agent per name in `runners`,
    /// and an edge log.
    fn new(runners: &[&str]) -> Self {
        let data = tempfile::tempdir().unwrap();
        let log = EdgeLog::open(data.path()).unwrap();
        let path = log.path();
        let registry = ParticipantRegistry::new();
        let stamps = Stamps::default();
        let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = runners
            .iter()
            .map(|name| {
                let runner: Arc<dyn VirtualAgent> = Arc::new(Recording {
                    name: name.to_string(),
                    registry: registry.clone(),
                    stamps: stamps.clone(),
                });
                (name.to_string(), runner)
            })
            .collect();
        let calls = Arc::new(BrokerCalls::new(
            registry.clone(),
            Arc::new(runners),
            Some(Arc::new(Mutex::new(log))),
        ));
        registry.install_calls(&calls);
        Self {
            calls,
            registry,
            stamps,
            log: path,
            _data: data,
        }
    }

    /// Admit a node as `spec` and register a connection for it whose
    /// frames land on the receiver.
    fn connect(&self, spec: &str) -> (String, mpsc::Receiver<BrokerFrame>) {
        let admitted = self.registry.admit(spec, AgentOrigin::Local, None).unwrap();
        let (tx, rx) = mpsc::channel(16);
        let name = self
            .registry
            .register(admitted, ParticipantCard::default(), tx);
        (name, rx)
    }

    /// Hand `name` a task the root's call to it was granted under `chain`
    /// (spec names after the root's): its task id.
    async fn hand(&self, name: &str, root_task_id: &str, chain: &[&str]) -> String {
        let mut stamped = CallChain::root(root_task_id);
        for spec in chain {
            stamped = stamped.extend(spec).unwrap();
        }
        let task = DelegatedTask::new("go").with_call(Some(CallStamp {
            caller: None,
            from_run: None,
            chain: stamped,
        }));
        let (task_id, _updates) = self
            .registry
            .submit_task(name, task)
            .await
            .expect("the node is connected");
        task_id
    }

    fn resolve(&self, name: &str) -> Caller {
        self.calls.resolve(&Peer::Node(name.to_string()))
    }

    async fn invoke(
        &self,
        peer: Peer,
        agent: &str,
        run: Option<&str>,
    ) -> Vec<Result<CallEvent, CallError>> {
        self.calls
            .call(peer, CallRequest::InvokeAgent(params(agent, run)))
            .collect()
            .await
    }

    fn spawned(&self) -> Vec<Option<CallStamp>> {
        self.stamps.lock().unwrap().clone()
    }

    /// The edge log's rows as `(kind, from, to, outcome)`.
    fn rows(&self) -> Vec<(EdgeKind, String, String, String)> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<EdgeRow>(line).unwrap())
            .map(|row| (row.kind, row.from, row.to, row.outcome))
            .collect()
    }
}

fn params(agent: &str, run: Option<&str>) -> InvokeAgentParams {
    InvokeAgentParams {
        agent: agent.to_string(),
        prompt: "go".to_string(),
        handle: None,
        include_trace: false,
        spawn_context: None,
        remaining: Default::default(),
        run: run.map(str::to_string),
    }
}

fn refused_with(events: &[Result<CallEvent, CallError>], needle: &str) -> bool {
    matches!(events, [Err(CallError::Refused(why))] if why.contains(needle))
}

/// ADR-0023 § 2: a node with no broker-made chain is `External`, and is
/// refused everywhere: a connected node no task was handed, and a name no
/// node was admitted under. Nothing is spawned, and each refusal is one
/// row (a message one, for a post).
#[tokio::test]
async fn chainless_node_is_external() {
    let kit = Kit::new(&["kit-a"]);
    let (lead, _frames) = kit.connect("lead");

    for name in [lead.as_str(), "never-admitted-0"] {
        assert_eq!(kit.resolve(name), Caller::External(Admitter::Chainless));
        let peer = Peer::Node(name.to_string());

        let events = kit.invoke(peer.clone(), "kit-a", None).await;
        assert!(refused_with(&events, "chainless"), "{events:?}");
        let listed: Vec<_> = kit
            .calls
            .call(peer.clone(), CallRequest::ListAgents)
            .collect()
            .await;
        assert!(refused_with(&listed, "chainless"), "{listed:?}");
        let posted: Vec<_> = kit
            .calls
            .call(
                peer,
                CallRequest::SendMessage(SendMessageParams {
                    to: ROOT_NAME.to_string(),
                    text: "hi".to_string(),
                }),
            )
            .collect()
            .await;
        let [Ok(CallEvent::Result(CallResult::Posted(status)))] = &posted[..] else {
            panic!("a post answers with a status: {posted:?}");
        };
        assert_eq!(
            *status,
            MessageStatus::Refused {
                reason: RefusalReason::NotOnTree
            }
        );
    }
    assert!(kit.spawned().is_empty(), "nothing was spawned");
    let kinds: Vec<EdgeKind> = kit.rows().into_iter().map(|row| row.0).collect();
    assert_eq!(
        kinds,
        [
            EdgeKind::Refusal,
            EdgeKind::Refusal,
            EdgeKind::Message,
            EdgeKind::Refusal,
            EdgeKind::Refusal,
            EdgeKind::Message,
        ]
    );
}

/// A participant between tasks serves no run, so it is chainless: its
/// task's result closed the run it was handed with.
#[tokio::test]
async fn idle_participant_is_chainless() {
    let kit = Kit::new(&["kit-a"]);
    let (lead, _frames) = kit.connect("lead");
    let task = kit.hand(&lead, "t-1", &["lead"]).await;
    assert!(matches!(kit.resolve(&lead), Caller::Node(_)));

    assert!(kit.registry.on_frame(
        &lead,
        ParticipantFrame::Status {
            task_id: task.clone(),
            state: TaskState::Completed,
            message: None,
            metadata: None,
        },
    ));
    assert_eq!(kit.resolve(&lead), Caller::External(Admitter::Chainless));
    assert_eq!(kit.registry.open_runs(), 0);
    let events = kit.invoke(Peer::Node(lead), "kit-a", Some(&task)).await;
    assert!(refused_with(&events, "chainless"), "{events:?}");
    assert!(kit.spawned().is_empty());
}

/// ADR-0023 § 1: every task a node is handed opens its own run, named by
/// its task id, under the chain its grant stamped; a task no grant stamped
/// is not handed over. A spawned worker has exactly one run.
#[tokio::test]
async fn submitted_task_opens_its_own_run() {
    let kit = Kit::new(&[]);
    let (lead, _frames) = kit.connect("lead");
    assert_eq!(kit.registry.open_runs(), 0);

    let first = kit.hand(&lead, "t-1", &["lead"]).await;
    assert_eq!(kit.registry.open_runs(), 1);
    let second = kit.hand(&lead, "t-2", &["boss", "lead"]).await;
    assert_eq!(kit.registry.open_runs(), 2);
    let (spec, runs) = kit.registry.open_runs_of(&lead).unwrap();
    assert_eq!(spec, "lead");
    let runs: Vec<(String, Vec<String>)> = runs
        .into_iter()
        .map(|run| (run.name, run.chain.chain))
        .collect();
    assert_eq!(
        runs,
        [
            (first, vec!["root".to_string(), "lead".to_string()]),
            (
                second,
                vec!["root".to_string(), "boss".to_string(), "lead".to_string()]
            ),
        ]
    );

    assert!(
        kit.registry
            .submit_task(&lead, DelegatedTask::new("no grant"))
            .await
            .is_none(),
        "a task with no stamp opens no run and is not handed over"
    );
    assert_eq!(kit.registry.open_runs(), 2);

    // A worker the root's call spawns: exactly one run while it works.
    let registry = ParticipantRegistry::new();
    let seen = Arc::new(Mutex::new(None));
    let runner: Arc<dyn VirtualAgent> = Arc::new(Answering {
        registry: registry.clone(),
        runs_seen: seen.clone(),
    });
    let calls = Arc::new(BrokerCalls::new(
        registry.clone(),
        Arc::new(BTreeMap::from([("worker".to_string(), runner)])),
        None,
    ));
    registry.install_calls(&calls);
    let events: Vec<_> = calls
        .call(Peer::Root, CallRequest::InvokeAgent(params("worker", None)))
        .collect()
        .await;
    assert!(
        matches!(events.last(), Some(Ok(CallEvent::Result(_)))),
        "{events:?}"
    );
    assert_eq!(*seen.lock().unwrap(), Some(1), "one run while it worked");
    assert_eq!(registry.open_runs(), 0, "its result closed it");
}

/// A call to a name nobody serves is refused by the gate, and the refusal
/// holds even when a participant registers under that name before the
/// call's stream is read: the effect acts on the grant, never a second
/// lookup.
#[tokio::test]
async fn unknown_callee_refused_by_gate() {
    let kit = Kit::new(&[]);
    let call = kit
        .calls
        .call(Peer::Root, CallRequest::InvokeAgent(params("late-0", None)));
    let (late, mut frames) = kit.connect("late");
    assert_eq!(late, "late-0", "registered under the name the call named");

    let events: Vec<_> = call.collect().await;
    assert_eq!(events, [Err(CallError::UnknownAgent("late-0".to_string()))]);
    assert!(
        frames.try_recv().is_err(),
        "the late node was handed nothing"
    );
    assert_eq!(
        kit.rows(),
        [(
            EdgeKind::Refusal,
            ROOT_NAME.to_string(),
            "late-0".to_string(),
            "unknown agent".to_string()
        )]
    );
}

/// The roster is a gate rule: a node's call to a virtual agent outside its
/// roster is refused, and its row written, before the call's stream is
/// read — nothing is spawned. One on its roster spawns.
#[tokio::test]
async fn roster_refused_before_spawn() {
    let kit = Kit::new(&["kit-a", "kit-b"]);
    let (lead, _frames) = kit.connect("lead");
    let task = kit.hand(&lead, "t-1", &["lead"]).await;
    kit.registry.set_node_context(
        &lead,
        SpawnContext {
            roster: vec!["kit-a".to_string()],
            ..SpawnContext::default()
        },
    );

    let call = kit.calls.call(
        Peer::Node(lead.clone()),
        CallRequest::InvokeAgent(params("kit-b", Some(&task))),
    );
    let why = format!("'kit-b' is not on {lead}'s roster");
    assert_eq!(
        kit.rows(),
        [(
            EdgeKind::Refusal,
            lead.clone(),
            "kit-b".to_string(),
            format!("refused: {why}")
        )],
        "refused before the stream was read"
    );
    let events: Vec<_> = call.collect().await;
    assert_eq!(events, [Err(CallError::Refused(why))]);
    assert!(kit.spawned().is_empty(), "nothing was spawned");

    kit.invoke(Peer::Node(lead), "kit-a", Some(&task)).await;
    assert_eq!(kit.spawned().len(), 1, "on its roster, it spawns");
}

/// GT-0b: an `agent.invoke` naming a run that is not the caller's own open
/// run — another node's, one nobody has, or none at all — is refused, with
/// no effect. Naming its own is granted, under that run.
#[tokio::test]
async fn invoke_naming_foreign_run_refused() {
    let kit = Kit::new(&["kit-a"]);
    let (a, _a_frames) = kit.connect("alpha");
    let (b, _b_frames) = kit.connect("beta");
    let own = kit.hand(&a, "t-a", &["alpha"]).await;
    let foreign = kit.hand(&b, "t-b", &["beta"]).await;

    for run in [Some(foreign.as_str()), Some("task-nobody"), None] {
        let events = kit.invoke(Peer::Node(a.clone()), "kit-a", run).await;
        assert!(
            matches!(&events[..], [Err(CallError::Refused(_))]),
            "{run:?}: {events:?}"
        );
    }
    assert!(kit.spawned().is_empty(), "nothing was spawned");
    let rows = kit.rows();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| row.0 == EdgeKind::Refusal && row.1 == a)
    );

    kit.invoke(Peer::Node(a.clone()), "kit-a", Some(&own)).await;
    let spawned = kit.spawned();
    let [Some(stamp)] = &spawned[..] else {
        panic!("one spawn, stamped: {spawned:?}");
    };
    assert_eq!(stamp.caller.as_deref(), Some(a.as_str()));
    assert_eq!(stamp.chain.chain, ["root", "alpha", "kit-a"]);
    assert_eq!(stamp.chain.root_task_id, "t-a");
}

/// GT-0b: a second task handed to a busy node does not block its
/// delegation — there is no "two open runs" refusal — and each call
/// carries the chain of the run it names.
#[tokio::test]
async fn busy_node_second_task_does_not_block_invoke() {
    let kit = Kit::new(&["kit-x"]);
    let (lead, _frames) = kit.connect("lead");
    let first = kit.hand(&lead, "t-1", &["lead"]).await;
    let second = kit.hand(&lead, "t-2", &["boss", "lead"]).await;
    assert_eq!(kit.registry.open_runs(), 2, "the node serves two tasks");

    kit.invoke(Peer::Node(lead.clone()), "kit-x", Some(&second))
        .await;
    kit.invoke(Peer::Node(lead.clone()), "kit-x", Some(&first))
        .await;
    let chains: Vec<(String, Vec<String>)> = kit
        .spawned()
        .into_iter()
        .map(|stamp| stamp.expect("stamped"))
        .map(|stamp| (stamp.chain.root_task_id, stamp.chain.chain))
        .collect();
    assert_eq!(
        chains,
        [
            (
                "t-2".to_string(),
                ["root", "boss", "lead", "kit-x"].map(String::from).to_vec()
            ),
            (
                "t-1".to_string(),
                ["root", "lead", "kit-x"].map(String::from).to_vec()
            ),
        ]
    );
    assert!(
        kit.rows().iter().all(|row| row.0 != EdgeKind::Refusal),
        "nothing was refused"
    );
}

/// A run closes on its task's cancel and on its connection's loss (and on
/// a hold's close, which arrives with ADR-0022's holds): a request after
/// that is chainless.
#[tokio::test]
async fn a_run_closes_on_cancel_and_connection_loss() {
    let kit = Kit::new(&["kit-a"]);
    let (lead, _frames) = kit.connect("lead");

    let task = kit.hand(&lead, "t-1", &["lead"]).await;
    assert!(matches!(kit.resolve(&lead), Caller::Node(_)));
    kit.registry.cancel_task(&lead, &task);
    assert_eq!(kit.resolve(&lead), Caller::External(Admitter::Chainless));
    assert_eq!(kit.registry.open_runs(), 0);
    let events = kit
        .invoke(Peer::Node(lead.clone()), "kit-a", Some(&task))
        .await;
    assert!(refused_with(&events, "chainless"), "{events:?}");

    let task = kit.hand(&lead, "t-2", &["lead"]).await;
    assert!(matches!(kit.resolve(&lead), Caller::Node(_)));
    kit.registry.deregister(&lead);
    assert_eq!(kit.resolve(&lead), Caller::External(Admitter::Chainless));
    assert_eq!(kit.registry.open_runs(), 0);
    let events = kit.invoke(Peer::Node(lead), "kit-a", Some(&task)).await;
    assert!(refused_with(&events, "chainless"), "{events:?}");
    assert!(kit.spawned().is_empty());
}

/// A log sink for one closure's tracing output.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// `f`'s value and everything it logged.
fn logged<T>(f: impl FnOnce() -> T) -> (T, String) {
    let out = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(out.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let value = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    (value, text)
}

/// ADR-0023 § 1: every outcome is logged with the typed caller and the row
/// it matched before any effect — the root's grant before its worker is
/// spawned, a node's refusal with the node and its row.
#[tokio::test]
async fn every_outcome_is_logged_before_any_effect() {
    let kit = Kit::new(&["kit-a"]);
    let (call, log) = logged(|| {
        kit.calls
            .call(Peer::Root, CallRequest::InvokeAgent(params("kit-a", None)))
    });
    let line = log
        .lines()
        .find(|line| line.contains("chatty::gate"))
        .unwrap_or_else(|| panic!("no gate line in {log}"));
    assert!(
        line.contains("granted")
            && line.contains("caller=root")
            && line.contains("row=root/agent.invoke"),
        "{line}"
    );
    assert!(kit.spawned().is_empty(), "logged before the effect");
    let _: Vec<_> = call.collect().await;
    assert_eq!(kit.spawned().len(), 1, "then the effect");

    let (lead, _frames) = kit.connect("lead");
    let task = kit.hand(&lead, "t-1", &["lead"]).await;
    let (_call, log) = logged(|| {
        kit.calls.call(
            Peer::Node(lead.clone()),
            CallRequest::InvokeAgent(params("nobody", Some(&task))),
        )
    });
    let line = log
        .lines()
        .find(|line| line.contains("chatty::gate"))
        .unwrap_or_else(|| panic!("no gate line in {log}"));
    assert!(
        line.contains("refused")
            && line.contains(&format!("caller=node:{lead}"))
            && line.contains("row=node/agent.invoke"),
        "{line}"
    );
}

// ---------------------------------------------------------------------------
// At the wire: when a run opens and closes, as a worker's lines race it
// ---------------------------------------------------------------------------

/// A node on a connection the broker made, writing raw v3 lines.
struct Raw {
    name: String,
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl Raw {
    async fn connect(registry: &ParticipantRegistry, spec: &str) -> Self {
        let LocalConnection { name, worker_end } = open_connection(registry, spec, None).unwrap();
        worker_end.set_nonblocking(true).unwrap();
        let (read, write) = UnixStream::from_std(worker_end).unwrap().into_split();
        let mut raw = Self {
            name,
            lines: BufReader::new(read).lines(),
            write,
        };
        raw.send(&[json!({"id": 1, "method": "session.hello", "params": {"card": {}}})])
            .await;
        raw.reply(1).await;
        while !registry.is_registered(&raw.name) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        raw
    }

    /// Write `frames` in one write.
    async fn send(&mut self, frames: &[Value]) {
        let mut out = String::new();
        for frame in frames {
            let mut frame = frame.clone();
            frame["v"] = json!(3);
            out.push_str(&format!("{frame}\n"));
        }
        self.write.write_all(out.as_bytes()).await.unwrap();
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("a line in time")
            .unwrap()
            .expect("the connection is open");
        serde_json::from_str(&line).unwrap()
    }

    /// The next `task.run`: its request id and its task id.
    async fn task_run(&mut self) -> (Value, String) {
        loop {
            let frame = self.next().await;
            if frame["method"] == "task.run" {
                let task = frame["params"]["taskId"].as_str().unwrap().to_string();
                return (frame["id"].clone(), task);
            }
        }
    }

    /// The broker's answer to this node's request `id`.
    async fn reply(&mut self, id: u64) -> Value {
        loop {
            let frame = self.next().await;
            if frame["id"] == id && frame.get("method").is_none() {
                return frame;
            }
        }
    }
}

fn invoke_line(id: u64, agent: &str, run: &str) -> Value {
    json!({"id": id, "method": "agent.invoke",
           "params": {"agent": agent, "prompt": "go", "run": run}})
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never: {what}");
}

/// A worker that sends `agent.invoke` the moment its `task.run` arrives is
/// never chainless: the run opened before the frame was queued.
#[tokio::test]
async fn invoke_on_task_run_is_never_chainless() {
    let kit = Kit::new(&["leaf"]);
    let mut mid = Raw::connect(&kit.registry, "mid").await;
    let (_task, _updates) = kit
        .registry
        .submit_task(&mid.name, DelegatedTask::from_root("go"))
        .await
        .unwrap();

    let (_run, task) = mid.task_run().await;
    mid.send(&[invoke_line(2, "leaf", &task)]).await;
    let reply = mid.reply(2).await;
    assert!(reply.get("result").is_some(), "granted: {reply}");
    assert_eq!(kit.spawned().len(), 1);
}

/// A request the worker sent before its task's result is decided before
/// the result is handled, under the run still open, and granted. One sent
/// after the result was handled is chainless.
#[tokio::test]
async fn trailing_request_before_result_is_granted() {
    let kit = Kit::new(&["leaf"]);
    let mut mid = Raw::connect(&kit.registry, "mid").await;
    let (_task, _updates) = kit
        .registry
        .submit_task(&mid.name, DelegatedTask::from_root("go"))
        .await
        .unwrap();

    let (run, task) = mid.task_run().await;
    // The call, then the task's result, in one write.
    mid.send(&[
        invoke_line(2, "leaf", &task),
        json!({"id": run, "result": {"state": "completed"}}),
    ])
    .await;
    until("the trailing call reached its effect", || {
        kit.spawned().len() == 1
    })
    .await;
    until("the result closed the run", || {
        kit.registry.open_runs() == 0
    })
    .await;

    mid.send(&[invoke_line(3, "leaf", &task)]).await;
    let reply = mid.reply(3).await;
    assert_eq!(reply["error"]["kind"], "refused", "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .is_some_and(|why| why.contains("chainless")),
        "{reply}"
    );
    assert_eq!(kit.spawned().len(), 1, "the late call had no effect");
}

/// A virtual agent whose worker answers its one task at once, after noting
/// how many runs its node had open when the task arrived.
struct Answering {
    registry: ParticipantRegistry,
    runs_seen: Arc<Mutex<Option<usize>>>,
}

struct Answered {
    name: String,
    task_id: String,
}

impl WorkerHandle for Answered {
    fn name(&self) -> &str {
        &self.name
    }

    fn task_id(&self) -> Option<&str> {
        Some(&self.task_id)
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&chatty_fabric::wire::TaskMetadata>) {}
}

impl VirtualAgent for Answering {
    fn agent_name(&self) -> &str {
        "worker"
    }

    fn agent_card(&self) -> ParticipantCard {
        ParticipantCard {
            name: "worker".to_string(),
            ..Default::default()
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        Box::pin(async move {
            let mut raw = Raw::connect(&self.registry, "worker").await;
            let name = raw.name.clone();
            let registry = self.registry.clone();
            let seen = self.runs_seen.clone();
            tokio::spawn(async move {
                let (run, _task) = raw.task_run().await;
                let open = registry.open_runs_of(&raw.name).map(|(_, runs)| runs.len());
                *seen.lock().unwrap() = open;
                raw.send(&[json!({"id": run, "result": {"state": "completed"}})])
                    .await;
                // Hold the connection until the broker lets go.
                while raw.lines.next_line().await.is_ok_and(|line| line.is_some()) {}
            });
            let (task_id, updates) = self
                .registry
                .submit_task(&name, task)
                .await
                .ok_or_else(|| anyhow::anyhow!("gone before its task"))?;
            let handle: Box<dyn WorkerHandle> = Box::new(Answered { name, task_id });
            Ok((handle, updates))
        })
    }
}
