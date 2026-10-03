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
