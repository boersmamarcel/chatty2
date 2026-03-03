use once_cell::sync::Lazy;
use rig::completion::Message;
use tiktoken_rs::{CoreBPE, cl100k_base, o200k_base};

// ── Static BPE instances ──────────────────────────────────────────────────────
// CoreBPE construction is expensive (~50 ms first call). Build once globally
// and reuse across all counting calls. Both instances are Sync so sharing them
// across threads is safe.

static CL100K: Lazy<CoreBPE> = Lazy::new(|| cl100k_base().expect("cl100k_base init failed"));
static O200K: Lazy<CoreBPE> = Lazy::new(|| o200k_base().expect("o200k_base init failed"));

// ── Encoding selection ────────────────────────────────────────────────────────

/// Which BPE encoding best approximates the active model's tokenizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// `cl100k_base` — GPT-4, Claude (approximation ±5%), Mistral (approximation),
    /// Gemini (approximation ±15%), and most Ollama models.
    Cl100k,
    /// `o200k_base` — GPT-4o, GPT-4o-mini, o1, o3, o4 family.
    O200k,
}

impl Encoding {
    fn bpe(&self) -> &'static CoreBPE {
        match self {
            Encoding::Cl100k => &CL100K,
            Encoding::O200k => &O200K,
        }
    }
}

// ── TokenCounter ─────────────────────────────────────────────────────────────

/// A lightweight, thread-safe token counter wrapping a tiktoken-rs BPE encoder.
///
/// Cheap to create (just an enum discriminant — the underlying BPE is a Lazy static).
/// Safe to use from `tokio::spawn_blocking` and any background thread.
///
/// # Accuracy
/// - **OpenAI models**: exact for `cl100k_base` / `o200k_base` families.
/// - **Anthropic Claude**: ±5% (uses `cl100k_base` as approximation).
/// - **Google Gemini**: ±10–15% (SentencePiece differs significantly; labelled as estimate).
/// - **Mistral / Ollama**: ±5–10% depending on model.
///
/// All displayed estimates in the UI should be labelled with a `~` prefix to
/// communicate that they are approximations.
pub struct TokenCounter {
    encoding: Encoding,
}

impl TokenCounter {
    /// Select the most appropriate tokenizer for a given model identifier.
    ///
    /// Uses the raw API `model_identifier` string (e.g. `"gpt-4o-2024-11-20"`,
    /// `"claude-sonnet-4-20250514"`), **not** chatty's internal UUID-based `id`.
    ///
    /// # Matching rules (checked in order)
    /// 1. Starts with `"gpt-4o"`, `"o1"`, `"o3"`, `"o4"` → `o200k_base`
    /// 2. Everything else → `cl100k_base`
    pub fn for_model(model_identifier: &str) -> Self {
        let id = model_identifier.to_ascii_lowercase();
        let encoding = if id.starts_with("gpt-4o")
            || id.starts_with("o1-")
            || id.starts_with("o1")
            || id.starts_with("o3")
            || id.starts_with("o4")
        {
            Encoding::O200k
        } else {
            Encoding::Cl100k
        };
        Self { encoding }
    }

    /// Which encoding is being used (useful for logging / debugging).
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Count the tokens in an arbitrary UTF-8 string.
    ///
    /// Uses `encode_with_special_tokens` so that special tokens (e.g. `<|endoftext|>`)
    /// are counted rather than treated as errors.
    ///
    /// Returns 0 for empty strings.
    pub fn count(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        self.encoding.bpe().encode_with_special_tokens(text).len()
    }

    /// Count the tokens in a rig `Message` by first serialising it to JSON.
    ///
    /// JSON is a good proxy for what providers actually tokenize: it captures the
    /// role field, content-type wrapper, and message text in one pass. This
    /// consistently over-estimates slightly compared to raw text counting, which is
    /// the safe direction for a progress bar (avoids false "you have headroom" reads).
    ///
    /// Returns 0 if serialisation fails.
    pub fn count_message(&self, message: &Message) -> usize {
        match serde_json::to_string(message) {
            Ok(json) => self.count(&json),
            Err(_) => 0,
        }
    }

    /// Count the tokens across an entire conversation history.
    ///
    /// Iterates messages sequentially; safe to call from a `spawn_blocking` task.
    /// For very long histories (100 K+ tokens) this may take ~10 ms — keep it on a
    /// background thread.
    pub fn count_history(&self, history: &[Message]) -> usize {
        history.iter().map(|m| self.count_message(m)).sum()
    }

    /// Count a raw preamble / system-prompt string.
    ///
    /// Identical to `count()` but named separately so call sites are self-documenting.
    #[inline]
    pub fn count_preamble(&self, preamble: &str) -> usize {
        self.count(preamble)
    }

    /// Estimate the token cost of a set of tool JSON schemas.
    ///
    /// Rather than requiring callers to produce exact schema JSON, this function
    /// accepts a `tool_count` and multiplies it by the token cost of a representative
    /// medium-complexity tool schema. The sample schema was chosen to be typical of
    /// chatty's own tools (shell/filesystem/git) and is counted with the active BPE
    /// so the estimate scales correctly with encoding differences.
    ///
    /// The formula is:
    /// ```text
    /// tokens ≈ tool_count × tokens_per_sample_schema
    /// ```
    /// which gives a much tighter bound than the former `tool_count × 300` constant.
    pub fn estimate_tool_tokens(&self, tool_count: usize) -> usize {
        if tool_count == 0 {
            return 0;
        }
        // A representative medium-complexity tool schema sent to an LLM provider.
        // This JSON mirrors the shape of chatty's shell/filesystem tools.
        static SAMPLE_SCHEMA: &str = r#"{"type":"function","function":{"name":"run_command","description":"Execute a shell command in the active workspace directory and return its stdout, stderr, and exit code. The command runs in a sandboxed environment.","parameters":{"type":"object","properties":{"command":{"type":"string","description":"The shell command to execute. Use full paths when possible."},"timeout":{"type":"integer","description":"Maximum execution time in seconds. Defaults to 30.","default":30}},"required":["command"]}}}"#;
        let tokens_per_schema = self.count(SAMPLE_SCHEMA).max(1);
        tool_count * tokens_per_schema
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cl100k_selected_for_claude() {
        let c = TokenCounter::for_model("claude-sonnet-4-20250514");
        assert_eq!(c.encoding(), Encoding::Cl100k);
    }

    #[test]
    fn cl100k_selected_for_gpt4() {
        let c = TokenCounter::for_model("gpt-4-turbo");
        assert_eq!(c.encoding(), Encoding::Cl100k);
    }

    #[test]
    fn o200k_selected_for_gpt4o() {
        let c = TokenCounter::for_model("gpt-4o-2024-11-20");
        assert_eq!(c.encoding(), Encoding::O200k);
    }

    #[test]
    fn o200k_selected_for_o1() {
        let c = TokenCounter::for_model("o1-preview");
        assert_eq!(c.encoding(), Encoding::O200k);
    }

    #[test]
    fn o200k_selected_for_o3() {
        let c = TokenCounter::for_model("o3-mini");
        assert_eq!(c.encoding(), Encoding::O200k);
    }

    #[test]
    fn cl100k_selected_for_ollama() {
        let c = TokenCounter::for_model("llama3.2:latest");
        assert_eq!(c.encoding(), Encoding::Cl100k);
    }

    #[test]
    fn cl100k_selected_for_mistral() {
        let c = TokenCounter::for_model("mistral-large-latest");
        assert_eq!(c.encoding(), Encoding::Cl100k);
    }

    #[test]
    fn empty_string_counts_zero() {
        let c = TokenCounter::for_model("gpt-4");
        assert_eq!(c.count(""), 0);
    }

    #[test]
    fn hello_world_has_known_cl100k_count() {
        // "Hello, world!" encodes to ["Hello", ",", " world", "!"] in cl100k → 4 tokens
        let c = TokenCounter::for_model("gpt-4");
        assert_eq!(c.count("Hello, world!"), 4);
    }

    #[test]
    fn count_returns_nonzero_for_nonempty_text() {
        let c = TokenCounter::for_model("claude-3-5-sonnet-20241022");
        assert!(c.count("The quick brown fox jumps over the lazy dog.") > 0);
    }

    #[test]
    fn count_history_empty_slice_is_zero() {
        let c = TokenCounter::for_model("gpt-4");
        assert_eq!(c.count_history(&[]), 0);
    }

    #[test]
    fn count_preamble_matches_count() {
        let c = TokenCounter::for_model("gpt-4");
        let text = "You are a helpful AI assistant.";
        assert_eq!(c.count_preamble(text), c.count(text));
    }

    #[test]
    fn estimate_tool_tokens_zero_for_zero_tools() {
        let c = TokenCounter::for_model("gpt-4");
        assert_eq!(c.estimate_tool_tokens(0), 0);
    }

    #[test]
    fn estimate_tool_tokens_scales_with_count() {
        let c = TokenCounter::for_model("gpt-4");
        let one = c.estimate_tool_tokens(1);
        let ten = c.estimate_tool_tokens(10);
        assert_eq!(ten, one * 10);
        // Sanity: a single tool schema should be 50–200 tokens
        assert!(one >= 50, "tool schema too small: {one}");
        assert!(one <= 300, "tool schema too large: {one}");
    }

    #[test]
    fn model_identifier_matching_is_case_insensitive() {
        // The for_model() downcases the identifier
        let upper = TokenCounter::for_model("GPT-4O-2024-11-20");
        let lower = TokenCounter::for_model("gpt-4o-2024-11-20");
        assert_eq!(upper.encoding(), lower.encoding());
    }
}
