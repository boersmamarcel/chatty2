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
//! - [`Stack::step_up`]: a step-up assertion, approved the way a person does
//!   on the registry's `/step-up` page (CX-0).
//! - [`start_gateway`]: chatty's protocol gateway pointed at the stack's
//!   runner (rows 5.4, 5.10).

pub mod proxy;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chatty_core::install;
use chatty_core::settings::models::extensions_store::{ExtensionsModel, InstalledExtension};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use hive_client::models::ModuleMetadata;
use hive_client::{HiveRegistryClient, HiveSession, TokenPair};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::RwLock;

/// The version `seed-e2e.sh` publishes every fixture at.
pub const SEEDED_VERSION: &str = "0.1.0";

/// Where the stack is and what it was seeded with.
///
/// | Env var | Default |
/// | -- | -- |
/// | `HIVE_E2E_BASE_URL` | `http://localhost:8080` (the registry) |
/// | `HIVE_E2E_RUNNER_URL` | `http://localhost:8081` |
/// | `HIVE_E2E_FIXTURES` | `<workspace>/target/wasm-fixtures` |
/// | `CHATTY_HIVE_ROOT_KEY` | none: required (the stack's dev root public key) |
///
/// `CHATTY_HIVE_ROOT_KEY` is the key chatty itself reads
/// ([`hive_client::trust`]): every `HiveRegistryClient` the suite builds for
/// the local stack, or a proxy in front of it, trusts it. It is required so
/// that the refusal rows (6.1, 6.2, 6.6) cannot pass merely because no root
/// is trusted at all. `scripts/hive-e2e.sh` reads it from the registry's
/// startup log (`root_public_key=<hex>`).
pub struct Stack {
    pub registry: String,
    /// The registry root public key (hex) chatty trusts for this stack.
    pub root_key: String,
    pub runner: String,
    pub fixtures: PathBuf,
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
        let registry = env("HIVE_E2E_BASE_URL", "http://localhost:8080")
            .trim_end_matches('/')
            .to_string();
        let root_key = std::env::var(hive_client::trust::ROOT_KEY_ENV).unwrap_or_default();
        assert!(
            !root_key.trim().is_empty(),
            "set {} to the stack's dev root public key (scripts/hive-e2e.sh reads it from \
             the registry's `root_public_key=` startup log line)",
            hive_client::trust::ROOT_KEY_ENV
        );
        assert!(
            hive_client::trust::is_local_registry(&registry),
            "{registry} is not a local registry, so chatty ignores {}",
            hive_client::trust::ROOT_KEY_ENV
        );
        Self {
            registry,
            root_key,
            runner: env("HIVE_E2E_RUNNER_URL", "http://localhost:8081")
                .trim_end_matches('/')
                .to_string(),
            fixtures: std::env::var_os("HIVE_E2E_FIXTURES")
                .map(PathBuf::from)
                .unwrap_or(workspace_fixtures),
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

    /// A freshly registered user upgraded to publisher, through the
    /// `publisher_role` step-up (CX-0).
    pub async fn publisher(&self) -> User {
        let mut user = self.user().await;
        let (status, me) = send(
            self.request(reqwest::Method::GET, "/me")
                .bearer_auth(&user.pair.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "/api/me: {me}");
        let id = me["id"].as_str().expect("a user id").to_string();
        let assertion = self
            .step_up(&user, "publisher_role", &id, "publisher")
            .await;
        let (status, body) = send(
            self.request(reqwest::Method::PUT, "/me/role/publisher")
                .bearer_auth(&user.pair.token)
                .header(hive_client::client::STEP_UP_HEADER, assertion),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "publisher upgrade: {body}");
        user.pair.token = body["token"].as_str().expect("a token").to_string();
        user
    }

    /// A step-up assertion for `user`'s `action` over `target` and `value`,
    /// got the way a person gets one (CX-0): file the request, open the
    /// registry's `/step-up` page, sign in again with the password, approve
    /// it, and collect the assertion.
    pub async fn step_up(&self, user: &User, action: &str, target: &str, value: &str) -> String {
        let (status, filed) = send(
            self.request(reqwest::Method::POST, "/step-up/requests")
                .bearer_auth(&user.pair.token)
                .json(&json!({ "action": action, "target": target, "value": value })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "file a step-up: {filed}");
        let id = filed["id"].as_str().expect("a request id");
        self.approve_step_up(user, id).await
    }

    /// Approve `user`'s filed step-up request `id` on the registry's page,
    /// as the person does (sign in again, approve), and collect its
    /// assertion.
    pub async fn approve_step_up(&self, user: &User, id: &str) -> String {
        let page = format!("{}/step-up", self.registry);
        let sign_in = self.page(self.http.get(&page)).await;
        let signed_in = self
            .page(form(
                self.http.post(&page),
                &[
                    ("nonce", form_nonce(&sign_in).as_str()),
                    ("email", user.email.as_str()),
                    ("password", user.password.as_str()),
                ],
            ))
            .await;
        assert!(
            signed_in.contains(id),
            "the re-authenticated page lists request {id}: {signed_in}"
        );
        self.page(form(
            self.http.post(format!("{page}/approve")),
            &[
                ("nonce", form_nonce(&signed_in).as_str()),
                ("request_id", id),
            ],
        ))
        .await;

        let (status, approved) = send(
            self.request(reqwest::Method::GET, &format!("/step-up/requests/{id}"))
                .bearer_auth(&user.pair.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "collect the step-up: {approved}");
        assert_eq!(approved["status"], "approved", "{approved}");
        approved["assertion"]
            .as_str()
            .expect("an assertion")
            .to_string()
    }

    /// One `/step-up` page load, as a top-level navigation (the page refuses
    /// to be framed), insisting on a 200.
    async fn page(&self, request: reqwest::RequestBuilder) -> String {
        let response = request
            .header("sec-fetch-dest", "document")
            .header("x-forwarded-for", random_ip())
            .send()
            .await
            .expect("the registry answers");
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert_eq!(status, StatusCode::OK, "step-up page: {text}");
        text
    }

    /// `POST /api/modules` with `manifest` (TOML) and `wasm`, under a
    /// `module_publish` step-up for the manifest's name and version.
    pub async fn publish(
        &self,
        publisher: &User,
        manifest: &str,
        wasm: Vec<u8>,
    ) -> (StatusCode, Value) {
        let assertion = match manifest_name_version(manifest) {
            Some((name, version)) => Some(
                self.step_up(publisher, "module_publish", &name, &version)
                    .await,
            ),
            // Not a manifest the registry can read: let it say so.
            None => None,
        };
        let form = reqwest::multipart::Form::new()
            .text("manifest", manifest.to_string())
            .part(
                "wasm",
                reqwest::multipart::Part::bytes(wasm)
                    .file_name("module.wasm")
                    .mime_str("application/wasm")
                    .expect("a mime type"),
            );
        let mut request = self
            .request(reqwest::Method::POST, "/modules")
            .bearer_auth(&publisher.pair.token)
            .multipart(form);
        if let Some(assertion) = assertion {
            request = request.header(hive_client::client::STEP_UP_HEADER, assertion);
        }
        send(request).await
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
        let body = json!({ "price_per_call": price_per_call, "free_tier_calls": 0 }).to_string();
        let assertion = self.step_up(publisher, "pricing_set", name, &body).await;
        let (status, body) = send(
            self.request(reqwest::Method::PUT, &format!("/modules/{name}/pricing"))
                .bearer_auth(&publisher.pair.token)
                .header(hive_client::client::STEP_UP_HEADER, assertion)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body),
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

// ── Step-up ───────────────────────────────────────────────────────────────

/// `request` with `fields` as an `application/x-www-form-urlencoded` body.
fn form(request: reqwest::RequestBuilder, fields: &[(&str, &str)]) -> reqwest::RequestBuilder {
    let encode = |s: &str| {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    request
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
}

/// The `nonce` hidden field of a `/step-up` page form.
fn form_nonce(page: &str) -> String {
    let marker = "name=\"nonce\" value=\"";
    let at = page.find(marker).expect("a nonce field") + marker.len();
    page[at..]
        .split('"')
        .next()
        .expect("a nonce value")
        .to_string()
}

/// A manifest's name and version, flat or under `[module]`: what a
/// `module_publish` step-up binds.
fn manifest_name_version(manifest: &str) -> Option<(String, String)> {
    let top: toml::Table = manifest.parse().ok()?;
    let listing = match top.get("module") {
        Some(toml::Value::Table(module)) => module,
        _ => &top,
    };
    let field = |key: &str| listing.get(key)?.as_str().map(str::to_string);
    Some((field("name")?, field("version")?))
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
/// to `runner` as the user `client` is signed in as. Returns its base URL:
/// a test-only loopback listener that adds the launch token itself.
pub async fn start_gateway(client: Arc<HiveRegistryClient>, runner: &str) -> String {
    let registry = ModuleRegistry::new(Arc::new(NoLocalLlm), ResourceLimits::default())
        .expect("an empty module registry");
    let gateway = ProtocolGateway::new(Arc::new(RwLock::new(registry)))
        .with_hive_client(client)
        .with_runner_url(runner);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    // This test listener stands in for a caller holding the gateway's
    // launch token (EN-0d): it adds it to every request.
    let bearer: axum::http::HeaderValue = format!("Bearer {}", gateway.token().as_str())
        .parse()
        .expect("a token is a valid header value");
    let router = gateway.build_router().layer(axum::middleware::map_request(
        move |mut request: axum::extract::Request| {
            let bearer = bearer.clone();
            async move {
                request
                    .headers_mut()
                    .insert(axum::http::header::AUTHORIZATION, bearer);
                request
            }
        },
    ));
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
    fn the_step_up_page_nonce_is_read_from_its_form() {
        let page = "<form method=\"post\" action=\"/step-up\">\
                    <input type=\"hidden\" name=\"nonce\" value=\"abc123\">";
        assert_eq!(form_nonce(page), "abc123");
    }

    #[test]
    fn a_publish_step_up_binds_the_manifests_name_and_version() {
        assert_eq!(
            manifest_name_version(&flat_manifest("m-e2e", "1.2.3", "")),
            Some(("m-e2e".to_string(), "1.2.3".to_string()))
        );
        assert_eq!(
            manifest_name_version("[module]\nname = \"m\"\nversion = \"2.0.0\"\n"),
            Some(("m".to_string(), "2.0.0".to_string()))
        );
        assert_eq!(manifest_name_version("not toml ["), None);
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
