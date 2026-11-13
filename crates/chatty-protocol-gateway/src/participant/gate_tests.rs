//! `decide` on its own (ADR-0023, GT-0): the rows, over plain snapshots,
//! with no broker behind them.

use std::sync::Mutex;

use chatty_core::agent_spec::AgentSpec;
use chatty_core::services::delegation_policy::SpecPolicy;
use chatty_fabric::{MessageStatus, Remaining};
use serde_json::json;

use super::*;

/// A policy that grants what `inner` grants and records every time it is
/// consulted, by whom.
struct Watching {
    inner: SpecPolicy,
    consulted: Mutex<Vec<String>>,
}

impl Watching {
    fn new(specs: impl IntoIterator<Item = AgentSpec>) -> Self {
        Self {
            inner: SpecPolicy::new(specs),
            consulted: Mutex::new(Vec::new()),
        }
    }

    /// The root runs as `root`.
    fn rooted(mut self, root: AgentSpec) -> Self {
        self.inner = self.inner.with_root(Some(root));
        self
    }

    fn consulted(&self) -> Vec<String> {
        self.consulted.lock().unwrap().clone()
    }
}

impl CallPolicy for Watching {
    fn may_call(&self, caller: &str, callee: &str) -> Result<(), Refusal> {
        self.consulted
            .lock()
            .unwrap()
            .push(format!("{caller} -> {callee}"));
        self.inner.may_call(caller, callee)
    }

    fn root_may_call(&self, callee: &str) -> Result<(), Refusal> {
        self.consulted
            .lock()
            .unwrap()
            .push(format!("root -> {callee}"));
        self.inner.root_may_call(callee)
    }

    fn root_spec(&self) -> Option<&str> {
        self.inner.root_spec()
    }

    fn budget(&self, callee: &str) -> Remaining {
        self.consulted
            .lock()
            .unwrap()
            .push(format!("budget {callee}"));
        self.inner.budget(callee)
    }
}

/// A spec that may call anyone.
fn delegating(name: &str) -> AgentSpec {
    let mut spec = AgentSpec::named(name);
    spec.swarm.delegates_to = vec!["*".to_string()];
    spec
}

/// A spec only `callers` may call.
fn called_by(name: &str, callers: &[&str]) -> AgentSpec {
    let mut spec = AgentSpec::named(name);
    spec.swarm.callers = Some(callers.iter().map(|c| c.to_string()).collect());
    spec
}

fn invoke(agent: &str, run: Option<&str>) -> InvokeAgentParams {
    InvokeAgentParams {
        agent: agent.to_string(),
        prompt: "go".to_string(),
        handle: None,
        include_trace: false,
        spawn_context: None,
        remaining: Remaining::default(),
        run: run.map(str::to_string),
    }
}

fn run_id(n: u64) -> RunId {
    serde_json::from_value(json!(n)).expect("a run id")
}

/// A snapshot whose `agent.invoke` reaches the registered `spec`'s node.
fn reaching<'a>(policy: &'a dyn CallPolicy, spec: &str) -> Result<Snapshot<'a>, Unreadable> {
    Ok(Snapshot {
        policy,
        now: SystemTime::now(),
        root_task_id: "t-1".to_string(),
        callee: Callee::Node {
            name: format!("{spec}-0"),
            spec: spec.to_string(),
        },
        owner: Owner::None,
        addressee: Addressee::None,
        binding: None,
        answer_nonce: None,
        handle: HandleCheck::None,
    })
}

/// The node `name`, admitted as `spec`, serving one run named `run` at
/// `root → spec`.
fn node(name: &str, spec: &str, run: &str) -> Caller {
    Caller::Node(NodeCaller {
        name: name.to_string(),
        spec: spec.to_string(),
        runs: vec![OpenRun {
            name: run.to_string(),
            id: run_id(7),
            chain: CallChain::root("t-node").extend(spec).unwrap(),
        }],
    })
}

/// Every caller from outside the tree a GT-0 broker could be handed.
fn outsiders() -> [Caller; 4] {
    [
        Caller::External(Admitter::Chainless),
        Caller::External(Admitter::LaunchToken),
        Caller::External(Admitter::Key {
            id: "k-1".to_string(),
            owner: "alice".to_string(),
        }),
        Caller::Remote(RemoteCaller {
            peer: "stream-1".to_string(),
        }),
    ]
}

/// ADR-0023 § 2: `Remote` and `External` callers are never looked up or
/// glob-matched as specs, `*` included. A policy whose callee admits `*`,
/// a pattern matching `external:launch`, or `root`, grants none of them an
/// `agent.invoke` — and is never even consulted for one.
#[test]
fn remote_and_external_never_match_glob() {
    for callers in [
        &["*"][..],
        &["external:*"],
        &["external:launch"],
        &["root"],
        &["*:*"],
    ] {
        let policy = Watching::new([called_by("target", callers)]);
        for caller in outsiders() {
            let params = invoke("target", Some("task-1"));
            let decision = decide(
                &caller,
                &Request::Invoke(&params),
                &reaching(&policy, "target"),
            );
            assert!(
                matches!(decision.outcome, Err(Refused::Caller(_))),
                "{caller} with callers {callers:?}: {:?}",
                decision.outcome
            );
            assert_eq!(decision.row.method, Method::Invoke);
        }
        assert_eq!(
            policy.consulted(),
            Vec::<String>::new(),
            "no outsider is matched against callers {callers:?}"
        );
    }

    // The same policy does grant a node whose spec it names, so the
    // refusals above are the caller's kind, not the spec.
    let policy = Watching::new([called_by("target", &["*"]), delegating("lead")]);
    let params = invoke("target", Some("task-1"));
    let decision = decide(
        &node("lead-0", "lead", "task-1"),
        &Request::Invoke(&params),
        &reaching(&policy, "target"),
    );
    assert!(matches!(decision.outcome, Ok(Grant::Invoke { .. })));
    assert_eq!(policy.consulted(), ["lead -> target", "budget target"]);
}

/// ADR-0023 § 3: outsiders get no row but a refusal, whatever they ask.
#[test]
fn outsiders_are_refused_every_request() {
    let policy = Watching::new([]);
    let post = SendMessageParams {
        to: ROOT_NAME.to_string(),
        text: "hi".to_string(),
        run: None,
    };
    let params = invoke("target", None);
    for caller in outsiders() {
        for request in [
            Request::Invoke(&params),
            Request::List,
            Request::Post(&post),
            Request::Answer {
                id: "question-1",
                nonce: None,
            },
            Request::Cancel { node: "n" },
            Request::TakeRunMessages,
            Request::Take,
            Request::DropHandle {
                name: "fix",
                run: None,
            },
            Request::TaskGet { id: "task-1" },
        ] {
            let decision = decide(&caller, &request, &reaching(&policy, "target"));
            assert!(decision.outcome.is_err(), "{caller} {request:?}");
        }
    }
    assert!(policy.consulted().is_empty());
}

/// ADR-0023 § 7: a snapshot that does not add up refuses, before any
/// effect, as a distinct `internal` refusal — for every caller and
/// request, the root's included.
#[test]
fn gate_error_refuses() {
    let unreadable: Result<Snapshot<'_>, Unreadable> = Err(Unreadable(
        "participant 'x-0' has no admitted node".to_string(),
    ));
    let params = invoke("x-0", Some("task-1"));
    for caller in [Caller::Root, node("lead-0", "lead", "task-1")] {
        for request in [
            Request::Invoke(&params),
            Request::List,
            Request::TakeRunMessages,
        ] {
            let decision = decide(&caller, &request, &unreadable);
            let Err(refused) = decision.outcome else {
                panic!("{caller} {request:?} was granted over an unreadable snapshot");
            };
            assert_eq!(
                refused,
                Refused::Internal("participant 'x-0' has no admitted node".to_string())
            );
            assert_eq!(
                refused.to_call_error(),
                CallError::Refused("internal: participant 'x-0' has no admitted node".to_string())
            );
        }
    }
}

/// GT-0b at the gate: a node's `agent.invoke` is decided under the run it
/// names, which must be one of its own open runs.
#[test]
fn a_node_calls_from_the_run_it_names() {
    let policy = Watching::new([delegating("lead")]);
    let caller = node("lead-0", "lead", "task-1");
    for (run, granted) in [
        (Some("task-1"), true),
        (Some("task-9"), false),
        (None, false),
    ] {
        let params = invoke("coder", run);
        let decision = decide(
            &caller,
            &Request::Invoke(&params),
            &reaching(&policy, "coder"),
        );
        match (decision.outcome, granted) {
            (Ok(Grant::Invoke { stamp, target }), true) => {
                assert_eq!(stamp.from_run, Some(run_id(7)));
                assert_eq!(stamp.chain.chain, ["root", "lead", "coder"]);
                assert_eq!(
                    target,
                    InvokeTarget::Submit {
                        node: "coder-0".to_string()
                    }
                );
            }
            (Err(Refused::Caller(_)), false) => {}
            (outcome, _) => panic!("{run:?}: {outcome:?}"),
        }
    }
}

/// The local root's own requests are the root's alone (ADR-0023 § 3).
#[test]
fn only_the_root_answers_cancels_and_takes_its_messages() {
    let policy = Watching::new([]);
    let snapshot = reaching(&policy, "x");
    for request in [
        Request::Answer {
            id: "question-1",
            nonce: None,
        },
        Request::Cancel { node: "n" },
        Request::TakeRunMessages,
    ] {
        assert!(decide(&Caller::Root, &request, &snapshot).outcome.is_ok());
        assert!(matches!(
            decide(&node("lead-0", "lead", "task-1"), &request, &snapshot).outcome,
            Err(Refused::Caller(_))
        ));
    }
    // And a name no node goes by is not on the tree, for the root too.
    let post = SendMessageParams {
        to: "anyone".to_string(),
        text: "x".to_string(),
        run: None,
    };
    let refused = decide(&Caller::Root, &Request::Post(&post), &snapshot).outcome;
    assert_eq!(refused, Err(Refused::Message(RefusalReason::NotOnTree)));
    assert_eq!(
        serde_json::to_value(MessageStatus::Refused {
            reason: RefusalReason::NotOnTree
        })
        .unwrap()["reason"],
        "not_on_tree"
    );
}

/// EN-2a's `human.approve` under the gate (ADR-0023 § 3): a node raises one
/// from the task it serves, whose run's chain stamps the asker; one serving
/// two tasks is refused rather than guessed for; the root only answers;
/// outsiders get nothing.
#[test]
fn human_approve_is_raised_from_the_run_a_node_serves() {
    let policy = Watching::new([]);
    let snapshot = reaching(&policy, "x");
    let request = ApprovalRequest {
        kind: chatty_fabric::ApprovalKind::Exec,
        command_or_path: "ls".to_string(),
        diff_stat: None,
        asker: None,
    };

    let decision = decide(
        &node("lead-0", "lead", "task-1"),
        &Request::Approve(&request),
        &snapshot,
    );
    assert_eq!(decision.row.to_string(), "node/human.approve");
    assert_eq!(
        decision.outcome,
        Ok(Grant::Approve {
            chain: CallChain::root("t-node").extend("lead").unwrap()
        })
    );

    let Caller::Node(mut busy) = node("lead-0", "lead", "task-1") else {
        unreachable!()
    };
    busy.runs.push(OpenRun {
        name: "task-2".to_string(),
        id: run_id(8),
        chain: CallChain::root("t-other").extend("lead").unwrap(),
    });
    assert!(matches!(
        decide(&Caller::Node(busy), &Request::Approve(&request), &snapshot).outcome,
        Err(Refused::Caller(_))
    ));

    assert!(
        decide(&Caller::Root, &Request::Approve(&request), &snapshot)
            .outcome
            .is_err()
    );
    assert_eq!(
        decide(
            &Caller::Root,
            &Request::AnswerApproval {
                id: "approval-1",
                nonce: None
            },
            &snapshot
        )
        .outcome,
        Ok(Grant::AnswerApproval)
    );
    assert!(
        decide(
            &node("lead-0", "lead", "task-1"),
            &Request::AnswerApproval {
                id: "approval-1",
                nonce: None
            },
            &snapshot
        )
        .outcome
        .is_err()
    );
    for caller in outsiders() {
        assert!(
            decide(&caller, &Request::Approve(&request), &snapshot)
                .outcome
                .is_err()
        );
    }
    assert!(policy.consulted().is_empty(), "approvals consult no spec");
}

/// EN-2b's `human.ask` under the gate (ADR-0023 § 3), mirroring
/// `human.approve`: a node raises one from the task it serves, whose run's
/// chain stamps the asker; one serving two tasks is refused; the root only
/// answers; outsiders get nothing.
#[test]
fn human_ask_is_raised_from_the_run_a_node_serves() {
    let policy = Watching::new([]);
    let snapshot = reaching(&policy, "x");
    let request = chatty_fabric::AskRequest {
        questions: Vec::new(),
        asker: None,
        origin: None,
    };

    let decision = decide(
        &node("lead-0", "lead", "task-1"),
        &Request::Ask(&request),
        &snapshot,
    );
    assert_eq!(decision.row.to_string(), "node/human.ask");
    assert_eq!(
        decision.outcome,
        Ok(Grant::Ask {
            chain: CallChain::root("t-node").extend("lead").unwrap()
        })
    );

    let Caller::Node(mut busy) = node("lead-0", "lead", "task-1") else {
        unreachable!()
    };
    busy.runs.push(OpenRun {
        name: "task-2".to_string(),
        id: run_id(8),
        chain: CallChain::root("t-other").extend("lead").unwrap(),
    });
    assert!(matches!(
        decide(&Caller::Node(busy), &Request::Ask(&request), &snapshot).outcome,
        Err(Refused::Caller(_))
    ));

    assert!(
        decide(&Caller::Root, &Request::Ask(&request), &snapshot)
            .outcome
            .is_err()
    );
    assert_eq!(
        decide(
            &Caller::Root,
            &Request::Answer {
                id: "question-1",
                nonce: None
            },
            &snapshot
        )
        .outcome,
        Ok(Grant::Answer)
    );
    for caller in outsiders() {
        assert!(
            decide(&caller, &Request::Ask(&request), &snapshot)
                .outcome
                .is_err()
        );
    }
    assert!(policy.consulted().is_empty(), "questions consult no spec");
}

/// HS-4a: the hosted client's rows. Bound to the broker's `(tenant, user)`;
/// answers need their nonce; conversations are its rows alone; a local
/// root on a hosted broker is refused; on a typed root only cancel and read
/// pass.
#[test]
fn hosted_client_rows() {
    let policy = crate::participant::LocalPermissive::new();
    let binding = Binding {
        tenant: "acme".to_string(),
        user: "ada".to_string(),
        root: BrokerRoot::Hosted,
    };
    let ada = Caller::HostedRoot(HostedClient {
        tenant: "acme".to_string(),
        user: "ada".to_string(),
    });
    let other = Caller::HostedRoot(HostedClient {
        tenant: "evil".to_string(),
        user: "ada".to_string(),
    });
    fn bound<'a>(
        policy: &'a dyn CallPolicy,
        binding: &'a Binding,
        nonce: Option<&str>,
    ) -> Result<Snapshot<'a>, Unreadable> {
        reaching(policy, "analyst").map(|snapshot| Snapshot {
            binding: Some(binding),
            answer_nonce: nonce.map(str::to_string),
            handle: HandleCheck::None,
            ..snapshot
        })
    }
    let snapshot = bound(&policy, &binding, Some("n-1"));
    let granted =
        |caller: &Caller, request: &Request<'_>| decide(caller, request, &snapshot).outcome.is_ok();

    let answer = Request::Answer {
        id: "question-1",
        nonce: Some("n-1"),
    };
    let conversation = Request::Conversation(ConversationOp::Create);
    for request in [
        Request::List,
        Request::Cancel { node: "analyst-0" },
        Request::TakeRunMessages,
        answer,
        conversation,
    ] {
        assert!(granted(&ada, &request), "{request:?}");
        assert!(!granted(&other, &request), "cross-tenant {request:?}");
        assert!(!granted(&Caller::Root, &request), "local root {request:?}");
    }
    for nonce in [None, Some("n-2")] {
        assert!(!granted(
            &ada,
            &Request::AnswerApproval {
                id: "approval-1",
                nonce
            }
        ));
    }
    // Conversations are no local root's row, bound or not.
    assert!(
        decide(&Caller::Root, &conversation, &reaching(&policy, "analyst"))
            .outcome
            .is_err()
    );

    let typed = Binding {
        root: BrokerRoot::Typed { key_owner: None },
        ..binding.clone()
    };
    let snapshot = bound(&policy, &typed, Some("n-1"));
    let typed_granted = |request: &Request<'_>| decide(&ada, request, &snapshot).outcome.is_ok();
    assert!(typed_granted(&Request::Cancel { node: "analyst-0" }));
    assert!(typed_granted(&Request::TakeRunMessages));
    for request in [Request::List, answer, conversation] {
        assert!(!typed_granted(&request), "typed root {request:?}");
    }
}

fn node_id(n: u64) -> chatty_fabric::NodeId {
    serde_json::from_value(json!(n)).expect("a node id")
}

/// TM-5's rows: the human (the root) may message any live node mid-run, a
/// node its own live child mid-run and its owner at TM-2's points; a
/// sibling is still not on the tree, an ended node is `recipient_ended`,
/// and only a node takes mid-run messages.
#[test]
fn mid_run_message_rows() {
    let policy = Watching::new([]);
    let to = |name: &str| SendMessageParams {
        to: name.to_string(),
        text: "look at X instead".to_string(),
        run: None,
    };
    let with = |owner: Owner, addressee: Addressee| {
        reaching(&policy, "target").map(|snapshot| Snapshot {
            owner,
            addressee,
            ..snapshot
        })
    };
    let child = |owner: Option<&str>, ended: bool| Addressee::Node {
        id: node_id(7),
        owner: owner.map(str::to_string),
        ended,
    };
    let mid_run = Ok(Grant::Post {
        to: PostTo::Node(node_id(7)),
        at: Delivery::ToolRound,
    });
    let lead = node("lead-0", "lead", "task-1");

    // The human, to any live node — its own child or a grandchild alike.
    for addressee in [child(None, false), child(Some("lead-0"), false)] {
        let snapshot = with(Owner::None, addressee);
        assert_eq!(
            decide(&Caller::Root, &Request::Post(&to("coder-1")), &snapshot).outcome,
            mid_run
        );
    }
    assert_eq!(
        decide(
            &Caller::Root,
            &Request::Post(&to("coder-1")),
            &with(Owner::None, child(None, true))
        )
        .outcome,
        Err(Refused::Message(RefusalReason::RecipientEnded))
    );

    // A node, to its own child: mid-run.
    let snapshot = with(Owner::Root, child(Some("lead-0"), false));
    assert_eq!(
        decide(&lead, &Request::Post(&to("coder-1")), &snapshot).outcome,
        mid_run
    );
    // ... to its owner: TM-2's points, as before.
    assert_eq!(
        decide(&lead, &Request::Post(&to(ROOT_NAME)), &snapshot).outcome,
        Ok(Grant::Post {
            to: PostTo::Root,
            at: Delivery::Run,
        })
    );
    // ... to a node someone else owns (a sibling, a grandchild): refused.
    let snapshot = with(Owner::Root, child(Some("lead-1"), false));
    assert_eq!(
        decide(&lead, &Request::Post(&to("coder-1")), &snapshot).outcome,
        Err(Refused::Message(RefusalReason::NotOnTree))
    );

    // Only a node takes mid-run messages; the root reads its own at its
    // next run.
    let snapshot = reaching(&policy, "target");
    assert_eq!(
        decide(&lead, &Request::Take, &snapshot).outcome,
        Ok(Grant::Take)
    );
    assert!(matches!(
        decide(&Caller::Root, &Request::Take, &snapshot).outcome,
        Err(Refused::Caller(_))
    ));
    assert!(
        policy.consulted().is_empty(),
        "no spec rule decides a message"
    );
}

/// TM-4: a node's message to its own idle handle is granted at TM-2's
/// run delivery; the human has no handles to message.
#[test]
fn handle_message_rows() {
    let policy = Watching::new([]);
    let snapshot = reaching(&policy, "target").map(|snapshot| Snapshot {
        addressee: Addressee::Handle,
        ..snapshot
    });
    let to = SendMessageParams {
        to: "fixer".to_string(),
        text: "use the second approach".to_string(),
        run: None,
    };
    assert_eq!(
        decide(
            &node("lead-0", "lead", "task-1"),
            &Request::Post(&to),
            &snapshot
        )
        .outcome,
        Ok(Grant::Post {
            to: PostTo::Handle,
            at: Delivery::Run,
        })
    );
    assert_eq!(
        decide(&Caller::Root, &Request::Post(&to), &snapshot).outcome,
        Err(Refused::Message(RefusalReason::NotOnTree))
    );
}

/// AGE-1138: a root that runs as spec `lead` has `lead` on its chain, so a
/// member allowed to call `lead` is refused as a cycle, not given a second
/// leader. A plain root has no such spec and the call is granted.
#[test]
fn the_roots_own_spec_is_a_cycle_for_a_member() {
    let member = |root_spec: Option<&str>| {
        Caller::Node(NodeCaller {
            name: "a-0".to_string(),
            spec: "a".to_string(),
            runs: vec![OpenRun {
                name: "task-1".to_string(),
                id: run_id(7),
                chain: CallChain::root_as("t-node", root_spec).extend("a").unwrap(),
            }],
        })
    };
    let params = invoke("lead", Some("task-1"));

    let policy = Watching::new([delegating("a")]).rooted(delegating("lead"));
    let decision = decide(
        &member(Some("lead")),
        &Request::Invoke(&params),
        &reaching(&policy, "lead"),
    );
    assert_eq!(
        decision.outcome,
        Err(Refused::Delegation(Refusal::Cycle {
            chain: vec!["root".to_string(), "a".to_string()],
            callee: "lead".to_string(),
        }))
    );

    // The root itself asks for its own spec: the same refusal.
    let decision = decide(
        &Caller::Root,
        &Request::Invoke(&invoke("lead", None)),
        &reaching(&policy, "lead"),
    );
    assert!(matches!(
        decision.outcome,
        Err(Refused::Delegation(Refusal::Cycle { .. }))
    ));

    // A plain root: nothing is on the chain but the root and the member.
    let plain = Watching::new([delegating("a"), delegating("lead")]);
    let decision = decide(
        &member(None),
        &Request::Invoke(&params),
        &reaching(&plain, "lead"),
    );
    assert!(matches!(decision.outcome, Ok(Grant::Invoke { .. })));
}

/// `agent.invoke` naming the handle `name` as `party`, in a run rooted at
/// `root`, against a handle in `status`.
fn handle_call(
    caller: &Caller,
    party: HandleParty,
    root: RunRoot,
    status: HandleStatus,
) -> Result<Grant, Refused> {
    let policy = Watching::new([delegating("lead"), delegating("worker")]);
    let mut snapshot = reaching(&policy, "worker").unwrap();
    snapshot.handle = HandleCheck::Named {
        name: "fix".to_string(),
        party,
        root,
        status,
    };
    let params = InvokeAgentParams {
        handle: Some("fix".to_string()),
        ..invoke("worker", Some("r-1"))
    };
    decide(caller, &Request::Invoke(&params), &Ok(snapshot)).outcome
}

fn owned_by(root: RunRoot, owner: HandleParty, ancestors: Vec<HandleParty>) -> HandleStatus {
    HandleStatus::Live(chatty_fabric::HandleOwner::new(root, owner, ancestors))
}

/// Invariant 5 at the gate (`handle_owner_rule`): the owner and its
/// ancestors task a live handle; anyone else is `not_owner`, and an ended
/// handle is `handle_expired` for everyone.
#[test]
fn handle_owner_rule_is_a_decide_row() {
    let lead = node("lead-0", "lead", "r-1");
    let sibling = node("coder-3", "lead", "r-1");
    let status = || {
        owned_by(
            RunRoot::Owner,
            HandleParty::Node("lead-0".into()),
            vec![HandleParty::Root],
        )
    };
    let owner = handle_call(
        &lead,
        HandleParty::Node("lead-0".into()),
        RunRoot::Owner,
        status(),
    );
    assert!(matches!(owner, Ok(Grant::Invoke { .. })), "{owner:?}");
    let ancestor = handle_call(&Caller::Root, HandleParty::Root, RunRoot::Owner, status());
    assert!(matches!(ancestor, Ok(Grant::Invoke { .. })), "{ancestor:?}");
    let other = handle_call(
        &sibling,
        HandleParty::Node("coder-3".into()),
        RunRoot::Owner,
        status(),
    );
    assert_eq!(
        other.unwrap_err().to_call_error(),
        CallError::Handle(HandleError::NotOwner {
            name: "fix".into(),
            caller: "node 'coder-3'".into(),
            owner: "node 'lead-0'".into(),
        })
    );
    let ended = handle_call(
        &Caller::Root,
        HandleParty::Root,
        RunRoot::Owner,
        HandleStatus::Ended,
    );
    assert_eq!(
        ended.unwrap_err().to_call_error(),
        CallError::Handle(HandleError::Expired { name: "fix".into() })
    );
    // A free name starts a handle, for anyone the other rows admit.
    let free = handle_call(
        &sibling,
        HandleParty::Node("coder-3".into()),
        RunRoot::Owner,
        HandleStatus::Free,
    );
    assert!(matches!(free, Ok(Grant::Invoke { .. })), "{free:?}");
}

/// ADR-0023 § 3 "Resume handles": a handle's owner includes its run's root
/// kind and key. An `External`-rooted run resumes only its own key's
/// handles, and an owner-rooted run never resumes an `External`-rooted
/// one, even as the root itself. GT-3 has not landed, so no broker opens an
/// `External`-rooted run yet; these are the row on its own, over a node
/// whose run the snapshot says is `External`-rooted.
#[test]
fn an_external_rooted_run_resumes_only_its_key_s_handles() {
    let key = |id: &str| RunRoot::External {
        key: id.to_string(),
    };
    let lead = node("lead-0", "lead", "r-1");
    let party = || HandleParty::Node("lead-0".into());
    let k1_handle = || owned_by(key("k1"), party(), vec![]);

    let same_key = handle_call(&lead, party(), key("k1"), k1_handle());
    assert!(matches!(same_key, Ok(Grant::Invoke { .. })), "{same_key:?}");
    let other_key = handle_call(&lead, party(), key("k2"), k1_handle());
    assert!(
        matches!(
            other_key,
            Err(Refused::Handle(HandleError::NotOwner { .. }))
        ),
        "{other_key:?}"
    );
    // An external key's run cannot reach the human's handles either.
    let owners = handle_call(
        &lead,
        party(),
        key("k1"),
        owned_by(RunRoot::Owner, party(), vec![]),
    );
    assert!(
        matches!(owners, Err(Refused::Handle(HandleError::NotOwner { .. }))),
        "{owners:?}"
    );
}

#[test]
fn an_owner_rooted_run_cannot_resume_an_external_rooted_handle() {
    let external = || {
        owned_by(
            RunRoot::External { key: "k1".into() },
            HandleParty::Node("lead-0".into()),
            vec![],
        )
    };
    let lead = node("lead-0", "lead", "r-1");
    let same_name = handle_call(
        &lead,
        HandleParty::Node("lead-0".into()),
        RunRoot::Owner,
        external(),
    );
    assert!(
        matches!(
            same_name,
            Err(Refused::Handle(HandleError::NotOwner { .. }))
        ),
        "{same_name:?}"
    );
    // Not even the root, the ancestor of every owner-rooted handle.
    let root = handle_call(&Caller::Root, HandleParty::Root, RunRoot::Owner, external());
    assert!(
        matches!(root, Err(Refused::Handle(HandleError::NotOwner { .. }))),
        "{root:?}"
    );
}

/// `handle.drop`: the owner and its ancestors only, a live handle only, and
/// never from outside the tree.
#[test]
fn only_the_owner_line_drops_a_handle() {
    let policy = Watching::new([]);
    let snapshot = |party: HandleParty, status: HandleStatus| {
        let mut snapshot = reaching(&policy, "worker").unwrap();
        snapshot.callee = Callee::None;
        snapshot.handle = HandleCheck::Named {
            name: "fix".into(),
            party,
            root: RunRoot::Owner,
            status,
        };
        Ok(snapshot)
    };
    let live = || owned_by(RunRoot::Owner, HandleParty::Root, vec![]);
    let granted = decide(
        &Caller::Root,
        &Request::DropHandle {
            name: "fix",
            run: None,
        },
        &snapshot(HandleParty::Root, live()),
    );
    assert_eq!(granted.outcome, Ok(Grant::DropHandle));
    assert_eq!(granted.row.method.as_str(), "handle.drop");
    let node_drop = decide(
        &node("coder-0", "worker", "r-1"),
        &Request::DropHandle {
            name: "fix",
            run: Some("r-1"),
        },
        &snapshot(HandleParty::Node("coder-0".into()), live()),
    );
    assert!(matches!(
        node_drop.outcome,
        Err(Refused::Handle(HandleError::NotOwner { .. }))
    ));
    // A node drops from a run it serves, as it calls from one (GT-0b).
    let owner_runless = decide(
        &node("coder-0", "worker", "r-1"),
        &Request::DropHandle {
            name: "fix",
            run: None,
        },
        &snapshot(
            HandleParty::Node("coder-0".into()),
            owned_by(RunRoot::Owner, HandleParty::Node("coder-0".into()), vec![]),
        ),
    );
    assert!(matches!(owner_runless.outcome, Err(Refused::Caller(_))));
    let free = decide(
        &Caller::Root,
        &Request::DropHandle {
            name: "fix",
            run: None,
        },
        &snapshot(HandleParty::Root, HandleStatus::Free),
    );
    assert!(matches!(free.outcome, Err(Refused::Caller(_))));
    for outsider in outsiders() {
        let refused = decide(
            &outsider,
            &Request::DropHandle {
                name: "fix",
                run: None,
            },
            &snapshot(HandleParty::Root, live()),
        );
        assert!(
            matches!(refused.outcome, Err(Refused::Caller(_))),
            "{outsider}"
        );
    }
}
