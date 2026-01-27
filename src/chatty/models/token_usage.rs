use serde::{Deserialize, Serialize};

/// Token usage for a single message exchange
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    /// Input tokens consumed
    pub input_tokens: u32,

    /// Output tokens generated
    pub output_tokens: u32,

    /// Estimated cost in USD (computed at save time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

impl TokenUsage {
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            estimated_cost_usd: None,
        }
    }

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
