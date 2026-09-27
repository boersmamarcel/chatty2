//! AGE-300 / ADR-0011 C1: a worker on a connection the broker made for it
//! is served as an A2A agent by the gateway; ADR-0020: the connection names
//! it, and nothing registers on the shared socket.
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
use chatty_protocol_gateway::participant::{ParticipantRegistry, open_connection};
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
        tokio::spawn(chatty_protocol_gateway::participant::serve(listener));

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

/// A participant on a connection the broker made for it, which then answers
/// each task with a scripted sequence of frames.
struct StubParticipant {
    /// The name the broker welcomed it under.
    name: String,
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: tokio::net::unix::OwnedWriteHalf,
}

impl StubParticipant {
    /// Admit a node as `spec`, take the worker's end of its connection, and
    /// say hello with a card that claims `spec` as its name (ignored).
    async fn connect(participants: &ParticipantRegistry, spec: &str) -> Self {
        let connection =
            open_connection(participants, spec).expect("the broker makes a connection");
        let mut stub = Self::over(connection.worker_end, spec).await;
        let welcome = stub.next_frame().await;
        assert_eq!(welcome["type"], "welcome", "the hello is answered");
        assert_eq!(welcome["name"], connection.name);
        stub.name = connection.name;
        stub
    }

    /// Say hello over `worker_end` without waiting for the answer.
    async fn over(worker_end: std::os::unix::net::UnixStream, card_name: &str) -> Self {
        worker_end.set_nonblocking(true).unwrap();
        let stream = UnixStream::from_std(worker_end).unwrap();
        let (read, write) = stream.into_split();
        let mut stub = Self {
            name: String::new(),
            lines: BufReader::new(read).lines(),
            write,
        };

        stub.send(json!({
            "type": "hello",
            "card": {
                "name": card_name,
                "description": "a stub worker",
                "version": "0.1.0",
                "skills": [{ "name": "echo", "description": "repeats the task" }],
            }
        }))
        .await;
        stub
    }

    /// Send `frame` as v2.
    async fn send(&mut self, mut frame: Value) {
        frame["v"] = json!(2);
        self.send_raw(frame).await;
    }

    /// Send `frame` exactly as given.
    async fn send_raw(&mut self, frame: Value) {
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
        let frame: Value = serde_json::from_str(&line).expect("the broker sends JSON");
        assert_eq!(frame["v"], 2, "every broker frame is v2: {frame}");
        frame
    }

    /// The next line, or `None` once the broker has closed the connection.
    async fn next_line(&mut self) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("the broker answers or closes within five seconds")
            .expect("the socket is readable")
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
    let mut stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    let name = stub.name.clone();

    let stub_task = tokio::spawn(async move {
        let prompt = stub.answer_one_task("the file defines Foo").await;
        (stub, prompt)
    });

    let client = A2aClient::new();
    let mut stream = client
        .send_message_stream(&harness.agent(&name), "summarise foo.rs")
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
    let mut stub = StubParticipant::connect(&harness.participants, "asking-worker").await;
    let name = stub.name.clone();

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
    let agent = harness.agent(&name);
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
    harness.await_deregistration(&name).await;
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
    let mut stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    let name = stub.name.clone();
    let stub_task = tokio::spawn(async move { stub.answer_one_task("42").await });

    let answer = A2aClient::new()
        .send_message(&harness.agent(&name), "what is six times seven")
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

    let mut stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    let name = stub.name.clone();
    let stub_task = tokio::spawn(async move { stub.answer_one_task_frame("ok").await });
    let mut agent = harness.agent(&name);
    // `invoke_agent` puts `api_key` on the request as `Authorization: Bearer`.
    agent.api_key = Some("eyJ.user.token".to_string());
    A2aClient::new()
        .send_message(&agent, "who am I")
        .await
        .expect("message/send succeeds");
    let task = stub_task.await.unwrap();
    assert_eq!(task["bearer"], "eyJ.user.token");

    let mut stub = StubParticipant::connect(&harness.participants, "other-worker").await;
    let name = stub.name.clone();
    let stub_task = tokio::spawn(async move { stub.answer_one_task_frame("ok").await });
    A2aClient::new()
        .send_message(&harness.agent(&name), "who am I")
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
    let mut stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    let name = stub.name.clone();

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
        A2aClient::new().send_message(&harness.agent(&name), "migrate the schema"),
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
    harness.await_deregistration(&name).await;
}

/// The card a participant published at registration is served at its
/// well-known path, and `A2aClient` reads it as any other agent's.
#[tokio::test]
async fn a_participants_card_is_served_and_lists_it_on_the_aggregated_card() {
    let harness = Harness::start().await;
    let _stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    let name = _stub.name.clone();

    let card = A2aClient::new()
        .fetch_agent_card(&harness.agent(&name))
        .await
        .expect("the participant's card is served");
    assert_eq!(card.name, name);
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
        names.contains(&name.as_str()),
        "the gateway's aggregated card lists what it can address: {names:?}"
    );
}

/// ADR-0011 C10: `with_virtual_agent` is additive. Two named runners are
/// each served at `/a2a/{name}` with their own card, and both are on the
/// aggregated card as this machine's — which is what `list_agents` reads.
#[tokio::test]
async fn several_virtual_agents_are_each_served_by_name_and_all_listed() {
    use chatty_protocol_gateway::participant::LocalRunner;

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
        let runner = LocalRunner::new("/bin/sh", gateway.participants())
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
    let stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    let name = stub.name.clone();
    assert_eq!(name, "stub-worker-0");
    assert!(harness.participants.is_registered(&name));

    drop(stub);
    harness.await_deregistration(&name).await;

    // A replacement is a new node with a new name: a name that has meant one
    // node never means another (ADR-0020).
    let replacement = StubParticipant::connect(&harness.participants, "stub-worker").await;
    assert_eq!(replacement.name, "stub-worker-1");
    assert!(!harness.participants.is_registered(&name));
}

/// A participant that dies mid-task fails that task rather than leaving the
/// caller hanging on a process that no longer exists.
#[tokio::test]
async fn a_participant_that_dies_mid_task_fails_its_open_task() {
    let harness = Harness::start().await;
    let mut stub = StubParticipant::connect(&harness.participants, "flaky-worker").await;
    let name = stub.name.clone();

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
        .send_message_stream(&harness.agent(&name), "run something")
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
    harness.await_deregistration(&name).await;
}

/// Read the shared socket's answer to `first_line`: an `error` frame, then
/// the connection closed.
async fn shared_socket_reply(socket: &PathBuf, first_line: &str) -> Value {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    write.write_all(first_line.as_bytes()).await.unwrap();
    write.write_all(b"\n").await.unwrap();

    let reply: Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("the socket answers before it closes"),
    )
    .unwrap();
    assert!(
        lines.next_line().await.unwrap().is_none(),
        "and then closes the connection"
    );
    reply
}

/// ADR-0020, invariant 1: nothing registers on the shared socket — not a v1
/// `register` for a name the broker is about to hand out, not a v2 `hello`
/// — and the name stays the broker's to give.
#[tokio::test]
async fn nothing_registers_on_the_shared_socket() {
    let harness = Harness::start().await;

    let v1 = shared_socket_reply(
        &harness.socket,
        r#"{"type":"register","card":{"name":"stub-worker-0"}}"#,
    )
    .await;
    assert_eq!(v1["v"], 2);
    assert_eq!(v1["type"], "error");
    assert!(v1["reason"].as_str().unwrap().contains("v2"), "{v1}");

    let hello = shared_socket_reply(
        &harness.socket,
        r#"{"v":2,"type":"hello","card":{"name":"stub-worker-0"}}"#,
    )
    .await;
    assert_eq!(hello["type"], "error");
    assert!(
        hello["reason"]
            .as_str()
            .unwrap()
            .contains("made by the broker"),
        "{hello}"
    );

    assert!(harness.participants.names().is_empty());
    let stub = StubParticipant::connect(&harness.participants, "stub-worker").await;
    assert_eq!(stub.name, "stub-worker-0", "the name was never taken");
}

/// ADR-0020, invariant 3: the name in a registering card has no effect. A
/// v2 worker whose card says `evil` is admitted, listed and served under
/// the name the broker assigned.
#[tokio::test]
async fn card_name_is_ignored() {
    let harness = Harness::start().await;
    let connection = open_connection(&harness.participants, "local-coder").unwrap();
    let assigned = connection.name.clone();
    let mut stub = StubParticipant::over(connection.worker_end, "evil").await;

    let welcome = stub.next_frame().await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["name"], "local-coder-0");
    assert_eq!(welcome["name"], assigned);
    assert_eq!(welcome["scope"], "root");
    assert!(welcome["owner"].is_null(), "the root asked for it");

    assert_eq!(harness.participants.names(), vec![assigned.clone()]);
    assert!(!harness.participants.is_registered("evil"));
    let card = A2aClient::new()
        .fetch_agent_card(&harness.agent(&assigned))
        .await
        .expect("the card is served under the assigned name");
    assert_eq!(card.name, "local-coder-0");
    assert_eq!(
        card.description, "a stub worker",
        "the rest of the card stands"
    );
    let status = reqwest::get(format!(
        "{}/a2a/evil/.well-known/agent.json",
        harness.base_url
    ))
    .await
    .unwrap()
    .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "`evil` is nobody");
}

/// A frame without `v` gets an error frame naming v2 and the connection is
/// closed — as a worker's first frame, and mid-session.
#[tokio::test]
async fn v1_frame_is_refused() {
    let harness = Harness::start().await;

    // A v1 `register` on a broker-made connection.
    let connection = open_connection(&harness.participants, "old-worker").unwrap();
    let name = connection.name.clone();
    connection.worker_end.set_nonblocking(true).unwrap();
    let (read, mut write) = UnixStream::from_std(connection.worker_end)
        .unwrap()
        .into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"{\"type\":\"register\",\"card\":{\"name\":\"old-worker\"}}\n")
        .await
        .unwrap();
    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(reply["v"], 2);
    assert_eq!(reply["type"], "error");
    assert!(reply["reason"].as_str().unwrap().contains("v2"), "{reply}");
    assert!(lines.next_line().await.unwrap().is_none(), "then closed");
    assert!(!harness.participants.is_registered(&name));

    // A welcomed worker that drops `v` from a later frame is closed too.
    let mut stub = StubParticipant::connect(&harness.participants, "drifting-worker").await;
    let name = stub.name.clone();
    stub.send_raw(json!({ "type": "status", "taskId": "t", "state": "working" }))
        .await;
    let reply = stub.next_frame().await;
    assert_eq!(reply["type"], "error");
    assert!(reply["reason"].as_str().unwrap().contains("v2"), "{reply}");
    assert!(stub.next_line().await.is_none(), "then closed");
    harness.await_deregistration(&name).await;
}

/// A stand-in worker in `sh`, on the connection the runner hands it at
/// descriptor 3: it says hello, reads its welcome and its task, answers
/// `done`, and waits to be reaped.
const ANSWERING_WORKER: &str = r#"printf '{"v":2,"type":"hello"}\n' >&3
read -r welcome <&3
read -r task <&3
id=$(printf '%s' "$task" | sed 's/.*"taskId":"\([^"]*\)".*/\1/')
printf '{"v":2,"type":"artifact","taskId":"%s","text":"done","lastChunk":true}\n' "$id" >&3
printf '{"v":2,"type":"status","taskId":"%s","state":"completed"}\n' "$id" >&3
exec sleep 30"#;

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
async fn start_evidence_runner() -> (tempfile::TempDir, String) {
    use chatty_protocol_gateway::participant::{LocalRunner, TaskEvidence, WorkerWorkspace};

    let dir = tempfile::tempdir().expect("a temp dir for the worker");
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let modules = Arc::new(RwLock::new(
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
    ));
    let mut gateway = ProtocolGateway::new(modules, 0);
    let participants = gateway.participants();

    let cwd = dir.path().to_path_buf();
    let runner = LocalRunner::new("/bin/sh", participants.clone())
        .with_args(["-c", ANSWERING_WORKER])
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

    (dir, base_url)
}

/// ADR-0011 C12 / AGE-406, Do item 4, on `message/send`: the envelope is a
/// structured field on the task result as well as a fenced block in the
/// answer, so a trace — or Harbor's ATIF — reads it without parsing prose.
#[tokio::test]
async fn message_send_carries_the_evidence_envelope_as_prose_and_as_a_field() {
    let (_dir, base_url) = start_evidence_runner().await;

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
    let (_dir, base_url) = start_evidence_runner().await;

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
