use serde::{Deserialize, Serialize};

/// Token usage reported by the provider for **one** completion request.
///
/// A single user turn is usually several requests: the first one answers or
/// calls a tool, and every tool result triggers another. Prompt caching is a
/// per-request property (the second request should hit the cache the first
/// one wrote), so this is the unit cache hit rate is computed from. The
/// per-exchange [`TokenUsage`] is derived by summing these.
///
/// Provider-normalised: `input_tokens` is the *uncached* share of the prompt,
/// so `input + cache_read + cache_write` is the whole prompt regardless of
/// whether the provider reports cache reads as a subset of its input count
/// (OpenAI-compatible) or separately from it (Anthropic). The normalisation
/// happens once, in `llm_service`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApiCallUsage {
    /// One-based index of the request within the exchange.
    pub turn: u32,
    /// Prompt tokens billed at the full input rate (not served from cache).
    pub input_tokens: u32,
    /// Prompt tokens served from the provider's cache.
    #[serde(default)]
    pub cache_read_tokens: u32,
    /// Prompt tokens written to the provider's cache on this request.
    #[serde(default)]
    pub cache_write_tokens: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
}

impl ApiCallUsage {
    /// The whole prompt for this request: uncached + cached + cache-written.
    pub fn prompt_tokens(&self) -> u32 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    /// Share of the prompt served from cache, or `None` for an empty prompt.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let prompt = self.prompt_tokens();
        (prompt > 0).then(|| self.cache_read_tokens as f64 / prompt as f64)
    }
}

/// Per-million-token prices used to cost an exchange.
///
/// Cache rates are optional because most model configs only carry input and
/// output prices. When absent, cached tokens are priced at the input rate,
/// which over-estimates (Anthropic bills cache reads at 10% of input) but
/// never hides spend.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TokenPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
}

/// Token usage for a single message exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Uncached input tokens across every request in the exchange.
    ///
    /// Historic records (before per-call usage was tracked) hold the
    /// provider's aggregated input count here, which for OpenAI-compatible
    /// providers included cached tokens.
    pub input_tokens: u32,

    /// Output tokens generated
    pub output_tokens: u32,

    /// Input tokens served from the provider's prompt cache (sum over calls).
    #[serde(default)]
    pub cache_read_tokens: u32,

    /// Input tokens written to the provider's prompt cache (sum over calls).
    #[serde(default)]
    pub cache_write_tokens: u32,

    /// Estimated cost in USD (computed at save time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,

    /// Number of LLM API requests in this exchange (1 = no tool calls).
    #[serde(default = "default_turn_count")]
    pub api_turn_count: u32,

    /// Per-request usage, in request order. Empty for records written before
    /// per-call usage was tracked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<ApiCallUsage>,
}

fn default_turn_count() -> u32 {
    1
}

impl Default for TokenUsage {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated_cost_usd: None,
            api_turn_count: 1,
            calls: Vec::new(),
        }
    }
}

impl TokenUsage {
    #[allow(dead_code)]
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            ..Default::default()
        }
    }

    /// Create a new TokenUsage with an explicit turn count.
    pub fn with_turn_count(input_tokens: u32, output_tokens: u32, api_turn_count: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            api_turn_count: api_turn_count.max(1),
            ..Default::default()
        }
    }

    /// Build the exchange record from its per-request usages.
    ///
    /// Totals are sums over `calls`; `api_turn_count` is `calls.len()`. An
    /// empty list yields the default (zero) record with one turn.
    pub fn from_calls(calls: Vec<ApiCallUsage>) -> Self {
        let mut usage = Self {
            api_turn_count: (calls.len() as u32).max(1),
            ..Default::default()
        };
        for call in &calls {
            usage.input_tokens = usage.input_tokens.saturating_add(call.input_tokens);
            usage.output_tokens = usage.output_tokens.saturating_add(call.output_tokens);
            usage.cache_read_tokens = usage
                .cache_read_tokens
                .saturating_add(call.cache_read_tokens);
            usage.cache_write_tokens = usage
                .cache_write_tokens
                .saturating_add(call.cache_write_tokens);
        }
        usage.calls = calls;
        usage
    }

    #[allow(dead_code)]
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    /// The whole prompt across the exchange: uncached + cached + cache-written.
    pub fn prompt_tokens(&self) -> u32 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    /// Share of the exchange's prompt tokens served from cache, or `None` when
    /// there was no prompt.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let prompt = self.prompt_tokens();
        (prompt > 0).then(|| self.cache_read_tokens as f64 / prompt as f64)
    }

    /// The last request's usage, which is the one whose prompt size reflects
    /// the current context fill.
    pub fn last_call(&self) -> Option<&ApiCallUsage> {
        self.calls.last()
    }

    /// Calculate cost from per-million-token prices.
    ///
    /// Cached tokens use the cache rates when configured and the input rate
    /// otherwise (see [`TokenPricing`]).
    pub fn calculate_cost(&mut self, pricing: &TokenPricing) {
        const M: f64 = 1_000_000.0;
        let input_cost = (self.input_tokens as f64 / M) * pricing.input_per_million;
        let output_cost = (self.output_tokens as f64 / M) * pricing.output_per_million;
        let cache_read_cost = (self.cache_read_tokens as f64 / M)
            * pricing
                .cache_read_per_million
                .unwrap_or(pricing.input_per_million);
        let cache_write_cost = (self.cache_write_tokens as f64 / M)
            * pricing
                .cache_write_per_million
                .unwrap_or(pricing.input_per_million);
        self.estimated_cost_usd =
            Some(input_cost + output_cost + cache_read_cost + cache_write_cost);
    }
}

/// Aggregated token usage for entire conversation
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConversationTokenUsage {
    /// Per-message token usage (parallel to message history)
    pub message_usages: Vec<TokenUsage>,

    /// Cached total for quick access
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    #[serde(default)]
    pub total_cache_read_tokens: u32,
    #[serde(default)]
    pub total_cache_write_tokens: u32,
    pub total_estimated_cost_usd: f64,
}

/// Format a token count for human-readable display.
///
/// - `< 1_000` → raw number (`"500"`)
/// - `1_000 – 999_999` → K suffix (`"16.3K"`, `"1K"`)
/// - `>= 1_000_000` → M suffix (`"1.2M"`)
pub fn format_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        let m = count as f64 / 1_000_000.0;
        let s = format!("{:.1}M", m);
        s.replace(".0M", "M") // drop trailing .0
    } else if count >= 1_000 {
        let k = count as f64 / 1_000.0;
        let s = format!("{:.1}K", k);
        s.replace(".0K", "K") // drop trailing .0
    } else {
        count.to_string()
    }
}

/// Format a USD cost for display.
///
/// - `>= $0.01` → 2 decimal places (`"$0.12"`)
/// - `>= $0.001` → 3 decimal places (`"$0.003"`)
/// - `> 0` → 4 decimal places (`"$0.0001"`) or `"< $0.0001"` floor
/// - `0` → `"$0.00"`
pub fn format_cost(cost: f64) -> String {
    if cost == 0.0 {
        "$0.00".to_string()
    } else if cost >= 0.01 {
        format!("${:.2}", cost)
    } else if cost >= 0.001 {
        format!("${:.3}", cost)
    } else if cost >= 0.0001 {
        format!("${:.4}", cost)
    } else {
        "< $0.0001".to_string()
    }
}

/// Format a cache hit rate as a whole percentage (`"82%"`).
pub fn format_hit_rate(rate: f64) -> String {
    format!("{:.0}%", (rate * 100.0).clamp(0.0, 100.0))
}

impl ConversationTokenUsage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_usage(&mut self, usage: TokenUsage) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        self.total_cache_read_tokens += usage.cache_read_tokens;
        self.total_cache_write_tokens += usage.cache_write_tokens;
        if let Some(cost) = usage.estimated_cost_usd {
            self.total_estimated_cost_usd += cost;
        }
        self.message_usages.push(usage);
    }

    /// The most recent exchange's usage.
    pub fn last_usage(&self) -> Option<&TokenUsage> {
        self.message_usages.last()
    }

    /// Recalculate totals from per-message usages
    #[allow(dead_code)]
    pub fn recalculate_totals(&mut self) {
        self.total_input_tokens = self.message_usages.iter().map(|u| u.input_tokens).sum();
        self.total_output_tokens = self.message_usages.iter().map(|u| u.output_tokens).sum();
        self.total_cache_read_tokens = self
            .message_usages
            .iter()
            .map(|u| u.cache_read_tokens)
            .sum();
        self.total_cache_write_tokens = self
            .message_usages
            .iter()
            .map(|u| u.cache_write_tokens)
            .sum();
        self.total_estimated_cost_usd = self
            .message_usages
            .iter()
            .filter_map(|u| u.estimated_cost_usd)
            .sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(turn: u32, input: u32, read: u32, write: u32, output: u32) -> ApiCallUsage {
        ApiCallUsage {
            turn,
            input_tokens: input,
            cache_read_tokens: read,
            cache_write_tokens: write,
            output_tokens: output,
        }
    }

    #[test]
    fn from_calls_sums_every_bucket_and_counts_turns() {
        let usage = TokenUsage::from_calls(vec![
            call(1, 1_000, 0, 9_000, 50),
            call(2, 200, 9_000, 0, 30),
            call(3, 300, 9_000, 200, 20),
        ]);
        assert_eq!(usage.input_tokens, 1_500);
        assert_eq!(usage.cache_read_tokens, 18_000);
        assert_eq!(usage.cache_write_tokens, 9_200);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.api_turn_count, 3);
        assert_eq!(usage.calls.len(), 3);
        assert_eq!(usage.last_call().map(|c| c.turn), Some(3));
    }

    #[test]
    fn from_calls_with_nothing_is_one_empty_turn() {
        let usage = TokenUsage::from_calls(Vec::new());
        assert_eq!(usage.api_turn_count, 1);
        assert_eq!(usage.prompt_tokens(), 0);
        assert_eq!(usage.cache_hit_rate(), None);
    }

    #[test]
    fn cache_hit_rate_is_share_of_whole_prompt() {
        let c = call(2, 200, 9_000, 800, 30);
        assert_eq!(c.prompt_tokens(), 10_000);
        assert_eq!(c.cache_hit_rate(), Some(0.9));
        assert_eq!(call(1, 0, 0, 0, 0).cache_hit_rate(), None);
    }

    #[test]
    fn cost_uses_cache_rates_when_configured_and_input_rate_otherwise() {
        let mut usage = TokenUsage::from_calls(vec![call(1, 1_000_000, 1_000_000, 1_000_000, 0)]);
        usage.calculate_cost(&TokenPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: Some(0.3),
            cache_write_per_million: Some(3.75),
        });
        assert!((usage.estimated_cost_usd.unwrap() - (3.0 + 0.3 + 3.75)).abs() < 1e-9);

        usage.calculate_cost(&TokenPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: None,
            cache_write_per_million: None,
        });
        assert!((usage.estimated_cost_usd.unwrap() - 9.0).abs() < 1e-9);
    }

    #[test]
    fn records_without_cache_fields_still_load() {
        // A conversation persisted before per-call usage existed.
        let json = r#"{"message_usages":[{"input_tokens":1234,"output_tokens":56,"estimated_cost_usd":0.01}],
            "total_input_tokens":1234,"total_output_tokens":56,"total_estimated_cost_usd":0.01}"#;
        let usage: ConversationTokenUsage = serde_json::from_str(json).unwrap();
        let first = &usage.message_usages[0];
        assert_eq!(first.input_tokens, 1234);
        assert_eq!(first.cache_read_tokens, 0);
        assert_eq!(first.api_turn_count, 1);
        assert!(first.calls.is_empty());
        assert_eq!(usage.total_cache_read_tokens, 0);
    }

    #[test]
    fn conversation_totals_track_cache_buckets() {
        let mut conv = ConversationTokenUsage::new();
        conv.add_usage(TokenUsage::from_calls(vec![call(1, 100, 0, 900, 10)]));
        conv.add_usage(TokenUsage::from_calls(vec![call(1, 50, 900, 0, 10)]));
        assert_eq!(conv.total_cache_write_tokens, 900);
        assert_eq!(conv.total_cache_read_tokens, 900);
        assert_eq!(
            conv.last_usage().unwrap().cache_hit_rate(),
            Some(900.0 / 950.0)
        );

        conv.recalculate_totals();
        assert_eq!(conv.total_cache_read_tokens, 900);
    }

    #[test]
    fn hit_rate_formats_as_whole_percent() {
        assert_eq!(format_hit_rate(0.823), "82%");
        assert_eq!(format_hit_rate(1.0), "100%");
    }
}
