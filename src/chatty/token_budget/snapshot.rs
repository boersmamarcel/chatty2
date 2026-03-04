/// A point-in-time view of how tokens are distributed across context components.
///
/// Computed once before each prompt is sent (in a background thread via `tokio::spawn_blocking`),
/// then published through a `tokio::sync::watch` channel so the UI can read it cheaply on every
/// repaint without blocking the chat flow.
#[derive(Clone, Debug)]
pub struct TokenBudgetSnapshot {
    /// When this snapshot was computed (used for staleness detection in the UI)
    #[allow(dead_code)]
    pub computed_at: std::time::Instant,

    /// Hard token limit for the active model (from `ModelConfig.max_context_window`)
    pub model_context_limit: usize,

    /// Tokens reserved for model response output (from `TokenTrackingSettings.response_reserve`)
    pub response_reserve: usize,

    // ── Pre-send estimates (computed via tiktoken-rs before each prompt) ────
    /// Tokens consumed by the system preamble (user-authored prompt text)
    pub preamble_tokens: usize,
    /// Tokens consumed by tool JSON schemas sent to the provider
    pub tool_definitions_tokens: usize,
    /// Tokens consumed by conversation history (all prior messages)
    pub conversation_history_tokens: usize,
    /// Tokens consumed by the new user message being sent
    pub latest_user_message_tokens: usize,

    // ── Post-response actuals (populated after each turn via provider Usage) ─
    /// Raw input token count reported by the provider API for the last turn.
    /// May differ from `estimated_total()` due to provider-specific overhead tokens.
    pub actual_input_tokens: Option<usize>,
    /// Raw output token count reported by the provider API for the last turn.
    pub actual_output_tokens: Option<usize>,

    /// ID of the conversation this snapshot belongs to.
    /// Used to detect and discard stale snapshots when the user switches conversations.
    pub conversation_id: String,
}

impl TokenBudgetSnapshot {
    /// Effective token budget = model hard limit minus the response reserve.
    /// This is the budget available for *input* content.
    pub fn effective_budget(&self) -> usize {
        self.model_context_limit
            .saturating_sub(self.response_reserve)
    }

    /// Sum of all estimated input token components.
    pub fn estimated_total(&self) -> usize {
        self.preamble_tokens
            + self.tool_definitions_tokens
            + self.conversation_history_tokens
            + self.latest_user_message_tokens
    }

    /// Tokens remaining before the effective budget is exhausted.
    /// Saturates to 0 (never goes negative).
    pub fn remaining(&self) -> usize {
        self.effective_budget()
            .saturating_sub(self.estimated_total())
    }

    /// Utilization ratio in the range `0.0–1.0+` (can exceed 1.0 when over budget).
    pub fn utilization(&self) -> f64 {
        let budget = self.effective_budget();
        if budget == 0 {
            return 0.0;
        }
        self.estimated_total() as f64 / budget as f64
    }

    /// Per-component fractions of the effective budget, each in `0.0–1.0`.
    /// Values are independent — they do NOT necessarily sum to 1.0 (remaining space is implied).
    pub fn component_fractions(&self) -> ComponentFractions {
        let budget = self.effective_budget() as f64;
        if budget <= 0.0 {
            return ComponentFractions::default();
        }
        ComponentFractions {
            preamble: (self.preamble_tokens as f64 / budget).clamp(0.0, 1.0),
            tools: (self.tool_definitions_tokens as f64 / budget).clamp(0.0, 1.0),
            history: (self.conversation_history_tokens as f64 / budget).clamp(0.0, 1.0),
            user_msg: (self.latest_user_message_tokens as f64 / budget).clamp(0.0, 1.0),
        }
    }

    /// Traffic-light status for UI colour coding.
    /// Thresholds can be made configurable via `TokenTrackingSettings` in a later pass;
    /// the default values match the MemGPT research (70%/90%).
    pub fn status(&self) -> ContextStatus {
        match self.utilization() {
            u if u < 0.50 => ContextStatus::Normal,
            u if u < 0.70 => ContextStatus::Moderate,
            u if u < 0.90 => ContextStatus::High,
            _ => ContextStatus::Critical,
        }
    }

    /// Whether the provider has returned actual token counts for this snapshot.
    pub fn has_actuals(&self) -> bool {
        self.actual_input_tokens.is_some()
    }

    /// Difference between the actual provider-reported input tokens and our pre-send estimate.
    /// Positive means we under-estimated; negative means we over-estimated.
    /// Returns `None` if actuals have not yet been received.
    pub fn estimation_delta(&self) -> Option<i64> {
        self.actual_input_tokens
            .map(|actual| actual as i64 - self.estimated_total() as i64)
    }
}

// ── ComponentFractions ────────────────────────────────────────────────────────

/// Per-component fractions of the effective token budget (each 0.0–1.0).
/// Consumed directly by the GPUI stacked-bar render function.
#[derive(Clone, Debug, Default)]
pub struct ComponentFractions {
    /// Fraction used by the system preamble
    pub preamble: f64,
    /// Fraction used by tool JSON schemas
    pub tools: f64,
    /// Fraction used by conversation history messages
    pub history: f64,
    /// Fraction used by the latest user message
    pub user_msg: f64,
}

impl ComponentFractions {
    /// Fraction of the budget that is still free (remaining headroom).
    /// Clamped to `0.0–1.0` — may be 0.0 when over budget.
    pub fn remaining(&self) -> f64 {
        (1.0 - self.preamble - self.tools - self.history - self.user_msg).clamp(0.0, 1.0)
    }

    /// True when all components are zero (e.g. snapshot has not been computed yet).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.preamble == 0.0 && self.tools == 0.0 && self.history == 0.0 && self.user_msg == 0.0
    }
}

// ── ContextStatus ─────────────────────────────────────────────────────────────

/// Traffic-light status derived from the current utilization ratio.
/// Drives bar colour and popover messaging in the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextStatus {
    /// < 50 % — plenty of headroom, no special indication
    Normal,
    /// 50–70 % — moderate usage, no special indication
    Moderate,
    /// 70–90 % — bar turns amber, soft warning
    High,
    /// > 90 % — bar turns red, suggest summarization
    Critical,
}

impl ContextStatus {
    /// Human-readable label for the popover.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            ContextStatus::Normal => "Normal",
            ContextStatus::Moderate => "Moderate",
            ContextStatus::High => "High",
            ContextStatus::Critical => "Critical — consider summarizing",
        }
    }

    /// Whether this status warrants a warning colour in the UI.
    pub fn is_warning(&self) -> bool {
        matches!(self, ContextStatus::High | ContextStatus::Critical)
    }

    /// Whether this status warrants urgent intervention.
    pub fn is_critical(&self) -> bool {
        matches!(self, ContextStatus::Critical)
    }
}

// ── ContextPressureEvent ──────────────────────────────────────────────────────

/// Emitted (via GPUI's event system) when context utilization crosses a threshold.
///
/// Subscribe in `app_controller.rs` to show warnings, block agent loops,
/// or trigger automatic summarization.
///
/// # Example
/// ```rust
/// cx.subscribe(&notifier, |_, _, event: &ContextPressureEvent, _cx| {
///     match event {
///         ContextPressureEvent::CriticalPressure { utilization, .. } => {
///             warn!(%utilization, "Context window critically full");
///         }
///         _ => {}
///     }
/// }).detach();
/// ```
// `Relieved` variant and GPUI EventEmitter subscription are not yet wired up.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum ContextPressureEvent {
    /// Utilization crossed the high threshold on the way up (default: 70 %).
    /// Informational — no action required, but the user should be aware.
    HighPressure {
        utilization: f64,
        estimated_tokens: usize,
        conversation_id: String,
    },

    /// Utilization crossed the critical threshold (default: 90 %).
    /// Action needed — consider summarization before the next message.
    CriticalPressure {
        utilization: f64,
        estimated_tokens: usize,
        conversation_id: String,
    },

    /// Utilization dropped back below the high threshold (e.g. after summarization).
    Relieved {
        utilization: f64,
        /// How many tokens were freed by the operation that relieved pressure.
        tokens_freed: usize,
        conversation_id: String,
    },
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(
        preamble: usize,
        tools: usize,
        history: usize,
        user_msg: usize,
        limit: usize,
        reserve: usize,
    ) -> TokenBudgetSnapshot {
        TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: limit,
            response_reserve: reserve,
            preamble_tokens: preamble,
            tool_definitions_tokens: tools,
            conversation_history_tokens: history,
            latest_user_message_tokens: user_msg,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "test".to_string(),
        }
    }

    #[test]
    fn effective_budget_subtracts_reserve() {
        let snap = make_snapshot(0, 0, 0, 0, 128_000, 4_096);
        assert_eq!(snap.effective_budget(), 123_904);
    }

    #[test]
    fn effective_budget_saturates_at_zero() {
        let snap = make_snapshot(0, 0, 0, 0, 1_000, 5_000);
        assert_eq!(snap.effective_budget(), 0);
    }

    #[test]
    fn estimated_total_sums_components() {
        let snap = make_snapshot(1_000, 2_000, 30_000, 500, 128_000, 4_096);
        assert_eq!(snap.estimated_total(), 33_500);
    }

    #[test]
    fn remaining_computes_headroom() {
        let snap = make_snapshot(1_000, 2_000, 30_000, 500, 128_000, 4_096);
        assert_eq!(snap.remaining(), snap.effective_budget() - 33_500);
    }

    #[test]
    fn remaining_saturates_at_zero_when_over_budget() {
        let snap = make_snapshot(100_000, 20_000, 10_000, 500, 128_000, 4_096);
        assert_eq!(snap.remaining(), 0);
    }

    #[test]
    fn utilization_normal_status() {
        let snap = make_snapshot(1_000, 2_000, 5_000, 100, 128_000, 4_096);
        assert_eq!(snap.status(), ContextStatus::Normal);
    }

    #[test]
    fn utilization_high_status_at_80_pct() {
        let budget = 100_000usize;
        let snap = make_snapshot(60_000, 10_000, 10_000, 500, budget + 4_096, 4_096);
        assert_eq!(snap.status(), ContextStatus::High);
    }

    #[test]
    fn utilization_critical_status_at_95_pct() {
        let budget = 100_000usize;
        let snap = make_snapshot(80_000, 10_000, 5_000, 500, budget + 4_096, 4_096);
        assert_eq!(snap.status(), ContextStatus::Critical);
    }

    #[test]
    fn component_fractions_sum_correctly() {
        let snap = make_snapshot(10_000, 5_000, 40_000, 1_000, 128_000, 4_096);
        let frac = snap.component_fractions();
        let total = frac.preamble + frac.tools + frac.history + frac.user_msg;
        // Should be approximately 56_000 / 123_904 ≈ 0.452
        assert!((total - 0.452).abs() < 0.01, "total fraction = {total}");
    }

    #[test]
    fn component_fractions_remaining() {
        let snap = make_snapshot(10_000, 5_000, 40_000, 1_000, 128_000, 4_096);
        let frac = snap.component_fractions();
        let remaining = frac.remaining();
        assert!(
            remaining > 0.0 && remaining < 1.0,
            "remaining = {remaining}"
        );
    }

    #[test]
    fn estimation_delta_when_no_actuals() {
        let snap = make_snapshot(1_000, 2_000, 5_000, 100, 128_000, 4_096);
        assert_eq!(snap.estimation_delta(), None);
    }

    #[test]
    fn estimation_delta_with_actuals() {
        let mut snap = make_snapshot(1_000, 2_000, 5_000, 100, 128_000, 4_096);
        snap.actual_input_tokens = Some(8_500);
        // estimated_total = 8_100, actual = 8_500, delta = +400
        assert_eq!(snap.estimation_delta(), Some(400));
    }

    #[test]
    fn context_status_labels_not_empty() {
        for status in [
            ContextStatus::Normal,
            ContextStatus::Moderate,
            ContextStatus::High,
            ContextStatus::Critical,
        ] {
            assert!(!status.label().is_empty());
        }
    }

    #[test]
    fn context_status_is_warning_flags() {
        assert!(!ContextStatus::Normal.is_warning());
        assert!(!ContextStatus::Moderate.is_warning());
        assert!(ContextStatus::High.is_warning());
        assert!(ContextStatus::Critical.is_warning());
    }

    #[test]
    fn context_status_is_critical_flag() {
        assert!(!ContextStatus::Normal.is_critical());
        assert!(!ContextStatus::Moderate.is_critical());
        assert!(!ContextStatus::High.is_critical());
        assert!(ContextStatus::Critical.is_critical());
    }
}
