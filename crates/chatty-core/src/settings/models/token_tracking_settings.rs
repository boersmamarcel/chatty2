use serde::{Deserialize, Serialize};

/// Settings that control token budget tracking behaviour.
///
/// Registered as a GPUI global in `main.rs` and read by:
/// - `manager::gather_snapshot_inputs()` — for the response reserve
/// - `manager::check_pressure()` — for the configurable thresholds
/// - `TokenContextBarView` — to decide whether to show the bar at all
///
/// Persistence via `json_file_repository` can be added in a follow-up;
/// for now, the defaults are applied at startup via `cx.set_global(TokenTrackingSettings::default())`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenTrackingSettings {
    /// Show the token context bar in the status footer.
    ///
    /// When `false`, the bar is hidden entirely and no token counting is performed.
    /// Default: `true`
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Tokens to reserve for model output (subtracted from the effective budget).
    ///
    /// The effective budget shown in the bar is `model_context_limit - response_reserve`.
    /// This prevents the bar from showing 100% when there are still tokens available
    /// for the model to generate a response.
    ///
    /// Default: `4096` (a reasonable minimum for most text responses)
    #[serde(default = "default_response_reserve")]
    pub response_reserve: usize,

    /// Utilization fraction at which the bar turns amber and a `HighPressure` event fires.
    ///
    /// Must be in the range `0.0–1.0` and must be less than `critical_threshold`.
    /// Default: `0.70` (70 % — matches MemGPT research recommendation)
    #[serde(default = "default_high_threshold")]
    pub high_threshold: f64,

    /// Utilization fraction at which the bar turns red and a `CriticalPressure` event fires.
    ///
    /// At this level the user should summarize the conversation before continuing.
    /// Must be in the range `0.0–1.0` and must be greater than `high_threshold`.
    /// Default: `0.90` (90 % — matches MemGPT research recommendation)
    #[serde(default = "default_critical_threshold")]
    pub critical_threshold: f64,

    /// Automatically summarize the oldest half of the conversation when
    /// `critical_threshold` is crossed.
    ///
    /// When `false` (default), a `CriticalPressure` event is fired and the user
    /// sees a warning, but no automatic action is taken. The user can click the
    /// "Summarize" button in the context bar popover to trigger summarization manually.
    ///
    /// When `true`, `summarizer::summarize_oldest_half()` is called automatically
    /// the first time each conversation crosses the critical threshold.
    ///
    /// Default: `false` — manual first, auto opt-in later
    #[serde(default)]
    pub auto_summarize: bool,

    /// Optional model ID (chatty's internal UUID, not the API model name) to use
    /// for summarization instead of the active conversation's model.
    ///
    /// Useful when you want to use a cheaper/faster model for compression
    /// (e.g. a local `qwen3:8b` via Ollama, or `gpt-4o-mini` for cloud users)
    /// while using a more capable model for normal conversation.
    ///
    /// `None` (default) means: use the active conversation's own agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarization_model_id: Option<String>,
}

// ── Default value functions ───────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn default_response_reserve() -> usize {
    4_096
}

fn default_high_threshold() -> f64 {
    0.70
}

fn default_critical_threshold() -> f64 {
    0.90
}

// ── Trait implementations ─────────────────────────────────────────────────────

impl Default for TokenTrackingSettings {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            response_reserve: default_response_reserve(),
            high_threshold: default_high_threshold(),
            critical_threshold: default_critical_threshold(),
            auto_summarize: false,
            summarization_model_id: None,
        }
    }
}

impl TokenTrackingSettings {
    /// Validate the settings and clamp/fix any out-of-range values.
    ///
    /// Called after deserialisation to guard against corrupted config files.
    /// Returns a corrected copy; does not mutate `self`.
    #[allow(dead_code)]
    pub fn validated(mut self) -> Self {
        // Clamp thresholds to [0.0, 1.0]
        self.high_threshold = self.high_threshold.clamp(0.0, 1.0);
        self.critical_threshold = self.critical_threshold.clamp(0.0, 1.0);

        // Ensure high < critical; if inverted, swap them
        if self.high_threshold >= self.critical_threshold {
            std::mem::swap(&mut self.high_threshold, &mut self.critical_threshold);
        }

        // Reserve must be positive; floor at 256 tokens
        if self.response_reserve < 256 {
            self.response_reserve = 256;
        }

        self
    }

    /// Return `true` if the bar should be rendered for a conversation whose model
    /// has `max_context_window` configured.
    #[allow(dead_code)]
    pub fn should_show_bar(&self) -> bool {
        self.enabled
    }

    /// Return `true` if the given utilization ratio crosses the high threshold.
    #[allow(dead_code)]
    pub fn is_high(&self, utilization: f64) -> bool {
        utilization >= self.high_threshold
    }

    /// Return `true` if the given utilization ratio crosses the critical threshold.
    #[allow(dead_code)]
    pub fn is_critical(&self, utilization: f64) -> bool {
        utilization >= self.critical_threshold
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_sensible() {
        let s = TokenTrackingSettings::default();
        assert!(s.enabled);
        assert_eq!(s.response_reserve, 4_096);
        assert!((s.high_threshold - 0.70).abs() < f64::EPSILON);
        assert!((s.critical_threshold - 0.90).abs() < f64::EPSILON);
        assert!(!s.auto_summarize);
        assert!(s.summarization_model_id.is_none());
    }

    #[test]
    fn validated_clamps_thresholds_above_one() {
        let s = TokenTrackingSettings {
            high_threshold: 1.5,
            critical_threshold: 2.0,
            ..Default::default()
        }
        .validated();
        assert!(s.high_threshold <= 1.0);
        assert!(s.critical_threshold <= 1.0);
    }

    #[test]
    fn validated_swaps_inverted_thresholds() {
        let s = TokenTrackingSettings {
            high_threshold: 0.95, // higher than critical
            critical_threshold: 0.60,
            ..Default::default()
        }
        .validated();
        assert!(s.high_threshold < s.critical_threshold);
    }

    #[test]
    fn validated_floors_tiny_response_reserve() {
        let s = TokenTrackingSettings {
            response_reserve: 10,
            ..Default::default()
        }
        .validated();
        assert_eq!(s.response_reserve, 256);
    }

    #[test]
    fn validated_leaves_valid_settings_unchanged() {
        let original = TokenTrackingSettings::default();
        let validated = original.clone().validated();
        assert_eq!(validated.response_reserve, original.response_reserve);
        assert!((validated.high_threshold - original.high_threshold).abs() < f64::EPSILON);
        assert!((validated.critical_threshold - original.critical_threshold).abs() < f64::EPSILON);
    }

    #[test]
    fn is_high_false_below_threshold() {
        let s = TokenTrackingSettings::default();
        assert!(!s.is_high(0.50));
        assert!(!s.is_high(0.69));
    }

    #[test]
    fn is_high_true_at_and_above_threshold() {
        let s = TokenTrackingSettings::default();
        assert!(s.is_high(0.70));
        assert!(s.is_high(0.85));
    }

    #[test]
    fn is_critical_false_below_threshold() {
        let s = TokenTrackingSettings::default();
        assert!(!s.is_critical(0.70));
        assert!(!s.is_critical(0.89));
    }

    #[test]
    fn is_critical_true_at_and_above_threshold() {
        let s = TokenTrackingSettings::default();
        assert!(s.is_critical(0.90));
        assert!(s.is_critical(1.00));
    }

    #[test]
    fn roundtrips_through_json() {
        let original = TokenTrackingSettings {
            enabled: false,
            response_reserve: 8_192,
            high_threshold: 0.65,
            critical_threshold: 0.85,
            auto_summarize: true,
            summarization_model_id: Some("some-model-id".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: TokenTrackingSettings = serde_json::from_str(&json).unwrap();
        assert!(!decoded.enabled);
        assert_eq!(decoded.response_reserve, 8_192);
        assert!((decoded.high_threshold - 0.65).abs() < f64::EPSILON);
        assert!((decoded.critical_threshold - 0.85).abs() < f64::EPSILON);
        assert!(decoded.auto_summarize);
        assert_eq!(
            decoded.summarization_model_id.as_deref(),
            Some("some-model-id")
        );
    }

    #[test]
    fn should_show_bar_respects_enabled_flag() {
        let mut s = TokenTrackingSettings::default();
        assert!(s.should_show_bar());
        s.enabled = false;
        assert!(!s.should_show_bar());
    }
}
