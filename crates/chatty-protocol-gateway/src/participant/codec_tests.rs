//! The v3 envelope, line by line (EN-1). Connection-level behaviour — what
//! closes a connection and what does not — is in
//! `tests/participant_socket.rs`.

use super::*;
use crate::participant::protocol::{InputAnswer, InputQuestion};
use chatty_fabric::InvokeAgentParams;
use serde_json::json;

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
        bearer: None,
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
            input: None,
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
            metadata: Some(json!({"usage": {"inputTokens": 12}})),
            input: None,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&done),
        json!({"v": 3, "id": 1, "result": {"state": "completed", "metadata": {"usage": {"inputTokens": 12}}}})
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
    assert_eq!(metadata.unwrap()["usage"]["inputTokens"], 12);

    // The task is over: a late event for it is dropped, not fatal.
    assert!(broker.decode(&working).unwrap().is_none());
}

#[test]
fn a_parked_task_and_its_answer_travel_as_interim_notifications() {
    let (broker, worker) = connected();
    let input = InputRequest {
        id: "req-1".into(),
        questions: vec![InputQuestion::Question {
            id: "q1".into(),
            question: "Which database?".into(),
            options: vec!["Postgres".into(), "SQLite".into()],
        }],
    };
    let parked = ParticipantFrame::Status {
        task_id: "task-1".into(),
        state: TaskState::InputRequired,
        message: Some("Which database?".into()),
        metadata: None,
        input: Some(input.clone()),
    };
    let line = worker.encode(&parked).unwrap().unwrap();
    let json = value(&line);
    assert_eq!(json["method"], "task.input_required");
    assert_eq!(json["params"]["id"], 1);
    assert_eq!(
        json["params"]["input"]["questions"][0]["options"][1],
        "SQLite"
    );
    let Some(ParticipantFrame::Status {
        state, input: read, ..
    }) = broker.decode(&line).unwrap()
    else {
        panic!("a status");
    };
    assert_eq!(state, TaskState::InputRequired);
    assert_eq!(read, Some(input));

    let answer = TaskInput {
        request_id: "req-1".into(),
        answers: vec![InputAnswer {
            id: "q1".into(),
            answer: "Postgres".into(),
            custom: false,
        }],
    };
    let line = broker
        .encode(&BrokerFrame::Input {
            task_id: "task-1".into(),
            input: answer.clone(),
        })
        .unwrap()
        .unwrap();
    let json = value(&line);
    assert_eq!(json["method"], "task.input");
    assert_eq!(json["params"]["id"], 1);
    assert_eq!(json["params"]["input"]["requestId"], "req-1");
    let Some(BrokerFrame::Input { task_id, input }) = worker.decode(&line).unwrap() else {
        panic!("an input");
    };
    assert_eq!(task_id, "task-1");
    assert_eq!(input, answer);

    // `custom` is optional on the way in.
    let bare = r#"{"v":3,"method":"task.input","params":{"id":1,"input":{"requestId":"r","answers":[{"id":"q1","answer":"x"}]}}}"#;
    let Some(BrokerFrame::Input { input, .. }) = worker.decode(bare).unwrap() else {
        panic!("an input");
    };
    assert!(!input.answers[0].custom);
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
            bearer: Some(TaskBearer::new("eyJ.token")),
            capture_conversation: true,
            spawn_context: None,
            handoff: None,
            budget: Box::new(budget.clone()),
            swarm_events: true,
        })
        .unwrap()
        .unwrap();
    let json = value(&line);
    assert_eq!(json["params"]["bearer"], "eyJ.token");
    assert_eq!(json["params"]["captureConversation"], true);
    assert_eq!(json["params"]["swarmEvents"], true);
    assert_eq!(
        json["params"]["budget"],
        json!({"turns": 2, "seconds": 30, "usd": 0.02})
    );
    let Some(BrokerFrame::Task {
        bearer,
        capture_conversation,
        budget: read,
        swarm_events,
        ..
    }) = worker.decode(&line).unwrap()
    else {
        panic!("a task");
    };
    assert_eq!(bearer.unwrap().expose(), "eyJ.token");
    assert!(capture_conversation && swarm_events);
    assert_eq!(*read, budget);

    let bare = r#"{"v":3,"id":2,"method":"task.run","params":{"taskId":"t2","text":"x"}}"#;
    let Some(BrokerFrame::Task {
        bearer,
        capture_conversation,
        budget,
        swarm_events,
        ..
    }) = worker.decode(bare).unwrap()
    else {
        panic!("a task");
    };
    assert!(bearer.is_none() && !capture_conversation && !swarm_events);
    assert!(budget.is_unlimited());
}

/// TB-1: a swarm event carries one item and nothing a worker could tag it
/// with.
#[test]
fn a_swarm_event_drops_a_forged_tag_when_parsed() {
    let (broker, _worker) = connected();
    let line = r#"{"v":3,"method":"task.event","params":{"kind":"swarm","id":1,"root_task_id":"forged",
                   "event":{"kind":"tool_call_started","id":"c1","name":"shell",
                            "node":"root","chain":{"root_task_id":"forged","chain":["root"],"depth":0}}}}"#;
    let Some(ParticipantFrame::Event { task_id, event }) = broker.decode(line).unwrap() else {
        panic!("an event");
    };
    assert_eq!(task_id, "task-1");
    assert_eq!(
        event,
        SwarmItem::ToolCallStarted {
            id: "c1".into(),
            name: "shell".into()
        }
    );
}

#[test]
fn a_hello_carries_a_card() {
    let broker = BrokerCodec::new();
    let line = r#"{"v":3,"id":1,"method":"session.hello","params":{"card":{"name":"worker-1",
                   "description":"a worker","skills":[{"name":"edit"}]}}}"#;
    let Some(ParticipantFrame::Hello { card }) = broker.decode(line).unwrap() else {
        panic!("a hello");
    };
    assert_eq!(card.name, "worker-1", "carried, and ignored by the broker");
    assert_eq!(card.skills[0].name, "edit");
    assert_eq!(card.version, "");
    assert!(card.display_name.is_none());

    let bare = BrokerCodec::new();
    let Some(ParticipantFrame::Hello { card }) = bare
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
        json!({"v": 3, "id": 4, "error": {"kind": "refused", "message": "no"}})
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
        })
        .unwrap();
    let refused = r#"{"v":3,"id":1,"error":{"kind":"refused","message":"no"}}"#;
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
            event: json!({"Step": "read_file"}),
        },
    );
    assert!(matches!(progress, BrokerFrame::CallProgress { id: 7, .. }));
    let line = broker
        .encode(&BrokerFrame::CallResult {
            id: 3,
            result: json!([]),
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
        json!({"v": 3, "id": 4, "error": {"kind": "unknown_agent", "message": "lead"}})
    );
    assert!(matches!(
        worker.decode(&line).unwrap(),
        Some(BrokerFrame::CallError { id: 9, error: CallError::UnknownAgent(m) }) if m == "lead"
    ));

    // A question on call 7 and its withdrawal, then the answer to one.
    let question = down(
        &broker,
        &worker,
        BrokerFrame::CallInputRequired {
            id: 2,
            task: "callee".into(),
            request: json!({"id": "r", "questions": []}),
        },
    );
    assert!(matches!(
        question,
        BrokerFrame::CallInputRequired { id: 7, .. }
    ));
    let line = broker
        .encode(&BrokerFrame::CallInputWithdrawn {
            id: 2,
            task: "callee".into(),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&line),
        json!({"v": 3, "method": "call.input_withdrawn", "params": {"id": 2, "task": "callee"}})
    );
    assert!(matches!(
        worker.decode(&line).unwrap(),
        Some(BrokerFrame::CallInputWithdrawn { id: 7, .. })
    ));
    let answer = up(
        &worker,
        &broker,
        ParticipantFrame::CallInput {
            id: 7,
            task: "callee".into(),
            input: TaskInput {
                request_id: "r".into(),
                answers: vec![],
            },
        },
    );
    assert!(matches!(answer, ParticipantFrame::CallInput { id: 2, .. }));

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
                result: json!({})
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
        r#"{"v":3,"id":1,"method":"human.ask","params":{}}"#,
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
            result: json!([]),
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
        r#"{"v":3,"id":9,"error":{"kind":"failed","message":"x"}}"#,
        r#"{"v":3,"method":"task.event","params":{"kind":"status","id":9,"state":"working"}}"#,
        r#"{"v":3,"method":"req.cancel","params":{"id":9}}"#,
        r#"{"v":3,"method":"call.input","params":{"id":9,"task":"t","input":{"requestId":"r","answers":[]}}}"#,
    ] {
        assert!(broker.decode(line).unwrap().is_none(), "{line}");
    }
    for line in [
        r#"{"v":3,"id":9,"result":{}}"#,
        r#"{"v":3,"id":9,"error":{"kind":"failed","message":"x"}}"#,
        r#"{"v":3,"method":"req.progress","params":{"id":9,"event":{}}}"#,
        r#"{"v":3,"method":"req.cancel","params":{"id":9}}"#,
        r#"{"v":3,"method":"call.input_withdrawn","params":{"id":9,"task":"t"}}"#,
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
        .decode(r#"{"v":3,"id":1,"error":{"kind":"failed","message":"boom"}}"#)
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
            event: json!({"Text": "x".repeat(100)}),
        },
        BrokerFrame::CallResult {
            id: 12,
            result: json!({"success": true}),
        },
    ] {
        let bound = BrokerCodec::line_len_bound(&frame);
        let line = broker.encode(&frame).unwrap().unwrap();
        assert!(bound >= line.len(), "{bound} < {}: {line}", line.len());
        assert!(bound < line.len() + 40, "a tight bound: {bound} for {line}");
    }
}
