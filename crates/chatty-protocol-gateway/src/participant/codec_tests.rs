//! The v3 envelope, line by line (EN-1). Connection-level behaviour — what
//! closes a connection and what does not — is in
//! `tests/participant_socket.rs`.

use super::*;
use chatty_core::services::a2a_client::{
    CONVERSATION_METADATA_KEY, CONVERSATION_TOO_LARGE_METADATA_KEY, TRACE_METADATA_KEY,
    USAGE_METADATA_KEY,
};
use chatty_core::services::handoff::{
    HANDOFF_INVALID_COUNT_METADATA_KEY, HANDOFF_INVALID_METADATA_KEY, HANDOFF_METADATA_KEY,
};
use chatty_fabric::wire::{
    HandoffInvalid, Opaque, TaskIdentity, TaskMetadata, WireModelRef, WireProgress, WireUsage,
    WireUsageLine, WorkerSwarmItem,
};
use chatty_fabric::{
    Answer, AskReply, AskRequest, Asker, ConversationScope, InvokeAgentParams, NodeName, Question,
    Remaining, SendMessageParams,
};
use serde_json::{Value, json};

fn value(line: &str) -> Value {
    serde_json::from_str(line).expect("a line is JSON")
}

/// Nothing outside the fabric makes a name; the wire is where one comes from.
fn node(name: &str) -> NodeName {
    serde_json::from_value(json!(name)).unwrap()
}

fn task(task_id: &str) -> BrokerFrame {
    BrokerFrame::Task {
        task_id: task_id.into(),
        text: "do it".into(),
        identity: None,
        capture_conversation: false,
        spawn_context: None,
        handoff: None,
        budget: Box::default(),
        swarm_events: false,
    }
}

/// A broker and a worker codec that have exchanged a hello and a welcome,
/// and one `task.run` for `task-1`.
fn connected() -> (BrokerCodec, WorkerCodec) {
    let broker = BrokerCodec::new();
    let worker = WorkerCodec::new();
    let hello = worker
        .encode(&ParticipantFrame::Hello {
            card: ParticipantCard::default(),
            schema: chatty_fabric::wire::schema::hash().to_string(),
        })
        .unwrap()
        .unwrap();
    assert!(matches!(
        broker.decode(&hello).unwrap(),
        Some(ParticipantFrame::Hello { .. })
    ));
    let welcome = broker
        .encode(&BrokerFrame::Welcome {
            name: node("coder-0"),
            scope: ConversationScope::new("root"),
            owner: None,
        })
        .unwrap()
        .unwrap();
    assert!(matches!(
        worker.decode(&welcome).unwrap(),
        Some(BrokerFrame::Welcome { .. })
    ));
    let run = broker.encode(&task("task-1")).unwrap().unwrap();
    assert!(matches!(
        worker.decode(&run).unwrap(),
        Some(BrokerFrame::Task { .. })
    ));
    (broker, worker)
}

/// Send `frame` from the worker to the broker.
fn up(worker: &WorkerCodec, broker: &BrokerCodec, frame: ParticipantFrame) -> ParticipantFrame {
    let line = worker.encode(&frame).unwrap().expect("a line");
    broker.decode(&line).unwrap().expect("a frame")
}

/// Send `frame` from the broker to the worker.
fn down(broker: &BrokerCodec, worker: &WorkerCodec, frame: BrokerFrame) -> BrokerFrame {
    let line = broker.encode(&frame).unwrap().expect("a line");
    worker.decode(&line).unwrap().expect("a frame")
}

#[test]
fn a_session_uses_the_documented_envelopes() {
    let broker = BrokerCodec::new();
    let worker = WorkerCodec::new();
    let hello = worker
        .encode(&ParticipantFrame::Hello {
            card: ParticipantCard::default(),
            schema: chatty_fabric::wire::schema::hash().to_string(),
        })
        .unwrap()
        .unwrap();
    let hello = value(&hello);
    assert_eq!(hello["v"], 3);
    assert_eq!(hello["id"], 1);
    assert_eq!(hello["method"], "session.hello");
    broker.decode(&hello.to_string()).unwrap();

    let welcome = broker
        .encode(&BrokerFrame::Welcome {
            name: node("local-coder-0"),
            scope: ConversationScope::new("root"),
            owner: None,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&welcome),
        json!({"v": 3, "id": 1, "result": {"name": "local-coder-0", "scope": "root", "owner": null}})
    );
    let Some(BrokerFrame::Welcome { name, scope, owner }) = worker.decode(&welcome).unwrap() else {
        panic!("a welcome");
    };
    assert_eq!(name.as_str(), "local-coder-0");
    assert_eq!(scope.as_str(), "root");
    assert!(owner.is_none());

    // The broker numbers its own requests: its first task.run is 1 too.
    let run = broker.encode(&task("task-1")).unwrap().unwrap();
    assert_eq!(
        value(&run),
        json!({"v": 3, "id": 1, "method": "task.run", "params": {"taskId": "task-1", "text": "do it"}})
    );
    worker.decode(&run).unwrap();

    let working = worker
        .encode(&ParticipantFrame::Status {
            task_id: "task-1".into(),
            state: TaskState::Working,
            message: Some("read_file".into()),
            metadata: None,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&working),
        json!({"v": 3, "method": "task.event",
               "params": {"kind": "status", "id": 1, "state": "working", "message": "read_file"}})
    );
    let artifact = worker
        .encode(&ParticipantFrame::Artifact {
            task_id: "task-1".into(),
            text: "foo".into(),
            last_chunk: false,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&artifact),
        json!({"v": 3, "method": "task.event",
               "params": {"kind": "artifact", "id": 1, "text": "foo", "lastChunk": false}})
    );
    let done = worker
        .encode(&ParticipantFrame::Status {
            task_id: "task-1".into(),
            state: TaskState::Completed,
            message: None,
            metadata: Some(TaskMetadata {
                trace: Some("### read_file (ok)".into()),
                ..TaskMetadata::default()
            }),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&done),
        json!({"v": 3, "id": 1, "result": {"state": "completed", "metadata": {"trace": "### read_file (ok)"}}})
    );
    for line in [&working, &artifact] {
        broker.decode(line).unwrap().expect("a frame");
    }
    let Some(ParticipantFrame::Status {
        task_id,
        state,
        metadata,
        ..
    }) = broker.decode(&done).unwrap()
    else {
        panic!("the terminal status");
    };
    assert_eq!(task_id, "task-1", "the result names its task.run");
    assert_eq!(state, TaskState::Completed);
    assert_eq!(
        metadata.unwrap().trace.as_deref(),
        Some("### read_file (ok)")
    );

    // The task is over: a late event for it is dropped, not fatal.
    assert!(broker.decode(&working).unwrap().is_none());
}

fn the_question() -> AskRequest {
    AskRequest {
        questions: vec![Question {
            id: "q1".into(),
            question: "Which database?".into(),
            options: vec!["Postgres".into(), "SQLite".into()],
        }],
        asker: None,
        origin: None,
    }
}

/// EN-2b: a worker's question is a `human.ask` request and its answers
/// that request's result; a task cannot park on an `input-required`
/// status any more.
#[test]
fn a_question_is_a_human_ask_request_and_its_answers_its_result() {
    let (broker, worker) = connected();
    let line = worker
        .encode(&ParticipantFrame::Ask {
            id: 4,
            request: the_question(),
        })
        .unwrap()
        .unwrap();
    let json = value(&line);
    assert_eq!(json["method"], "human.ask");
    assert_eq!(json["params"]["questions"][0]["options"][1], "SQLite");
    let request_id = json["id"].as_u64().unwrap();
    let Some(ParticipantFrame::Ask { id, request }) = broker.decode(&line).unwrap() else {
        panic!("a question");
    };
    assert_eq!((id, request), (request_id, the_question()));

    let answers = vec![Answer {
        id: "q1".into(),
        answer: "Postgres".into(),
        custom: false,
    }];
    let line = broker
        .encode(&BrokerFrame::Answer {
            id: request_id,
            answers: Ok(answers.clone()),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "id": request_id, "result": [{"id": "q1", "answer": "Postgres", "custom": false}]})
    );
    let Some(BrokerFrame::Answer { id, answers: read }) = worker.decode(&line).unwrap() else {
        panic!("the answers");
    };
    assert_eq!((id, read), (4, Ok(answers)));

    let parked = ParticipantFrame::Status {
        task_id: "task-1".into(),
        state: TaskState::InputRequired,
        message: None,
        metadata: None,
    };
    assert!(matches!(
        worker.encode(&parked),
        Err(FrameError::Malformed(_))
    ));
}

/// EN-2b: the broker relays a callee's question to its caller as its own
/// `human.ask`, naming its id for the question; the caller's result is the
/// answers or `escalate`, and a `req.cancel` withdraws it.
#[test]
fn a_relayed_question_is_a_broker_request_answered_or_escalated() {
    let (broker, worker) = connected();
    let mut stamped = the_question();
    stamped.asker = Some(Asker {
        agent: "leaf-0".into(),
        chain: vec!["mid".into(), "leaf".into()],
    });
    let relay = |question: &str| {
        broker
            .encode(&BrokerFrame::Ask {
                question: question.into(),
                request: stamped.clone(),
            })
            .unwrap()
            .unwrap()
    };
    let line = relay("question-1");
    let json = value(&line);
    assert_eq!(json["method"], "human.ask");
    assert_eq!(json["params"]["question"], "question-1");
    assert_eq!(json["params"]["request"]["asker"]["agent"], "leaf-0");
    let Some(BrokerFrame::Ask { question, request }) = worker.decode(&line).unwrap() else {
        panic!("a relayed question");
    };
    assert_eq!((question.as_str(), &request), ("question-1", &stamped));

    let line = worker
        .encode(&ParticipantFrame::AskReply {
            question: "question-1".into(),
            reply: AskReply::Escalate,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "id": json["id"], "result": "escalate"})
    );
    assert!(matches!(
        broker.decode(&line).unwrap(),
        Some(ParticipantFrame::AskReply { question, reply: AskReply::Escalate }) if question == "question-1"
    ));

    // Withdrawn: the worker hears a req.cancel naming it, and its late
    // reply is not sent.
    let line = relay("question-2");
    let id = value(&line)["id"].clone();
    worker.decode(&line).unwrap();
    let cancel = broker
        .encode(&BrokerFrame::CancelAsk {
            question: "question-2".into(),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&cancel),
        json!({"v": 3, "method": "req.cancel", "params": {"id": id}})
    );
    assert!(matches!(
        worker.decode(&cancel).unwrap(),
        Some(BrokerFrame::CancelAsk { question }) if question == "question-2"
    ));
    assert!(
        worker
            .encode(&ParticipantFrame::AskReply {
                question: "question-2".into(),
                reply: AskReply::Escalate,
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_task_run_carries_its_fields_and_omits_the_defaults() {
    let broker = BrokerCodec::new();
    let worker = WorkerCodec::new();
    let budget = Remaining {
        turns: Some(2),
        seconds: Some(30),
        usd: Some(0.02),
    };
    let line = broker
        .encode(&BrokerFrame::Task {
            task_id: "t".into(),
            text: "x".into(),
            identity: Some(TaskIdentity::new("acme", "ada")),
            capture_conversation: true,
            spawn_context: None,
            handoff: None,
            budget: Box::new(budget.clone()),
            swarm_events: true,
        })
        .unwrap()
        .unwrap();
    let json = value(&line);
    assert_eq!(
        json["params"]["identity"],
        json!({"tenant": "acme", "user": "ada"})
    );
    assert_eq!(json["params"]["captureConversation"], true);
    assert_eq!(json["params"]["swarmEvents"], true);
    assert_eq!(
        json["params"]["budget"],
        json!({"turns": 2, "seconds": 30, "usd": 0.02})
    );
    let Some(BrokerFrame::Task {
        identity,
        capture_conversation,
        budget: read,
        swarm_events,
        ..
    }) = worker.decode(&line).unwrap()
    else {
        panic!("a task");
    };
    assert_eq!(identity, Some(TaskIdentity::new("acme", "ada")));
    assert!(capture_conversation && swarm_events);
    assert_eq!(*read, budget);

    let bare = r#"{"v":3,"id":2,"method":"task.run","params":{"taskId":"t2","text":"x"}}"#;
    let Some(BrokerFrame::Task {
        identity,
        capture_conversation,
        budget,
        swarm_events,
        ..
    }) = worker.decode(bare).unwrap()
    else {
        panic!("a task");
    };
    assert!(identity.is_none() && !capture_conversation && !swarm_events);
    assert!(budget.is_unlimited());
}

/// TB-1: a swarm event carries one item and nothing a worker could tag it
/// with; a forged tag, or an item that is not the worker's to report, does
/// not decode (EN-3a).
#[test]
fn a_swarm_event_with_a_forged_tag_does_not_decode() {
    let (broker, _worker) = connected();
    let line = r#"{"v":3,"method":"task.event","params":{"kind":"swarm","id":1,
                   "event":{"kind":"tool_call_started","id":"c1","name":"shell"}}}"#;
    let Some(ParticipantFrame::Event { task_id, event }) = broker.decode(line).unwrap() else {
        panic!("an event");
    };
    assert_eq!(task_id, "task-1");
    assert_eq!(
        event,
        WorkerSwarmItem::ToolCallStarted {
            id: "c1".into(),
            name: "shell".into()
        }
    );
    for forged in [
        r#"{"v":3,"method":"task.event","params":{"kind":"swarm","id":1,"root_task_id":"forged",
            "event":{"kind":"tool_call_started","id":"c1","name":"shell"}}}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"swarm","id":1,
            "event":{"kind":"tool_call_started","id":"c1","name":"shell","node":"root"}}}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"swarm","id":1,
            "event":{"kind":"usage","usage":{"inputTokens":1000000}}}}"#,
    ] {
        assert!(
            matches!(broker.decode(forged), Err(FrameError::Malformed(_))),
            "{forged}"
        );
    }
}

#[test]
fn a_hello_carries_a_card() {
    let broker = BrokerCodec::new();
    let line = r#"{"v":3,"id":1,"method":"session.hello","params":{"card":{"name":"worker-1",
                   "description":"a worker","skills":[{"name":"edit"}]}}}"#;
    let Some(ParticipantFrame::Hello { card, .. }) = broker.decode(line).unwrap() else {
        panic!("a hello");
    };
    assert_eq!(card.name, "worker-1", "carried, and ignored by the broker");
    assert_eq!(card.skills[0].name, "edit");
    assert_eq!(card.version, "");
    assert!(card.display_name.is_none());

    let bare = BrokerCodec::new();
    let Some(ParticipantFrame::Hello { card, .. }) = bare
        .decode(r#"{"v":3,"id":1,"method":"session.hello","params":{}}"#)
        .unwrap()
    else {
        panic!("a hello");
    };
    assert!(card.name.is_empty());
}

#[test]
fn a_refused_hello_is_an_error_for_its_id() {
    let broker = BrokerCodec::new();
    broker
        .decode(r#"{"v":3,"id":4,"method":"session.hello","params":{}}"#)
        .unwrap();
    let line = broker
        .encode(&BrokerFrame::Error {
            reason: "no".into(),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "id": 4, "error": {"kind": "refused", "reason": "no", "message": "refused: no"}})
    );
    // With no hello pending there is nothing to answer.
    assert!(
        broker
            .encode(&BrokerFrame::Error {
                reason: "no".into()
            })
            .unwrap()
            .is_none()
    );

    let worker = WorkerCodec::new();
    worker
        .encode(&ParticipantFrame::Hello {
            card: ParticipantCard::default(),
            schema: chatty_fabric::wire::schema::hash().to_string(),
        })
        .unwrap();
    let refused =
        r#"{"v":3,"id":1,"error":{"kind":"refused","reason":"no","message":"refused: no"}}"#;
    assert!(matches!(
        worker.decode(refused).unwrap(),
        Some(BrokerFrame::Error { reason }) if reason == "no"
    ));
}

#[test]
fn calls_map_to_requests_and_back() {
    let (broker, worker) = connected();
    // The worker's transport numbers its calls itself; the codec gives each
    // a request id of its own.
    let invoke = ParticipantFrame::Call {
        id: 7,
        request: CallRequest::InvokeAgent(InvokeAgentParams {
            agent: "local-reviewer".into(),
            prompt: "review it".into(),
            handle: None,
            include_trace: false,
            spawn_context: None,
            remaining: Default::default(),
            run: None,
        }),
    };
    let line = worker.encode(&invoke).unwrap().unwrap();
    let json = value(&line);
    assert_eq!(json["method"], "agent.invoke");
    assert_eq!(json["id"], 2, "the hello was 1");
    assert_eq!(json["params"]["agent"], "local-reviewer");
    let Some(ParticipantFrame::Call {
        id,
        request: CallRequest::InvokeAgent(params),
    }) = broker.decode(&line).unwrap()
    else {
        panic!("a call");
    };
    assert_eq!(id, 2, "the broker keys the call by the request id");
    assert_eq!(params.prompt, "review it");

    let list = worker
        .encode(&ParticipantFrame::Call {
            id: 8,
            request: CallRequest::ListAgents,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&list),
        json!({"v": 3, "id": 3, "method": "agent.list"})
    );
    assert!(matches!(
        broker.decode(&list).unwrap(),
        Some(ParticipantFrame::Call {
            id: 3,
            request: CallRequest::ListAgents
        })
    ));
    let post = up(
        &worker,
        &broker,
        ParticipantFrame::Call {
            id: 9,
            request: CallRequest::SendMessage(SendMessageParams {
                to: "lead".into(),
                text: "hi".into(),
            }),
        },
    );
    assert!(matches!(
        post,
        ParticipantFrame::Call {
            id: 4,
            request: CallRequest::SendMessage(_)
        }
    ));

    // The answers come back under the worker's own call ids.
    let progress = down(
        &broker,
        &worker,
        BrokerFrame::CallProgress {
            id: 2,
            event: WireProgress::Step("read_file".into()),
        },
    );
    assert!(matches!(progress, BrokerFrame::CallProgress { id: 7, .. }));
    let line = broker
        .encode(&BrokerFrame::CallResult {
            id: 3,
            result: CallResult::Agents(Vec::new()),
        })
        .unwrap()
        .unwrap();
    assert_eq!(value(&line), json!({"v": 3, "id": 3, "result": []}));
    assert!(matches!(
        worker.decode(&line).unwrap(),
        Some(BrokerFrame::CallResult { id: 8, .. })
    ));
    let line = broker
        .encode(&BrokerFrame::CallError {
            id: 4,
            error: CallError::UnknownAgent("lead".into()),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "id": 4, "error": {"kind": "unknown_agent", "agent": "lead",
                                         "message": "unknown agent: lead"}})
    );
    assert!(matches!(
        worker.decode(&line).unwrap(),
        Some(BrokerFrame::CallError { id: 9, error: CallError::UnknownAgent(m) }) if m == "lead"
    ));

    // The worker withdraws call 7: the broker hears req.cancel for 2, and
    // anything it still sends for 2 is dropped at both ends.
    let cancel = worker
        .encode(&ParticipantFrame::CancelCall { id: 7 })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&cancel),
        json!({"v": 3, "method": "req.cancel", "params": {"id": 2}})
    );
    assert!(matches!(
        broker.decode(&cancel).unwrap(),
        Some(ParticipantFrame::CancelCall { id: 2 })
    ));
    let late = r#"{"v":3,"id":2,"result":{}}"#;
    assert!(worker.decode(late).unwrap().is_none());
    assert!(
        broker
            .encode(&BrokerFrame::CallResult {
                id: 2,
                result: CallResult::Agents(Vec::new()),
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_cancel_names_the_task_run() {
    let (broker, worker) = connected();
    let line = broker
        .encode(&BrokerFrame::Cancel {
            task_id: "task-1".into(),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "method": "req.cancel", "params": {"id": 1}})
    );
    assert!(matches!(
        worker.decode(&line).unwrap(),
        Some(BrokerFrame::Cancel { task_id }) if task_id == "task-1"
    ));
    // A cancel for a task that was never run is not sent.
    assert!(
        broker
            .encode(&BrokerFrame::Cancel {
                task_id: "nobody".into()
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_envelope_is_strict() {
    let broker = BrokerCodec::new();
    for (line, why) in [
        (r#"{"id":1,"method":"session.hello"}"#, "no v"),
        (
            r#"{"v":4,"id":1,"method":"session.hello"}"#,
            "another version",
        ),
        (r#"{"v":3}"#, "none of method, result, error"),
        (
            r#"{"v":3,"id":1,"result":{},"error":{}}"#,
            "a result and an error",
        ),
        (
            r#"{"v":3,"method":"task.event","result":{}}"#,
            "a method and a result",
        ),
        (r#"{"v":3,"result":{}}"#, "a result without an id"),
        (
            r#"{"v":3,"id":1,"result":{},"params":{}}"#,
            "params on a result",
        ),
        (
            r#"{"v":3,"id":1,"method":"agent.list","extra":1}"#,
            "an unknown field",
        ),
        (
            r#"{"v":3,"id":1,"method":"agent.list","params":{}}"#,
            "params where none are taken",
        ),
        (
            r#"{"v":3,"id":1,"method":"agent.invoke"}"#,
            "no params where some are needed",
        ),
        (r#"{"v":3,"id":-1,"method":"agent.list"}"#, "a negative id"),
        ("[]", "not an object"),
        ("not json", "not JSON"),
    ] {
        assert!(broker.decode(line).is_err(), "{why}: {line}");
    }
    // The previous version, as an old worker would write it.
    let previous = PROTOCOL_VERSION - 1;
    let old = format!(r#"{{"v":{previous},"type":"hello"}}"#);
    assert!(
        matches!(broker.decode(&old), Err(FrameError::Malformed(_))),
        "an unknown field"
    );
    let old = format!(r#"{{"v":{previous},"method":"task.event","params":{{}}}}"#);
    assert!(matches!(
        broker.decode(&old),
        Err(FrameError::WrongVersion(v)) if v == previous
    ));
}

#[test]
fn a_method_of_the_other_direction_does_not_decode() {
    let broker = BrokerCodec::new();
    let worker = WorkerCodec::new();
    for line in [
        r#"{"v":3,"id":1,"method":"task.run","params":{"taskId":"t","text":"x"}}"#,
        r#"{"v":3,"method":"req.progress","params":{"id":1,"event":{}}}"#,
        r#"{"v":3,"method":"task.input","params":{"id":1,"input":{"requestId":"r","answers":[]}}}"#,
    ] {
        assert!(
            matches!(broker.decode(line), Err(FrameError::WrongDirection(_))),
            "the broker refuses {line}"
        );
    }
    for line in [
        r#"{"v":3,"id":1,"method":"session.hello","params":{}}"#,
        r#"{"v":3,"id":1,"method":"agent.list"}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"status","id":1,"state":"working"}}"#,
        r#"{"v":3,"method":"call.input","params":{"id":1,"task":"t","input":{"requestId":"r","answers":[]}}}"#,
        // EN-2a: the broker never sends an approval to a worker.
        r#"{"v":3,"id":1,"method":"human.approve","params":{"kind":"exec","command_or_path":"ls"}}"#,
    ] {
        assert!(
            matches!(worker.decode(line), Err(FrameError::WrongDirection(_))),
            "the worker refuses {line}"
        );
    }
}

/// EN-2a: `human.approve` is a worker's request with a typed verdict for
/// its result. The worker's approval numbers map to request ids and back,
/// a `req.cancel` of one is an approval's withdrawal, and a verdict for an
/// approval already withdrawn or answered is not sent.
#[test]
fn an_approval_is_a_request_with_a_verdict() {
    use chatty_fabric::{ApprovalKind, ApprovalRequest, ApprovalVerdict};
    let (broker, worker) = connected();
    let request = ApprovalRequest {
        kind: ApprovalKind::Exec,
        command_or_path: "[shell] echo hi".into(),
        diff_stat: None,
        asker: None,
    };
    let line = worker
        .encode(&ParticipantFrame::Approve {
            id: 7,
            request: request.clone(),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "id": 2, "method": "human.approve",
               "params": {"kind": "exec", "command_or_path": "[shell] echo hi"}})
    );
    let Some(ParticipantFrame::Approve { id, request: read }) = broker.decode(&line).unwrap()
    else {
        panic!("an approval");
    };
    assert_eq!((id, read), (2, request.clone()));

    let verdict = BrokerFrame::Approval {
        id: 2,
        verdict: ApprovalVerdict::Approved,
    };
    let line = broker.encode(&verdict).unwrap().unwrap();
    assert_eq!(value(&line), json!({"v": 3, "id": 2, "result": "approved"}));
    assert!(matches!(
        worker.decode(&line).unwrap(),
        Some(BrokerFrame::Approval {
            id: 7,
            verdict: ApprovalVerdict::Approved
        })
    ));
    assert!(
        broker.encode(&verdict).unwrap().is_none(),
        "an answered approval is not answered twice"
    );

    // Withdrawn: a cancel of an approval, and no verdict after it.
    assert!(matches!(
        up(
            &worker,
            &broker,
            ParticipantFrame::Approve { id: 8, request }
        ),
        ParticipantFrame::Approve { id: 3, .. }
    ));
    assert!(matches!(
        up(&worker, &broker, ParticipantFrame::CancelApproval { id: 8 }),
        ParticipantFrame::CancelApproval { id: 3 }
    ));
    assert!(
        broker
            .encode(&BrokerFrame::Approval {
                id: 3,
                verdict: ApprovalVerdict::Denied,
            })
            .unwrap()
            .is_none()
    );
    assert!(
        worker
            .encode(&ParticipantFrame::CancelApproval { id: 8 })
            .unwrap()
            .is_none(),
        "nothing left to withdraw"
    );
}

#[test]
fn an_inflight_id_cannot_be_reused() {
    let (broker, worker) = connected();
    let invoke = r#"{"v":3,"id":5,"method":"agent.list"}"#;
    broker.decode(invoke).unwrap();
    assert!(matches!(
        broker.decode(invoke),
        Err(FrameError::ReusedId(5))
    ));
    // Once answered, the id is no longer in flight.
    broker
        .encode(&BrokerFrame::CallResult {
            id: 5,
            result: CallResult::Agents(Vec::new()),
        })
        .unwrap()
        .unwrap();
    broker.decode(invoke).unwrap();

    let run = r#"{"v":3,"id":1,"method":"task.run","params":{"taskId":"again","text":"x"}}"#;
    assert!(matches!(worker.decode(run), Err(FrameError::ReusedId(1))));
}

#[test]
fn a_response_to_nothing_in_flight_is_dropped() {
    let (broker, worker) = connected();
    for line in [
        r#"{"v":3,"id":9,"result":{"state":"completed"}}"#,
        r#"{"v":3,"id":9,"error":{"kind":"failed","reason":"x","message":"x"}}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"status","id":9,"state":"working"}}"#,
        r#"{"v":3,"method":"req.cancel","params":{"id":9}}"#,
    ] {
        assert!(broker.decode(line).unwrap().is_none(), "{line}");
    }
    for line in [
        r#"{"v":3,"id":9,"result":{}}"#,
        r#"{"v":3,"id":9,"error":{"kind":"failed","reason":"x","message":"x"}}"#,
        r#"{"v":3,"method":"req.progress","params":{"id":9,"event":{"Text":"x"}}}"#,
        r#"{"v":3,"method":"req.cancel","params":{"id":9}}"#,
    ] {
        assert!(worker.decode(line).unwrap().is_none(), "{line}");
    }
    // A duplicate result: the first ends the task.run, the second is late.
    let done = r#"{"v":3,"id":1,"result":{"state":"completed"}}"#;
    assert!(broker.decode(done).unwrap().is_some());
    assert!(broker.decode(done).unwrap().is_none());
}

#[test]
fn a_task_run_result_is_terminal_and_an_event_is_not() {
    let (broker, _worker) = connected();
    assert!(
        broker
            .decode(r#"{"v":3,"id":1,"result":{"state":"working"}}"#)
            .is_err()
    );
    let (broker, _worker) = connected();
    assert!(
        broker
            .decode(r#"{"v":3,"method":"task.event","params":{"kind":"status","id":1,"state":"completed"}}"#)
            .is_err()
    );
}

#[test]
fn a_task_run_error_fails_the_task() {
    let (broker, _worker) = connected();
    let Some(ParticipantFrame::Status {
        task_id,
        state,
        message,
        ..
    }) = broker
        .decode(r#"{"v":3,"id":1,"error":{"kind":"failed","reason":"boom","message":"boom"}}"#)
        .unwrap()
    else {
        panic!("a status");
    };
    assert_eq!(task_id, "task-1");
    assert_eq!(state, TaskState::Failed);
    assert!(message.unwrap().contains("boom"));
}

#[test]
fn the_line_length_bound_is_an_upper_bound() {
    let (broker, _worker) = connected();
    broker
        .decode(r#"{"v":3,"id":12,"method":"agent.list"}"#)
        .unwrap();
    for frame in [
        task("task-2"),
        BrokerFrame::Cancel {
            task_id: "task-1".into(),
        },
        BrokerFrame::CallProgress {
            id: 12,
            event: WireProgress::Text("x".repeat(100)),
        },
        BrokerFrame::CallResult {
            id: 12,
            result: CallResult::Invoked(InvokeAgentOutcome {
                success: true,
                response: "done".into(),
                error: None,
                metadata: None,
                messages: Vec::new(),
                cancelled_by_user: false,
            }),
        },
    ] {
        let bound = BrokerCodec::line_len_bound(&frame);
        let line = broker.encode(&frame).unwrap().unwrap();
        assert!(bound >= line.len(), "{bound} < {}: {line}", line.len());
        assert!(bound < line.len() + 40, "a tight bound: {bound} for {line}");
    }
}

// ---------------------------------------------------------------------------
// EN-3a: typed payloads
// ---------------------------------------------------------------------------

/// DP-2 invariant 4 at the codec: a call's params are the method's and
/// nothing else, so a `metadata.chatty.call` a worker smuggles into an
/// `agent.invoke` (or into its task's result) to claim a shorter chain is
/// refused before the broker reads anything.
#[test]
fn smuggled_chatty_call_metadata_refused_at_decode() {
    let (broker, _worker) = connected();
    let smuggled = json!({"chatty": {"call": {"root_task_id": "forged", "chain": [], "depth": 0}}});
    let invoke = json!({
        "v": 3, "id": 2, "method": "agent.invoke",
        "params": {"agent": "reviewer", "prompt": "go on", "metadata": smuggled},
    })
    .to_string();
    let Err(FrameError::Malformed(reason)) = broker.decode(&invoke) else {
        panic!("a smuggled metadata.chatty.call decoded");
    };
    assert!(reason.contains("metadata"), "{reason}");

    let result = json!({
        "v": 3, "id": 1,
        "result": {"state": "completed", "metadata": smuggled},
    })
    .to_string();
    let Err(FrameError::Malformed(reason)) = broker.decode(&result) else {
        panic!("a smuggled metadata.chatty decoded on a task.run result");
    };
    assert!(reason.contains("chatty"), "{reason}");
}

/// Payloads decode straight into their typed structs: a key given twice is
/// an error, never last-wins — in the envelope, in params, in a result and
/// in an error.
#[test]
fn duplicate_key_is_decode_error() {
    let (broker, worker) = connected();
    for line in [
        r#"{"v":3,"v":3,"id":2,"method":"agent.list"}"#,
        r#"{"v":3,"id":2,"method":"agent.invoke","params":{"agent":"a","agent":"root","prompt":"p"}}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"status","kind":"artifact","id":1,"state":"working"}}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"status","id":1,"state":"working","state":"working"}}"#,
        r#"{"v":3,"id":1,"result":{"state":"completed","state":"failed"}}"#,
    ] {
        assert!(
            matches!(broker.decode(line), Err(FrameError::Malformed(_))),
            "{line}"
        );
    }

    let call = worker
        .encode(&ParticipantFrame::Call {
            id: 1,
            request: CallRequest::ListAgents,
        })
        .unwrap()
        .unwrap();
    let id = value(&call)["id"].as_u64().unwrap();
    let doubled = format!(
        r#"{{"v":3,"id":{id},"error":{{"kind":"refused","reason":"a","reason":"b","message":"m"}}}}"#
    );
    assert!(matches!(
        worker.decode(&doubled),
        Err(FrameError::Malformed(_))
    ));
}

/// The error of an error response is a closed set: a `kind` this build
/// does not know is a decode error, which closes the connection — never a
/// generic failure a peer could make up.
#[test]
fn unknown_error_kind_is_decode_error() {
    let (_broker, worker) = connected();
    let call = worker
        .encode(&ParticipantFrame::Call {
            id: 1,
            request: CallRequest::ListAgents,
        })
        .unwrap()
        .unwrap();
    let id = value(&call)["id"].as_u64().unwrap();
    let unknown =
        format!(r#"{{"v":3,"id":{id},"error":{{"kind":"teapot","message":"short and stout"}}}}"#);
    let Err(FrameError::Malformed(reason)) = worker.decode(&unknown) else {
        panic!("an unknown error kind decoded");
    };
    assert!(reason.contains("teapot"), "{reason}");

    // Every kind there is decodes, and `message` is only display text.
    let known = format!(
        r#"{{"v":3,"id":{id},"error":{{"kind":"protocol","reason":"schema","message":"ignored"}}}}"#
    );
    assert!(matches!(
        worker.decode(&known).unwrap(),
        Some(BrokerFrame::CallError { id: 1, error: CallError::Protocol(reason) }) if reason == "schema"
    ));
}

/// Every key the worker's mapper writes on a terminal status, plus the
/// runner's evidence the broker adds, is a field of [`TaskMetadata`]: a
/// status carrying all of them crosses the wire and comes back equal, under
/// exactly the keys chatty-core names.
#[test]
fn task_metadata_roundtrips_every_worker_key() {
    let (broker, worker) = connected();
    let everything = TaskMetadata {
        usage: Some(WireUsage::from_lines(vec![WireUsageLine {
            model: Some(WireModelRef {
                provider: "open_router".into(),
                model_id: "kit/coder".into(),
            }),
            input_tokens: 20,
            output_tokens: 3,
            cache_read_tokens: 1,
            cache_write_tokens: 2,
            at: Some(1_790_000_000_123),
            duration_ms: 42,
        }])),
        trace: Some("### read_file (ok)".into()),
        conversation: Some(
            Opaque::from_value(&json!([{"role": "user", "content": "hi"}])).unwrap(),
        ),
        conversation_too_large: Some(33_554_433),
        handoff: Some(Opaque::from_value(&json!({"files_changed": ["README.md"]})).unwrap()),
        handoff_invalid: Some(HandoffInvalid {
            role: "coder".into(),
            errors: vec!["/files_changed: required".into()],
        }),
        handoff_invalid_count: Some(2),
        evidence: Some(
            Opaque::from_value(&json!({"branch": "kit-coder-0", "commits": 1})).unwrap(),
        ),
    };
    let line = worker
        .encode(&ParticipantFrame::Status {
            task_id: "task-1".into(),
            state: TaskState::Completed,
            message: None,
            metadata: Some(everything.clone()),
        })
        .unwrap()
        .unwrap();

    let keys: std::collections::BTreeSet<String> = value(&line)["result"]["metadata"]
        .as_object()
        .expect("metadata is an object")
        .keys()
        .cloned()
        .collect();
    let written: std::collections::BTreeSet<String> = [
        USAGE_METADATA_KEY,
        TRACE_METADATA_KEY,
        CONVERSATION_METADATA_KEY,
        CONVERSATION_TOO_LARGE_METADATA_KEY,
        HANDOFF_METADATA_KEY,
        HANDOFF_INVALID_METADATA_KEY,
        HANDOFF_INVALID_COUNT_METADATA_KEY,
        // The runner's, added by the broker (`with_evidence`).
        "evidence",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(keys, written);

    let Some(ParticipantFrame::Status { metadata, .. }) = broker.decode(&line).unwrap() else {
        panic!("the terminal status");
    };
    assert_eq!(metadata, Some(everything));
}

fn ocr() -> chatty_fabric::LockedPlugin {
    chatty_fabric::LockedPlugin {
        module: "ocr".into(),
        version: "1.2.0".into(),
        sha256: "ab".repeat(32),
    }
}

/// MK-2 (ADR-0024 § 2): a paid plugin's tool call crosses the worker's
/// socket as a `module.call` request, its answer as that request's result,
/// and a refusal as the typed `needs_acceptance` / `fee_refused` errors —
/// each back under the worker's own call id, unchanged.
#[test]
fn module_call_round_trips_on_the_wire() {
    use chatty_fabric::{FeeRefusalReason, ItemKind, ItemRef, ModuleCallOutcome, ModuleCallParams};

    let (broker, worker) = connected();
    let params = ModuleCallParams {
        plugin: ocr(),
        tool: "read".into(),
        arguments: r#"{"page":1}"#.into(),
        run: Some("task-1".into()),
    };
    let calls = [11, 12, 13, 14];
    let mut ids = Vec::new();
    for id in calls {
        let line = worker
            .encode(&ParticipantFrame::Call {
                id,
                request: CallRequest::ModuleCall(params.clone()),
            })
            .unwrap()
            .unwrap();
        let json = value(&line);
        assert_eq!(json["method"], "module.call");
        assert_eq!(
            json["params"],
            json!({"plugin": {"module": "ocr", "version": "1.2.0", "sha256": "ab".repeat(32)},
                   "tool": "read", "arguments": "{\"page\":1}", "run": "task-1"})
        );
        let Some(ParticipantFrame::Call {
            id: request_id,
            request: CallRequest::ModuleCall(decoded),
        }) = broker.decode(&line).unwrap()
        else {
            panic!("a module.call");
        };
        assert_eq!(decoded, params);
        ids.push(request_id);
    }

    let answers = [
        Ok(ModuleCallOutcome::Result {
            content: "page one".into(),
        }),
        Ok(ModuleCallOutcome::ToolError {
            message: "no such page".into(),
        }),
        Err(CallError::NeedsAcceptance {
            spec: "auditor".into(),
            version: "2.0.0".into(),
        }),
        Err(CallError::FeeRefused {
            item: ItemRef {
                kind: ItemKind::Plugin,
                name: "ocr".into(),
                version: "1.2.0".into(),
            },
            reason: FeeRefusalReason::Cap,
            resets_at: Some("2026-11-01T00:00:00Z".into()),
        }),
    ];
    for ((request_id, call), answer) in ids.into_iter().zip(calls).zip(answers) {
        let frame = match answer.clone() {
            Ok(outcome) => BrokerFrame::CallResult {
                id: request_id,
                result: CallResult::ModuleCalled(outcome),
            },
            Err(error) => BrokerFrame::CallError {
                id: request_id,
                error,
            },
        };
        let line = broker.encode(&frame).unwrap().unwrap();
        match (worker.decode(&line).unwrap(), answer) {
            (
                Some(BrokerFrame::CallResult {
                    id,
                    result: CallResult::ModuleCalled(outcome),
                }),
                Ok(expected),
            ) => {
                assert_eq!(id, call);
                assert_eq!(outcome, expected);
            }
            (Some(BrokerFrame::CallError { id, error }), Err(expected)) => {
                assert_eq!(id, call);
                assert_eq!(error, expected);
                if let CallError::FeeRefused { .. } = expected {
                    assert_eq!(
                        value(&line)["error"],
                        json!({"kind": "fee_refused",
                               "item": {"kind": "plugin", "name": "ocr", "version": "1.2.0"},
                               "reason": "cap", "resets_at": "2026-11-01T00:00:00Z",
                               "message": "fee_refused: plugin ocr@1.2.0: cap, resets at \
                                           2026-11-01T00:00:00Z"})
                    );
                }
            }
            (other, expected) => panic!("{expected:?} came back as {other:?}"),
        }
    }

    // The broker may not send it: a `module.call` is the worker's request.
    let line = json!({"v": 3, "id": 99, "method": "module.call",
                      "params": {"plugin": {"module": "ocr", "version": "1.2.0",
                                            "sha256": "ab"}, "tool": "read", "arguments": "{}"}})
    .to_string();
    assert!(worker.decode(&line).is_err());
}
