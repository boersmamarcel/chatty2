//! A gateway on an ephemeral port, loaded with real fixture modules, plus
//! the few helpers every row needs.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::test_support::{FakeLlm, FakeResponse, fixture_path};
use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Which modules a gateway loads
// ---------------------------------------------------------------------------

/// One module to load. The two real plugins load from their shipped
/// `module.toml`; a fixture is staged under a manifest of its own, because
/// the fixtures declare no `[protocols]` and would be unreachable once the
/// gateway enforces the `mcp` flag (PL-H4).
pub struct Module {
    fixture: &'static str,
    name: Option<&'static str>,
    mcp: Option<bool>,
    /// What the user granted it in Settings (SEC-11): `.chatty-grants.json`.
    grants: Vec<&'static str>,
}

impl Module {
    /// `echo` or `benford`, exactly as shipped.
    pub fn shipped(fixture: &'static str) -> Self {
        Self {
            fixture,
            name: None,
            mcp: None,
            grants: Vec::new(),
        }
    }

    /// A PL-E1 fixture served over MCP.
    pub fn fixture(fixture: &'static str) -> Self {
        Self {
            fixture,
            name: None,
            mcp: Some(true),
            grants: Vec::new(),
        }
    }

    /// Serve this module's wasm under another name.
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    /// The user granted this module `capability` (`llm`, `file`):
    /// a module served with no spec gets none of them otherwise (SEC-11).
    pub fn granted(mut self, capability: &'static str) -> Self {
        self.grants.push(capability);
        self
    }

    /// `[protocols] mcp`, the one protocol a plugin is served on (PL-U3).
    pub fn mcp(mut self, mcp: bool) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// The directory the registry loads: the staged fixture itself, or a
    /// copy of its wasm under `root` with a written manifest.
    fn directory(&self, root: &Path) -> PathBuf {
        let wasm = fixture_path(self.fixture);
        let shipped = wasm
            .parent()
            .expect("a fixture has a directory")
            .to_path_buf();
        let (Some(mcp), name) = (self.mcp, self.name.unwrap_or(self.fixture)) else {
            assert!(self.name.is_none(), "a renamed module needs a manifest");
            return shipped;
        };

        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("a module directory");
        let wasm_file = format!("{}.wasm", self.fixture);
        std::fs::copy(&wasm, dir.join(&wasm_file)).expect("the fixture wasm copies");
        let manifest = format!(
            "[module]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
             description = \"conformance copy of {fixture}\"\nwasm = \"{wasm_file}\"\n\n\
             [protocols]\nmcp = {mcp}\n",
            fixture = self.fixture,
        );
        std::fs::write(dir.join("module.toml"), manifest).expect("the manifest writes");
        if !self.grants.is_empty() {
            let grants = serde_json::json!({ "granted": self.grants });
            std::fs::write(dir.join(".chatty-grants.json"), grants.to_string())
                .expect("the grants write");
        }
        dir
    }
}

// ---------------------------------------------------------------------------
// The gateway under test
// ---------------------------------------------------------------------------

pub struct Gateway {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    /// The one provider behind every module's `llm::complete`: what a guest
    /// forwards to it is what the guest itself received.
    pub llm: Arc<FakeLlm>,
    /// Who is connected, and where a test admits a participant (3.14).
    #[cfg(unix)]
    pub participants: chatty_protocol_gateway::participant::ParticipantRegistry,
    pub http: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Gateway {
    /// Load `modules`, script the fake LLM, and serve on an ephemeral port.
    pub async fn start(modules: Vec<Module>, script: Vec<FakeResponse>) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let llm = Arc::new(FakeLlm::new(script));
        let provider: Arc<dyn LlmProvider> = llm.clone();
        let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
        for module in &modules {
            let path = module.directory(dir.path());
            registry
                .load(&path)
                .unwrap_or_else(|e| panic!("{} loads: {e:#}", path.display()));
        }

        let gateway = ProtocolGateway::new(Arc::new(RwLock::new(registry)));

        #[cfg(unix)]
        let participants = gateway.participants();

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port");
        let base = format!("http://{}", tcp.local_addr().unwrap());
        let router = with_launch_token(&gateway);
        tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        Self {
            base,
            llm,
            #[cfg(unix)]
            participants,
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap(),
            _dir: dir,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// POST `body` as JSON; the status and the body parsed as JSON
    /// (`Value::Null` when it is not JSON).
    pub async fn post(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let resp = self
            .http
            .post(self.url(path))
            .json(body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path}: {e}"));
        let status = resp.status();
        let bytes = resp.bytes().await.unwrap_or_default();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

// ---------------------------------------------------------------------------
// Real third-party clients
// ---------------------------------------------------------------------------

/// `tool` on `PATH`, if it is there.
pub fn on_path(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
}

/// Say why a real-client test did not run. Written straight to stderr, which
/// the test harness does not capture, so the reason shows in every run
/// rather than only in a failing one.
pub fn skip(row: &str, reason: &str) {
    let _ = writeln!(std::io::stderr(), "SKIP {row}: {reason}");
}

/// A measurement worth seeing in a green run too (3.11), uncaptured like
/// [`skip`].
pub fn record(row: &str, line: &str) {
    let _ = writeln!(std::io::stderr(), "{row}: {line}");
}

/// Run a client process to completion within `limit`. Its package fetch
/// (npx) is the only network a test here does.
pub async fn run_client(
    row: &str,
    mut command: tokio::process::Command,
    limit: Duration,
) -> std::process::Output {
    command
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    let output = tokio::time::timeout(limit, command.output())
        .await
        .unwrap_or_else(|_| panic!("{row}: the client did not finish within {limit:?}"))
        .unwrap_or_else(|e| panic!("{row}: the client did not start: {e}"));
    assert!(
        output.status.success(),
        "{row}: the client failed ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

/// The gateway's router with its launch token added to every request: this
/// test's own listener stands in for a caller that holds the token (EN-0d).
pub fn with_launch_token(gateway: &ProtocolGateway) -> axum::Router {
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
