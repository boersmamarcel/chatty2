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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chatty_fabric::UsagePricer;
use serde::Serialize;

use crate::models::token_usage::{Cost, PriceBook, TokenUsage, price};
use crate::services::a2a_client::usage_from_wire;

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

/// A task's own dollar budget (PL-D2, DP-3): the usage lines the run has
/// seen so far — its own and its callees', each naming its model and time
/// (AGE-682) — priced on read with [`price`], the same function the display
/// uses, against a cap.
///
/// The cap is the spec's `[budget] cap_usd` (per task; not
/// `TokenTrackingSettings::cap_usd`, the hosted monthly cap), narrowed by
/// what a caller had left when it delegated this run ([`narrow`](Self::narrow)).
/// With no cap the gate is inactive. A line whose model the price book does
/// not price adds nothing: the gate is inactive for that line.
///
/// `invoke_agent` asks it through the run's
/// [`RunBudget`](crate::services::run_budget::RunBudget) before every
/// delegation, and a spent budget refuses with `budget_spent: usd`. As a
/// [`SpendGate`] it answers [`CapExceeded`] with the task's spend.
///
/// Clones share one record.
#[derive(Clone, Debug, Default)]
pub struct LocalSpendGate(Arc<Mutex<Spend>>);

#[derive(Debug, Default)]
struct Spend {
    cap_usd: Option<f64>,
    book: PriceBook,
    lines: Vec<TokenUsage>,
}

impl LocalSpendGate {
    /// A gate over `cap_usd` (`None`: inactive) pricing with `book`.
    pub fn new(cap_usd: Option<f64>, book: PriceBook) -> Self {
        Self(Arc::new(Mutex::new(Spend {
            cap_usd,
            book,
            lines: Vec::new(),
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Spend> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cap_usd(&self) -> Option<f64> {
        self.lock().cap_usd
    }

    /// Price lines with `book` from now on (and the ones already seen).
    pub fn set_price_book(&self, book: PriceBook) {
        self.lock().book = book;
    }

    /// Lower the cap to `usd` — what a caller had left — if that is
    /// tighter; a gate with no cap takes `usd` as its cap.
    pub fn narrow(&self, usd: f64) {
        let mut spend = self.lock();
        spend.cap_usd = Some(spend.cap_usd.map_or(usd, |cap| cap.min(usd)));
    }

    /// Usage lines the run spent: a model call's own, or a callee's.
    pub fn record(&self, lines: impl IntoIterator<Item = TokenUsage>) {
        self.lock().lines.extend(lines);
    }

    /// What the lines seen so far cost, and how many could not be priced.
    pub fn spent(&self) -> Cost {
        let spend = self.lock();
        price(&spend.lines, &spend.book)
    }

    /// The cap less what is spent; `None` when the gate has no cap.
    pub fn remaining_usd(&self) -> Option<f64> {
        let cap = self.cap_usd()?;
        Some(cap - self.spent().usd)
    }
}

#[async_trait]
impl SpendGate for LocalSpendGate {
    async fn check(&self) -> Result<(), CapExceeded> {
        match self.cap_usd() {
            Some(cap_usd) => {
                let spent = self.spent().usd;
                if spent >= cap_usd {
                    Err(CapExceeded {
                        month_to_date_usd: spent,
                        cap_usd,
                    })
                } else {
                    Ok(())
                }
            }
            None => Ok(()),
        }
    }
}

/// The broker's pricing of a callee's reported usage for its edge-log row
/// (DP-3): the dollars, or `unpriced` when any line's model has no price.
impl UsagePricer for PriceBook {
    fn usd(&self, usage: &chatty_fabric::wire::WireUsage) -> Option<String> {
        let lines = usage_from_wire(usage);
        if lines.is_empty() {
            return None;
        }
        let cost = price(&lines, self);
        Some(if cost.unpriced_lines > 0 {
            "unpriced".to_string()
        } else {
            format!("{:.6}", cost.usd)
        })
    }
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

    use crate::models::token_usage::{ModelRef, TokenPricing};
    use crate::settings::models::providers_store::ProviderType;

    fn model(id: &str) -> ModelRef {
        ModelRef {
            provider: ProviderType::OpenRouter,
            model_id: id.to_string(),
        }
    }

    fn line(model_id: &str, input: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            model: Some(model(model_id)),
            ..TokenUsage::default()
        }
    }

    /// $1 per thousand input tokens for `priced`, nothing for any other.
    fn book() -> PriceBook {
        let mut book = PriceBook::default();
        book.insert(
            model("priced"),
            TokenPricing {
                input_per_million: 1_000.0,
                ..TokenPricing::default()
            },
        );
        book
    }

    #[tokio::test]
    async fn a_local_gate_prices_the_lines_it_has_seen_against_its_cap() {
        let gate = LocalSpendGate::new(Some(0.10), book());
        assert_eq!(gate.remaining_usd(), Some(0.10));
        gate.record([line("priced", 80)]);
        assert!((gate.spent().usd - 0.08).abs() < 1e-9);
        assert_eq!(gate.check().await, Ok(()));
        gate.record([line("priced", 20)]);
        assert!(gate.remaining_usd().unwrap() <= 1e-9);
        assert!(gate.check().await.is_err());
    }

    #[test]
    fn an_unpriced_line_leaves_the_gate_inactive_for_that_line() {
        let gate = LocalSpendGate::new(Some(0.10), book());
        gate.record([line("unknown", 1_000_000)]);
        let spent = gate.spent();
        assert_eq!(spent.usd, 0.0);
        assert_eq!(spent.unpriced_lines, 1);
        assert_eq!(gate.remaining_usd(), Some(0.10));
    }

    #[test]
    fn a_gate_without_a_cap_is_inactive_until_a_caller_narrows_it() {
        let gate = LocalSpendGate::new(None, book());
        gate.record([line("priced", 1_000)]);
        assert_eq!(gate.remaining_usd(), None);
        gate.narrow(2.5);
        gate.narrow(3.0);
        assert_eq!(gate.cap_usd(), Some(2.5), "narrowing never raises the cap");
        assert!((gate.remaining_usd().unwrap() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn the_broker_prices_reported_usage_or_says_unpriced() {
        let report =
            |model_id: &str| crate::services::a2a_client::wire_usage(&[line(model_id, 80)]);
        assert_eq!(book().usd(&report("priced")).as_deref(), Some("0.080000"));
        assert_eq!(book().usd(&report("unknown")).as_deref(), Some("unpriced"));
        assert_eq!(
            book().usd(&crate::services::a2a_client::wire_usage(&[])),
            None
        );
    }

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
