use serde::{Deserialize, Serialize};

/// Token usage for a single message exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens consumed (note: rig-core accumulates across multi-turn exchanges)
    pub input_tokens: u32,

    /// Output tokens generated
    pub output_tokens: u32,

    /// Estimated cost in USD (computed at save time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,

    /// Number of LLM API turns in this exchange (1 = no tool calls).
    /// rig-core accumulates input_tokens across all turns, so dividing by this
    /// gives a rough per-turn average closer to actual context fill.
    #[serde(default = "default_turn_count")]
    pub api_turn_count: u32,
}

fn default_turn_count() -> u32 {
    1
}

impl Default for TokenUsage {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost_usd: None,
            api_turn_count: 1,
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

    #[allow(dead_code)]
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    /// Calculate cost based on model pricing (cost per million tokens)
    pub fn calculate_cost(&mut self, cost_per_million_input: f64, cost_per_million_output: f64) {
        let input_cost = (self.input_tokens as f64 / 1_000_000.0) * cost_per_million_input;
        let output_cost = (self.output_tokens as f64 / 1_000_000.0) * cost_per_million_output;
        self.estimated_cost_usd = Some(input_cost + output_cost);
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

impl ConversationTokenUsage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_usage(&mut self, usage: TokenUsage) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        if let Some(cost) = usage.estimated_cost_usd {
            self.total_estimated_cost_usd += cost;
        }
        self.message_usages.push(usage);
    }

    /// Recalculate totals from per-message usages
    #[allow(dead_code)]
    pub fn recalculate_totals(&mut self) {
        self.total_input_tokens = self.message_usages.iter().map(|u| u.input_tokens).sum();
        self.total_output_tokens = self.message_usages.iter().map(|u| u.output_tokens).sum();
        self.total_estimated_cost_usd = self
            .message_usages
            .iter()
            .filter_map(|u| u.estimated_cost_usd)
            .sum();
    }
}
