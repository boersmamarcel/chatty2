//! Pre-invocation credit guard with cached balance checking.
//!
//! The [`CreditGuard`] decides whether one call to a paid module may run:
//! inside the publisher's free tier (from the registry), or on a positive
//! credit balance. It caches the balance for a configurable TTL and applies
//! optimistic local deductions to avoid round-trips on every call. A paid
//! call whose credits cannot be established is refused: an unreachable
//! registry or an expired sign-in must not make a paid module free.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::client::HiveRegistryClient;
use crate::error::ClientError;

/// Why a paid call was refused.
#[derive(Debug, Clone)]
pub enum CreditRefusal {
    /// The free tier is spent and the balance is not positive.
    Insufficient(InsufficientFunds),
    /// The balance could not be read (signed out, sign-in expired, registry
    /// unreachable).
    Unverified { module_name: String, reason: String },
}

impl std::fmt::Display for CreditRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Insufficient(funds) => funds.fmt(f),
            Self::Unverified {
                module_name,
                reason,
            } => write!(
                f,
                "Cannot verify credits for paid module '{module_name}' ({reason}); \
                 sign in to Hive in chatty and try again"
            ),
        }
    }
}

impl std::error::Error for CreditRefusal {}

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

/// How an admitted paid call is paid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// One of the publisher's free calls.
    FreeTier,
    /// The user's credit balance.
    Credits,
}

/// One module's free tier as this process counts it: the allowance and
/// the calls used, the registry's count when first read plus every free
/// call admitted here since (those may not be reported yet).
struct FreeTier {
    allowance: i64,
    used: i64,
}

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
    free_tiers: Mutex<HashMap<String, FreeTier>>,
    ttl: Duration,
}

impl CreditGuard {
    /// Create a new guard with the given client and cache TTL.
    pub fn new(client: Arc<HiveRegistryClient>, ttl: Duration) -> Self {
        Self {
            client,
            cache: Mutex::new(None),
            free_tiers: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Create a guard with the default 30-second TTL.
    pub fn with_default_ttl(client: Arc<HiveRegistryClient>) -> Self {
        Self::new(client, Duration::from_secs(30))
    }

    /// Admit one call to the paid module `module_name`, or refuse it.
    ///
    /// A call inside the publisher's free tier (`free_tier_calls` from the
    /// registry, counted against the user's recorded calls) is admitted
    /// whatever the balance; after that the balance must be positive. A
    /// balance that cannot be read refuses the call (fail closed).
    pub async fn admit(&self, module_name: &str) -> Result<Admission, CreditRefusal> {
        if self.take_free_call(module_name).await {
            return Ok(Admission::FreeTier);
        }
        let balance = self
            .get_balance()
            .await
            .map_err(|e| CreditRefusal::Unverified {
                module_name: module_name.to_string(),
                reason: e.to_string(),
            })?;
        if balance <= 0 {
            return Err(CreditRefusal::Insufficient(InsufficientFunds {
                balance_tokens: balance,
                module_name: module_name.to_string(),
            }));
        }
        Ok(Admission::Credits)
    }

    /// Spend one of `module_name`'s free calls, if any is left. The tier is
    /// read from the registry once per guard; a registry that cannot say
    /// (no pricing row, usage unreadable) leaves no free calls.
    async fn take_free_call(&self, module_name: &str) -> bool {
        let mut tiers = self.free_tiers.lock().await;
        if !tiers.contains_key(module_name) {
            let tier = self.read_free_tier(module_name).await;
            tiers.insert(module_name.to_string(), tier);
        }
        let tier = tiers
            .get_mut(module_name)
            .expect("inserted above when missing");
        if tier.used < tier.allowance {
            tier.used += 1;
            true
        } else {
            false
        }
    }

    async fn read_free_tier(&self, module_name: &str) -> FreeTier {
        let none = FreeTier {
            allowance: 0,
            used: 0,
        };
        let allowance = match self.client.get_module_pricing(module_name).await {
            Ok(pricing) => i64::from(pricing.free_tier_calls.max(0)),
            Err(_) => return none,
        };
        if allowance == 0 {
            return none;
        }
        match self.client.get_my_modules_usage().await {
            Ok(usage) => FreeTier {
                allowance,
                used: usage
                    .items
                    .iter()
                    .filter(|m| m.module_name == module_name)
                    .map(|m| m.total_invocations)
                    .sum(),
            },
            Err(_) => none,
        }
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
            if let Some(ref cached) = *cache
                && cached.fetched_at.elapsed() < self.ttl
            {
                return Ok(cached.balance_tokens);
            }
        }
        self.refresh().await
    }
}
