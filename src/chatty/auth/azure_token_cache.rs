use anyhow::{Context, Result};
use azure_core::auth::TokenCredential;
use azure_identity::{DefaultAzureCredential, TokenCredentialOptions};
use gpui::Global;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;

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
pub struct AzureTokenCache {
    /// Singleton credential instance (reused for all token fetches)
    credential: Arc<DefaultAzureCredential>,
    /// Cached token with expiry timestamp
    cached_token: Arc<RwLock<Option<CachedToken>>>,
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
        })
    }

    /// Get a valid token, refreshing if needed
    ///
    /// Returns cached token if it has > 5 minutes until expiry.
    /// Otherwise, fetches a fresh token from Azure.
    pub async fn get_token(&self) -> Result<String> {
        // Check if cached token is still valid
        {
            let cached = self.cached_token.read().await;
            if let Some(ref token) = *cached {
                if let Ok(ttl) = token.expires_at.duration_since(SystemTime::now())
                    && ttl > Duration::from_secs(TOKEN_REFRESH_THRESHOLD_SECS)
                {
                    tracing::debug!(ttl_seconds = ttl.as_secs(), "Using cached Azure token");
                    return Ok(token.token.clone());
                }
                tracing::info!("Azure token expired or expiring soon, refreshing");
            }
        }

        // Token expired or not cached, refresh it
        self.refresh_token().await
    }

    /// Force token refresh (called on 401 errors or manual refresh)
    ///
    /// Fetches a new token from Azure and updates the cache.
    /// This method should be called when:
    /// - A 401 Unauthorized error is detected
    /// - Token is near expiry (< 5 minutes)
    /// - No cached token exists
    pub async fn refresh_token(&self) -> Result<String> {
        tracing::info!("Refreshing Azure Entra ID token");

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
