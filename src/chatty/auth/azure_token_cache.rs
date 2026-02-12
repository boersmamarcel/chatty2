use anyhow::{Context, Result};
use azure_core::auth::TokenCredential;
use azure_identity::{DefaultAzureCredential, TokenCredentialOptions};
use gpui::Global;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, RwLock};

const AZURE_OPENAI_SCOPE: &str = "https://cognitiveservices.azure.com/.default";
const TOKEN_REFRESH_THRESHOLD_SECS: u64 = 5 * 60; // 5 minutes

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: SystemTime,
}

/// Global cache for Azure Entra ID credentials and tokens
///
/// This cache stores a single `DefaultAzureCredential` instance (expensive to create)
/// and manages token lifecycle with automatic refresh before expiry.
///
/// Token refresh triggers:
/// - No cached token exists
/// - Cached token expires in < 5 minutes
/// - Explicit refresh request (e.g., after 401 error)
///
/// Thread safety:
/// - `cached_token` uses RwLock for concurrent reads
/// - `refresh_lock` prevents duplicate simultaneous refreshes
#[derive(Clone)]
pub struct AzureTokenCache {
    /// Singleton credential instance (reused for all token fetches)
    credential: Arc<DefaultAzureCredential>,
    /// Cached token with expiry timestamp
    cached_token: Arc<RwLock<Option<CachedToken>>>,
    /// Mutex to prevent concurrent refresh operations
    refresh_lock: Arc<Mutex<()>>,
}

impl Global for AzureTokenCache {}

impl AzureTokenCache {
    /// Create a new token cache with Azure credentials
    ///
    /// Uses `DefaultAzureCredential` which tries (in order):
    /// 1. Environment variables (AZURE_CLIENT_ID, AZURE_TENANT_ID, AZURE_CLIENT_SECRET)
    /// 2. Managed Identity (if running on Azure)
    /// 3. Azure CLI (`az login`)
    /// 4. Interactive browser authentication (if configured)
    pub fn new() -> Result<Self> {
        tracing::info!("Creating Azure token cache with DefaultAzureCredential");

        let credential = DefaultAzureCredential::create(TokenCredentialOptions::default())
            .context("Failed to create DefaultAzureCredential")?;

        Ok(Self {
            credential: Arc::new(credential),
            cached_token: Arc::new(RwLock::new(None)),
            refresh_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Get a valid token, refreshing if needed
    ///
    /// Returns cached token if it has > 5 minutes until expiry.
    /// Otherwise, fetches a fresh token from Azure.
    ///
    /// Uses double-check locking to prevent race conditions:
    /// 1. First check: read lock (fast path for valid tokens)
    /// 2. Acquire refresh lock
    /// 3. Second check: another thread may have refreshed while we waited
    /// 4. Refresh if still needed
    pub async fn get_token(&self) -> Result<String> {
        // First check: fast path for valid cached tokens
        {
            let cached = self.cached_token.read().await;
            if let Some(ref token) = *cached
                && let Ok(ttl) = token.expires_at.duration_since(SystemTime::now())
                && ttl > Duration::from_secs(TOKEN_REFRESH_THRESHOLD_SECS)
            {
                tracing::debug!(ttl_seconds = ttl.as_secs(), "Using cached Azure token");
                return Ok(token.token.clone());
            }
        }

        // Token needs refresh - acquire lock to prevent duplicate refreshes
        let _guard = self.refresh_lock.lock().await;

        // Second check: another thread may have refreshed while we waited for the lock
        {
            let cached = self.cached_token.read().await;
            if let Some(ref token) = *cached
                && let Ok(ttl) = token.expires_at.duration_since(SystemTime::now())
                && ttl > Duration::from_secs(TOKEN_REFRESH_THRESHOLD_SECS)
            {
                tracing::debug!(
                    ttl_seconds = ttl.as_secs(),
                    "Using token refreshed by another thread"
                );
                return Ok(token.token.clone());
            }
        }

        // Still needs refresh - we hold the lock, safe to refresh
        tracing::info!("Azure token expired or expiring soon, refreshing");
        self.do_refresh_token().await
    }

    /// Force token refresh (called on 401 errors or manual refresh)
    ///
    /// Fetches a new token from Azure and updates the cache.
    /// This method should be called when:
    /// - A 401 Unauthorized error is detected during an API call
    ///
    /// Uses mutex to prevent concurrent refresh operations.
    pub async fn refresh_token(&self) -> Result<String> {
        // Acquire lock to prevent duplicate refreshes
        let _guard = self.refresh_lock.lock().await;

        tracing::info!("Forcing Azure token refresh");
        self.do_refresh_token().await
    }

    /// Internal method to perform the actual token refresh
    ///
    /// MUST be called while holding the refresh_lock to prevent race conditions.
    /// Use `refresh_token()` or `get_token()` instead of calling this directly.
    async fn do_refresh_token(&self) -> Result<String> {
        tracing::debug!("Fetching new token from Azure");

        let token_response = self
            .credential
            .get_token(&[AZURE_OPENAI_SCOPE])
            .await
            .context("Failed to refresh Azure Entra ID token")?;

        let token_string = token_response.token.secret().to_string();
        let expires_at: SystemTime = token_response.expires_on.into();

        // Cache the new token
        {
            let mut cached = self.cached_token.write().await;
            *cached = Some(CachedToken {
                token: token_string.clone(),
                expires_at,
            });
        }

        tracing::info!(
            expires_at = ?expires_at,
            "Azure token refreshed successfully"
        );

        Ok(token_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_cached_token_within_threshold() {
        // Token that expires in 10 minutes should still be valid
        let token = CachedToken {
            token: "test-token".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(10 * 60),
        };

        let ttl = token.expires_at.duration_since(SystemTime::now()).unwrap();
        assert!(ttl > Duration::from_secs(TOKEN_REFRESH_THRESHOLD_SECS));
    }

    #[test]
    fn test_cached_token_needs_refresh() {
        // Token that expires in 3 minutes should need refresh
        let token = CachedToken {
            token: "test-token".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(3 * 60),
        };

        let ttl = token.expires_at.duration_since(SystemTime::now()).unwrap();
        assert!(ttl < Duration::from_secs(TOKEN_REFRESH_THRESHOLD_SECS));
    }

    #[test]
    fn test_cached_token_expired() {
        // Token that expired 5 minutes ago
        let token = CachedToken {
            token: "test-token".to_string(),
            expires_at: SystemTime::now() - Duration::from_secs(5 * 60),
        };

        // duration_since should return an error for past times
        assert!(token.expires_at.duration_since(SystemTime::now()).is_err());
    }

    #[test]
    fn test_token_refresh_threshold_constant() {
        // Verify the threshold is set to 5 minutes as documented
        assert_eq!(TOKEN_REFRESH_THRESHOLD_SECS, 5 * 60);
    }

    #[test]
    fn test_azure_openai_scope_constant() {
        // Verify the scope is correct for Azure OpenAI
        assert_eq!(
            AZURE_OPENAI_SCOPE,
            "https://cognitiveservices.azure.com/.default"
        );
    }
}
