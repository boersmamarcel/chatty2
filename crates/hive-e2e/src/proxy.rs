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
//!   returns — forwarded untouched, signing-chain headers stripped, body
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
use base64::Engine as _;
use bytes::Bytes;
use ed25519_dalek::{Signer, SigningKey};
use hive_client::verify::{
    Capabilities, HEADER_CERTIFICATE, HEADER_CERTIFICATE_SIGNATURE, HEADER_MANIFEST,
    HEADER_MANIFEST_SIGNATURE, PublisherCertificate, SignedManifest,
};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use serde_json::Value;
use sha2::{Digest, Sha256};

const HASH: &str = "x-wasm-sha256";
/// The four headers that carry the signing chain (AGE-704).
const CHAIN_HEADERS: [&str; 4] = [
    HEADER_MANIFEST,
    HEADER_MANIFEST_SIGNATURE,
    HEADER_CERTIFICATE,
    HEADER_CERTIFICATE_SIGNATURE,
];

/// What must be escaped inside one path segment.
const SEGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'/').add(b'%').add(b'?').add(b'#');

/// What a module download returns through the proxy.
#[derive(Clone, Debug, Default)]
pub enum Download {
    /// The registry's response, untouched.
    #[default]
    Forward,
    /// The four `X-Hive-*` signing-chain headers removed (row 6.1).
    StripSignature,
    /// One byte of the body flipped, headers intact (row 6.3).
    CorruptBody,
    /// Body, hash and the whole signing chain replaced by an attacker's own,
    /// self-consistent set (row 6.2).
    Swap(Payload),
    /// The registry's genuine, signed download of another module version
    /// (row 6.6).
    Replay { name: String, version: String },
    /// The registry's headers over a body of `declared` zero bytes, of which
    /// at most `serve_at_most` are sent before the stream is cut (row 6.5).
    Huge { declared: u64, serve_at_most: u64 },
}

/// A self-consistent download: bytes and a full signing chain for them, in
/// the registry's header encodings.
#[derive(Clone, Debug)]
pub struct Payload {
    pub wasm: Vec<u8>,
    pub sha256: String,
    /// The four `X-Hive-*` headers, name and value.
    pub chain: Vec<(&'static str, String)>,
}

impl Payload {
    /// `wasm` published as `name@version` under a root and a publisher key
    /// no registry has ever used: every link verifies except the first.
    pub fn signed_by_attacker(wasm: Vec<u8>, name: &str, version: &str) -> Self {
        Self::signed(
            wasm,
            name,
            version,
            &SigningKey::from_bytes(&[0x42; 32]),
            &SigningKey::from_bytes(&[0x43; 32]),
        )
    }

    /// `wasm` published as `name@version` the way hive-registry signs
    /// (AGE-704): `root` certifies `publisher`, which signs the manifest.
    pub fn signed(
        wasm: Vec<u8>,
        name: &str,
        version: &str,
        root: &SigningKey,
        publisher: &SigningKey,
    ) -> Self {
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let sha256 = hex::encode(Sha256::digest(&wasm));
        let certificate = PublisherCertificate {
            not_before: 1_790_000_000,
            public_key: hex::encode(publisher.verifying_key().to_bytes()),
            publisher_id: "00000000-0000-4000-8000-000000000666".to_string(),
        }
        .canonical_bytes();
        let manifest = SignedManifest {
            capabilities: Capabilities::default(),
            name: name.to_string(),
            sha256: sha256.clone(),
            version: version.to_string(),
            wit_version: "0.3.0".to_string(),
        }
        .canonical_bytes();
        let chain = vec![
            (HEADER_MANIFEST, b64(&manifest)),
            (
                HEADER_MANIFEST_SIGNATURE,
                b64(&publisher.sign(&manifest).to_bytes()),
            ),
            (HEADER_CERTIFICATE, b64(&certificate)),
            (
                HEADER_CERTIFICATE_SIGNATURE,
                b64(&root.sign(&certificate).to_bytes()),
            ),
        ];
        Self {
            wasm,
            sha256,
            chain,
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
            for name in CHAIN_HEADERS {
                headers.remove(name);
            }
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
            for (name, value) in &payload.chain {
                let name = axum::http::HeaderName::from_bytes(name.as_bytes())
                    .expect("a valid header name");
                headers.insert(name, header(value));
            }
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
    //! S6 rows rely on, and the client refuses what each one serves. These
    //! are hermetic and run on every PR.
    use super::*;
    use hive_client::{HiveRegistryClient, TrustLevel};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const WASM: &[u8] = b"\0asm\x01\0\0\0genuine module bytes";

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[0x0a; 32])
    }

    /// A client through `proxy` that trusts the mock registry's root.
    fn client(proxy: &TamperProxy) -> HiveRegistryClient {
        HiveRegistryClient::new(&proxy.url)
            .with_local_root_key(&hex::encode(root().verifying_key().to_bytes()))
    }

    /// A registry serving `good@1.0.0` and `other@2.0.0`, each signed by the
    /// publisher key its root certified, with their version lists.
    async fn registry() -> (MockServer, Payload) {
        let server = MockServer::start().await;
        let publisher = SigningKey::from_bytes(&[0x0b; 32]);
        let genuine = Payload::signed(WASM.to_vec(), "good", "1.0.0", &root(), &publisher);
        let other = Payload::signed(
            b"\0asm\x01\0\0\0other".to_vec(),
            "other",
            "2.0.0",
            &root(),
            &publisher,
        );
        for (name, version, payload) in [("good", "1.0.0", &genuine), ("other", "2.0.0", &other)] {
            let mut response = ResponseTemplate::new(200)
                .insert_header(HASH, payload.sha256.as_str())
                .set_body_bytes(payload.wasm.clone());
            for (header, value) in &payload.chain {
                response = response.insert_header(*header, value.as_str());
            }
            Mock::given(method("GET"))
                .and(path(format!("/api/modules/{name}/{version}")))
                .respond_with(response)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/api/modules/{name}/versions")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "items": [{
                        "module_name": name, "version": version, "wasm_hash": payload.sha256,
                        "wasm_size_bytes": payload.wasm.len(), "manifest": { "name": name },
                        "published_at": "2026-09-27T00:00:00Z",
                        "signature": null, "publisher_public_key": null,
                    }],
                    "page": 1, "per_page": 20, "total": 1,
                })))
                .mount(&server)
                .await;
        }
        (server, genuine)
    }

    async fn refusal(proxy: &TamperProxy) -> String {
        client(proxy)
            .download("good", "1.0.0")
            .await
            .expect_err("the download is refused")
            .to_string()
    }

    #[tokio::test]
    async fn forwards_a_download_untouched() {
        let (registry, genuine) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        let download = client(&proxy).download("good", "1.0.0").await.unwrap();
        assert_eq!(download.wasm, genuine.wasm);
        assert_eq!(download.trust_level, TrustLevel::Signed);
        assert_eq!(download.manifest["name"], "good");
    }

    #[tokio::test]
    async fn strips_the_signing_chain_headers() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::StripSignature);
        let err = refusal(&proxy).await;
        assert!(err.contains("missing X-Hive-Manifest"), "{err}");
    }

    #[tokio::test]
    async fn corrupts_the_body_under_intact_headers() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::CorruptBody);
        let err = refusal(&proxy).await;
        assert!(err.contains("hash mismatch"), "{err}");
    }

    #[tokio::test]
    async fn swaps_in_a_self_consistent_attacker_payload() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        let attacker =
            Payload::signed_by_attacker(b"\0asm\x01\0\0\0evil".to_vec(), "good", "1.0.0");
        proxy.set_download(Download::Swap(attacker));
        let err = refusal(&proxy).await;
        assert!(err.contains("not signed by the registry root key"), "{err}");
    }

    #[tokio::test]
    async fn replays_another_modules_signed_download() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::Replay {
            name: "other".into(),
            version: "2.0.0".into(),
        });
        let err = refusal(&proxy).await;
        assert!(err.contains("is for other@2.0.0"), "{err}");
    }

    #[tokio::test]
    async fn cuts_a_huge_body_at_the_serve_limit() {
        let (registry, _) = registry().await;
        let proxy = TamperProxy::start(&registry.uri()).await;
        proxy.set_download(Download::Huge {
            declared: 2 << 30,
            serve_at_most: 4 << 20,
        });
        let begun = client(&proxy)
            .begin_download("good", "1.0.0")
            .await
            .unwrap();
        assert_eq!(begun.total_size, 2 << 30);
        let err = client(&proxy).download("good", "1.0.0").await.unwrap_err();
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
        let client = client(&proxy);
        let versions = client.list_versions("../../evil").await.unwrap();
        assert_eq!(versions.items[0].module_name, "../../evil");
        let mut begun = client.begin_download("../../evil", "1.0.0").await.unwrap();
        assert_eq!(begun.read_body(|_| {}).await.unwrap(), WASM);
    }
}
