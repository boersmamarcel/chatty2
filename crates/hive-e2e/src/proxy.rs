//! The tampering proxy (evaluation plan §2 D): an HTTP server that sits
//! between chatty and the registry, forwards every request, and rewrites
//! what comes back according to the rules a test sets.
//!
//! It is an axum server rather than a wiremock mock because it has to
//! *forward* (wiremock responders are synchronous and canned) and because
//! row 6.5 streams a 2 GiB body, which a wiremock `ResponseTemplate` would
//! have to hold in memory. Its own tests run it in front of a wiremock
//! registry.
//!
//! Rules:
//! - [`Download`]: what a module download (`GET /api/modules/{n}/{v}`)
//!   returns — forwarded untouched, signature headers stripped, body
//!   corrupted, swapped for an attacker's signed payload, another module's
//!   download replayed, or an endless body.
//! - [`TamperProxy::rename`]: the registry's module `real` is shown to
//!   chatty as `shown` in every JSON body, and requests for `shown` go to
//!   `real`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use ed25519_dalek::{Signer, SigningKey};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use serde_json::Value;
use sha2::{Digest, Sha256};

const HASH: &str = "x-wasm-sha256";
const SIGNATURE: &str = "x-signature";
const PUBLIC_KEY: &str = "x-publisher-public-key";

/// What must be escaped inside one path segment.
const SEGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'/').add(b'%').add(b'?').add(b'#');

/// What a module download returns through the proxy.
#[derive(Clone, Debug, Default)]
pub enum Download {
    /// The registry's response, untouched.
    #[default]
    Forward,
    /// `x-signature` and `x-publisher-public-key` removed (row 6.1).
    StripSignature,
    /// One byte of the body flipped, headers intact (row 6.3).
    CorruptBody,
    /// Body, hash, signature and key all replaced by an attacker's own,
    /// self-consistent set (row 6.2).
    Swap(Payload),
    /// The registry's genuine, signed download of another module version
    /// (row 6.6).
    Replay { name: String, version: String },
    /// The registry's headers over a body of `declared` zero bytes, of which
    /// at most `serve_at_most` are sent before the stream is cut (row 6.5).
    Huge { declared: u64, serve_at_most: u64 },
}

/// A self-consistent download: bytes, their SHA-256, and an Ed25519
/// signature over the hash by `public_key`, in the registry's encodings.
#[derive(Clone, Debug)]
pub struct Payload {
    pub wasm: Vec<u8>,
    pub sha256: String,
    pub signature: String,
    pub public_key: String,
}

impl Payload {
    /// `wasm` signed by a key the registry has never seen.
    pub fn signed_by_attacker(wasm: Vec<u8>) -> Self {
        Self::signed(wasm, &SigningKey::from_bytes(&[0x42; 32]))
    }

    /// `wasm` signed by `key` the way hive-registry signs: the signature
    /// covers the hex SHA-256 string, base64; the key is hex.
    pub fn signed(wasm: Vec<u8>, key: &SigningKey) -> Self {
        use base64::Engine as _;
        let sha256 = hex::encode(Sha256::digest(&wasm));
        let signature = base64::engine::general_purpose::STANDARD
            .encode(key.sign(sha256.as_bytes()).to_bytes());
        Self {
            wasm,
            sha256,
            signature,
            public_key: hex::encode(key.verifying_key().to_bytes()),
        }
    }
}

#[derive(Clone, Default)]
struct Rules {
    download: Download,
    /// (shown, real)
    rename: Option<(String, String)>,
}

struct Inner {
    upstream: String,
    http: reqwest::Client,
    rules: Mutex<Rules>,
    served: AtomicU64,
}

/// A running proxy in front of one registry.
pub struct TamperProxy {
    /// `http://127.0.0.1:<port>`: point a `HiveRegistryClient` here.
    pub url: String,
    inner: Arc<Inner>,
}

impl TamperProxy {
    /// Serve on an ephemeral loopback port, forwarding to `upstream`.
    pub async fn start(upstream: &str) -> Self {
        let inner = Arc::new(Inner {
            upstream: upstream.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("an HTTP client"),
            rules: Mutex::new(Rules::default()),
            served: AtomicU64::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let app = axum::Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&inner));
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Self { url, inner }
    }

    /// Set what module downloads return from now on.
    pub fn set_download(&self, download: Download) {
        self.inner.rules.lock().expect("rules lock").download = download;
    }

    /// Show the registry's module `real` to chatty as `shown`.
    pub fn rename(&self, shown: &str, real: &str) {
        self.inner.rules.lock().expect("rules lock").rename =
            Some((shown.to_string(), real.to_string()));
    }

    /// Body bytes a [`Download::Huge`] stream has handed to the socket.
    pub fn bytes_served(&self) -> u64 {
        self.inner.served.load(Ordering::SeqCst)
    }
}

async fn handle(State(inner): State<Arc<Inner>>, request: Request) -> Response {
    match forward(&inner, request).await {
        Ok(response) => response,
        Err(e) => (StatusCode::BAD_GATEWAY, format!("tamper proxy: {e}")).into_response(),
    }
}

async fn forward(inner: &Arc<Inner>, request: Request) -> Result<Response, String> {
    let rules = inner.rules.lock().expect("rules lock").clone();
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| format!("request body: {e}"))?;

    let mut segments: Vec<String> = parts
        .uri
        .path()
        .split('/')
        .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
        .collect();
    if let Some((shown, real)) = &rules.rename {
        for segment in &mut segments {
            if segment == shown {
                *segment = real.clone();
            }
        }
    }
    let is_download =
        parts.method == axum::http::Method::GET && download_target(&segments).is_some();
    if let (true, Download::Replay { name, version }) = (is_download, &rules.download) {
        segments = vec![
            String::new(),
            "api".into(),
            "modules".into(),
            name.clone(),
            version.clone(),
        ];
    }
    let path = segments
        .iter()
        .map(|s| utf8_percent_encode(s, SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/");
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();

    let mut upstream = inner
        .http
        .request(
            parts.method.clone(),
            format!("{}{path}{query}", inner.upstream),
        )
        .body(body);
    for (name, value) in &parts.headers {
        if name != "host" && name != "content-length" {
            upstream = upstream.header(name, value);
        }
    }
    let resp = upstream
        .send()
        .await
        .map_err(|e| format!("upstream: {e}"))?;
    let status = resp.status();
    let mut headers = HeaderMap::new();
    for (name, value) in resp.headers() {
        if name != "content-length" && name != "transfer-encoding" && name != "connection" {
            headers.append(name, value.clone());
        }
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("upstream body: {e}"))?;

    if is_download && status.is_success() {
        return Ok(tamper_download(
            inner,
            &rules.download,
            status,
            headers,
            body,
        ));
    }

    let body = match &rules.rename {
        Some((shown, real)) => rename_json(&body, real, shown).unwrap_or(body),
        None => body,
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

/// `/api/modules/{name}/{version}` (not `/versions` or `/pricing`).
fn download_target(segments: &[String]) -> Option<(&str, &str)> {
    match segments {
        [empty, api, modules, name, version]
            if empty.is_empty()
                && api == "api"
                && modules == "modules"
                && version != "versions"
                && version != "pricing" =>
        {
            Some((name, version))
        }
        _ => None,
    }
}

fn tamper_download(
    inner: &Arc<Inner>,
    rule: &Download,
    status: StatusCode,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = match rule {
        Download::Forward | Download::Replay { .. } => Body::from(body),
        Download::StripSignature => {
            headers.remove(SIGNATURE);
            headers.remove(PUBLIC_KEY);
            Body::from(body)
        }
        Download::CorruptBody => {
            let mut bytes = body.to_vec();
            if let Some(last) = bytes.last_mut() {
                *last ^= 0xff;
            }
            Body::from(bytes)
        }
        Download::Swap(payload) => {
            headers.insert(HASH, header(&payload.sha256));
            headers.insert(SIGNATURE, header(&payload.signature));
            headers.insert(PUBLIC_KEY, header(&payload.public_key));
            Body::from(payload.wasm.clone())
        }
        Download::Huge {
            declared,
            serve_at_most,
        } => {
            headers.insert("content-length", header(&declared.to_string()));
            huge_body(Arc::clone(inner), *serve_at_most)
        }
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

/// Zeros in 1 MiB chunks, counted into `inner.served`, cut with an error
/// once `serve_at_most` bytes are out: the test process never holds more
/// than that, whatever the client does.
fn huge_body(inner: Arc<Inner>, serve_at_most: u64) -> Body {
    const CHUNK: usize = 1 << 20;
    let zeros = Bytes::from(vec![0u8; CHUNK]);
    let stream = futures_util::stream::unfold(0u64, move |sent| {
        let zeros = zeros.clone();
        let inner = Arc::clone(&inner);
        async move {
            if sent == u64::MAX {
                return None;
            }
            if sent >= serve_at_most {
                let cut = std::io::Error::other("tamper proxy: serve limit reached");
                return Some((Err(cut), u64::MAX));
            }
            inner.served.fetch_add(CHUNK as u64, Ordering::SeqCst);
            Some((Ok(zeros), sent + CHUNK as u64))
        }
    });
    Body::from_stream(stream)
}

fn header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("a valid header value")
}

/// Every string field named `name` or `module_name` whose value is `real`
/// becomes `shown`. `None` when the body is not JSON.
fn rename_json(body: &[u8], real: &str, shown: &str) -> Option<Bytes> {
    fn walk(value: &mut Value, real: &str, shown: &str) {
        match value {
            Value::Object(map) => {
                for (key, field) in map.iter_mut() {
                    if (key == "name" || key == "module_name") && field.as_str() == Some(real) {
                        *field = Value::String(shown.to_string());
                    } else {
                        walk(field, real, shown);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| walk(v, real, shown)),
            _ => {}
        }
    }
    let mut json: Value = serde_json::from_slice(body).ok()?;
    walk(&mut json, real, shown);
    Some(Bytes::from(json.to_string()))
}

#[cfg(test)]
mod tests {
    //! The proxy in front of a wiremock registry: each rule does what the
    //! S6 rows rely on. These are hermetic and run on every PR.
    use super::*;
    use hive_client::{HiveRegistryClient, TrustLevel};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const WASM: &[u8] = b"\0asm\x01\0\0\0genuine module bytes";

    /// A registry serving `good@1.0.0` (signed by the publisher key) and
    /// `other@2.0.0`, with their version lists and metadata.
    async fn registry() -> (MockServer, Payload) {
        let server = MockServer::start().await;
        let genuine = Payload::signed(WASM.to_vec(), &SigningKey::from_bytes(&[7; 32]));
        let other = Payload::signed(
            b"\0asm\x01\0\0\0other".to_vec(),
            &SigningKey::from_bytes(&[7; 32]),
        );
        for (name, version, payload) in [("good", "1.0.0", &genuine), ("other", "2.0.0", &other)] {
            Mock::given(method("GET"))
                .and(path(format!("/api/modules/{name}/{version}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header(HASH, payload.sha256.as_str())
                        .insert_header(SIGNATURE, payload.signature.as_str())
                        .insert_header(PUBLIC_KEY, payload.public_key.as_str())
                        .set_body_bytes(payload.wasm.clone()),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/api/modules/{name}/versions")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "items": [{
                        "module_name": name, "version": version, "wasm_hash": payload.sha256,
                        "wasm_size_bytes": payload.wasm.len(), "manifest": { "name": name },
                        "published_at": "2026-09-27T00:00:00Z",
                        "signature": payload.signature, "publisher_public_key": payload.public_key,
                    }],
                    "page": 1, "per_page": 20, "total": 1,
                })))
                .mount(&server)
                .await;
        }
        (server, genuine)
    }

    #[tokio::test]
    async fn forwards_a_download_untouched() {
        let (registry, genuine) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        let download = HiveRegistryClient::new(&proxy.url)
            .download("good", "1.0.0")
            .await
            .unwrap();
        assert_eq!(download.wasm, genuine.wasm);
        assert_eq!(download.trust_level, TrustLevel::Signed);
        assert_eq!(download.manifest["name"], "good");
    }

    #[tokio::test]
    async fn strips_the_signature_headers() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::StripSignature);
        let download = HiveRegistryClient::new(&proxy.url)
            .begin_download("good", "1.0.0")
            .await
            .unwrap();
        assert!(download.registry_hash.is_some());
        assert_eq!(download.signature, None);
        assert_eq!(download.publisher_public_key, None);
    }

    #[tokio::test]
    async fn corrupts_the_body_under_intact_headers() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::CorruptBody);
        let err = HiveRegistryClient::new(&proxy.url)
            .download("good", "1.0.0")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("hash mismatch"), "{err}");
    }

    #[tokio::test]
    async fn swaps_in_a_self_consistent_attacker_payload() {
        let (registry, genuine) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        let attacker = Payload::signed_by_attacker(b"\0asm\x01\0\0\0evil".to_vec());
        proxy.set_download(Download::Swap(attacker.clone()));
        let download = HiveRegistryClient::new(&proxy.url)
            .begin_download("good", "1.0.0")
            .await
            .unwrap();
        assert_eq!(
            download.registry_hash.as_deref(),
            Some(attacker.sha256.as_str())
        );
        assert_eq!(
            download.publisher_public_key.as_deref(),
            Some(attacker.public_key.as_str())
        );
        assert_ne!(attacker.public_key, genuine.public_key);
    }

    #[tokio::test]
    async fn replays_another_modules_signed_download() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::Replay {
            name: "other".into(),
            version: "2.0.0".into(),
        });
        let download = HiveRegistryClient::new(&proxy.url)
            .download("good", "1.0.0")
            .await
            .unwrap();
        assert_eq!(download.wasm, b"\0asm\x01\0\0\0other");
    }

    #[tokio::test]
    async fn cuts_a_huge_body_at_the_serve_limit() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::Huge {
            declared: 2 << 30,
            serve_at_most: 4 << 20,
        });
        let begun = HiveRegistryClient::new(&proxy.url)
            .begin_download("good", "1.0.0")
            .await
            .unwrap();
        assert_eq!(begun.total_size, 2 << 30);
        let err = HiveRegistryClient::new(&proxy.url)
            .download("good", "1.0.0")
            .await
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(
            proxy.bytes_served() <= 2 * (4 << 20),
            "{}",
            proxy.bytes_served()
        );
    }

    #[tokio::test]
    async fn renames_a_module_both_ways() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.rename("../../evil", "good");
        let client = HiveRegistryClient::new(&proxy.url);
        let versions = client.list_versions("../../evil").await.unwrap();
        assert_eq!(versions.items[0].module_name, "../../evil");
        let download = client.download("../../evil", "1.0.0").await.unwrap();
        assert_eq!(download.wasm, WASM);
    }
}
