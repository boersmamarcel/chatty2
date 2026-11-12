//! Resumable handles' lifecycle in the broker (RC-4, AGE-653): the owner
//! rule as the gate decides it, one conversation's handles out of another's
//! reach, an ended handle refused with nothing spawned, an owner's end
//! ending its handles, and `tasks/get` over this root process's recent
//! tasks. On a fake clock where time matters; the workers are a fake
//! virtual agent that answers at once with a captured conversation.

use std::sync::atomic::AtomicU64;
use std::time::{Duration, SystemTime};

use chatty_fabric::wire::Opaque;
use chatty_fabric::{HandleError, Remaining};
use serde_json::json;

use super::*;
use crate::participant::registry::RunGuard;
use crate::participant::{ParticipantCard, WorkerFuture, WorkerHandle};

const WORKER: &str = "worker";
const LEAD: &str = "lead";

/// One worker the fake agent started.
#[derive(Debug, Clone)]
struct Spawned {
    node: String,
    run: String,
    task_id: String,
    /// Whether its task restored a handle's conversation.
    resumed: bool,
}

/// A virtual agent whose workers are admitted under their caller, serve
/// one run, and end their task with a captured conversation — once
/// `release` lets them, when it is set.
struct Saving {
    name: &'static str,
    registry: ParticipantRegistry,
    spawned: Arc<Mutex<Vec<Spawned>>>,
    release: Option<Arc<tokio::sync::Semaphore>>,
}

struct SavingWorker {
    node: String,
    task_id: String,
    _run: RunGuard,
}

impl WorkerHandle for SavingWorker {
    fn name(&self) -> &str {
        &self.node
    }

    fn task_id(&self) -> Option<&str> {
        Some(&self.task_id)
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&chatty_fabric::wire::TaskMetadata>) {}
}

impl VirtualAgent for Saving {
    fn agent_name(&self) -> &str {
        self.name
    }

    fn agent_card(&self) -> ParticipantCard {
        ParticipantCard {
            name: self.name.to_string(),
            ..Default::default()
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        let stamp = task.call.clone().expect("a broker call stamps its task");
        let node = self
            .registry
            .admit_under(self.name, stamp.caller.as_deref());
        let run = self
            .registry
            .open_run(&node, stamp.caller.as_deref(), stamp.from_run, stamp.chain)
            .expect("a run");
        let mut spawned = self.spawned.lock().unwrap();
        let task_id = format!("task-{}", spawned.len());
        spawned.push(Spawned {
            node: node.clone(),
            run: run.name(),
            task_id: task_id.clone(),
            resumed: task.resume.is_some(),
        });
        drop(spawned);
        let (tx, rx) = mpsc::unbounded_channel();
        let release = self.release.clone();
        let said = json!([{ "role": "user", "content": task.text }]);
        tokio::spawn(async move {
            if let Some(release) = release {
                release.acquire().await.expect("open").forget();
            }
            let _ = tx.send(TaskUpdate::Status {
                state: TaskState::Completed,
                message: None,
                metadata: Some(TaskMetadata {
                    conversation: Some(Opaque::from_value(&said).unwrap()),
                    ..Default::default()
                }),
            });
        });
        let worker = SavingWorker {
            node,
            task_id,
            _run: run,
        };
        Box::pin(async move { Ok((Box::new(worker) as Box<dyn WorkerHandle>, rx)) })
    }
}

struct Kit {
    calls: Arc<BrokerCalls>,
    registry: ParticipantRegistry,
    spawned: Arc<Mutex<Vec<Spawned>>>,
    /// Seconds since the epoch, on the broker's clock.
    now: Arc<AtomicU64>,
}

impl Kit {
    fn new(release: Option<Arc<tokio::sync::Semaphore>>) -> Self {
        let registry = ParticipantRegistry::new();
        let spawned = Arc::new(Mutex::new(Vec::new()));
        let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = [WORKER, LEAD]
            .into_iter()
            .map(|name| {
                let runner: Arc<dyn VirtualAgent> = Arc::new(Saving {
                    name,
                    registry: registry.clone(),
                    spawned: spawned.clone(),
                    release: release.clone(),
                });
                (name.to_string(), runner)
            })
            .collect();
        let now = Arc::new(AtomicU64::new(1_000_000));
        let read = now.clone();
        let clock: Clock = Arc::new(move || {
            SystemTime::UNIX_EPOCH + Duration::from_secs(read.load(Ordering::SeqCst))
        });
        let calls =
            Arc::new(BrokerCalls::new(registry.clone(), Arc::new(runners), None).with_clock(clock));
        registry.install_calls(&calls);
        Self {
            calls,
            registry,
            spawned,
            now,
        }
    }

    fn advance(&self, by: Duration) {
        self.now.fetch_add(by.as_secs(), Ordering::SeqCst);
    }

    fn spawned(&self) -> Vec<Spawned> {
        self.spawned.lock().unwrap().clone()
    }

    /// A node admitted as `spec` under `owner` (`None`: the root), serving
    /// one run of the conversation `scope`: its name and the run's name.
    fn node(&self, spec: &str, owner: Option<&str>, scope: &str) -> (String, String) {
        let name = self.registry.admit_under(spec, owner);
        let root_task_id = format!("t-{name}");
        locked(&self.calls.scopes).insert(root_task_id.clone(), ConversationScope::new(scope));
        let chain = CallChain::root(root_task_id).extend(spec).unwrap();
        let run = self
            .registry
            .open_run(&name, owner, None, chain)
            .expect("a run");
        let run_name = run.name();
        std::mem::forget(run);
        (name, run_name)
    }

    /// `peer`'s `agent.invoke` of the worker under `handle`, from the run
    /// `run` (a node's) or the root of the conversation `scope`: how it
    /// ended.
    async fn call(
        &self,
        peer: Peer,
        scope: Option<&str>,
        run: Option<&str>,
        handle: &str,
    ) -> Result<InvokeAgentOutcome, CallError> {
        self.call_agent(WORKER, peer, scope, run, handle).await
    }

    /// [`call`](Self::call) of `agent`.
    async fn call_agent(
        &self,
        agent: &str,
        peer: Peer,
        scope: Option<&str>,
        run: Option<&str>,
        handle: &str,
    ) -> Result<InvokeAgentOutcome, CallError> {
        let scope = scope.map(ConversationScope::new);
        let mut stream = self.calls.call_in(
            peer,
            scope.as_ref(),
            CallRequest::InvokeAgent(InvokeAgentParams {
                agent: agent.to_string(),
                prompt: format!("task under {handle}"),
                handle: Some(handle.to_string()),
                include_trace: false,
                spawn_context: None,
                remaining: Remaining::default(),
                run: run.map(str::to_string),
            }),
        );
        while let Some(event) = stream.next().await {
            match event {
                Ok(CallEvent::Result(CallResult::Invoked(outcome))) => return Ok(outcome),
                Ok(_) => {}
                Err(error) => return Err(error),
            }
        }
        panic!("the call ended with no result");
    }

    async fn root(&self, scope: &str, handle: &str) -> Result<InvokeAgentOutcome, CallError> {
        self.call(Peer::Root, Some(scope), None, handle).await
    }
}

fn not_owner(result: &Result<InvokeAgentOutcome, CallError>) -> bool {
    matches!(result, Err(CallError::Handle(HandleError::NotOwner { .. })))
}

/// Invariant 5 (`handle_owner_rule`): a handle a node started is tasked by
/// that node and by its ancestors — here the root — and refused
/// `not_owner`, with nothing spawned, for a sibling and for the node's own
/// child. A handle's worker acts as its handle, so a handle it starts
/// outlives the worker and its handle may task it.
#[tokio::test]
async fn handle_owner_rule() {
    let kit = Kit::new(None);
    let (lead, lead_run) = kit.node("lead", None, "conv-a");
    let first = kit
        .call(Peer::Node(lead.clone()), None, Some(&lead_run), "fix")
        .await
        .expect("the owner starts it");
    assert_eq!(first.handle.as_deref(), Some("fix"));

    let (sibling, sibling_run) = kit.node("coder", None, "conv-a");
    let (child, child_run) = kit.node("sub", Some(&lead), "conv-a");
    let before = kit.spawned().len();
    for (node, run) in [(sibling, sibling_run), (child, child_run)] {
        let refused = kit
            .call(Peer::Node(node.clone()), None, Some(&run), "fix")
            .await;
        assert!(not_owner(&refused), "{node}: {refused:?}");
    }
    assert_eq!(kit.spawned().len(), before, "a refusal spawns nothing");

    // The owner's ancestor, the root of its conversation, resumes it.
    let resumed = kit
        .root("conv-a", "fix")
        .await
        .expect("an ancestor tasks it");
    assert!(resumed.success, "{resumed:?}");
    assert!(kit.spawned().last().unwrap().resumed);

    // The review's probe: node `coder` starts handle H, H's worker starts
    // handle G, and `coder` — the owner of G's owner — tasks G.
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let kit = Arc::new(Kit::new(Some(release.clone())));
    let (coder, coder_run) = kit.node("coder", None, "conv-a");
    let h = {
        let (kit, coder, run) = (kit.clone(), coder.clone(), coder_run.clone());
        tokio::spawn(async move {
            kit.call_agent(LEAD, Peer::Node(coder), None, Some(&run), "H")
                .await
        })
    };
    let h_worker = started(&kit, 1).await;
    let g = {
        let kit = kit.clone();
        tokio::spawn(async move {
            kit.call(Peer::Node(h_worker.node), None, Some(&h_worker.run), "G")
                .await
        })
    };
    started(&kit, 2).await;
    release.add_permits(2);
    g.await.unwrap().expect("H's worker starts G");
    h.await.unwrap().expect("coder starts H");
    release.add_permits(1);
    kit.call(Peer::Node(coder), None, Some(&coder_run), "G")
        .await
        .expect("the owner of G's owner tasks G");
    assert!(kit.spawned().last().unwrap().resumed);
}

/// Wait until the fake agent has started `n` workers; the last one.
async fn started(kit: &Kit, n: usize) -> Spawned {
    loop {
        let spawned = kit.spawned();
        if spawned.len() >= n {
            return spawned[n - 1].clone();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The worker of a handle's task calls as its handle: a handle it starts
/// is the handle's, so the handle's next task — another worker — resumes
/// it, and the root, its ancestor, may too.
#[tokio::test]
async fn a_handle_owns_what_its_workers_start() {
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let kit = Arc::new(Kit::new(Some(release.clone())));
    let lead_task = |kit: Arc<Kit>| {
        tokio::spawn(async move {
            kit.call_agent(LEAD, Peer::Root, Some("conv-a"), None, "lead-h")
                .await
        })
    };
    let child_task = |kit: Arc<Kit>, worker: Spawned| {
        tokio::spawn(async move {
            kit.call(Peer::Node(worker.node), None, Some(&worker.run), "child")
                .await
        })
    };
    // The lead handle's first task starts `child`.
    let first = lead_task(kit.clone());
    let lead_0 = started(&kit, 1).await;
    let child_0 = child_task(kit.clone(), lead_0);
    started(&kit, 2).await;
    // Both tasks may end now, in either order.
    release.add_permits(2);
    child_0
        .await
        .unwrap()
        .expect("the lead's worker starts child");
    first.await.unwrap().expect("the lead's first task");

    // Its second task, a new worker, resumes `child` as the same handle.
    let second = lead_task(kit.clone());
    let lead_1 = started(&kit, 3).await;
    assert!(lead_1.resumed);
    let child_1 = child_task(kit.clone(), lead_1);
    let resumed_child = started(&kit, 4).await;
    assert!(resumed_child.resumed, "the child handle was resumed");
    release.add_permits(2);
    child_1
        .await
        .unwrap()
        .expect("the lead handle tasks its child");
    second.await.unwrap().expect("the lead's second task");

    // The root is the child handle's ancestor.
    release.add_permits(1);
    kit.root("conv-a", "child")
        .await
        .expect("the root tasks its descendant's handle");
}

/// Invariant 8 (`handle_scope_isolation`): one broker serves two root
/// conversations; the second neither sees nor resumes the first's handle,
/// even by name, and cannot read its tasks.
#[tokio::test]
async fn handle_scope_isolation() {
    let kit = Kit::new(None);
    kit.root("conv-a", "fix").await.expect("conv-a starts fix");
    let in_b = kit
        .root("conv-b", "fix")
        .await
        .expect("conv-b starts its own");
    assert!(in_b.success);
    let spawned = kit.spawned();
    assert!(
        !spawned[1].resumed,
        "conv-b's call started fresh, not conv-a's conversation"
    );
    assert_eq!(
        kit.calls.handles().names(&ConversationScope::new("conv-a")),
        ["fix"]
    );
    // A node of conv-b, whose run hangs from conv-b's root, sees conv-b's
    // handle as conv-b's root owns it, and so cannot reach it.
    let (node, run) = kit.node("coder", None, "conv-b");
    let refused = kit.call(Peer::Node(node), None, Some(&run), "fix").await;
    assert!(not_owner(&refused), "{refused:?}");
    // conv-a's task is not conv-b's to read.
    let task = &spawned[0].task_id;
    let a = ConversationScope::new("conv-a");
    let b = ConversationScope::new("conv-b");
    assert!(kit.calls.tasks_get(Some(&a), task).unwrap().is_some());
    assert_eq!(kit.calls.tasks_get(Some(&b), task).unwrap(), None);
}

/// Invariant 7 (`expired_handle_refused`): a handle idle for 30 min has
/// ended; tasking it is the typed `handle_expired`, and spawns nothing.
/// The same for a handle its owner dropped.
#[tokio::test]
async fn expired_handle_refused() {
    let kit = Kit::new(None);
    kit.root("conv-a", "fix").await.expect("started");
    // 30 min, as a literal: the spec's number, not the constant's.
    kit.advance(Duration::from_secs(1800));
    let spawned = kit.spawned().len();
    let refused = kit.root("conv-a", "fix").await;
    assert_eq!(
        refused.unwrap_err(),
        CallError::Handle(HandleError::Expired {
            name: "fix".to_string()
        })
    );
    assert_eq!(kit.spawned().len(), spawned, "nothing spawned");

    let scope = ConversationScope::new("conv-a");
    kit.root("conv-a", "other").await.expect("started");
    kit.calls
        .drop_handle(&Peer::Root, Some(&scope), None, "other")
        .expect("the owner drops it");
    let spawned = kit.spawned().len();
    let refused = kit.root("conv-a", "other").await.unwrap_err();
    assert!(
        refused.to_string().starts_with("handle_expired: "),
        "{refused}"
    );
    assert_eq!(kit.spawned().len(), spawned, "nothing spawned");

    // A node drops its own handle from the run it serves (the run names
    // its conversation, as its calls' runs do).
    let (lead, run) = kit.node("lead", None, "conv-a");
    kit.call(Peer::Node(lead.clone()), None, Some(&run), "mine")
        .await
        .expect("the node starts it");
    kit.calls
        .drop_handle(&Peer::Node(lead.clone()), None, Some(&run), "mine")
        .expect("the owner node drops it");
    let refused = kit.call(Peer::Node(lead), None, Some(&run), "mine").await;
    assert!(
        matches!(refused, Err(CallError::Handle(HandleError::Expired { .. }))),
        "{refused:?}"
    );
}

/// Invariant 6 at the broker (`handle_lifecycle`'s owner end): a handle
/// whose owner is a plain call's worker ends when that call ends; one the
/// root conversation owns ends with the conversation.
#[tokio::test]
async fn an_owners_end_ends_its_handles() {
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let kit = Arc::new(Kit::new(Some(release.clone())));
    // The root's plain call, held open.
    let outer = {
        let kit = kit.clone();
        tokio::spawn(async move {
            let mut stream = kit.calls.call(
                Peer::Root,
                CallRequest::InvokeAgent(InvokeAgentParams {
                    agent: WORKER.to_string(),
                    prompt: "plain".to_string(),
                    handle: None,
                    include_trace: false,
                    spawn_context: None,
                    remaining: Remaining::default(),
                    run: None,
                }),
            );
            while stream.next().await.is_some() {}
        })
    };
    let worker = loop {
        if let Some(worker) = kit.spawned().first().cloned() {
            break worker;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    // The plain call's worker starts a handle of its own.
    let inner = {
        let kit = kit.clone();
        let worker = worker.clone();
        tokio::spawn(async move {
            kit.call_agent(
                LEAD,
                Peer::Node(worker.node),
                None,
                Some(&worker.run),
                "inner",
            )
            .await
        })
    };
    release.add_permits(1);
    release.add_permits(1);
    inner.await.unwrap().expect("the worker's handle task ran");
    outer.await.unwrap();
    let key = HandleKey::new(ConversationScope::new(ROOT_SCOPE), "inner");
    assert_eq!(kit.calls.handles().status(&key), HandleStatus::Ended);

    // The root conversation's end ends its handles.
    let kit = Kit::new(None);
    kit.root("conv-a", "fix").await.expect("started");
    let scope = ConversationScope::new("conv-a");
    kit.calls.end_conversation(&scope);
    let key = HandleKey::new(scope, "fix");
    assert_eq!(kit.calls.handles().status(&key), HandleStatus::Free);
    // Its saved conversation is gone: the name starts afresh.
    kit.root("conv-a", "fix").await.expect("a fresh handle");
    assert!(!kit.spawned().last().unwrap().resumed);
}

/// `tasks_get_answers_recent`: the root's `tasks/get` answers for a live
/// task and for one that finished within 10 min, with the handle as its
/// `contextId`; not after, and not for a task this process never ran.
#[tokio::test]
async fn tasks_get_answers_recent() {
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let kit = Arc::new(Kit::new(Some(release.clone())));
    let scope = ConversationScope::new("conv-a");
    let running = {
        let kit = kit.clone();
        tokio::spawn(async move { kit.root("conv-a", "fix").await })
    };
    let task = loop {
        if let Some(worker) = kit.spawned().first().cloned() {
            break worker.task_id;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let live = kit.calls.tasks_get(Some(&scope), &task).unwrap().unwrap();
    assert_eq!(live["status"]["state"], "working");
    assert_eq!(live["contextId"], "fix");
    assert_eq!(live["kind"], "task");
    release.add_permits(1);
    running.await.unwrap().expect("the task ran");

    let done = kit.calls.tasks_get(Some(&scope), &task).unwrap().unwrap();
    assert_eq!(done["status"]["state"], "completed");
    assert_eq!(done["contextId"], "fix");
    assert_eq!(
        crate::participant::recent_tasks::RECENT_TASKS,
        Duration::from_secs(600)
    );
    kit.advance(Duration::from_secs(599));
    assert!(kit.calls.tasks_get(Some(&scope), &task).unwrap().is_some());
    kit.advance(Duration::from_secs(1));
    assert_eq!(kit.calls.tasks_get(Some(&scope), &task).unwrap(), None);
    assert_eq!(
        kit.calls.tasks_get(Some(&scope), "task-unknown").unwrap(),
        None
    );
}
