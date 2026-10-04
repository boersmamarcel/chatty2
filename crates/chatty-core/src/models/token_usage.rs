use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::settings::models::providers_store::ProviderType;

/// The model a usage line was spent on (AGE-682): the provider plus the
/// model id as it was sent to that provider. A usage line is a fact — tokens,
/// model, time — and its price is looked up from this when it is read
/// ([`price`]), never carried with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: ProviderType,
    pub model_id: String,
}

impl ModelRef {
    /// A usage line's model as the wire names it; an error for a provider
    /// this build does not know.
    pub fn from_wire(model: &chatty_fabric::wire::WireModelRef) -> serde_json::Result<Self> {
        Ok(Self {
            provider: serde_json::from_value(serde_json::Value::String(model.provider.clone()))?,
            model_id: model.model_id.clone(),
        })
    }
}

impl From<&ModelRef> for chatty_fabric::wire::WireModelRef {
    fn from(model: &ModelRef) -> Self {
        Self {
            // A unit variant serialises as its snake_case name.
            provider: serde_json::to_value(&model.provider)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            model_id: model.model_id.clone(),
        }
    }
}

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApiCallUsage {
    /// One-based index of the request within the exchange; 0 for the
    /// aggregate and for a call outside the model loop (a compaction
    /// summary, AGE-683).
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
    /// The share of `output_tokens` the provider says went to reasoning
    /// ("thinking"). `0` when it reports none, which rig cannot tell apart
    /// from a provider that does not report the field at all.
    #[serde(default)]
    pub reasoning_tokens: u32,
    /// The model that served the request (AGE-682). `None` on records
    /// written before it was tracked, which are therefore unpriced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// When the request finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<SystemTime>,
    /// How long the request took, in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,
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

impl TokenPricing {
    /// What these token counts cost at these prices.
    fn cost(&self, input: u32, output: u32, cache_read: u32, cache_write: u32) -> f64 {
        const M: f64 = 1_000_000.0;
        let input_cost = (input as f64 / M) * self.input_per_million;
        let output_cost = (output as f64 / M) * self.output_per_million;
        let cache_read_cost = (cache_read as f64 / M)
            * self
                .cache_read_per_million
                .unwrap_or(self.input_per_million);
        let cache_write_cost = (cache_write as f64 / M)
            * self
                .cache_write_per_million
                .unwrap_or(self.input_per_million);
        input_cost + output_cost + cache_read_cost + cache_write_cost
    }
}

/// Where a usage line's price comes from (AGE-682): a lookup from
/// `(model, at)` to [`TokenPricing`]. Locally it is the model roster's
/// current prices (`ModelConfig::token_pricing`), so `at` does not change the
/// answer; it is part of the lookup so a dated price list can answer it.
#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    entries: Vec<(ModelRef, TokenPricing)>,
}

impl PriceBook {
    /// Add `model`'s prices, unless the book already prices it.
    pub fn insert(&mut self, model: ModelRef, pricing: TokenPricing) {
        if !self.entries.iter().any(|(known, _)| *known == model) {
            self.entries.push((model, pricing));
        }
    }

    /// Price `model` at `pricing`, replacing whatever the book held for it.
    pub fn set(&mut self, model: ModelRef, pricing: TokenPricing) {
        self.entries.retain(|(known, _)| *known != model);
        self.entries.push((model, pricing));
    }

    /// `model`'s prices at `at`, or `None` when the book does not price it.
    pub fn pricing(&self, model: &ModelRef, _at: Option<SystemTime>) -> Option<TokenPricing> {
        self.entries
            .iter()
            .find(|(known, _)| known == model)
            .map(|(_, pricing)| *pricing)
    }
}

/// The price of a set of usage lines: what the priced ones cost, and how many
/// could not be priced (their model is unknown, or has no prices). An
/// unpriced line adds nothing to `usd` and is counted here instead, so it
/// reads as unpriced rather than as $0.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cost {
    pub usd: f64,
    pub unpriced_lines: usize,
}

/// Price usage lines against `book` (AGE-682). This is the one place a
/// line's cost is computed: `estimated_cost_usd` on a line and the
/// conversation's total are caches of what this returns.
///
/// A line with per-request records is the sum of its requests, each at its
/// own model (a request without one is the line's model); a line without
/// them is priced whole at its model. A line any part of which has no model,
/// or a model the book does not price, is unpriced.
pub fn price(lines: &[TokenUsage], book: &PriceBook) -> Cost {
    let mut cost = Cost::default();
    for line in lines {
        match line_cost(line, book) {
            Some(usd) => cost.usd += usd,
            None => cost.unpriced_lines += 1,
        }
    }
    cost
}

/// One line's cost; see [`price`].
fn line_cost(line: &TokenUsage, book: &PriceBook) -> Option<f64> {
    let pricing_for = |model: Option<&ModelRef>, at| model.and_then(|m| book.pricing(m, at));
    if line.calls.is_empty() {
        let pricing = pricing_for(line.model.as_ref(), line.at)?;
        return Some(pricing.cost(
            line.input_tokens,
            line.output_tokens,
            line.cache_read_tokens,
            line.cache_write_tokens,
        ));
    }
    line.calls.iter().try_fold(0.0, |total, call| {
        let pricing = pricing_for(call.model.as_ref().or(line.model.as_ref()), call.at)?;
        Some(
            total
                + pricing.cost(
                    call.input_tokens,
                    call.output_tokens,
                    call.cache_read_tokens,
                    call.cache_write_tokens,
                ),
        )
    })
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

    /// The agent this usage was spent by on the conversation's behalf, when
    /// it is a delegated line rather than the turn's own requests (AGE-415):
    /// what a worker reported on its terminal status, folded into the
    /// leader's conversation so the bill follows the bearer. A path from
    /// this agent down the tree (TB-3): `"reviewer"` is the reviewer's own
    /// spend, `"reviewer/local-coder"` is what the reviewer's coder spent,
    /// so the root has one line per agent in the tree. `None` for the
    /// turn's own usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_to: Option<String>,

    /// The plugin whose `llm::complete` calls this line is, when it is a
    /// plugin's line rather than the turn's own requests (PL-U2, AGE-616):
    /// a spec's WASM plugin runs inside the agent's turn, on the agent's
    /// provider client, and its spend is folded into the turn under its own
    /// name. `None` for every other line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,

    /// The model the line was spent on (AGE-682); per-request records carry
    /// their own, which win. `None` on records written before it was
    /// tracked, which are therefore unpriced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,

    /// When the line's last request finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<SystemTime>,

    /// Time spent in the line's requests, in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,
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
            delegated_to: None,
            plugin: None,
            model: None,
            at: None,
            duration_ms: 0,
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

    /// This line as the caller of `agent` books it (TB-3): `agent`'s own
    /// spend is named `agent`, and a line `agent` forwarded from below it
    /// keeps its path under `agent/`.
    pub fn delegated_by(mut self, agent: &str) -> Self {
        self.delegated_to = Some(match self.delegated_to.take() {
            Some(below) => format!("{agent}/{below}"),
            None => agent.to_string(),
        });
        self
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
    /// Totals are sums over `calls`; `api_turn_count` is `calls.len()`. The
    /// line's model is the calls' model when they all share one, `at` is the
    /// last call's and `duration_ms` the sum. An empty list yields the
    /// default (zero) record with one turn.
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
            usage.duration_ms = usage.duration_ms.saturating_add(call.duration_ms);
        }
        let first_model = calls.first().and_then(|c| c.model.as_ref());
        if calls.iter().all(|c| c.model.as_ref() == first_model) {
            usage.model = first_model.cloned();
        }
        usage.at = calls.last().and_then(|c| c.at);
        usage.calls = calls;
        usage
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

    /// Cache this line's cost from `book` in `estimated_cost_usd`: `None`
    /// when the line is unpriced (see [`price`]).
    pub fn price(&mut self, book: &PriceBook) {
        self.estimated_cost_usd = line_cost(self, book);
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

/// One agent's share of a conversation's bill (TB-3): its lines summed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentSpend {
    /// The agent's path down the tree from the conversation's own agent
    /// (`"reviewer"`, `"reviewer/local-coder"`); `None` is the
    /// conversation's agent itself, its plugins included.
    pub agent: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// What the agent's priced lines cost, from each line's cached price.
    pub cost_usd: f64,
    /// The agent's lines that could not be priced (see [`price`]).
    pub unpriced_lines: usize,
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

    /// The bill per agent in the tree (TB-3): the conversation's own agent
    /// first, then each delegated path in the order it first spent. The
    /// rows sum to the totals.
    pub fn by_agent(&self) -> Vec<AgentSpend> {
        let mut rows = vec![AgentSpend::default()];
        for line in &self.message_usages {
            let index = match rows.iter().position(|r| r.agent == line.delegated_to) {
                Some(index) => index,
                None => {
                    rows.push(AgentSpend {
                        agent: line.delegated_to.clone(),
                        ..AgentSpend::default()
                    });
                    rows.len() - 1
                }
            };
            let row = &mut rows[index];
            row.input_tokens += u64::from(line.input_tokens);
            row.output_tokens += u64::from(line.output_tokens);
            row.cache_read_tokens += u64::from(line.cache_read_tokens);
            row.cache_write_tokens += u64::from(line.cache_write_tokens);
            match line.estimated_cost_usd {
                Some(cost) => row.cost_usd += cost,
                None => row.unpriced_lines += 1,
            }
        }
        if rows[0] == AgentSpend::default() {
            rows.remove(0);
        }
        rows
    }

    /// The most recent exchange's usage.
    pub fn last_usage(&self) -> Option<&TokenUsage> {
        self.message_usages.last()
    }

    /// Forget the stored cost of every line that names no model (AGE-682):
    /// such a line is unpriced whatever it was stored with, so a record
    /// written before lines carried their model loads unpriced, and the
    /// totals are recomputed without it.
    pub fn forget_unattributed_costs(&mut self) {
        let mut forgot = false;
        for line in &mut self.message_usages {
            let attributed = if line.calls.is_empty() {
                line.model.is_some()
            } else {
                line.model.is_some() || line.calls.iter().all(|c| c.model.is_some())
            };
            if !attributed && line.estimated_cost_usd.take().is_some() {
                forgot = true;
            }
        }
        if forgot {
            self.recalculate_totals();
        }
    }

    /// Recalculate totals from per-message usages
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
            reasoning_tokens: 0,
            ..Default::default()
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

    fn model(id: &str) -> ModelRef {
        ModelRef {
            provider: ProviderType::OpenRouter,
            model_id: id.to_string(),
        }
    }

    fn book(entries: &[(&str, f64, f64)]) -> PriceBook {
        let mut book = PriceBook::default();
        for (id, input, output) in entries {
            book.insert(
                model(id),
                TokenPricing {
                    input_per_million: *input,
                    output_per_million: *output,
                    ..Default::default()
                },
            );
        }
        book
    }

    #[test]
    fn cost_uses_cache_rates_when_configured_and_input_rate_otherwise() {
        let mut usage = TokenUsage::from_calls(vec![call(1, 1_000_000, 1_000_000, 1_000_000, 0)]);
        usage.model = Some(model("m"));
        let mut cached = PriceBook::default();
        cached.insert(
            model("m"),
            TokenPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
                cache_read_per_million: Some(0.3),
                cache_write_per_million: Some(3.75),
            },
        );
        usage.price(&cached);
        assert!((usage.estimated_cost_usd.unwrap() - (3.0 + 0.3 + 3.75)).abs() < 1e-9);

        usage.price(&book(&[("m", 3.0, 15.0)]));
        assert!((usage.estimated_cost_usd.unwrap() - 9.0).abs() < 1e-9);
    }

    /// A line is a fact; its model is how it is priced. A call on a model
    /// the book does not know — or a line that names none — is unpriced,
    /// not free.
    #[test]
    fn unknown_model_is_unpriced_not_zero() {
        let book = book(&[("known", 1.0, 1.0)]);
        let known = TokenUsage {
            model: Some(model("known")),
            ..TokenUsage::new(1_000_000, 0)
        };
        let unknown = TokenUsage {
            model: Some(model("unknown")),
            ..TokenUsage::new(1_000_000, 0)
        };
        let nameless = TokenUsage::new(1_000_000, 0);

        let mut line = unknown.clone();
        line.price(&book);
        assert_eq!(line.estimated_cost_usd, None, "unpriced, not $0");

        let cost = price(&[known, unknown, nameless], &book);
        assert!((cost.usd - 1.0).abs() < 1e-9);
        assert_eq!(cost.unpriced_lines, 2);

        // A turn whose calls span models is unpriced if any of them is.
        let mut mixed = TokenUsage::from_calls(vec![
            ApiCallUsage {
                model: Some(model("known")),
                ..call(1, 10, 0, 0, 1)
            },
            ApiCallUsage {
                model: Some(model("unknown")),
                ..call(2, 10, 0, 0, 1)
            },
        ]);
        mixed.price(&book);
        assert_eq!(mixed.estimated_cost_usd, None);
    }

    /// Each call is priced at its own model; a call that names none is the
    /// line's.
    #[test]
    fn calls_are_priced_at_their_own_model() {
        let book = book(&[("agent", 1.0, 0.0), ("utility", 10.0, 0.0)]);
        let mut usage = TokenUsage::from_calls(vec![
            ApiCallUsage {
                model: Some(model("utility")),
                duration_ms: 5,
                ..call(0, 1_000_000, 0, 0, 0)
            },
            ApiCallUsage {
                duration_ms: 7,
                ..call(1, 1_000_000, 0, 0, 0)
            },
        ]);
        assert_eq!(usage.model, None, "the calls do not share a model");
        assert_eq!(usage.duration_ms, 12);
        usage.model = Some(model("agent"));
        usage.price(&book);
        assert!((usage.estimated_cost_usd.unwrap() - 11.0).abs() < 1e-9);
    }

    /// AGE-682: no backward compatibility — a record written before lines
    /// named their model loads, and whatever cost it was stored with, it is
    /// unpriced.
    #[test]
    fn old_lines_load_unpriced() {
        let json = r#"{"message_usages":[
            {"input_tokens":1234,"output_tokens":56,"estimated_cost_usd":0.01},
            {"input_tokens":10,"output_tokens":5,"estimated_cost_usd":0.02,"api_turn_count":1,
             "calls":[{"turn":1,"input_tokens":10,"output_tokens":5}]}],
            "total_input_tokens":1244,"total_output_tokens":61,"total_estimated_cost_usd":0.03}"#;
        let usage = crate::models::Conversation::deserialize_token_usage(json).unwrap();
        assert_eq!(usage.message_usages.len(), 2);
        assert!(usage.message_usages.iter().all(|line| line.model.is_none()));
        assert!(
            usage
                .message_usages
                .iter()
                .all(|line| line.estimated_cost_usd.is_none())
        );
        assert_eq!(usage.total_estimated_cost_usd, 0.0);
        assert_eq!(usage.total_input_tokens, 1244, "the tokens are kept");
        let cost = price(&usage.message_usages, &book(&[("m", 1.0, 1.0)]));
        assert_eq!(cost.unpriced_lines, 2);
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

    /// TB-3: a line forwarded from below an agent keeps its path under that
    /// agent's name, one level per hop.
    #[test]
    fn delegated_by_prefixes_the_path() {
        let own = TokenUsage::new(1, 1).delegated_by("local-coder");
        assert_eq!(own.delegated_to.as_deref(), Some("local-coder"));
        let two_up = own.delegated_by("reviewer").delegated_by("planner");
        assert_eq!(
            two_up.delegated_to.as_deref(),
            Some("planner/reviewer/local-coder")
        );
    }

    /// TB-3: the bill per agent is the conversation's own agent first, then
    /// each path in the order it first spent, and the rows sum to the
    /// totals; an unpriced line is counted, not read as $0.
    #[test]
    fn by_agent_rows_sum_to_the_totals() {
        let line = |agent: Option<&str>, input: u32, cost: Option<f64>| TokenUsage {
            delegated_to: agent.map(str::to_string),
            estimated_cost_usd: cost,
            ..TokenUsage::new(input, input / 10)
        };
        let mut conv = ConversationTokenUsage::new();
        conv.add_usage(line(Some("reviewer"), 300, Some(0.3)));
        conv.add_usage(line(Some("reviewer/coder"), 200, None));
        conv.add_usage(line(None, 100, Some(0.1)));
        conv.add_usage(line(Some("reviewer"), 50, Some(0.05)));
        conv.add_usage(line(None, 10, Some(0.01)));

        let rows = conv.by_agent();
        let named: Vec<(Option<&str>, u64, usize)> = rows
            .iter()
            .map(|r| (r.agent.as_deref(), r.input_tokens, r.unpriced_lines))
            .collect();
        assert_eq!(
            named,
            [
                (None, 110, 0),
                (Some("reviewer"), 350, 0),
                (Some("reviewer/coder"), 200, 1),
            ]
        );
        let input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        let output: u64 = rows.iter().map(|r| r.output_tokens).sum();
        let cost: f64 = rows.iter().map(|r| r.cost_usd).sum();
        assert_eq!(input, u64::from(conv.total_input_tokens));
        assert_eq!(output, u64::from(conv.total_output_tokens));
        assert!((cost - conv.total_estimated_cost_usd).abs() < 1e-12);

        // A conversation that only delegated has no row for itself.
        let mut only = ConversationTokenUsage::new();
        only.add_usage(line(Some("coder"), 5, Some(0.0)));
        assert_eq!(only.by_agent().len(), 1);
    }

    /// AGE-415: a delegated line names its agent and round-trips; the turn's
    /// own usage — every record written before the field existed — carries
    /// no such key, so persisted bytes are unchanged.
    #[test]
    fn a_delegated_line_names_its_agent_and_an_own_line_does_not() {
        let own = TokenUsage::from_calls(vec![call(1, 10, 0, 0, 5)]);
        let json = serde_json::to_string(&own).unwrap();
        assert!(!json.contains("delegated_to"), "{json}");

        let delegated = TokenUsage {
            delegated_to: Some("local-coder".to_string()),
            ..TokenUsage::new(100, 20)
        };
        let json = serde_json::to_string(&delegated).unwrap();
        let back: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.delegated_to.as_deref(), Some("local-coder"));
        assert_eq!(back.input_tokens, 100);
        assert_eq!(back.output_tokens, 20);
    }

    #[test]
    fn hit_rate_formats_as_whole_percent() {
        assert_eq!(format_hit_rate(0.823), "82%");
        assert_eq!(format_hit_rate(1.0), "100%");
    }
}
