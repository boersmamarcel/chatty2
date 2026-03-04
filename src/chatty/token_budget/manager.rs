use gpui::BorrowAppContext;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::cache::{CachedTokenCounts, build_tool_hint};
use super::counter::TokenCounter;
use super::snapshot::{ContextPressureEvent, TokenBudgetSnapshot};

// ── GlobalTokenBudget ─────────────────────────────────────────────────────────

/// GPUI global that owns the token budget watch channel and the static-component cache.
///
/// # Data flow
/// ```text
/// run_llm_stream()
///   → compute_snapshot_background()   (reads cache, spawns blocking BPE counting)
///     → GlobalTokenBudget::publish()  (writes to watch::Sender — O(1), non-blocking)
///       → TokenContextBarView::render()
///           reads receiver.borrow().clone() on every repaint
/// ```
///
/// # Initialisation
/// Register in `main.rs` alongside other globals:
/// ```rust
/// cx.set_global(GlobalTokenBudget::new());
/// ```
pub struct GlobalTokenBudget {
    /// Write side of the channel. Held here so `publish()` / `clear()` / `send_modify()`
    /// can be called via `cx.global::<GlobalTokenBudget>()` — which only requires `&self`
    /// because `watch::Sender::send` / `send_modify` both take `&self`.
    pub sender: watch::Sender<Option<TokenBudgetSnapshot>>,

    /// Read side of the channel. Cloned and handed to `TokenContextBarView` once during
    /// initialisation so the view can call `receiver.borrow()` cheaply on every repaint.
    pub receiver: watch::Receiver<Option<TokenBudgetSnapshot>>,

    /// Per-conversation cache for preamble tokens and tool definition tokens.
    ///
    /// These components rarely change between turns (only when the user edits the model
    /// preamble or toggles tools in settings). Caching them avoids re-running BPE on
    /// the preamble text every single send. Access via `cx.update_global` to get `&mut`.
    pub cache: CachedTokenCounts,
}

impl gpui::Global for GlobalTokenBudget {}

impl GlobalTokenBudget {
    /// Create a new global with an empty (None) initial snapshot.
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(None);
        Self {
            sender,
            receiver,
            cache: CachedTokenCounts::new(),
        }
    }

    /// Publish a freshly-computed snapshot to all readers.
    ///
    /// This is a `&self` operation — `watch::Sender::send` does not require mutation of
    /// `GlobalTokenBudget`, so it can be called directly via `cx.global::<GlobalTokenBudget>()`.
    #[allow(dead_code)]
    pub fn publish(&self, snapshot: TokenBudgetSnapshot) {
        // Ignore SendError — it only occurs when all receivers have been dropped (i.e. shutdown).
        let _ = self.sender.send(Some(snapshot));
    }

    /// Clear the current snapshot, causing the bar to render in its "no data" state.
    ///
    /// Call this whenever the active conversation changes so the bar doesn't momentarily
    /// display stale data from the previous conversation.
    pub fn clear(&self) {
        let _ = self.sender.send(None);
    }

    /// Update the existing snapshot in-place with actual provider-reported token counts.
    ///
    /// Called from `finalize_completed_stream()` once the LLM API response arrives and
    /// we know the real input/output token counts for the last turn.
    ///
    /// `send_modify` is atomic: readers either see the old snapshot or the updated one,
    /// never a partially-written intermediate state.
    pub fn update_with_actuals(&self, input_tokens: u32, output_tokens: u32) {
        self.sender.send_modify(|opt| {
            if let Some(snap) = opt {
                snap.actual_input_tokens = Some(input_tokens as usize);
                snap.actual_output_tokens = Some(output_tokens as usize);
                debug!(
                    actual_in = input_tokens,
                    actual_out = output_tokens,
                    estimated = snap.estimated_total(),
                    delta = snap.estimation_delta().unwrap_or(0),
                    "Snapshot updated with provider actuals"
                );
            }
        });
    }

    /// Borrow the current snapshot without cloning.
    ///
    /// Returns a `watch::Ref` — zero-copy, holds a read lock until dropped.
    /// For the render path, prefer calling this directly on the `receiver` field.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> watch::Ref<'_, Option<TokenBudgetSnapshot>> {
        self.receiver.borrow()
    }
}

impl Default for GlobalTokenBudget {
    fn default() -> Self {
        Self::new()
    }
}

// ── Inputs bundle ─────────────────────────────────────────────────────────────

/// All inputs needed to compute a `TokenBudgetSnapshot`.
///
/// Gathered synchronously on the GPUI thread (inside `cx.update()`) before handing
/// off to `compute_snapshot_background()`.
pub struct SnapshotInputs {
    pub conversation_id: String,
    pub model_identifier: String,
    pub model_context_limit: usize,
    pub response_reserve: usize,
    pub preamble: String,
    pub history: Vec<rig::completion::Message>,
    pub user_message_text: String,
    // Populated for potential future use (e.g. re-running tool estimation in-task).
    #[allow(dead_code)]
    pub exec_settings: crate::settings::models::ExecutionSettingsModel,
    #[allow(dead_code)]
    pub mcp_server_count: usize,
    /// Pre-computed cached preamble tokens (read from `GlobalTokenBudget::cache` on
    /// the GPUI thread before `spawn_blocking`). Zero if the cache was cold.
    pub cached_preamble_tokens: usize,
    /// Pre-computed cached tool tokens. Zero if the cache was cold.
    pub cached_tool_tokens: usize,
    /// The tool hint string used to produce `cached_tool_tokens`. Stored for
    /// diagnostics; not consumed by `compute_snapshot_background`.
    #[allow(dead_code)]
    pub tool_hint: String,
    pub tool_count: usize,
    /// True when the preamble cache was warm for these inputs.
    pub preamble_cache_hit: bool,
    /// True when the tool cache was warm for these inputs.
    pub tool_cache_hit: bool,
}

// ── compute_snapshot_background ───────────────────────────────────────────────

/// Compute a `TokenBudgetSnapshot` on a background thread and publish it via the
/// global watch channel.
///
/// # Concurrency model
/// - Preamble tokens and tool tokens are read from `GlobalTokenBudget::cache` on the
///   GPUI thread (fast — just a hash check and integer copy when the cache is warm).
/// - Conversation history tokens and user-message tokens are counted inside
///   `tokio::spawn_blocking` so the BPE work never touches the UI thread.
/// - After the blocking task returns, the snapshot is published through
///   `GlobalTokenBudget::publish()`, and a pressure check is logged.
///
/// # Arguments
/// * `inputs`  — Everything gathered from globals on the GPUI thread.
/// * `budget`  — Reference to the global, used only to call `publish()`.
///
/// # Returns
/// The completed `TokenBudgetSnapshot`, or an error if `spawn_blocking` was cancelled.
pub async fn compute_snapshot_background(
    inputs: SnapshotInputs,
) -> anyhow::Result<TokenBudgetSnapshot> {
    let conv_id = inputs.conversation_id.clone();
    let model_id = inputs.model_identifier.clone();
    let model_context_limit = inputs.model_context_limit;
    let response_reserve = inputs.response_reserve;

    // Values pre-computed on the GPUI thread (cache hits)
    let cached_preamble = inputs.cached_preamble_tokens;
    let cached_tools = inputs.cached_tool_tokens;
    let preamble_cache_hit = inputs.preamble_cache_hit;
    let tool_cache_hit = inputs.tool_cache_hit;

    // Values needed inside spawn_blocking (must be Send + 'static)
    let preamble = inputs.preamble;
    let history = inputs.history;
    let user_message_text = inputs.user_message_text;
    let tool_count = inputs.tool_count;

    let snapshot = tokio::task::spawn_blocking(move || {
        let counter = TokenCounter::for_model(&model_id);

        // Re-count preamble only when the cache was cold (content changed)
        let preamble_tokens = if preamble_cache_hit {
            cached_preamble
        } else {
            counter.count_preamble(&preamble)
        };

        // Re-count tools only when the cache was cold
        let tool_tokens = if tool_cache_hit {
            cached_tools
        } else {
            counter.estimate_tool_tokens(tool_count)
        };

        // History and user message are always re-counted — they change every turn
        let history_tokens = counter.count_history(&history);
        let user_msg_tokens = counter.count(&user_message_text);

        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit,
            response_reserve,
            preamble_tokens,
            tool_definitions_tokens: tool_tokens,
            conversation_history_tokens: history_tokens,
            latest_user_message_tokens: user_msg_tokens,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: conv_id,
        };

        debug!(
            preamble_tokens,
            tool_tokens,
            history_tokens,
            user_msg_tokens,
            estimated_total = snap.estimated_total(),
            utilization = snap.utilization(),
            "Token budget snapshot computed"
        );

        snap
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking for token counting panicked: {e}"))?;

    Ok(snapshot)
}

// ── Pressure checking ─────────────────────────────────────────────────────────

/// Evaluate the snapshot's utilization against the configured thresholds and log
/// a warning when pressure is High or Critical.
///
/// This is a lightweight synchronous helper called after `compute_snapshot_background`
/// returns. Full GPUI `EventEmitter` integration (for the conversation manager to
/// subscribe and take action) is wired in Phase 11.
///
/// # Arguments
/// * `snapshot`    — The freshly-computed snapshot.
/// * `settings`    — Token tracking settings (provides configurable thresholds).
///   Pass `None` to use the hard-coded defaults from `ContextStatus`.
pub fn check_pressure(
    snapshot: &TokenBudgetSnapshot,
    settings: Option<&crate::settings::models::token_tracking_settings::TokenTrackingSettings>,
) -> Option<ContextPressureEvent> {
    let high_threshold = settings.map(|s| s.high_threshold).unwrap_or(0.70);
    let critical_threshold = settings.map(|s| s.critical_threshold).unwrap_or(0.90);

    let u = snapshot.utilization();

    if u >= critical_threshold {
        let event = ContextPressureEvent::CriticalPressure {
            utilization: u,
            estimated_tokens: snapshot.estimated_total(),
            conversation_id: snapshot.conversation_id.clone(),
        };
        warn!(
            utilization = format!("{:.1}%", u * 100.0),
            estimated_tokens = snapshot.estimated_total(),
            effective_budget = snapshot.effective_budget(),
            conversation_id = %snapshot.conversation_id,
            "Context window CRITICAL — consider summarizing the conversation"
        );
        Some(event)
    } else if u >= high_threshold {
        let event = ContextPressureEvent::HighPressure {
            utilization: u,
            estimated_tokens: snapshot.estimated_total(),
            conversation_id: snapshot.conversation_id.clone(),
        };
        info!(
            utilization = format!("{:.1}%", u * 100.0),
            estimated_tokens = snapshot.estimated_total(),
            effective_budget = snapshot.effective_budget(),
            conversation_id = %snapshot.conversation_id,
            "Context window high — approaching limit"
        );
        Some(event)
    } else {
        None
    }
}

// ── Helpers for use from app_controller.rs ────────────────────────────────────

/// Extract plain text from a slice of `rig::message::UserContent`.
///
/// Concatenates all `Text` variants with a single space. Non-text content
/// (images, PDFs) is skipped — we only want to count the message text tokens.
pub fn extract_user_message_text(contents: &[rig::message::UserContent]) -> String {
    contents
        .iter()
        .filter_map(|c| match c {
            rig::message::UserContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Gather `SnapshotInputs` from GPUI globals and the conversation store.
///
/// Returns `None` when the active conversation or its model config is not
/// available (e.g. during startup, or if no model has been configured).
///
/// This function must be called on the GPUI thread (inside `cx.update()`).
/// It both reads globals AND mutates `GlobalTokenBudget::cache` to update the
/// static-component counts before the blocking task runs.
pub fn gather_snapshot_inputs(
    conv_id: &str,
    user_message_text: String,
    history: Vec<rig::completion::Message>,
    cx: &mut gpui::App,
) -> Option<SnapshotInputs> {
    use crate::chatty::models::ConversationsStore;
    use crate::settings::models::mcp_store::McpServersModel;
    use crate::settings::models::token_tracking_settings::TokenTrackingSettings;
    use crate::settings::models::{ExecutionSettingsModel, ModelsModel};

    // ── Model config ──────────────────────────────────────────────────────────
    let conv = cx
        .global::<ConversationsStore>()
        .get_conversation(conv_id)?;
    let model_id_str = conv.model_id().to_string();

    let model_config = cx.global::<ModelsModel>().get_model(&model_id_str)?.clone();
    let model_context_limit = model_config.max_context_window.map(|v| v as usize)?;
    let model_identifier = model_config.model_identifier.clone();
    let preamble = model_config.preamble.clone();

    // ── Settings ──────────────────────────────────────────────────────────────
    let response_reserve = cx
        .try_global::<TokenTrackingSettings>()
        .map(|s| s.response_reserve)
        .unwrap_or(4096);

    let exec_settings = cx.global::<ExecutionSettingsModel>().clone();
    let mcp_server_count = cx.global::<McpServersModel>().enabled_count();

    // ── Tool hint (for cache key) ─────────────────────────────────────────────
    let (tool_count, tool_hint) = build_tool_hint(&exec_settings, mcp_server_count);

    // ── Warm the static-component cache on the GPUI thread ───────────────────
    // These checks are just an integer comparison (hash equality) — negligible cost.
    // The actual BPE counting for cache misses is done here too, but only for the
    // preamble and tool hint (both short strings, <5 ms even cold).
    let counter = TokenCounter::for_model(&model_identifier);

    let (cached_preamble_tokens, preamble_cache_hit) = {
        let was_warm = cx.global::<GlobalTokenBudget>().cache.has_preamble();
        let tokens = cx.update_global::<GlobalTokenBudget, _>(|budget, _cx| {
            budget.cache.preamble_tokens(&preamble, &counter)
        });
        (tokens, was_warm)
    };

    let (cached_tool_tokens, tool_cache_hit) = {
        let was_warm = cx.global::<GlobalTokenBudget>().cache.has_tools();
        let tokens = cx.update_global::<GlobalTokenBudget, _>(|budget, _cx| {
            budget.cache.tool_tokens(&tool_hint, tool_count, &counter)
        });
        (tokens, was_warm)
    };

    Some(SnapshotInputs {
        conversation_id: conv_id.to_string(),
        model_identifier,
        model_context_limit,
        response_reserve,
        preamble,
        history,
        user_message_text,
        exec_settings,
        mcp_server_count,
        cached_preamble_tokens,
        cached_tool_tokens,
        tool_hint,
        tool_count,
        preamble_cache_hit,
        tool_cache_hit,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_token_budget_new_has_empty_snapshot() {
        let budget = GlobalTokenBudget::new();
        assert!(budget.snapshot().is_none());
    }

    #[test]
    fn publish_makes_snapshot_readable() {
        let budget = GlobalTokenBudget::new();
        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: 128_000,
            response_reserve: 4_096,
            preamble_tokens: 1_000,
            tool_definitions_tokens: 2_000,
            conversation_history_tokens: 10_000,
            latest_user_message_tokens: 500,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "conv-1".to_string(),
        };
        budget.publish(snap);
        assert!(budget.snapshot().is_some());
    }

    #[test]
    fn clear_removes_snapshot() {
        let budget = GlobalTokenBudget::new();
        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: 128_000,
            response_reserve: 4_096,
            preamble_tokens: 0,
            tool_definitions_tokens: 0,
            conversation_history_tokens: 0,
            latest_user_message_tokens: 0,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "conv-1".to_string(),
        };
        budget.publish(snap);
        assert!(budget.snapshot().is_some());
        budget.clear();
        assert!(budget.snapshot().is_none());
    }

    #[test]
    fn update_with_actuals_sets_token_counts() {
        let budget = GlobalTokenBudget::new();
        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: 128_000,
            response_reserve: 4_096,
            preamble_tokens: 1_000,
            tool_definitions_tokens: 2_000,
            conversation_history_tokens: 10_000,
            latest_user_message_tokens: 500,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "conv-1".to_string(),
        };
        budget.publish(snap);
        budget.update_with_actuals(14_200, 350);

        let updated = budget.snapshot();
        let updated = updated.as_ref().unwrap();
        assert_eq!(updated.actual_input_tokens, Some(14_200));
        assert_eq!(updated.actual_output_tokens, Some(350));
    }

    #[test]
    fn update_with_actuals_is_noop_when_no_snapshot() {
        let budget = GlobalTokenBudget::new();
        // Should not panic when there's no snapshot to update
        budget.update_with_actuals(1_000, 200);
        assert!(budget.snapshot().is_none());
    }

    #[test]
    fn extract_user_message_text_gets_text_content() {
        use rig::completion::message::Text;
        use rig::message::UserContent;
        let contents = vec![UserContent::Text(Text {
            text: "Hello, world!".to_string(),
        })];
        let text = extract_user_message_text(&contents);
        assert_eq!(text, "Hello, world!");
    }

    #[test]
    fn extract_user_message_text_empty_on_no_text_content() {
        let contents: Vec<rig::message::UserContent> = vec![];
        assert_eq!(extract_user_message_text(&contents), "");
    }

    #[test]
    fn check_pressure_none_below_high_threshold() {
        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: 100_000,
            response_reserve: 0,
            preamble_tokens: 30_000,
            tool_definitions_tokens: 0,
            conversation_history_tokens: 0,
            latest_user_message_tokens: 0,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "c".to_string(),
        };
        // 30% utilization — well below 70% high threshold
        assert!(check_pressure(&snap, None).is_none());
    }

    #[test]
    fn check_pressure_high_between_thresholds() {
        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: 100_000,
            response_reserve: 0,
            preamble_tokens: 80_000, // 80% — high
            tool_definitions_tokens: 0,
            conversation_history_tokens: 0,
            latest_user_message_tokens: 0,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "c".to_string(),
        };
        let event = check_pressure(&snap, None);
        assert!(matches!(
            event,
            Some(ContextPressureEvent::HighPressure { .. })
        ));
    }

    #[test]
    fn check_pressure_critical_above_critical_threshold() {
        let snap = TokenBudgetSnapshot {
            computed_at: std::time::Instant::now(),
            model_context_limit: 100_000,
            response_reserve: 0,
            preamble_tokens: 95_000, // 95% — critical
            tool_definitions_tokens: 0,
            conversation_history_tokens: 0,
            latest_user_message_tokens: 0,
            actual_input_tokens: None,
            actual_output_tokens: None,
            conversation_id: "c".to_string(),
        };
        let event = check_pressure(&snap, None);
        assert!(matches!(
            event,
            Some(ContextPressureEvent::CriticalPressure { .. })
        ));
    }

    #[tokio::test]
    async fn compute_snapshot_background_returns_valid_snapshot() {
        let inputs = SnapshotInputs {
            conversation_id: "test-conv".to_string(),
            model_identifier: "gpt-4".to_string(),
            model_context_limit: 128_000,
            response_reserve: 4_096,
            preamble: "You are a helpful assistant.".to_string(),
            history: vec![],
            user_message_text: "Hello!".to_string(),
            exec_settings: crate::settings::models::ExecutionSettingsModel::default(),
            mcp_server_count: 0,
            cached_preamble_tokens: 0,
            cached_tool_tokens: 0,
            tool_hint: "tools:6,mcp:0".to_string(),
            tool_count: 6,
            preamble_cache_hit: false,
            tool_cache_hit: false,
        };

        let snap = compute_snapshot_background(inputs).await.unwrap();

        assert_eq!(snap.conversation_id, "test-conv");
        assert_eq!(snap.model_context_limit, 128_000);
        assert_eq!(snap.response_reserve, 4_096);
        assert!(snap.preamble_tokens > 0, "preamble should have tokens");
        assert!(snap.tool_definitions_tokens > 0, "tools should have tokens");
        assert_eq!(snap.conversation_history_tokens, 0); // empty history
        assert!(
            snap.latest_user_message_tokens > 0,
            "user message should have tokens"
        );
        assert!(snap.actual_input_tokens.is_none());
    }

    #[tokio::test]
    async fn compute_snapshot_uses_cached_values_when_hits() {
        let cached_pre = 999;
        let cached_tools = 1234;
        let inputs = SnapshotInputs {
            conversation_id: "test-conv".to_string(),
            model_identifier: "gpt-4".to_string(),
            model_context_limit: 128_000,
            response_reserve: 4_096,
            preamble: "You are a helpful assistant.".to_string(),
            history: vec![],
            user_message_text: "Hello".to_string(),
            exec_settings: crate::settings::models::ExecutionSettingsModel::default(),
            mcp_server_count: 0,
            cached_preamble_tokens: cached_pre,
            cached_tool_tokens: cached_tools,
            tool_hint: "t:6".to_string(),
            tool_count: 6,
            preamble_cache_hit: true, // cache is warm
            tool_cache_hit: true,     // cache is warm
        };

        let snap = compute_snapshot_background(inputs).await.unwrap();

        // Should use cached values verbatim
        assert_eq!(snap.preamble_tokens, cached_pre);
        assert_eq!(snap.tool_definitions_tokens, cached_tools);
    }
}
