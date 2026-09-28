//! AGE-300 / ADR-0011 C1: a worker on a connection the broker made for it
//! registers over that connection; ADR-0020: the connection names it, and
//! nothing registers on the shared socket.
//!
//! Serving a registered participant's task over loopback A2A JSON-RPC/SSE —
//! what this file tested until BI-7 — was retired: a role is reached over
//! the connection the broker made for its caller now
//! (`chatty_protocol_gateway::participant::BrokerCalls`), never over the
//! gateway's HTTP side (`crates/chatty-protocol-gateway/tests/
//! loopback_scope.rs` covers that refusal). What is left here is the wire
//! protocol itself: registration, the shared socket's refusal, and v1/v2
//! framing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::{ParticipantRegistry, open_connection};
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
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
            open_connection(participants, spec, None).expect("the broker makes a connection");
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
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
/// the name the broker assigned — checked on the registry directly, since
/// a role's card is no longer served over loopback (BI-7).
#[tokio::test]
async fn card_name_is_ignored() {
    let harness = Harness::start().await;
    let connection = open_connection(&harness.participants, "local-coder", None).unwrap();
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
    let card = harness
        .participants
        .card(&assigned)
        .expect("the card is served under the assigned name");
    assert_eq!(card.name, "local-coder-0");
    assert_eq!(
        card.description, "a stub worker",
        "the rest of the card stands"
    );
    assert!(
        harness.participants.card("evil").is_none(),
        "`evil` is nobody"
    );

    // The loopback route itself refuses `local-coder-0` — a registered role
    // — exactly as it refuses `evil`, which was never a name at all
    // (`loopback_scope.rs` pins the 403; this only re-confirms `evil` is
    // absent regardless of route).
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
    let connection = open_connection(&harness.participants, "old-worker", None).unwrap();
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
