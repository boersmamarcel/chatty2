//! The hosted per-user spend cap, asked at the one place a leader starts
//! spending on someone else's behalf (AGE-416, child of ADR-0010).
//!
//! Every spend control the fleet has is per process (`max_agent_turns`, the
//! endpoint budget, the silence timeout); nothing bounds a task *tree*. Until
//! the task frame carries a budget (ADR-0006), the tenant's monthly cap is
//! that bound, and `invoke_agent` asks it before opening a delegation.
//!
//! chatty2 ships the trait and the plumbing only. The implementation — the
//! month-to-date lookup — is hive's, since the usage ledger lives there. A
//! leader built without a gate (the desktop, chatty-tui) never asks and
//! behaves exactly as before.

use async_trait::async_trait;
use serde::Serialize;

/// The cap is already spent: a delegation must not start.
///
/// Serialises to the same three fields hive's `402` body carries —
/// `error: "cap_exceeded"`, `month_to_date`, `cap_usd` — so a refusal reads
/// the same whether it came from the HTTP turn route or from `invoke_agent`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "error", rename = "cap_exceeded")]
pub struct CapExceeded {
    /// What the user has spent this month, in USD.
    #[serde(rename = "month_to_date")]
    pub month_to_date_usd: f64,
    /// The monthly cap, in USD (`TokenTrackingSettings::cap_usd`).
    pub cap_usd: f64,
}

impl std::fmt::Display for CapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cap_exceeded: month-to-date ${:.2} \u{2265} cap ${:.2}; no delegation started",
            self.month_to_date_usd, self.cap_usd
        )
    }
}

impl std::error::Error for CapExceeded {}

/// Whether this user may start spending right now.
///
/// It compares month-to-date to the cap and nothing more: no estimate of what
/// the turn about to start will cost. A delegation that was already running
/// when the cap was crossed finishes — this is a gate at the entry, not a
/// kill switch.
#[async_trait]
pub trait SpendGate: Send + Sync {
    /// `Ok(())` to proceed; `Err` with the figures when the cap is spent.
    async fn check(&self) -> Result<(), CapExceeded>;
}

/// A gate with a fixed answer, for tests: what a hosted leader sees when its
/// tenant is under or over the cap, without a usage ledger behind it.
/// Test-only: enable `chatty-core/test-support` from a dev-dependency.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct FixedSpendGate(pub Result<(), CapExceeded>);

#[cfg(any(test, feature = "test-support"))]
impl FixedSpendGate {
    /// A tenant under its cap: every check passes.
    pub fn permitting() -> Self {
        Self(Ok(()))
    }

    /// A tenant over its cap: every check refuses with these figures.
    pub fn refusing(month_to_date_usd: f64, cap_usd: f64) -> Self {
        Self(Err(CapExceeded {
            month_to_date_usd,
            cap_usd,
        }))
    }
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl SpendGate for FixedSpendGate {
    async fn check(&self) -> Result<(), CapExceeded> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_serialises_to_the_three_fields_hive_uses() {
        let refused = CapExceeded {
            month_to_date_usd: 12.5,
            cap_usd: 10.0,
        };
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            serde_json::json!({
                "error": "cap_exceeded",
                "month_to_date": 12.5,
                "cap_usd": 10.0,
            })
        );
    }

    #[test]
    fn a_refusal_reads_as_the_typed_tool_error_text() {
        let refused = CapExceeded {
            month_to_date_usd: 12.5,
            cap_usd: 10.0,
        };
        assert_eq!(
            refused.to_string(),
            "cap_exceeded: month-to-date $12.50 \u{2265} cap $10.00; no delegation started"
        );
    }
}
