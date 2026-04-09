//! HTTP client for the Hive module registry.

use std::path::PathBuf;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    cache::Cache,
    error::ClientError,
    models::{
        AuthTokenResponse, CategoryList, DownloadResult, ListParams, ModuleList, ModuleMetadata,
        VersionList,
    },
    verify::{self, TrustLevel, VerifyInput},
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
    token: Option<String>,
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

    fn with_timeout_inner(base_url: impl Into<String>, timeout: std::time::Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
            cache: None,
            token: None,
        }
    }

    /// Enable the offline module-list cache, persisted under `dir`.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        self.cache = Some(Cache::new(dir)?);
        Ok(self)
    }

    /// Set the Bearer token used for authenticated endpoints (e.g. downloads).
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    // ── Authentication ─────────────────────────────────────────────────────

    /// Register a new account on the Hive registry.
    pub async fn register(
        &self,
        username: &str,
        email: &str,
        password: &str,
    ) -> Result<AuthTokenResponse, ClientError> {
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

        resp.json::<AuthTokenResponse>()
            .await
            .map_err(ClientError::from)
    }

    /// Log in with email and password.
    pub async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> Result<AuthTokenResponse, ClientError> {
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

        resp.json::<AuthTokenResponse>()
            .await
            .map_err(ClientError::from)
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
        self.get_json::<VersionList>(
            &format!("/api/modules/{}/versions", urlencoded(name)),
            &(),
        )
        .await
    }

    // ── List categories ────────────────────────────────────────────────────

    /// List all module categories.
    pub async fn list_categories(&self) -> Result<CategoryList, ClientError> {
        self.get_json::<CategoryList>("/api/categories", &()).await
    }

    // ── Download ───────────────────────────────────────────────────────────

    /// Download the `.wasm` binary for a specific version.
    ///
    /// Performs integrity (SHA-256) and signature (Ed25519) verification.
    /// Returns [`ClientError::SignatureInvalid`] on any mismatch.
    pub async fn download(
        &self,
        name: &str,
        version: &str,
    ) -> Result<DownloadResult, ClientError> {
        let url = format!(
            "{}/api/modules/{}/{}",
            self.base_url,
            urlencoded(name),
            urlencoded(version)
        );

        tracing::debug!(%url, "downloading module");

        let mut request = self.http.get(&url);
        if let Some(ref token) = self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = request.send().await?;
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

        let registry_hash =
            header_str(response.headers(), "x-wasm-sha256").map(str::to_owned);
        let signature =
            header_str(response.headers(), "x-signature").map(str::to_owned);
        let publisher_public_key =
            header_str(response.headers(), "x-publisher-public-key").map(str::to_owned);

        let wasm_vec: Vec<u8> = response.bytes().await?.to_vec();

        // Integrity check
        let computed_hash = hex::encode(Sha256::digest(&wasm_vec));
        if let Some(ref expected) = registry_hash {
            if &computed_hash != expected {
                return Err(ClientError::SignatureInvalid(format!(
                    "hash mismatch: expected {expected}, got {computed_hash}"
                )));
            }
        }

        // Signature verification
        let trust_level =
            verify_ed25519(&computed_hash, signature.as_deref(), publisher_public_key.as_deref())?;

        // Fetch manifest from version metadata
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
            wasm: wasm_vec,
            wasm_hash: registry_hash.unwrap_or(computed_hash),
            trust_level,
            signature,
            publisher_public_key,
            manifest,
        })
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    async fn get_json<T>(&self, path: &str, query: &impl Serialize) -> Result<T, ClientError>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self.http.get(&url).query(query).send().await?;
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

    async fn api_error(resp: reqwest::Response) -> ClientError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        ClientError::Http { status, body }
    }

    fn maybe_store_cache(&self, key: &str, list: &ModuleList) {
        if let Some(cache) = &self.cache {
            if let Err(e) = cache.store(key, list) {
                tracing::warn!(error = %e, "failed to write module list cache");
            }
        }
    }
}

// ── Signature verification helper ─────────────────────────────────────────

fn verify_ed25519(
    wasm_hash: &str,
    signature: Option<&str>,
    publisher_public_key: Option<&str>,
) -> Result<TrustLevel, ClientError> {
    match (signature, publisher_public_key) {
        (Some(sig), Some(pub_key)) => {
            let input = VerifyInput {
                wasm_hash: wasm_hash.to_string(),
                signature: sig.to_string(),
                publisher_public_key: pub_key.to_string(),
            };
            verify::verify_module(&input)
                .map_err(|e| ClientError::SignatureInvalid(e.to_string()))
        }
        _ => Ok(TrustLevel::Local),
    }
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
