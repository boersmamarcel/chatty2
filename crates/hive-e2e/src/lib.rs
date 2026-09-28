//! The chatty × Hive contract and trust suite (PL-E6, evaluation plan §3 S5
//! and S6).
//!
//! Every test in `tests/` runs against a live hive stack — Postgres, MinIO,
//! `hive-registry`, `hive-runner` and a wiremock LLM upstream, brought up by
//! `scripts/hive-e2e.sh` from hive's `docker/docker-compose.yml` (`e2e`
//! profile) and seeded with chatty2's WASM fixtures by hive's
//! `scripts/seed-e2e.sh`. So each is `#[ignore = "needs hive stack; run by
//! nightly"]`; the nightly workflow `plugin-e2e.yml` runs them with
//! `-- --ignored`.
//!
//! The tests assert **correct** behaviour. A row that is red today fails with
//! a message naming the finding and the PL-H issue that fixes it; this crate
//! never works around a contract bug.
//!
//! This library is the harness the tests share:
//!
//! - [`Stack`]: where the stack is (`HIVE_E2E_*` env vars), fresh users and
//!   publishers, publishing, fixtures.
//! - [`install_from_hive`]: the desktop's install flow without the UI.
//! - [`proxy::TamperProxy`]: sits between chatty and the registry and
//!   strips, swaps, corrupts or replays what a download returns, or renames a
//!   module (plan §2 D).
//! - [`mint_session_token`]: a session JWT with a chosen expiry (row 5.3).
//! - [`start_gateway`]: chatty's protocol gateway pointed at the stack's
//!   runner (rows 5.4, 5.10).

pub mod proxy;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chatty_core::install;
use chatty_core::settings::models::extensions_store::{ExtensionsModel, InstalledExtension};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use chrono::{DateTime, Utc};
use hive_client::models::ModuleMetadata;
use hive_client::{HiveRegistryClient, HiveSession, TokenPair};
use hmac::{KeyInit, Mac};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::RwLock;

/// The version `seed-e2e.sh` publishes every fixture at.
pub const SEEDED_VERSION: &str = "0.1.0";

/// The `JWT_SECRET` hive's compose file gives the registry and the runner.
const DEV_JWT_SECRET: &str = "dev-secret-change-in-production";

/// Where the stack is and what it was seeded with.
///
/// | Env var | Default |
/// | -- | -- |
/// | `HIVE_E2E_BASE_URL` | `http://localhost:8080` (the registry) |
/// | `HIVE_E2E_RUNNER_URL` | `http://localhost:8081` |
/// | `HIVE_E2E_FIXTURES` | `<workspace>/target/wasm-fixtures` |
/// | `HIVE_E2E_JWT_SECRET` | hive's compose default |
pub struct Stack {
    pub registry: String,
    pub runner: String,
    pub fixtures: PathBuf,
    pub jwt_secret: String,
    http: reqwest::Client,
}

impl Stack {
    pub fn from_env() -> Self {
        let env = |key: &str, default: &str| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let workspace_fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/wasm-fixtures");
        Self {
            registry: env("HIVE_E2E_BASE_URL", "http://localhost:8080")
                .trim_end_matches('/')
                .to_string(),
            runner: env("HIVE_E2E_RUNNER_URL", "http://localhost:8081")
                .trim_end_matches('/')
                .to_string(),
            fixtures: std::env::var_os("HIVE_E2E_FIXTURES")
                .map(PathBuf::from)
                .unwrap_or(workspace_fixtures),
            jwt_secret: env("HIVE_E2E_JWT_SECRET", DEV_JWT_SECRET),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("an HTTP client"),
        }
    }

    /// `<registry>/api<path>`.
    pub fn api(&self, path: &str) -> String {
        format!("{}/api{}", self.registry, path)
    }

    /// The bytes of fixture `name` (`<fixtures>/<name>/<name>.wasm`).
    pub fn fixture(&self, name: &str) -> Vec<u8> {
        let path = self.fixtures.join(name).join(format!("{name}.wasm"));
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "fixture {}: {e}; build them with scripts/build-wasm-fixtures.sh",
                path.display()
            )
        })
    }

    /// A raw request to the registry, from an address of its own: the
    /// registry rate-limits auth by `X-Forwarded-For` (20 a minute), and the
    /// suite registers more users than that.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.api(path))
            .header("x-forwarded-for", random_ip())
    }

    /// A freshly registered user.
    pub async fn user(&self) -> User {
        let id = short_id();
        let username = format!("e2e-{id}");
        let email = format!("{username}@example.com");
        let password = format!("e2e-password-{id}");
        let (status, body) = send(
            self.request(reqwest::Method::POST, "/auth/register")
                .json(&json!({ "username": username, "email": email, "password": password })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "register {username}: {body}");
        User {
            username,
            email,
            password,
            pair: serde_json::from_value(body).expect("register returns a token pair"),
        }
    }

    /// A freshly registered user upgraded to publisher.
    pub async fn publisher(&self) -> User {
        let mut user = self.user().await;
        let (status, body) = send(
            self.request(reqwest::Method::PUT, "/me/role/publisher")
                .bearer_auth(&user.pair.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "publisher upgrade: {body}");
        user.pair.token = body["token"].as_str().expect("a token").to_string();
        user
    }

    /// `POST /api/modules` with `manifest` (TOML) and `wasm`.
    pub async fn publish(
        &self,
        publisher: &User,
        manifest: &str,
        wasm: Vec<u8>,
    ) -> (StatusCode, Value) {
        let form = reqwest::multipart::Form::new()
            .text("manifest", manifest.to_string())
            .part(
                "wasm",
                reqwest::multipart::Part::bytes(wasm)
                    .file_name("module.wasm")
                    .mime_str("application/wasm")
                    .expect("a mime type"),
            );
        send(
            self.request(reqwest::Method::POST, "/modules")
                .bearer_auth(&publisher.pair.token)
                .multipart(form),
        )
        .await
    }

    /// Publish and insist it worked; returns the publish response.
    pub async fn publish_ok(&self, publisher: &User, manifest: &str, wasm: Vec<u8>) -> Value {
        let (status, body) = self.publish(publisher, manifest, wasm).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "publish failed: {body}\n{manifest}"
        );
        body
    }

    /// `DELETE /api/modules/{name}/{version}`.
    pub async fn delete_version(
        &self,
        publisher: &User,
        name: &str,
        version: &str,
    ) -> (StatusCode, Value) {
        send(
            self.request(
                reqwest::Method::DELETE,
                &format!("/modules/{name}/{version}"),
            )
            .bearer_auth(&publisher.pair.token),
        )
        .await
    }

    /// `PUT /api/modules/{name}/pricing`.
    pub async fn set_pricing(&self, publisher: &User, name: &str, price_per_call: f64) {
        let (status, body) = send(
            self.request(reqwest::Method::PUT, &format!("/modules/{name}/pricing"))
                .bearer_auth(&publisher.pair.token)
                .json(&json!({ "price_per_call": price_per_call, "free_tier_calls": 0 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "set pricing on {name}: {body}");
    }

    /// `GET /api/modules/{name}` as JSON.
    pub async fn module_json(&self, name: &str) -> (StatusCode, Value) {
        send(self.request(reqwest::Method::GET, &format!("/modules/{name}"))).await
    }
}

/// A registered user and the token pair registration returned. After
/// [`Stack::publisher`], `pair.token` is the publisher token; the refresh
/// token is still registration's.
pub struct User {
    pub username: String,
    pub email: String,
    pub password: String,
    pub pair: TokenPair,
}

impl User {
    /// A session holding this user's pair, against `base`.
    pub fn session(&self, base: &str) -> Arc<HiveSession> {
        Arc::new(HiveSession::new(base, Some(self.pair.clone())))
    }

    /// A hive-client signed in as this user, against `base` (the registry,
    /// or a [`proxy::TamperProxy`] in front of it).
    pub fn client(&self, base: &str) -> HiveRegistryClient {
        HiveRegistryClient::new(base).with_session(self.session(base))
    }
}

/// A flat manifest (the only form the registry parses today) for `name` at
/// `version`, plus any extra TOML appended verbatim.
pub fn flat_manifest(name: &str, version: &str, extra: &str) -> String {
    format!(
        "name = \"{name}\"\ndisplay_name = \"{name}\"\ndescription = \"PL-E6 e2e module {name}\"\n\
         version = \"{version}\"\nlicense = \"MIT\"\ncategory = \"developer-tools\"\n{extra}"
    )
}

/// A module name no other test run used: `<prefix>-<8 hex>`.
pub fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", short_id())
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

fn random_ip() -> String {
    let b = uuid::Uuid::new_v4().into_bytes();
    format!("10.{}.{}.{}", b[0], b[1], b[2].max(1))
}

/// Send and read the body as JSON (`Value::Null` when it is empty or not
/// JSON, e.g. a 204).
pub async fn send(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
    let resp = request.send().await.expect("the registry answers");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

// ── The desktop's install flow ────────────────────────────────────────────

/// chatty's data directory for this run: `$XDG_DATA_HOME` (Linux; what
/// `dirs::data_dir` reads), which the suite requires to be set so installs
/// land in a scratch directory and never in the developer's real
/// `~/.local/share/chatty/modules`.
pub fn data_home() -> PathBuf {
    let dir = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .expect("set XDG_DATA_HOME to a scratch directory: the suite installs modules into it");
    let real = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"));
    assert!(
        Some(&dir) != real.as_ref(),
        "XDG_DATA_HOME is the real data directory; point it at a scratch directory"
    );
    std::fs::create_dir_all(&dir).expect("XDG_DATA_HOME is creatable");
    dir
}

/// Where chatty installs modules (under [`data_home`]).
pub fn module_dir() -> PathBuf {
    data_home();
    PathBuf::from(chatty_core::settings::models::module_settings::default_module_dir())
}

/// Install `meta` at `version` the way the desktop's marketplace does
/// (`chatty-gpui` `extensions_controller::install_extension`), into
/// [`module_dir`]: a remote module gets a `module.toml` built from its
/// version manifest; a local one is downloaded by
/// `chatty_core::install::download_wasm_module` (name and version validated,
/// body capped at 64 MiB while it streams, hash and signature checked by
/// hive-client) and written, with its install record, by
/// `chatty_core::install::install_wasm_module`. Any refusal, from either
/// layer, is the `Err`.
pub async fn install_from_hive(
    client: &HiveRegistryClient,
    meta: &ModuleMetadata,
    version: &str,
) -> Result<InstalledExtension, String> {
    let module_dir = module_dir();
    let mut extensions = ExtensionsModel::default();
    if matches!(meta.execution_mode.as_str(), "remote" | "remote_only") {
        let manifest = client
            .list_versions(&meta.name)
            .await
            .map_err(|e| format!("list versions: {e}"))?
            .items
            .into_iter()
            .find(|v| v.version == version)
            .map(|v| v.manifest)
            .unwrap_or_default();
        install::install_remote_module(
            &meta.name,
            version,
            &meta.display_name,
            &meta.description,
            &meta.pricing_model,
            &manifest,
            &module_dir,
            &mut extensions,
        )
        .map_err(|e| format!("install: {e}"))
    } else {
        let download = install::download_wasm_module(client, &meta.name, version, |_, _| {})
            .await
            .map_err(|e| format!("download: {e}"))?;
        install::install_wasm_module(
            &download,
            &meta.name,
            version,
            &meta.display_name,
            &meta.description,
            &meta.pricing_model,
            &module_dir,
            &mut extensions,
        )
        .map_err(|e| format!("install: {e}"))
    }
}

// ── Tokens ────────────────────────────────────────────────────────────────

/// A copy of session JWT `template` (its claims: user, role, scope,
/// audience) re-signed with `secret` so that it expires at `expires_at`.
/// Stands in for "wait an hour" in row 5.3.
pub fn mint_session_token(template: &str, secret: &str, expires_at: DateTime<Utc>) -> String {
    let payload = template.split('.').nth(1).expect("a JWT");
    let mut claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).expect("base64url claims"))
            .expect("JSON claims");
    claims["exp"] = json!(expires_at.timestamp());
    claims["iat"] = json!((expires_at - chrono::TimeDelta::hours(1)).timestamp());
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signing_input = format!("{header}.{claims}");
    let mut mac =
        hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(signing_input.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{signing_input}.{signature}")
}

// ── The gateway ───────────────────────────────────────────────────────────

/// A host LLM that is never reached: remote modules run on the runner, whose
/// `llm::complete` goes to the stack's wiremock upstream.
struct NoLocalLlm;

impl LlmProvider for NoLocalLlm {
    fn complete(
        &self,
        _: &str,
        _: Vec<Message>,
        _: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("hive-e2e: no local LLM; remote modules call the runner's upstream".to_string())
    }
}

/// chatty's protocol gateway with no local modules, forwarding remote ones
/// to `runner` as the user `client` is signed in as. Returns its base URL
/// (an ephemeral loopback port).
pub async fn start_gateway(client: Arc<HiveRegistryClient>, runner: &str) -> String {
    let registry = ModuleRegistry::new(Arc::new(NoLocalLlm), ResourceLimits::default())
        .expect("an empty module registry");
    let gateway = ProtocolGateway::new(Arc::new(RwLock::new(registry)), 0)
        .with_hive_client(client)
        .with_runner_url(runner);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    base
}

/// A module registry (the desktop's loader) with no LLM behind it.
pub fn local_module_registry() -> ModuleRegistry {
    ModuleRegistry::new(Arc::new(NoLocalLlm), ResourceLimits::default()).expect("a module registry")
}

/// Send one A2A `message/send` for the remote `module` through the gateway,
/// which forwards it to the runner (the one remote path left until PL-H8b
/// removes it; the OpenAI route went with `chat`, PL-U3). The status and the
/// answer's text (or the whole body when there is none).
pub async fn send_via_gateway(gateway: &str, module: &str, prompt: &str) -> (StatusCode, String) {
    let (status, body) = send(
        reqwest::Client::new()
            .post(format!("{gateway}/a2a/{module}"))
            .timeout(Duration::from_secs(120))
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "message/send",
                "params": { "message": { "parts": [{ "kind": "text", "text": prompt }] } },
            })),
    )
    .await;
    let content = body
        .pointer("/result/artifacts/0/parts/0/text")
        .and_then(|text| text.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string());
    (status, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_token_keeps_the_claims_and_takes_the_new_expiry() {
        let claims = json!({ "sub": "u1", "username": "alice", "scope": "modules:read", "aud": "chatty", "iat": 1, "exp": 2 });
        let template = format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let expires_at = DateTime::from_timestamp(1_900_000_000, 0).unwrap();
        let token = mint_session_token(&template, "secret", expires_at);

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let minted: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(minted["exp"], 1_900_000_000);
        assert_eq!(minted["iat"], 1_900_000_000 - 3600);
        assert_eq!(minted["username"], "alice");
        assert_eq!(minted["aud"], "chatty");

        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
        assert_eq!(
            URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            mac.finalize().into_bytes().to_vec()
        );
    }

    #[test]
    fn flat_manifest_is_the_registry_shape() {
        let manifest: toml::Table =
            flat_manifest("abc-e2e", "1.2.3", "execution_mode = \"remote\"\n")
                .parse()
                .unwrap();
        assert_eq!(manifest["name"].as_str(), Some("abc-e2e"));
        assert_eq!(manifest["version"].as_str(), Some("1.2.3"));
        assert_eq!(manifest["execution_mode"].as_str(), Some("remote"));
    }
}
