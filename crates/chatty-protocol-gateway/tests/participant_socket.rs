//! AGE-300 / ADR-0011 C1: a process that registers over the participant
//! socket is served as an A2A agent by the gateway.
//!
//! The caller in these tests is `chatty_core::services::a2a_client::A2aClient`
//! — the client the app already uses for remote agents — talking HTTP to a
//! real bound port. Nothing is stubbed on the caller's side, because the
//! claim under test is precisely that a local child process is
//! indistinguishable from any other A2A agent to it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chatty_core::services::a2a_client::{A2aClient, A2aStreamEvent};
use chatty_core::settings::models::a2a_store::A2aAgentConfig;
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::ParticipantRegistry;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// A gateway on a real port, with a real participant socket
// ---------------------------------------------------------------------------

struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop".into())
    }
}

struct Harness {
    base_url: String,
    socket: PathBuf,
    participants: ParticipantRegistry,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir for the socket");
        let socket = dir.path().join("participants.sock");

        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let modules = Arc::new(RwLock::new(
            ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
        ));
        let gateway = ProtocolGateway::new(modules, 0).with_participant_socket(&socket);
        let participants = gateway.participants();

        let listener = chatty_protocol_gateway::participant::bind(&socket)
            .expect("the participant socket binds");
        tokio::spawn(chatty_protocol_gateway::participant::serve(
            listener,
            participants.clone(),
        ));

        // Port 0: the OS picks, so the tests never race for a fixed one.
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port");
        let base_url = format!("http://{}", tcp.local_addr().unwrap());
        let router = gateway.build_router();
        tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        Self {
            base_url,
            socket,
            participants,
            _dir: dir,
        }
    }

    /// An `A2aAgentConfig` addressing `name` on this gateway, as
    /// `invoke_agent` builds one for a local agent.
    fn agent(&self, name: &str) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: format!("{}/a2a/{}", self.base_url, name),
            api_key: None,
            enabled: true,
            skills: vec![],
        }
    }

    /// Wait for `name` to disappear from the registry.
    async fn await_deregistration(&self, name: &str) {
        for _ in 0..100 {
            if !self.participants.is_registered(name) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("participant '{name}' was still registered two seconds after its socket closed");
    }
}

// ---------------------------------------------------------------------------
// A stub participant
// ---------------------------------------------------------------------------

/// A participant that registers, then answers each task with a scripted
/// sequence of frames.
struct StubParticipant {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: tokio::net::unix::OwnedWriteHalf,
}

impl StubParticipant {
    async fn register(socket: &PathBuf, name: &str) -> Self {
        let stream = UnixStream::connect(socket)
            .await
            .expect("the participant socket accepts a connection");
        let (read, write) = stream.into_split();
        let mut stub = Self {
            lines: BufReader::new(read).lines(),
            write,
        };

        stub.send(json!({
            "type": "register",
            "card": {
                "name": name,
                "description": "a stub worker",
                "version": "0.1.0",
                "skills": [{ "name": "echo", "description": "repeats the task" }],
            }
        }))
        .await;

        let ack = stub.next_frame().await;
        assert_eq!(ack["type"], "registered", "registration is acknowledged");
        assert_eq!(ack["name"], name);
        stub
    }

    async fn send(&mut self, frame: Value) {
        let line = format!("{frame}\n");
        self.write.write_all(line.as_bytes()).await.unwrap();
        self.write.flush().await.unwrap();
    }

    async fn next_frame(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("a frame from the broker within five seconds")
            .expect("the socket is readable")
            .expect("the broker did not close the socket");
        serde_json::from_str(&line).expect("the broker sends JSON")
    }

    /// Wait for a task, then answer it: two progress updates, one artifact,
    /// and a terminal `completed`.
    async fn answer_one_task(&mut self, answer: &str) -> String {
        self.answer_one_task_frame(answer).await["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// [`answer_one_task`](Self::answer_one_task), returning the whole task
    /// frame for a test that wants more than its text.
    async fn answer_one_task_frame(&mut self, answer: &str) -> Value {
        let task = self.next_frame().await;
        assert_eq!(task["type"], "task");
        let task_id = task["taskId"].as_str().unwrap().to_string();

        for tool in ["read_file", "\u{2713} read_file"] {
            self.send(json!({
                "type": "status",
                "taskId": task_id,
                "state": "working",
                "message": tool,
            }))
            .await;
        }
        self.send(json!({
            "type": "artifact",
            "taskId": task_id,
            "text": answer,
            "lastChunk": true,
        }))
        .await;
        self.send(json!({
            "type": "status",
            "taskId": task_id,
            "state": "completed",
        }))
        .await;

        task
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The issue's "Done when": a task round-trips with at least two status
/// updates and one artifact, read back by the real A2A client.
#[tokio::test]
async fn a_registered_participant_round_trips_a_task_with_progress_and_an_artifact() {
    let harness = Harness::start().await;
    let mut stub = StubParticipant::register(&harness.socket, "stub-worker").await;

    let stub_task = tokio::spawn(async move {
        let prompt = stub.answer_one_task("the file defines Foo").await;
        (stub, prompt)
    });

    let client = A2aClient::new();
    let mut stream = client
        .send_message_stream(&harness.agent("stub-worker"), "summarise foo.rs")
        .await
        .expect("the gateway accepts message/stream for a participant");

    let mut progress: Vec<String> = Vec::new();
    let mut artifacts: Vec<String> = Vec::new();
    let mut final_state = None;

    while let Some(event) = stream.next().await {
        match event.expect("no stream error") {
            A2aStreamEvent::StatusUpdate {
                state,
                message,
                is_final,
                ..
            } => {
                if let Some(message) = message {
                    progress.push(message);
                }
                if is_final {
                    final_state = Some(state);
                    break;
                }
            }
            A2aStreamEvent::ArtifactUpdate { text, .. } => artifacts.push(text),
        }
    }

    let (_stub, prompt) = stub_task.await.unwrap();
    assert_eq!(prompt, "summarise foo.rs", "the prompt reaches the child");
    assert_eq!(
        progress,
        vec!["read_file".to_string(), "\u{2713} read_file".to_string()],
        "every per-tool progress update survives the round trip, in order"
    );
    assert_eq!(artifacts, vec!["the file defines Foo".to_string()]);
    assert_eq!(final_state.as_deref(), Some("completed"));
}

/// AGE-306 on the wire: a parked task's question reaches the A2A caller in
/// the status's `metadata.clarification`, the caller's `message/send` on the
/// same task id becomes an `input` frame on the socket, and the task goes on.
#[tokio::test]
async fn a_parked_tasks_question_is_served_and_its_answer_comes_back_as_an_input_frame() {
    use chatty_core::models::clarification_store::ClarificationAnswer;
    use chatty_core::services::a2a_client::A2aClarificationRequest;

    let harness = Harness::start().await;
    let mut stub = StubParticipant::register(&harness.socket, "asking-worker").await;

    let stub_task = tokio::spawn(async move {
        let task = stub.next_frame().await;
        let task_id = task["taskId"].as_str().unwrap().to_string();
        stub.send(json!({
            "type": "status",
            "taskId": task_id,
            "state": "input-required",
            "message": "Which database?",
            "input": {
                "id": "req-1",
                "questions": [{
                    "id": "q1",
                    "question": "Which database?",
                    "options": ["Postgres", "SQLite"],
                }],
            },
        }))
        .await;

        // The answer arrives on the socket, addressed to this task.
        let input = stub.next_frame().await;
        assert_eq!(input["type"], "input");
        assert_eq!(input["taskId"], task_id);
        assert_eq!(input["input"]["requestId"], "req-1");
        let chosen = input["input"]["answers"][0]["answer"]
            .as_str()
            .unwrap()
            .to_string();

        stub.send(json!({
            "type": "status",
            "taskId": task_id,
            "state": "working",
            "message": "\u{2713} ask_user",
        }))
        .await;
        stub.send(json!({
            "type": "artifact",
            "taskId": task_id,
            "text": format!("Using {chosen}."),
            "lastChunk": true,
        }))
        .await;
        stub.send(json!({
            "type": "status",
            "taskId": task_id,
            "state": "completed",
        }))
        .await;
        stub
    });

    let client = A2aClient::new();
    let agent = harness.agent("asking-worker");
    let mut stream = client
        .send_message_stream(&agent, "set up the database")
        .await
        .expect("the gateway accepts message/stream for a participant");

    let mut answered = false;
    let mut progress: Vec<String> = Vec::new();
    let mut artifacts: Vec<String> = Vec::new();
    let mut final_state = None;
    while let Some(event) = stream.next().await {
        match event.expect("no stream error") {
            A2aStreamEvent::StatusUpdate {
                task_id,
                state,
                message,
                metadata,
                is_final,
            } => {
                if state == "input-required" {
                    let request = A2aClarificationRequest::from_status_metadata(metadata.as_ref())
                        .expect("the parked status carries the request");
                    assert_eq!(request.id, "req-1");
                    assert_eq!(request.questions[0].options, vec!["Postgres", "SQLite"]);
                    assert_eq!(message.as_deref(), Some("Which database?"));
                    assert!(
                        harness.participants.owns_task(&task_id),
                        "parked, still open"
                    );

                    client
                        .send_task_input(
                            &agent,
                            &task_id,
                            &request.id,
                            &[ClarificationAnswer {
                                id: "q1".into(),
                                answer: "SQLite".into(),
                                custom: false,
                            }],
                        )
                        .await
                        .expect("the gateway accepts the answer on the parked task");
                    answered = true;
                } else if let Some(message) = message {
                    progress.push(message);
                }
                if is_final {
                    final_state = Some(state);
                    break;
                }
            }
            A2aStreamEvent::ArtifactUpdate { text, .. } => artifacts.push(text),
        }
    }

    let stub = stub_task.await.unwrap();
    assert!(answered, "the caller saw the parked state");
    assert_eq!(progress, vec!["\u{2713} ask_user".to_string()]);
    assert_eq!(artifacts, vec!["Using SQLite.".to_string()]);
    assert_eq!(final_state.as_deref(), Some("completed"));

    // A message on a task the broker does not hold open is a fresh task,
    // as every client that puts its own id on a new message relies on: with
    // the participant gone it is refused as one, not as a missing answer.
    drop(stub);
    harness.await_deregistration("asking-worker").await;
    let err = client
        .send_task_input(&agent, "task-nobody", "req-1", &[])
        .await
        .expect_err("an id the broker never minted is not a parked task");
    assert!(
        !err.to_string().contains("waiting for input"),
        "it was routed as a new task, not as an answer: {err}"
    );
}

/// `message/send` — the non-streaming method — returns the participant's
/// output where `A2aClient::send_message` looks for it.
#[tokio::test]
async fn message_send_returns_the_participants_answer() {
    let harness = Harness::start().await;
    let mut stub = StubParticipant::register(&harness.socket, "stub-worker").await;
    let stub_task = tokio::spawn(async move { stub.answer_one_task("42").await });

    let answer = A2aClient::new()
        .send_message(&harness.agent("stub-worker"), "what is six times seven")
        .await
        .expect("message/send succeeds");

    assert_eq!(stub_task.await.unwrap(), "what is six times seven");
    assert_eq!(answer, "42");
}

/// AGE-371: the caller's bearer rides the task frame to the worker, and a
/// caller without one puts nothing on the wire.
#[tokio::test]
async fn the_callers_bearer_reaches_the_worker_on_the_task_frame() {
    let harness = Harness::start().await;

    let mut stub = StubParticipant::register(&harness.socket, "stub-worker").await;
    let stub_task = tokio::spawn(async move { stub.answer_one_task_frame("ok").await });
    let mut agent = harness.agent("stub-worker");
    // `invoke_agent` puts `api_key` on the request as `Authorization: Bearer`.
    agent.api_key = Some("eyJ.user.token".to_string());
    A2aClient::new()
        .send_message(&agent, "who am I")
        .await
        .expect("message/send succeeds");
    let task = stub_task.await.unwrap();
    assert_eq!(task["bearer"], "eyJ.user.token");

    let mut stub = StubParticipant::register(&harness.socket, "other-worker").await;
    let stub_task = tokio::spawn(async move { stub.answer_one_task_frame("ok").await });
    A2aClient::new()
        .send_message(&harness.agent("other-worker"), "who am I")
        .await
        .expect("message/send succeeds");
    let task = stub_task.await.unwrap();
    assert!(
        task.get("bearer").is_none(),
        "no bearer on the request, none on the frame: {task}"
    );
}

/// AGE-321: a worker that asks a question under `message/send` ends the task
/// with the question in hand, instead of parking until its clarification
/// timeout on a caller that can never answer.
#[tokio::test]
async fn message_send_ends_promptly_when_the_worker_asks_a_question() {
    let harness = Harness::start().await;
    let mut stub = StubParticipant::register(&harness.socket, "stub-worker").await;

    let stub_task = tokio::spawn(async move {
        let task = stub.next_frame().await;
        assert_eq!(task["type"], "task");
        let task_id = task["taskId"].as_str().unwrap().to_string();

        stub.send(json!({
            "type": "status",
            "taskId": task_id,
            "state": "input-required",
            "message": "Which database?",
            "input": {
                "id": "req-1",
                "questions": [{
                    "id": "q1",
                    "question": "Which database?",
                    "options": ["Postgres", "SQLite"],
                }],
            },
        }))
        .await;

        // The worker now waits, as a parked one does. Nothing more is sent:
        // the broker ending the task is what un-parks it.
        stub
    });

    // A generous bound that is still far below the 300 s clarification
    // timeout: the point of the fix is that this does not wait one out.
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        A2aClient::new().send_message(&harness.agent("stub-worker"), "migrate the schema"),
    )
    .await
    .expect("the task ends without waiting out the clarification timeout");

    let error = answer.expect_err("a question a caller cannot answer fails the task");
    let text = format!("{error:#}");
    assert!(
        text.contains("Which database?"),
        "the failure has to quote the question, got: {text}"
    );
    assert!(
        text.contains("message/stream"),
        "and say how to ask so it can be answered, got: {text}"
    );

    // The worker is still parked on its question; dropping it closes the
    // socket, which is what a reaped worker's exit does in production.
    drop(stub_task.await.unwrap());
    harness.await_deregistration("stub-worker").await;
}

/// The card a participant published at registration is served at its
/// well-known path, and `A2aClient` reads it as any other agent's.
#[tokio::test]
async fn a_participants_card_is_served_and_lists_it_on_the_aggregated_card() {
    let harness = Harness::start().await;
    let _stub = StubParticipant::register(&harness.socket, "stub-worker").await;

    let card = A2aClient::new()
        .fetch_agent_card(&harness.agent("stub-worker"))
        .await
        .expect("the participant's card is served");
    assert_eq!(card.name, "stub-worker");
    assert_eq!(card.description, "a stub worker");
    assert_eq!(card.skills, vec!["echo".to_string()]);
    assert!(
        card.supports_streaming,
        "a participant streams, so `invoke_agent` uses message/stream"
    );

    let aggregated: Value = reqwest::get(format!("{}/.well-known/agent.json", harness.base_url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = aggregated["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(
        names.contains(&"stub-worker"),
        "the gateway's aggregated card lists what it can address: {names:?}"
    );
}

/// ADR-0011 C10: `with_virtual_agent` is additive. Two named runners are
/// each served at `/a2a/{name}` with their own card, and both are on the
/// aggregated card as this machine's — which is what `list_agents` reads.
#[tokio::test]
async fn several_virtual_agents_are_each_served_by_name_and_all_listed() {
    use chatty_protocol_gateway::participant::LocalRunner;

    let dir = tempfile::tempdir().expect("a temp dir for the socket");
    let socket = dir.path().join("participants.sock");
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let modules = Arc::new(RwLock::new(
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
    ));
    let mut gateway = ProtocolGateway::new(modules, 0);
    for (name, description) in [
        ("local-coder", "Model: qwen. Tools: the full set."),
        (
            "local-reviewer",
            "Model: gemma. Tool groups disabled: fs-write, shell, git.",
        ),
    ] {
        let runner = LocalRunner::new("/bin/sh", &socket, gateway.participants())
            .with_agent_name(name)
            .with_description(description);
        gateway = gateway.with_virtual_agent(Arc::new(runner));
    }
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base_url = format!("http://{}", tcp.local_addr().unwrap());
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });

    let client = A2aClient::new();
    for (name, description) in [
        ("local-coder", "Model: qwen. Tools: the full set."),
        (
            "local-reviewer",
            "Model: gemma. Tool groups disabled: fs-write, shell, git.",
        ),
    ] {
        let card = client
            .fetch_agent_card(&A2aAgentConfig {
                name: name.to_string(),
                url: format!("{base_url}/a2a/{name}"),
                api_key: None,
                enabled: true,
                skills: vec![],
            })
            .await
            .unwrap_or_else(|e| panic!("{name}'s card is served: {e:#}"));
        assert_eq!(card.name, name);
        assert_eq!(card.description, description);
    }

    let aggregated: Value = reqwest::get(format!("{base_url}/.well-known/agent.json"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<(&str, &str)> = aggregated["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| Some((a["name"].as_str()?, a["origin"].as_str()?)))
        .collect();
    assert_eq!(
        listed,
        vec![("local-coder", "local"), ("local-reviewer", "local")],
        "every virtual agent is on the aggregated card, in name order, as this machine's"
    );
}

/// The issue's second "Done when": a closed socket deregisters the stub.
#[tokio::test]
async fn closing_the_socket_deregisters_the_participant() {
    let harness = Harness::start().await;
    let stub = StubParticipant::register(&harness.socket, "stub-worker").await;
    assert!(harness.participants.is_registered("stub-worker"));

    drop(stub);
    harness.await_deregistration("stub-worker").await;

    // And the name is free again, so a replacement worker can take it.
    let _replacement = StubParticipant::register(&harness.socket, "stub-worker").await;
    assert!(harness.participants.is_registered("stub-worker"));
}

/// A participant that dies mid-task fails that task rather than leaving the
/// caller hanging on a process that no longer exists.
#[tokio::test]
async fn a_participant_that_dies_mid_task_fails_its_open_task() {
    let harness = Harness::start().await;
    let mut stub = StubParticipant::register(&harness.socket, "flaky-worker").await;

    let stub_task = tokio::spawn(async move {
        let task = stub.next_frame().await;
        assert_eq!(task["type"], "task");
        // Report a little progress, then vanish without a terminal status.
        stub.send(json!({
            "type": "status",
            "taskId": task["taskId"],
            "state": "working",
            "message": "shell",
        }))
        .await;
        drop(stub);
    });

    let client = A2aClient::new();
    let mut stream = client
        .send_message_stream(&harness.agent("flaky-worker"), "run something")
        .await
        .unwrap();

    let mut last = None;
    while let Some(event) = stream.next().await {
        if let A2aStreamEvent::StatusUpdate {
            state,
            message,
            is_final,
            ..
        } = event.expect("no stream error")
            && is_final
        {
            last = Some((state, message));
            break;
        }
    }

    stub_task.await.unwrap();
    let (state, message) = last.expect("the stream ends with a final event");
    assert_eq!(state, "failed");
    assert!(
        message.unwrap_or_default().contains("disconnected"),
        "the caller is told why, not just that"
    );
    harness.await_deregistration("flaky-worker").await;
}

/// A second participant may not take a name that is already claimed: the
/// first one's open tasks would be stranded.
#[tokio::test]
async fn a_duplicate_name_is_rejected_on_the_socket() {
    let harness = Harness::start().await;
    let _first = StubParticipant::register(&harness.socket, "stub-worker").await;

    let stream = UnixStream::connect(&harness.socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"{\"type\":\"register\",\"card\":{\"name\":\"stub-worker\"}}\n")
        .await
        .unwrap();

    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(reply["type"], "rejected");
    assert!(
        reply["reason"]
            .as_str()
            .unwrap()
            .contains("already registered"),
        "{reply}"
    );
    assert!(harness.participants.is_registered("stub-worker"));
}

/// The evidence block a stand-in runner hands back (AGE-406).
const EVIDENCE_BLOCK: &str = "\n\n```evidence\nbranch: sub-agent/local-agent-0\ncommits: 2\n```";

/// A gateway whose `local-agent` is a real `LocalRunner` whose workspace
/// hands back a fixed evidence envelope, plus its socket and base URL.
///
/// A fixed envelope rather than a real worktree: what these tests pin is
/// that whatever a runner collected reaches the caller in both shapes.
/// Collecting it from git is `chatty_core::services::worker_tree`'s own
/// tests, and both ends together are `chatty-tui`'s
/// `participant::equivalence`.
async fn start_evidence_runner() -> (tempfile::TempDir, PathBuf, String) {
    use chatty_protocol_gateway::participant::{LocalRunner, TaskEvidence, WorkerWorkspace};

    let dir = tempfile::tempdir().expect("a temp dir for the socket");
    let socket = dir.path().join("participants.sock");
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let modules = Arc::new(RwLock::new(
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
    ));
    let mut gateway = ProtocolGateway::new(modules, 0);
    let participants = gateway.participants();
    let listener =
        chatty_protocol_gateway::participant::bind(&socket).expect("the participant socket binds");
    tokio::spawn(chatty_protocol_gateway::participant::serve(
        listener,
        participants.clone(),
    ));

    let cwd = dir.path().to_path_buf();
    let runner = LocalRunner::new("/bin/sh", &socket, participants.clone())
        .with_args(["-c", "sleep 30"])
        .with_registration_timeout(Duration::from_secs(5))
        .with_workspace_factory(Arc::new(move |_worker: String| {
            let cwd = cwd.clone();
            Box::pin(async move {
                Ok(Some(WorkerWorkspace {
                    cwd,
                    evidence: Some(Box::new(|| {
                        Box::pin(async {
                            Some(TaskEvidence {
                                text: EVIDENCE_BLOCK.to_string(),
                                data: json!({
                                    "branch": "sub-agent/local-agent-0",
                                    "commits": 2,
                                    "verification": { "exit_code": 3 },
                                }),
                            })
                        })
                    })),
                    on_exit: Box::new(|_| {}),
                }))
            })
        }));
    gateway = gateway.with_virtual_agent(Arc::new(runner));

    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base_url = format!("http://{}", tcp.local_addr().unwrap());
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });

    (dir, socket, base_url)
}

/// ADR-0011 C12 / AGE-406, Do item 4, on `message/send`: the envelope is a
/// structured field on the task result as well as a fenced block in the
/// answer, so a trace — or Harbor's ATIF — reads it without parsing prose.
#[tokio::test]
async fn message_send_carries_the_evidence_envelope_as_prose_and_as_a_field() {
    let (_dir, socket, base_url) = start_evidence_runner().await;

    // The runner names its first worker deterministically, so a stub can
    // claim the name before the task is sent — the same split `runner.rs`'s
    // own tests use.
    let mut stub = StubParticipant::register(&socket, "local-agent-0").await;
    tokio::spawn(async move { stub.answer_one_task("done").await });

    let response: Value = reqwest::Client::new()
        .post(format!("{base_url}/a2a/local-agent"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": { "message": { "parts": [{ "type": "text", "text": "do it" }] } },
        }))
        .send()
        .await
        .expect("the gateway answers message/send")
        .json()
        .await
        .expect("the reply is JSON-RPC");

    let result = &response["result"];
    assert_eq!(result["status"]["state"], "completed");
    assert_eq!(
        result["artifacts"][0]["parts"][0]["text"],
        format!("done{EVIDENCE_BLOCK}"),
        "the block is appended to the worker's own answer"
    );
    assert_evidence(&result["status"]["metadata"]);
}

/// The same on `message/stream`, which is the method `invoke_agent`
/// actually uses: the envelope rides the *final* status event's `metadata`,
/// and the block arrives as the artifact chunk ahead of it.
#[tokio::test]
async fn message_stream_carries_the_evidence_envelope_on_its_final_status() {
    let (_dir, socket, base_url) = start_evidence_runner().await;

    let mut stub = StubParticipant::register(&socket, "local-agent-0").await;
    tokio::spawn(async move { stub.answer_one_task("done").await });

    let client = A2aClient::new();
    let mut stream = client
        .send_message_stream(
            &A2aAgentConfig {
                name: "local-agent".to_string(),
                url: format!("{base_url}/a2a/local-agent"),
                api_key: None,
                enabled: true,
                skills: vec![],
            },
            "do it",
        )
        .await
        .expect("the gateway accepts message/stream for a virtual agent");

    let mut artifacts = String::new();
    let mut final_metadata = None;
    let mut final_state = None;
    while let Some(event) = stream.next().await {
        match event.expect("no stream error") {
            A2aStreamEvent::ArtifactUpdate { text, .. } => artifacts.push_str(&text),
            A2aStreamEvent::StatusUpdate {
                state,
                is_final,
                metadata,
                ..
            } => {
                if is_final {
                    final_state = Some(state);
                    final_metadata = metadata;
                    break;
                }
            }
        }
    }

    assert_eq!(final_state.as_deref(), Some("completed"));
    assert_eq!(
        artifacts,
        format!("done{EVIDENCE_BLOCK}"),
        "the block reaches a streaming caller too, ahead of the final status"
    );
    assert_evidence(&final_metadata.expect("the terminal status carries the evidence envelope"));
}

/// The structured field, wherever it was read from.
fn assert_evidence(metadata: &Value) {
    let evidence = &metadata["evidence"];
    assert_eq!(evidence["branch"], "sub-agent/local-agent-0");
    assert_eq!(evidence["commits"], 2);
    assert_eq!(
        evidence["verification"]["exit_code"], 3,
        "the structured field carries what the prose does: {evidence}"
    );
}
