//! Pre-invocation credit guard with cached balance checking.
//!
//! The [`CreditGuard`] provides a fast, cached credit balance check before
//! module invocations. It caches the balance for a configurable TTL and
//! applies optimistic local deductions to avoid round-trips on every call.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::client::HiveRegistryClient;
use crate::error::ClientError;
use crate::models::ModulePricingInfo;

/// Error returned when a user has insufficient credits.
#[derive(Debug, Clone)]
pub struct InsufficientFunds {
    pub balance_tokens: i64,
    pub module_name: String,
}

impl std::fmt::Display for InsufficientFunds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Insufficient credits ({} tokens) for module '{}'",
            self.balance_tokens, self.module_name
        )
    }
}

impl std::error::Error for InsufficientFunds {}

struct CachedBalance {
    balance_tokens: i64,
    fetched_at: Instant,
}

/// Pre-invocation credit guard.
///
/// Caches the user's token balance and applies optimistic local deductions
/// to minimize network round-trips. Automatically refreshes the cache when
/// the TTL expires.
pub struct CreditGuard {
    client: Arc<HiveRegistryClient>,
    cache: Mutex<Option<CachedBalance>>,
    ttl: Duration,
}

impl CreditGuard {
    /// Create a new guard with the given client and cache TTL.
    pub fn new(client: Arc<HiveRegistryClient>, ttl: Duration) -> Self {
        Self {
            client,
            cache: Mutex::new(None),
            ttl,
        }
    }

    /// Create a guard with the default 30-second TTL.
    pub fn with_default_ttl(client: Arc<HiveRegistryClient>) -> Self {
        Self::new(client, Duration::from_secs(30))
    }

    /// Quick check whether the user has any credits at all.
    ///
    /// This is useful when per-module pricing info isn't available locally.
    /// Returns `Ok(())` if balance is positive or if the balance cannot be fetched
    /// (fail-open). Returns `Err(InsufficientFunds)` if balance is known to be ≤ 0.
    pub async fn has_credits(&self, module_name: &str) -> Result<(), InsufficientFunds> {
        let balance = match self.get_balance().await {
            Ok(b) => b,
            Err(_) => return Ok(()), // fail-open
        };
        if balance <= 0 {
            return Err(InsufficientFunds {
                balance_tokens: balance,
                module_name: module_name.to_string(),
            });
        }
        Ok(())
    }

    /// Check whether the user has sufficient funds for a module invocation.
    ///
    /// For free modules (pricing is `None` or `price_per_call` is `"0"` /
    /// `"0.000000"`), this always succeeds. For paid modules it checks the
    /// cached balance.
    ///
    /// Returns `Ok(())` if funds are sufficient, `Err(InsufficientFunds)` otherwise.
    pub async fn check_funds(
        &self,
        module_name: &str,
        pricing: Option<&ModulePricingInfo>,
    ) -> Result<(), InsufficientFunds> {
        // Free modules always pass
        let pricing = match pricing {
            Some(p) => p,
            None => return Ok(()),
        };

        let price: f64 = pricing.price_per_call.parse().unwrap_or(0.0);
        if price <= 0.0 {
            return Ok(());
        }

        // Fetch/refresh balance
        let balance = self.get_balance().await;

        // If we can't fetch balance, allow the call (fail-open for availability)
        let balance = match balance {
            Ok(b) => b,
            Err(_) => return Ok(()),
        };

        if balance <= 0 {
            return Err(InsufficientFunds {
                balance_tokens: balance,
                module_name: module_name.to_string(),
            });
        }

        Ok(())
    }

    /// Apply an optimistic local deduction after a successful invocation.
    /// This reduces the cached balance without a network call.
    pub async fn deduct_local(&self, tokens: i64) {
        let mut cache = self.cache.lock().await;
        if let Some(ref mut cached) = *cache {
            cached.balance_tokens = cached.balance_tokens.saturating_sub(tokens);
        }
    }

    /// Force refresh the cached balance from the server.
    pub async fn refresh(&self) -> Result<i64, ClientError> {
        let credit_balance = self.client.get_credit_balance().await?;
        let balance = credit_balance.balance_tokens;
        let mut cache = self.cache.lock().await;
        *cache = Some(CachedBalance {
            balance_tokens: balance,
            fetched_at: Instant::now(),
        });
        Ok(balance)
    }

    /// Get the current balance, using cache if still valid.
    async fn get_balance(&self) -> Result<i64, ClientError> {
        {
            let cache = self.cache.lock().await;
            if let Some(ref cached) = *cache {
                if cached.fetched_at.elapsed() < self.ttl {
                    return Ok(cached.balance_tokens);
                }
            }
        }
        self.refresh().await
    }
}
