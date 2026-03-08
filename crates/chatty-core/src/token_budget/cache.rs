use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::counter::TokenCounter;

// ── CachedTokenCounts ─────────────────────────────────────────────────────────

/// Memoized token counts for context components that change infrequently.
///
/// The preamble (system prompt text) and tool definitions rarely change between
/// turns in a conversation. Recomputing their token counts on every send is
/// wasteful. This struct caches the last-computed count behind a content hash
/// and recomputes only when the input text actually changes.
///
/// # Thread safety
/// `CachedTokenCounts` is `Send` but NOT `Sync` — it is designed to be owned by
/// a single place (e.g. `GlobalTokenBudget`) and mutated under `update_global`.
/// All BPE counting still happens on background threads; the cache just stores
/// the resulting `usize` values.
///
/// # Invalidation
/// Invalidation is automatic: pass any string to `preamble_tokens()` or
/// `tool_tokens()` and the cache checks the content hash. If the hash changed,
/// it calls back into `TokenCounter::count*()` and updates both the stored hash
/// and the stored count. No manual cache-busting is needed.
#[derive(Clone, Debug, Default)]
pub struct CachedTokenCounts {
    // ── Preamble cache ────────────────────────────────────────────────────────
    preamble_tokens: usize,
    preamble_hash: u64,

    // ── Tool definitions cache ────────────────────────────────────────────────
    /// Cached tool token count.
    tool_tokens: usize,
    /// Hash of the "tool hint" string used to detect configuration changes.
    /// The hint is a compact description like `"tools:12,mcp:3"` rather than
    /// the raw JSON schemas, which avoids storing potentially large strings.
    tool_hint_hash: u64,
}

impl CachedTokenCounts {
    /// Create a new, empty cache. All counts default to zero until first access.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached preamble token count, recomputing if `preamble_text` changed.
    ///
    /// The check is a single integer comparison (hash equality) — negligible overhead
    /// when the cache is warm. When the preamble changes (e.g. user edits the model's
    /// system prompt in settings), the new text is counted and the cache updated.
    ///
    /// # Arguments
    /// * `preamble_text` — The full system preamble string as it will be sent to the
    ///   provider. Include any augmentation text appended by the agent factory.
    /// * `counter` — A `TokenCounter` matching the active model's encoding.
    pub fn preamble_tokens(&mut self, preamble_text: &str, counter: &TokenCounter) -> usize {
        let h = hash_str(preamble_text);
        if h != self.preamble_hash || self.preamble_hash == 0 {
            self.preamble_tokens = counter.count_preamble(preamble_text);
            self.preamble_hash = h;
        }
        self.preamble_tokens
    }

    /// Return the cached tool-definition token count, recomputing if the tool
    /// configuration changed.
    ///
    /// Rather than requiring the caller to produce exact tool schema JSON (which
    /// is hard to access outside the agent factory), this accepts a compact
    /// `tool_hint` string that summarises the current tool configuration.
    /// A good hint encodes every axis that affects the tool list:
    ///
    /// ```text
    /// "exec:true,git:false,fs_r:true,fs_w:true,fetch:true,mcp:3,count:14"
    /// ```
    ///
    /// When the hint changes, `counter.estimate_tool_tokens(tool_count)` is called
    /// to recompute the estimate. The `tool_count` must be consistent with whatever
    /// is encoded in `tool_hint`.
    ///
    /// # Arguments
    /// * `tool_hint`  — Compact description of the current tool configuration.
    /// * `tool_count` — Total number of enabled tools (used for token estimation).
    /// * `counter`    — A `TokenCounter` matching the active model's encoding.
    pub fn tool_tokens(
        &mut self,
        tool_hint: &str,
        tool_count: usize,
        counter: &TokenCounter,
    ) -> usize {
        let h = hash_str(tool_hint);
        if h != self.tool_hint_hash || self.tool_hint_hash == 0 {
            self.tool_tokens = counter.estimate_tool_tokens(tool_count);
            self.tool_hint_hash = h;
        }
        self.tool_tokens
    }

    /// Reset the cache, forcing recomputation on the next call to either accessor.
    ///
    /// Call this when switching to a conversation whose model uses a different
    /// tokenizer encoding so stale counts from the previous model are discarded.
    #[allow(dead_code)]
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }

    /// True if the preamble cache has been populated at least once.
    pub fn has_preamble(&self) -> bool {
        self.preamble_hash != 0
    }

    /// True if the tool cache has been populated at least once.
    pub fn has_tools(&self) -> bool {
        self.tool_hint_hash != 0
    }

    /// Return the currently cached preamble token count without re-checking the hash.
    ///
    /// Useful when you only need to read the last-computed value and you know the
    /// preamble has not changed (e.g. when building a snapshot inside `spawn_blocking`
    /// after having called `preamble_tokens()` on the GPUI thread moments earlier).
    #[allow(dead_code)]
    pub fn cached_preamble_tokens(&self) -> usize {
        self.preamble_tokens
    }

    /// Return the currently cached tool token count without re-checking the hash.
    #[allow(dead_code)]
    pub fn cached_tool_tokens(&self) -> usize {
        self.tool_tokens
    }
}

// ── Tool hint builder ─────────────────────────────────────────────────────────

/// Build a compact tool-hint string and return the total enabled tool count.
///
/// The hint encodes every dimension of `ExecutionSettingsModel` that affects
/// which tools are added to the agent (mirroring the logic in `estimate_overhead()`
/// in the legacy `token_context_bar_view.rs`). When any field changes, the hint
/// string changes, causing `CachedTokenCounts::tool_tokens()` to recompute.
///
/// # Arguments
/// * `exec`      — Current execution settings (determines which tool groups are enabled)
/// * `mcp_count` — Number of currently enabled MCP servers
///
/// # Returns
/// `(tool_count, hint_string)` — the total number of tools and a compact
/// description string suitable as a cache key.
pub fn build_tool_hint(
    exec: &crate::settings::models::ExecutionSettingsModel,
    mcp_count: usize,
) -> (usize, String) {
    let mut tool_count: usize = 1; // list_tools is always present

    if exec.enabled {
        tool_count += 11; // shell (4) + git (7)
    }

    let has_workspace = exec.workspace_dir.is_some();
    if has_workspace && exec.filesystem_read_enabled {
        tool_count += 7; // fs_read (4) + search (3)
    }
    if has_workspace && exec.filesystem_write_enabled {
        tool_count += 5;
    }
    if exec.fetch_enabled {
        tool_count += 1;
    }

    tool_count += 1; // add_attachment (always present)
    tool_count += 4; // MCP management tools (always present)

    // Rough estimate: ~3 tools per enabled MCP server
    let mcp_tool_count = mcp_count * 3;
    tool_count += mcp_tool_count;

    let hint = format!(
        "exec:{},git:{},fs_r:{},fs_w:{},fetch:{},mcp_servers:{},total:{}",
        exec.enabled,
        exec.git_enabled,
        exec.filesystem_read_enabled && has_workspace,
        exec.filesystem_write_enabled && has_workspace,
        exec.fetch_enabled,
        mcp_count,
        tool_count,
    );

    (tool_count, hint)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn hash_str(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_budget::counter::TokenCounter;

    fn counter() -> TokenCounter {
        TokenCounter::for_model("gpt-4")
    }

    #[test]
    fn preamble_cache_hit_on_same_text() {
        let mut cache = CachedTokenCounts::new();
        let c = counter();
        let first = cache.preamble_tokens("You are a helpful assistant.", &c);
        let second = cache.preamble_tokens("You are a helpful assistant.", &c);
        assert_eq!(first, second);
        assert!(first > 0);
    }

    #[test]
    fn preamble_cache_miss_on_changed_text() {
        let mut cache = CachedTokenCounts::new();
        let c = counter();
        let short = cache.preamble_tokens("Hi", &c);
        let long = cache.preamble_tokens("You are a helpful assistant with deep knowledge.", &c);
        assert_ne!(short, long);
        assert!(long > short);
    }

    #[test]
    fn tool_cache_hit_on_same_hint() {
        let mut cache = CachedTokenCounts::new();
        let c = counter();
        let first = cache.tool_tokens("hint:abc", 5, &c);
        let second = cache.tool_tokens("hint:abc", 5, &c);
        assert_eq!(first, second);
        assert!(first > 0);
    }

    #[test]
    fn tool_cache_miss_on_changed_hint() {
        let mut cache = CachedTokenCounts::new();
        let c = counter();
        let small = cache.tool_tokens("tools:2", 2, &c);
        let large = cache.tool_tokens("tools:20", 20, &c);
        assert!(large > small);
    }

    #[test]
    fn invalidate_resets_all_cached_values() {
        let mut cache = CachedTokenCounts::new();
        let c = counter();
        // Populate the cache
        cache.preamble_tokens("hello", &c);
        cache.tool_tokens("t:1", 1, &c);
        assert!(cache.has_preamble());
        assert!(cache.has_tools());

        // Invalidate
        cache.invalidate();
        assert!(!cache.has_preamble());
        assert!(!cache.has_tools());
        assert_eq!(cache.cached_preamble_tokens(), 0);
        assert_eq!(cache.cached_tool_tokens(), 0);
    }

    #[test]
    fn has_preamble_false_before_first_access() {
        let cache = CachedTokenCounts::new();
        assert!(!cache.has_preamble());
    }

    #[test]
    fn has_tools_false_before_first_access() {
        let cache = CachedTokenCounts::new();
        assert!(!cache.has_tools());
    }

    #[test]
    fn cached_accessors_return_last_computed_values() {
        let mut cache = CachedTokenCounts::new();
        let c = counter();
        let expected = cache.preamble_tokens("Some preamble text here.", &c);
        assert_eq!(cache.cached_preamble_tokens(), expected);
    }

    // ── build_tool_hint ───────────────────────────────────────────────────────

    #[test]
    fn tool_hint_changes_when_execution_toggled() {
        use crate::settings::models::ExecutionSettingsModel;
        let mut exec = ExecutionSettingsModel::default();
        let (_, hint_off) = build_tool_hint(&exec, 0);
        exec.enabled = true;
        let (_, hint_on) = build_tool_hint(&exec, 0);
        assert_ne!(hint_off, hint_on);
    }

    #[test]
    fn tool_hint_changes_when_mcp_count_changes() {
        use crate::settings::models::ExecutionSettingsModel;
        let exec = ExecutionSettingsModel::default();
        let (count0, hint0) = build_tool_hint(&exec, 0);
        let (count3, hint3) = build_tool_hint(&exec, 3);
        assert_ne!(hint0, hint3);
        assert!(count3 > count0);
    }

    #[test]
    fn tool_count_minimum_is_seven() {
        // list_tools (1) + add_attachment (1) + MCP management (4) + fetch (1, enabled by default) = 7
        use crate::settings::models::ExecutionSettingsModel;
        let exec = ExecutionSettingsModel::default();
        let (count, _) = build_tool_hint(&exec, 0);
        assert_eq!(count, 7, "minimum tool count should be 7, got {count}");
    }

    #[test]
    fn tool_count_includes_execution_tools_when_enabled() {
        use crate::settings::models::ExecutionSettingsModel;
        let mut exec = ExecutionSettingsModel::default();
        exec.enabled = true;
        let (count, _) = build_tool_hint(&exec, 0);
        // 7 base (includes fetch) + 11 execution = 18
        assert_eq!(
            count, 18,
            "expected 18 tools with execution enabled, got {count}"
        );
    }

    #[test]
    fn hash_str_is_deterministic() {
        let h1 = hash_str("hello world");
        let h2 = hash_str("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_str_differs_for_different_inputs() {
        let h1 = hash_str("alpha");
        let h2 = hash_str("beta");
        assert_ne!(h1, h2);
    }
}
