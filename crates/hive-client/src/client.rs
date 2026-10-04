//! HTTP client for the Hive module registry.

use std::path::PathBuf;
use std::sync::Arc;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;

use crate::{
    cache::Cache,
    error::ClientError,
    models::{
        AgentSpecList, BegunDownload, CategoryList, CreditBalance, DownloadResult, ListParams,
        ModuleList, ModuleMetadata, ModulePricingInfo, PublishedSpec, TokenPair, VersionList,
    },
    secure_url::ensure_secure_url,
    session::{HiveSession, send_authed},
    trust,
    verify::{self, ModuleChain, TrustLevel},
};

/// HTTP client for the Hive module registry.
///
/// Supports browsing, searching, downloading, and authenticating against a
/// Hive registry instance. Optionally caches list/search results on disk for
/// offline resilience.
pub struct HiveRegistryClient {
    base_url: String,
    http: reqwest::Client,
    cache: Option<Cache>,
    session: Option<Arc<HiveSession>>,
    /// The registry root public keys downloads are verified against
    /// ([`trust::trusted_roots`]); a download verifies if any one of them
    /// signed it (SEC-3, AGE-816). Empty refuses every download.
    root_keys: Vec<String>,
    /// Why `base_url` is not allowed (SEC-16, AGE-756), computed once at
    /// construction since the URL never changes afterwards. `None` means
    /// every request may proceed.
    insecure_url: Option<String>,
    /// The registry's session key, once fetched and verified under
    /// `root_keys` ([`crate::session_key`]).
    session_key: tokio::sync::OnceCell<ed25519_dalek::VerifyingKey>,
}

impl HiveRegistryClient {
    /// Create a new client pointing at `base_url` (no trailing slash needed).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_timeout_inner(base_url, std::time::Duration::from_secs(30))
    }

    /// Create a new client with a custom HTTP timeout.
    pub fn with_timeout(base_url: impl Into<String>, timeout: std::time::Duration) -> Self {
        Self::with_timeout_inner(base_url, timeout)
    }

    /// The root keys come from [`trust::trusted_roots_from_env`]: the
    /// compiled production list, or `CHATTY_HIVE_ROOT_KEY` alone for a local
    /// registry.
    fn with_timeout_inner(base_url: impl Into<String>, timeout: std::time::Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            root_keys: trust::trusted_roots_from_env(&base_url),
            insecure_url: ensure_secure_url(&base_url).err(),
            base_url,
            http,
            cache: None,
            session: None,
            session_key: tokio::sync::OnceCell::new(),
        }
    }

    /// Refuse before any request when [`ensure_secure_url`] rejected this
    /// client's base URL at construction (SEC-16, AGE-756).
    fn ensure_secure(&self) -> Result<(), ClientError> {
        match &self.insecure_url {
            Some(reason) => Err(ClientError::InsecureUrl(reason.clone())),
            None => Ok(()),
        }
    }

    /// Trust `root_public_key_hex` for this registry instead of what
    /// [`new`](Self::new) found, under the same rule as `CHATTY_HIVE_ROOT_KEY`:
    /// honoured for a local (loopback) registry only.
    pub fn with_local_root_key(mut self, root_public_key_hex: &str) -> Self {
        self.root_keys = trust::trusted_roots(&self.base_url, Some(root_public_key_hex));
        self.session_key = tokio::sync::OnceCell::new();
        self
    }

    /// The root public keys downloads are verified against. A download
    /// verifies if any one of them signed it; empty refuses every download.
    pub fn root_keys(&self) -> &[String] {
        &self.root_keys
    }

    /// Enable the offline module-list cache, persisted under `dir`.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        self.cache = Some(Cache::new(dir)?);
        Ok(self)
    }

    /// Authenticate as the user signed in to `session` (e.g. for downloads).
    /// The session refreshes its access token when it is about to expire and
    /// after a 401, and every client sharing it sees the new one.
    pub fn with_session(mut self, session: Arc<HiveSession>) -> Self {
        self.session = Some(session);
        self
    }

    /// The signed-in user's current access token, refreshed first when it is
    /// about to expire. `None` without a session or when signed out.
    ///
    /// Used by the protocol gateway to forward the user's credentials to
    /// remote execution targets (hive-runner) so they can charge the correct
    /// user's credit balance.
    pub async fn access_token(&self) -> Option<String> {
        match &self.session {
            Some(session) => session.access_token().await,
            None => None,
        }
    }

    // ── Authentication ─────────────────────────────────────────────────────

    /// Register a new account on the Hive registry.
    pub async fn register(
        &self,
        username: &str,
        email: &str,
        password: &str,
    ) -> Result<TokenPair, ClientError> {
        self.ensure_secure()?;
        #[derive(Serialize)]
        struct RegisterBody<'a> {
            username: &'a str,
            email: &'a str,
            password: &'a str,
        }

        let url = format!("{}/api/auth/register", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&RegisterBody {
                username,
                email,
                password,
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }

        resp.json::<TokenPair>().await.map_err(ClientError::from)
    }

    /// Log in with email and password.
    pub async fn login(&self, email: &str, password: &str) -> Result<TokenPair, ClientError> {
        self.ensure_secure()?;
        #[derive(Serialize)]
        struct LoginBody<'a> {
            email: &'a str,
            password: &'a str,
        }

        let url = format!("{}/api/auth/login", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&LoginBody { email, password })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }

        resp.json::<TokenPair>().await.map_err(ClientError::from)
    }

    /// Exchange `refresh_token` for a new pair (`POST /api/auth/refresh`).
    /// The presented token is retired by the call. A rejected token —
    /// expired, revoked, or already rotated — is [`ClientError::Unauthorized`].
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, ClientError> {
        self.ensure_secure()?;
        let url = format!("{}/api/auth/refresh", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&RefreshTokenBody { refresh_token })
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }

        resp.json::<TokenPair>().await.map_err(ClientError::from)
    }

    /// Revoke `refresh_token` (`POST /api/auth/logout`). The access token it
    /// was paired with expires on its own within the hour.
    pub async fn logout(&self, refresh_token: &str) -> Result<(), ClientError> {
        self.ensure_secure()?;
        let url = format!("{}/api/auth/logout", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&RefreshTokenBody { refresh_token })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(())
    }

    // ── Search ─────────────────────────────────────────────────────────────

    /// Search the registry. Falls back to cached results when offline.
    pub async fn search(&self, query: &str) -> Result<ModuleList, ClientError> {
        #[derive(Serialize)]
        struct SearchParams<'a> {
            q: &'a str,
        }

        let result = self
            .get_json::<ModuleList>("/api/search", &SearchParams { q: query })
            .await;

        match result {
            Ok(list) => {
                self.maybe_store_cache(&format!("search_{query}"), &list);
                Ok(list)
            }
            Err(e) if e.is_offline() => {
                tracing::warn!(query = %query, "registry unreachable – checking offline cache");
                if let Some(cached) = self
                    .cache
                    .as_ref()
                    .and_then(|c| c.load(&format!("search_{query}")))
                {
                    return Ok(cached);
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    // ── Agent specs (HS-3, MK-T2) ──────────────────────────────────────────

    /// Search the published agent specs (the marketplace's Teams tab); an
    /// empty `query` lists them all.
    pub async fn search_agents(&self, query: &str) -> Result<AgentSpecList, ClientError> {
        #[derive(Serialize)]
        struct SearchParams<'a> {
            q: &'a str,
        }
        self.get_json("/api/agents", &SearchParams { q: query })
            .await
    }

    /// One published spec version with its signed chain. Not verified here:
    /// see [`verify::verify_spec_any`] with [`root_keys`](Self::root_keys).
    pub async fn get_agent_spec(
        &self,
        name: &str,
        version: &str,
    ) -> Result<PublishedSpec, ClientError> {
        self.get_json(
            &format!("/api/agents/{}/{}", urlencoded(name), urlencoded(version)),
            &(),
        )
        .await
    }

    /// Count an install of `name@version` (once per user and spec).
    pub async fn record_agent_install(&self, name: &str, version: &str) -> Result<(), ClientError> {
        let url = format!(
            "{}/api/agents/{}/{}/install",
            self.base_url,
            urlencoded(name),
            urlencoded(version)
        );
        let response = self.send_authed(|| self.http.post(&url)).await?;
        if !response.status().is_success() {
            return Err(Self::api_error(response).await);
        }
        Ok(())
    }

    // ── Get module ─────────────────────────────────────────────────────────

    /// Fetch metadata for a single module by name.
    pub async fn get_module(&self, name: &str) -> Result<ModuleMetadata, ClientError> {
        self.get_json::<ModuleMetadata>(&format!("/api/modules/{}", urlencoded(name)), &())
            .await
    }

    // ── List modules ───────────────────────────────────────────────────────

    /// List modules with optional filters. Falls back to cache when offline.
    pub async fn list_modules(&self, params: &ListParams) -> Result<ModuleList, ClientError> {
        let result = self.get_json::<ModuleList>("/api/modules", params).await;

        match result {
            Ok(list) => {
                self.maybe_store_cache("modules", &list);
                Ok(list)
            }
            Err(e) if e.is_offline() => {
                tracing::warn!("registry unreachable – checking offline cache");
                if let Some(cached) = self.cache.as_ref().and_then(|c| c.load("modules")) {
                    return Ok(cached);
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    // ── List versions ──────────────────────────────────────────────────────

    /// List all published versions of a module.
    pub async fn list_versions(&self, name: &str) -> Result<VersionList, ClientError> {
        self.get_json::<VersionList>(&format!("/api/modules/{}/versions", urlencoded(name)), &())
            .await
    }

    // ── List categories ────────────────────────────────────────────────────

    /// List all module categories.
    pub async fn list_categories(&self) -> Result<CategoryList, ClientError> {
        self.get_json::<CategoryList>("/api/categories", &()).await
    }

    // ── Download ───────────────────────────────────────────────────────────

    /// Begin a streaming download of the `.wasm` binary for a specific version.
    ///
    /// Returns a [`BegunDownload`] whose body can be consumed chunk-by-chunk,
    /// allowing callers to report progress to the UI between each chunk.
    /// Call [`HiveRegistryClient::finalize_download`] once all bytes have been
    /// collected to verify the signing chain and fetch the manifest.
    ///
    /// Refuses before any request when this client trusts no root key for
    /// its registry, and before reading the body when the response lacks any
    /// of the four chain headers.
    ///
    /// For a simple one-shot download without progress reporting, use
    /// [`download`][Self::download] instead.
    pub async fn begin_download(
        &self,
        name: &str,
        version: &str,
    ) -> Result<BegunDownload, ClientError> {
        if self.root_keys.is_empty() {
            return Err(ClientError::NoTrustedRoot {
                registry: self.base_url.clone(),
            });
        }
        let url = format!(
            "{}/api/modules/{}/{}",
            self.base_url,
            urlencoded(name),
            urlencoded(version)
        );

        tracing::debug!(%url, "beginning streaming download");

        let response = self.send_authed(|| self.http.get(&url)).await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ClientError::NotFound(format!("{name}@{version}")));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Http {
                status: status.as_u16(),
                body,
            });
        }

        let total_size = response.content_length().unwrap_or(0);
        let headers = response.headers();
        let chain = ModuleChain::from_headers(
            header_str(headers, verify::HEADER_MANIFEST),
            header_str(headers, verify::HEADER_MANIFEST_SIGNATURE),
            header_str(headers, verify::HEADER_CERTIFICATE),
            header_str(headers, verify::HEADER_CERTIFICATE_SIGNATURE),
        )
        .map_err(|e| ClientError::SignatureInvalid(e.to_string()))?;

        Ok(BegunDownload {
            total_size,
            chain,
            stream: Box::pin(response.bytes_stream()),
        })
    }

    /// Verify and finalise a download started with [`begin_download`][Self::begin_download].
    ///
    /// Verifies `chain` (the [`BegunDownload`]'s) against this client's root
    /// keys: any one of them → publisher certificate → signed manifest → the
    /// SHA-256 of `wasm`, and that the manifest names `name@version`. Then
    /// fetches the version manifest. Any failure is
    /// [`ClientError::SignatureInvalid`].
    pub async fn finalize_download(
        &self,
        wasm: Vec<u8>,
        chain: &ModuleChain,
        name: &str,
        version: &str,
    ) -> Result<DownloadResult, ClientError> {
        if self.root_keys.is_empty() {
            return Err(ClientError::NoTrustedRoot {
                registry: self.base_url.clone(),
            });
        }
        let verified = verify::verify_download_any(&self.root_keys, chain, &wasm, name, version)
            .map_err(|e| ClientError::SignatureInvalid(e.to_string()))?;

        let manifest = match self.list_versions(name).await {
            Ok(vl) => vl
                .items
                .into_iter()
                .find(|v| v.version == version)
                .map(|v| v.manifest)
                .unwrap_or(serde_json::Value::Null),
            Err(_) => serde_json::Value::Null,
        };

        Ok(DownloadResult {
            wasm,
            wasm_hash: verified.manifest.sha256.clone(),
            trust_level: TrustLevel::Signed,
            publisher_public_key: verified.certificate.public_key,
            signed_manifest: verified.manifest,
            manifest,
        })
    }

    /// Download the `.wasm` binary for a specific version.
    ///
    /// Verifies the signing chain as [`finalize_download`][Self::finalize_download]
    /// does. Returns [`ClientError::SignatureInvalid`] on any mismatch,
    /// [`ClientError::NoTrustedRoot`] without a root key, and
    /// [`ClientError::TooLarge`] for a body over
    /// [`MAX_DOWNLOAD_BYTES`](crate::MAX_DOWNLOAD_BYTES).
    ///
    /// For streaming with progress reporting, use [`begin_download`][Self::begin_download]
    /// and [`finalize_download`][Self::finalize_download] instead.
    pub async fn download(&self, name: &str, version: &str) -> Result<DownloadResult, ClientError> {
        let mut begun = self.begin_download(name, version).await?;
        let wasm = begun.read_body(|_| {}).await?;
        self.finalize_download(wasm, &begun.chain, name, version)
            .await
    }

    // ── Usage reporting ────────────────────────────────────────────────────

    /// Report a batch of usage events to the registry.
    pub async fn report_usage(
        &self,
        events: Vec<crate::models::UsageEvent>,
    ) -> Result<crate::models::UsageReportResponse, ClientError> {
        let url = format!("{}/api/usage/report", self.base_url);

        let body = crate::models::UsageReportRequest { events };

        let response = self
            .send_authed(|| self.http.post(&url).json(&body))
            .await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if !status.is_success() {
            return Err(Self::api_error(response).await);
        }

        response
            .json::<crate::models::UsageReportResponse>()
            .await
            .map_err(ClientError::from)
    }

    // ── Credits ─────────────────────────────────────────────────────────────

    /// Get the authenticated user's credit balance.
    pub async fn get_credit_balance(&self) -> Result<CreditBalance, ClientError> {
        self.get_json::<CreditBalance>("/api/credits/balance", &())
            .await
    }

    /// Get pricing configuration for a module.
    pub async fn get_module_pricing(&self, name: &str) -> Result<ModulePricingInfo, ClientError> {
        self.get_json::<ModulePricingInfo>(
            &format!("/api/modules/{}/pricing", urlencoded(name)),
            &(),
        )
        .await
    }

    /// The signed-in user's recorded calls per module: what a publisher's
    /// free tier is counted against.
    pub async fn get_my_modules_usage(
        &self,
    ) -> Result<crate::models::MyModuleUsageList, ClientError> {
        self.get_json("/api/usage/me/modules", &()).await
    }

    // ── Billing sessions (Phase 3b) ────────────────────────────────────────

    /// Acquire a billing session before module invocation.
    ///
    /// Reserves credits for the estimated usage and returns a signed JWT
    /// that the module can verify.
    pub async fn acquire_session(
        &self,
        module_name: &str,
        module_version: &str,
        estimated_tokens: i64,
    ) -> Result<crate::models::AcquireSessionResponse, ClientError> {
        let url = format!("{}/api/credits/acquire-session", self.base_url);

        let body = crate::models::AcquireSessionRequest {
            module_name: module_name.to_string(),
            module_version: module_version.to_string(),
            estimated_tokens,
        };

        let response = self
            .send_authed(|| self.http.post(&url).json(&body))
            .await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if !status.is_success() {
            return Err(Self::api_error(response).await);
        }

        let session = response
            .json::<crate::models::AcquireSessionResponse>()
            .await
            .map_err(ClientError::from)?;
        // Fail closed: a token that does not verify under the registry's
        // certified session key, or that is for another module, is never
        // handed to a module (CX-0b).
        let claims: crate::session_key::BillingClaims = self.verify_token(&session.token).await?;
        if claims.module_name != module_name || claims.ver != module_version {
            return Err(ClientError::Token(
                crate::session_key::TokenError::Malformed("the token is for another module"),
            ));
        }
        Ok(session)
    }

    /// Settle a billing session after module execution.
    ///
    /// Reports actual usage and releases unused reserved credits.
    pub async fn settle_session(
        &self,
        session_id: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<crate::models::SettleSessionResponse, ClientError> {
        let url = format!("{}/api/credits/settle-session", self.base_url);

        let body = crate::models::SettleSessionRequest {
            session_id: session_id.to_string(),
            input_tokens,
            output_tokens,
        };

        let response = self
            .send_authed(|| self.http.post(&url).json(&body))
            .await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if !status.is_success() {
            return Err(Self::api_error(response).await);
        }

        response
            .json::<crate::models::SettleSessionResponse>()
            .await
            .map_err(ClientError::from)
    }

    // ── Registry tokens (CX-0b) ────────────────────────────────────────────

    /// The registry's session key: fetched once from
    /// `GET /.well-known/hive-session-key` and trusted only if one of this
    /// client's root keys certified it. Refused outright when no root is
    /// trusted for this registry.
    pub async fn session_key(&self) -> Result<ed25519_dalek::VerifyingKey, ClientError> {
        self.ensure_secure()?;
        if self.root_keys.is_empty() {
            return Err(crate::session_key::TokenError::NoTrustedRoot.into());
        }
        self.session_key
            .get_or_try_init(|| async {
                let url = format!("{}/.well-known/hive-session-key", self.base_url);
                let response = self.http.get(&url).send().await?;
                if !response.status().is_success() {
                    return Err(Self::api_error(response).await);
                }
                let cert = response
                    .json::<crate::session_key::SessionKeyCertificate>()
                    .await?;
                Ok(cert.verify(&self.root_keys)?)
            })
            .await
            .copied()
    }

    /// The claims of `token` if the registry signed it (EdDSA, under its
    /// certified [`session_key`](Self::session_key)) and it has not expired.
    pub async fn verify_token<T: serde::de::DeserializeOwned>(
        &self,
        token: &str,
    ) -> Result<T, ClientError> {
        let key = self.session_key().await?;
        Ok(crate::session_key::verify_token(
            token,
            &key,
            chrono::Utc::now().timestamp(),
        )?)
    }

    // ── Step-up (CX-0) ─────────────────────────────────────────────────────

    /// File a step-up request for `action` over `target` and `value`
    /// (`POST /api/step-up/requests`). Nothing is granted until the person
    /// opens the answer's `page`, signs in again and approves it; then
    /// [`step_up_request`](Self::step_up_request) carries the assertion.
    pub async fn request_step_up(
        &self,
        action: crate::models::StepUpAction,
        target: &str,
        value: &str,
    ) -> Result<crate::models::StepUpRequest, ClientError> {
        let url = format!("{}/api/step-up/requests", self.base_url);
        let body = serde_json::json!({
            "action": action.as_str(),
            "target": target,
            "value": value,
        });
        let response = self
            .send_authed(|| self.http.post(&url).json(&body))
            .await?;
        Self::json_or_error(response).await
    }

    /// A step-up request of the signed-in user's, with its assertion once
    /// approved (`GET /api/step-up/requests/{id}`).
    pub async fn step_up_request(
        &self,
        id: uuid::Uuid,
    ) -> Result<crate::models::StepUpRequest, ClientError> {
        self.get_json(&format!("/api/step-up/requests/{id}"), &())
            .await
    }

    /// Poll request `id` every `interval` until it is approved, and return
    /// its assertion. An expired request, or none within `timeout`, is
    /// [`ClientError::StepUpNotApproved`].
    pub async fn wait_for_step_up(
        &self,
        id: uuid::Uuid,
        interval: std::time::Duration,
        timeout: std::time::Duration,
    ) -> Result<String, ClientError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let request = self.step_up_request(id).await?;
            match (request.status.as_str(), request.assertion) {
                ("approved", Some(assertion)) => return Ok(assertion),
                ("expired", _) => {
                    return Err(ClientError::StepUpNotApproved("it expired".to_string()));
                }
                _ => {}
            }
            if tokio::time::Instant::now() + interval > deadline {
                return Err(ClientError::StepUpNotApproved(
                    "it was not approved in time".to_string(),
                ));
            }
            tokio::time::sleep(interval).await;
        }
    }

    // ── External keys (CX-0) ───────────────────────────────────────────────

    /// The signed-in user's `External` keys (`GET /api/auth/api-keys`).
    pub async fn list_external_keys(&self) -> Result<Vec<crate::models::ExternalKey>, ClientError> {
        self.get_json("/api/auth/api-keys", &()).await
    }

    /// File the `api_key_create` step-up for `key`: its target is the public
    /// key and its value the exact body [`create_external_key`] sends.
    ///
    /// [`create_external_key`]: Self::create_external_key
    pub async fn request_external_key_step_up(
        &self,
        key: &crate::models::CreateExternalKey,
    ) -> Result<crate::models::StepUpRequest, ClientError> {
        self.request_step_up(
            crate::models::StepUpAction::ApiKeyCreate,
            &key.target(),
            &key.body(),
        )
        .await
    }

    /// Create an `External` key with an approved `api_key_create`
    /// `assertion` for exactly this `key`. There is no secret in the answer:
    /// the key is the public key the holder already has.
    pub async fn create_external_key(
        &self,
        key: &crate::models::CreateExternalKey,
        assertion: &str,
    ) -> Result<crate::models::ExternalKey, ClientError> {
        let url = format!("{}/api/auth/api-keys", self.base_url);
        let body = key.body();
        let response = self
            .send_authed(|| {
                self.http
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .header(STEP_UP_HEADER, assertion)
                    .body(body.clone())
            })
            .await?;
        Self::json_or_error(response).await
    }

    /// Revoke `External` key `id` (`DELETE /api/auth/api-keys/{id}`; no
    /// step-up).
    pub async fn revoke_external_key(&self, id: uuid::Uuid) -> Result<(), ClientError> {
        let url = format!("{}/api/auth/api-keys/{id}", self.base_url);
        let response = self.send_authed(|| self.http.delete(&url)).await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if !status.is_success() {
            return Err(Self::api_error(response).await);
        }
        Ok(())
    }

    async fn json_or_error<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, ClientError> {
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Unauthorized);
        }
        if !status.is_success() {
            return Err(Self::api_error(response).await);
        }
        response.json::<T>().await.map_err(ClientError::from)
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    async fn get_json<T>(&self, path: &str, query: &impl Serialize) -> Result<T, ClientError>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .send_authed(|| self.http.get(&url).query(query))
            .await?;
        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ClientError::NotFound(path.to_string()));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Http {
                status: status.as_u16(),
                body,
            });
        }

        response.json::<T>().await.map_err(ClientError::from)
    }

    async fn send_authed(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ClientError> {
        self.ensure_secure()?;
        send_authed(self.session.as_deref(), build)
            .await
            .map_err(ClientError::from)
    }

    async fn api_error(resp: reqwest::Response) -> ClientError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        ClientError::Http { status, body }
    }

    fn maybe_store_cache(&self, key: &str, list: &ModuleList) {
        if let Some(cache) = &self.cache
            && let Err(e) = cache.store(key, list)
        {
            tracing::warn!(error = %e, "failed to write module list cache");
        }
    }
}

/// The header a step-up assertion travels in (hive-registry's
/// `STEP_UP_HEADER`).
pub const STEP_UP_HEADER: &str = "hive-step-up";

#[derive(Serialize)]
struct RefreshTokenBody<'a> {
    refresh_token: &'a str,
}

// ── URL encoding helper ────────────────────────────────────────────────────

fn urlencoded(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC)
        .to_string()
        .replace("%2D", "-")
        .replace("%2E", ".")
        .replace("%5F", "_")
}

fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SEC-16 / AGE-756: a registry at a real host must use `https://`; a
    /// loopback registry (the docker-compose stack, a local dev registry)
    /// may keep using `http://`. Both cases must be decided before any
    /// request leaves — never as an HTTP error from the wire.
    #[tokio::test]
    async fn hive_registry_requires_https_off_loopback() {
        let remote = HiveRegistryClient::new("http://registry.hive.dev");
        let err = remote
            .search("anything")
            .await
            .expect_err("a plain-http remote registry must be refused");
        assert!(
            matches!(err, ClientError::InsecureUrl(_)),
            "expected InsecureUrl, got {err:?}"
        );

        // A loopback registry is allowed to use http://; the gate itself
        // must not be what stops the request (it may still fail because
        // nothing is listening on this port in the test environment).
        let local = HiveRegistryClient::new("http://127.0.0.1:1");
        let err = local
            .search("anything")
            .await
            .expect_err("nothing listens on this port");
        assert!(
            !matches!(err, ClientError::InsecureUrl(_)),
            "a loopback registry must not be rejected as insecure, got {err:?}"
        );

        // https:// is always fine, wherever the host is.
        let https = HiveRegistryClient::new("https://registry.hive.dev");
        assert!(https.ensure_secure().is_ok());
    }
}
