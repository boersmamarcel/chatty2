//! BI-7 (AGE-639): once a worker calls over its own connection (BI-4), the
//! gateway's loopback HTTP route stops serving roles. A role, a node or the
//! swarm directory returns 403; module routes and the remote-runner forward
//! (PL-H8b) are unaffected. The deleted `x-chatty-broker-caller` header
//! (AGE-628, #915) has no effect any more: identity comes from the
//! connection, not from a claim on this route.

use std::sync::Arc;
use std::time::Duration;

use chatty_fabric::EdgeLog;
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::open_connection;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

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

/// A gateway with one registered role (`stub-worker-0`), a real port, and an
/// edge log a test can read back.
struct Harness {
    base_url: String,
    role: String,
    edges_path: std::path::PathBuf,
    /// Kept alive so the role stays registered: dropping it closes the
    /// connection, which deregisters the node.
    _worker: UnixStream,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let socket = dir.path().join("run").join("participants.sock");

        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let modules = Arc::new(RwLock::new(
            ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
        ));
        let edge_log = EdgeLog::open(dir.path()).expect("the edge log opens");
        let edges_path = edge_log.path();
        let gateway = ProtocolGateway::new(modules)
            .with_participant_socket(&socket)
            .with_edge_log(edge_log);
        let participants = gateway.participants();

        // A role the loopback route must refuse: a connection the broker
        // made for it, exactly as a real worker gets (ADR-0020), welcomed
        // over the v3 protocol so it is actually live in the registry.
        let connection = open_connection(&participants, "stub-worker", None).expect("a connection");
        connection.worker_end.set_nonblocking(true).unwrap();
        let mut worker = UnixStream::from_std(connection.worker_end).unwrap();
        let hello = json!({
            "v": 3,
            "id": 1,
            "method": "session.hello",
            "params": {
                "card": {
                    "name": "stub-worker",
                    "description": "a stub worker",
                    "version": "0.1.0",
                    "skills": [],
                },
                "schema": chatty_fabric::wire::schema::hash(),
            }
        });
        worker
            .write_all(format!("{hello}\n").as_bytes())
            .await
            .unwrap();
        {
            let mut lines = BufReader::new(&mut worker).lines();
            let welcome = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .expect("the broker answers hello")
                .unwrap()
                .expect("the socket is readable");
            let welcome: serde_json::Value = serde_json::from_str(&welcome).unwrap();
            assert_eq!(welcome["result"]["name"], "stub-worker-0", "{welcome}");
        }

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port");
        let base_url = format!("http://{}", tcp.local_addr().unwrap());
        let router = with_launch_token(&gateway);
        tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        Self {
            base_url,
            role: connection.name,
            edges_path,
            _worker: worker,
            _dir: dir,
        }
    }

    /// Every row the edge log has written so far.
    fn edge_rows(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.edges_path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("an edge-log row is JSON"))
            .collect()
    }
}

/// The exact body every refusal answers with.
fn refusal_body() -> Value {
    json!({ "error": "fabric: roles are reached over the worker connection" })
}

/// `loopback_refuses_roles`: a role's card, a role's JSON-RPC route and the
/// swarm directory all return 403 with the same body, and each refusal
/// writes one edge-log row.
#[tokio::test]
async fn loopback_refuses_roles() {
    let harness = Harness::start().await;
    let client = reqwest::Client::new();

    // The role's card.
    let card = client
        .get(format!(
            "{}/a2a/{}/.well-known/agent.json",
            harness.base_url, harness.role
        ))
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(card.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        card.json::<Value>().await.expect("a JSON body"),
        refusal_body()
    );

    // The role's JSON-RPC route.
    let rpc = client
        .post(format!("{}/a2a/{}", harness.base_url, harness.role))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": { "message": { "parts": [{ "type": "text", "text": "hello" }] } },
        }))
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(rpc.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        rpc.json::<Value>().await.expect("a JSON body"),
        refusal_body()
    );

    // The swarm directory: this gateway has a role, so it is not disclosed.
    let directory = client
        .get(format!("{}/.well-known/agent.json", harness.base_url))
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(directory.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        directory.json::<Value>().await.expect("a JSON body"),
        refusal_body()
    );

    let rows = harness.edge_rows();
    let refusal_rows: Vec<&Value> = rows.iter().filter(|row| row["kind"] == "refusal").collect();
    assert_eq!(
        refusal_rows.len(),
        3,
        "one refusal row per refused request: {rows:?}"
    );
    for row in refusal_rows {
        assert!(
            row["outcome"]
                .as_str()
                .unwrap()
                .contains("roles are reached over the worker connection"),
            "{row}"
        );
    }
}

/// Do: delete the `x-chatty-broker-caller` header handling from #915.
/// Sending it changes nothing — a role is refused exactly the same with or
/// without it, because identity comes from the connection, not from a
/// header on this route.
#[tokio::test]
async fn loopback_ignores_caller_header() {
    let harness = Harness::start().await;
    let client = reqwest::Client::new();

    let with_header = client
        .post(format!("{}/a2a/{}", harness.base_url, harness.role))
        .header("x-chatty-broker-caller", "some-other-node")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": { "message": { "parts": [{ "type": "text", "text": "hello" }] } },
        }))
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(with_header.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        with_header.json::<Value>().await.expect("a JSON body"),
        refusal_body(),
        "the header claims a caller identity; the response is the plain refusal regardless"
    );
}

/// The gateway's router with its launch token added to every request: this
/// test's own listener stands in for a caller that holds the token (EN-0d).
fn with_launch_token(gateway: &ProtocolGateway) -> axum::Router {
    let bearer: axum::http::HeaderValue = format!("Bearer {}", gateway.token().as_str())
        .parse()
        .expect("a token is a valid header value");
    gateway
        .build_router()
        .layer(tower::util::MapRequestLayer::new(
            move |mut request: axum::extract::Request| {
                request
                    .headers_mut()
                    .insert(axum::http::header::AUTHORIZATION, bearer.clone());
                request
            },
        ))
}
