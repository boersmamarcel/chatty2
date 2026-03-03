use crate::chatty::models::conversations_store::ConversationsStore;
use crate::chatty::models::token_usage::{format_cost, format_tokens};
use crate::chatty::token_budget::{ContextStatus, GlobalTokenBudget, TokenBudgetSnapshot};
use crate::settings::models::token_tracking_settings::TokenTrackingSettings;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Sizable, button::*, h_flex};

// ── Layout constants ──────────────────────────────────────────────────────────

const FOOTER_BAR_WIDTH: f32 = 80.0;
const FOOTER_BAR_HEIGHT: f32 = 6.0;
const POPOVER_BAR_HEIGHT: f32 = 8.0;
const POPOVER_MIN_WIDTH: f32 = 260.0;
const POPOVER_MAX_WIDTH: f32 = 300.0;

// ── Segment colour constants (hex RGB) ───────────────────────────────────────
// These match the legend dots shown in the popover.

const COLOR_PREAMBLE: u32 = 0x60A5FA; // Blue-400  — system preamble
const COLOR_TOOLS: u32 = 0xA78BFA; // Violet-400 — tool JSON schemas
const COLOR_HISTORY: u32 = 0x34D399; // Emerald-400 — conversation history
const COLOR_USER_MSG: u32 = 0x22D3EE; // Cyan-400  — latest user message

// ── Main view type ────────────────────────────────────────────────────────────

#[derive(IntoElement, Default)]
pub struct TokenContextBarView;

impl TokenContextBarView {
    pub fn new() -> Self {
        Self
    }
}

// ── Snapshot reader ───────────────────────────────────────────────────────────

/// Read the latest `TokenBudgetSnapshot` from the watch channel global.
///
/// Returns `None` in three cases:
/// 1. `GlobalTokenBudget` has not been initialised yet (startup)
/// 2. No snapshot has been published (conversation with no `max_context_window`, or
///    before the first message is sent in a new conversation)
/// 3. The snapshot belongs to a different conversation (stale — cleared by
///    `load_conversation()` and a fresh one is about to arrive)
fn read_budget_snapshot(cx: &App) -> Option<TokenBudgetSnapshot> {
    // Guard: feature disabled
    if cx
        .try_global::<TokenTrackingSettings>()
        .is_some_and(|s| !s.should_show_bar())
    {
        return None;
    }

    let active_id = cx.global::<ConversationsStore>().active_id()?.clone();

    let snap = cx
        .try_global::<GlobalTokenBudget>()
        .and_then(|g| g.receiver.borrow().clone())?;

    // Guard: stale snapshot from a previous conversation
    if snap.conversation_id != active_id {
        return None;
    }

    Some(snap)
}

// ── Stacked bar builder ───────────────────────────────────────────────────────

/// Render a segmented horizontal bar showing each context component as a
/// proportional coloured strip. Segments are ordered:
///   preamble | tools | history | user_msg | remaining (theme bg)
///
/// The border colour signals the current `ContextStatus`:
/// - Normal / Moderate → theme border (no special colour)
/// - High              → amber (#F59E0B)
/// - Critical          → red   (#EF4444)
fn render_stacked_bar(
    snap: &TokenBudgetSnapshot,
    bar_width: f32,
    bar_height: f32,
    cx: &App,
) -> impl IntoElement {
    let frac = snap.component_fractions();
    let remaining = frac.remaining();

    let border_color: Hsla = match snap.status() {
        ContextStatus::Critical => rgb(0xEF4444).into(), // Red-500
        ContextStatus::High => rgb(0xF59E0B).into(),     // Amber-500
        _ => cx.theme().border,
    };

    div()
        .w(px(bar_width))
        .h(px(bar_height))
        .rounded_sm()
        .bg(cx.theme().border) // default background = "remaining" colour
        .border_1()
        .border_color(border_color)
        .overflow_hidden()
        .flex()
        .flex_row()
        .child(bar_segment(bar_width, frac.preamble as f32, COLOR_PREAMBLE))
        .child(bar_segment(bar_width, frac.tools as f32, COLOR_TOOLS))
        .child(bar_segment(bar_width, frac.history as f32, COLOR_HISTORY))
        .child(bar_segment(bar_width, frac.user_msg as f32, COLOR_USER_MSG))
        // Remaining: a slightly darker grey so it blends into the bg
        .when(remaining > 0.0, |this| {
            this.child(bar_segment(bar_width, remaining as f32, 0x374151))
        })
}

/// A single segment div with proportional width.
fn bar_segment(total_width: f32, fraction: f32, color_hex: u32) -> Div {
    let w = (total_width * fraction.clamp(0.0, 1.0)).max(0.0);
    div().w(px(w)).h_full().bg(rgb(color_hex))
}

// ── Empty bar (no snapshot) ───────────────────────────────────────────────────

/// Rendered while waiting for the first snapshot (no model configured,
/// new conversation before first send, or during conversation switch).
fn render_empty_trigger(cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .items_center()
        .child(
            div()
                .w(px(FOOTER_BAR_WIDTH))
                .h(px(FOOTER_BAR_HEIGHT))
                .rounded_sm()
                .bg(cx.theme().border),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("—"),
        )
}

// ── RenderOnce implementation ─────────────────────────────────────────────────

impl RenderOnce for TokenContextBarView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(snap) = read_budget_snapshot(cx) else {
            // No snapshot — render a static empty trigger with no popover
            return div()
                .id("token-context-bar-empty")
                .child(render_empty_trigger(cx));
        };

        // ── Pre-compute all display strings ───────────────────────────────────
        // Everything is computed here on the render thread. The closure passed
        // to `Popover::content` must be `'static`, so we copy/clone before it.

        let pct = (snap.utilization() * 100.0).clamp(0.0, 100.0) as f32;
        let pct_text = format!("{:.0}%", pct);

        // Context status for border colour
        let status = snap.status();

        // Summary line in popover
        let summary_text = format!(
            "~{} / {} tokens \u{00B7} {:.0}%",
            format_tokens(snap.estimated_total() as u32),
            format_tokens(snap.model_context_limit as u32),
            snap.utilization() * 100.0,
        );

        // Component breakdowns
        let frac = snap.component_fractions();
        let preamble_text = format!(
            "~{}  ({:.1}%)",
            format_tokens(snap.preamble_tokens as u32),
            frac.preamble * 100.0
        );
        let tools_text = format!(
            "~{}  ({:.1}%)",
            format_tokens(snap.tool_definitions_tokens as u32),
            frac.tools * 100.0
        );
        let history_text = format!(
            "~{}  ({:.1}%)",
            format_tokens(snap.conversation_history_tokens as u32),
            frac.history * 100.0
        );
        let user_msg_text = format!(
            "~{}  ({:.1}%)",
            format_tokens(snap.latest_user_message_tokens as u32),
            frac.user_msg * 100.0
        );
        let remaining_text = format!(
            "~{}  ({:.1}%)",
            format_tokens(snap.remaining() as u32),
            frac.remaining() * 100.0
        );

        // Actual token counts from the provider (only shown once they arrive)
        let has_actuals = snap.has_actuals();
        let actual_input_text = snap
            .actual_input_tokens
            .map(|t| format_tokens(t as u32))
            .unwrap_or_default();
        let actual_output_text = snap
            .actual_output_tokens
            .map(|t| format_tokens(t as u32))
            .unwrap_or_default();
        let delta_text = snap.estimation_delta().map(|d| {
            if d > 0 {
                format!(
                    "+{} (under-estimate)",
                    format_tokens(d.unsigned_abs() as u32)
                )
            } else if d < 0 {
                format!(
                    "-{} (over-estimate)",
                    format_tokens(d.unsigned_abs() as u32)
                )
            } else {
                "exact".to_string()
            }
        });

        // Critical status label
        let is_critical = status.is_critical();
        let status_label = if is_critical {
            Some("⚠ Context nearly full — consider summarizing")
        } else {
            None
        };

        // Session totals from ConversationTokenUsage (unchanged from v1)
        let store = cx.global::<ConversationsStore>();
        let session_totals: Option<(u32, u32, f64)> = store
            .active_id()
            .and_then(|id| store.get_conversation(id))
            .map(|c| {
                let u = c.token_usage();
                (
                    u.total_input_tokens,
                    u.total_output_tokens,
                    u.total_estimated_cost_usd,
                )
            });

        let (session_input, session_output, session_cost) = session_totals.unwrap_or((0, 0, 0.0));
        let has_session = session_input > 0 || session_output > 0;
        let has_cost = session_cost > 0.0;
        let session_input_text = format_tokens(session_input);
        let session_output_text = format_tokens(session_output);
        let cost_text = format_cost(session_cost);

        // Clone snap fractions for the popover closure (must be 'static)
        let snap_pct = pct;
        let snap_context_limit = snap.model_context_limit;
        let snap_status = snap.status();

        // ── Trigger: small stacked bar + percentage ───────────────────────────
        let trigger = Button::new("token-context-trigger").ghost().xsmall().child(
            h_flex()
                .gap_1()
                .items_center()
                .child(render_stacked_bar(
                    &snap,
                    FOOTER_BAR_WIDTH,
                    FOOTER_BAR_HEIGHT,
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(if status.is_warning() {
                            match snap.status() {
                                ContextStatus::Critical => rgb(0xEF4444).into(),
                                ContextStatus::High => rgb(0xF59E0B).into(),
                                _ => cx.theme().muted_foreground,
                            }
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(pct_text),
                ),
        );

        // ── Colour constants for popover (must be copy types) ─────────────────
        let dot_preamble: Hsla = rgb(COLOR_PREAMBLE).into();
        let dot_tools: Hsla = rgb(COLOR_TOOLS).into();
        let dot_history: Hsla = rgb(COLOR_HISTORY).into();
        let dot_user_msg: Hsla = rgb(COLOR_USER_MSG).into();

        // ── Build and return the popover ──────────────────────────────────────
        div()
            .id("token-context-bar")
            .child(
                Popover::new("token-context-popover")
                    .trigger(trigger)
                    .content(move |_, _window, cx| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .min_w(px(POPOVER_MIN_WIDTH))
                            .max_w(px(POPOVER_MAX_WIDTH))
                            // Summary line
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(summary_text.clone()),
                            )
                            // Bar in popover (tall version for readability)
                            .child(render_stacked_bar(
                                &snap,
                                POPOVER_MAX_WIDTH - 24.0,
                                POPOVER_BAR_HEIGHT,
                                cx,
                            ))
                            // Component breakdown legend
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    // Preamble
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(10.0))
                                                    .h(px(10.0))
                                                    .rounded_sm()
                                                    .bg(dot_preamble),
                                            )
                                            .child(format!("Preamble: {}", preamble_text.clone())),
                                    )
                                    // Tools
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(10.0))
                                                    .h(px(10.0))
                                                    .rounded_sm()
                                                    .bg(dot_tools),
                                            )
                                            .child(format!("Tools: {}", tools_text.clone())),
                                    )
                                    // History
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(10.0))
                                                    .h(px(10.0))
                                                    .rounded_sm()
                                                    .bg(dot_history),
                                            )
                                            .child(format!("History: {}", history_text.clone())),
                                    )
                                    // User message
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(10.0))
                                                    .h(px(10.0))
                                                    .rounded_sm()
                                                    .bg(dot_user_msg),
                                            )
                                            .child(format!("Latest message: {}", user_msg_text.clone())),
                                    )
                                    // Remaining
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .w(px(10.0))
                                                    .h(px(10.0))
                                                    .rounded_sm()
                                                    .bg(cx.theme().border),
                                            )
                                            .child(format!("Remaining: {}", remaining_text.clone())),
                                    ),
                            )
                            // Actual counts section (only if available)
                            .when(has_actuals, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .text_xs()
                                        .pt_2()
                                        .border_t_1()
                                        .border_color(cx.theme().border)
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(cx.theme().foreground)
                                                .child("Actual (from provider):"),
                                        )
                                        .child(format!("Input: {}", actual_input_text.clone()))
                                        .child(format!("Output: {}", actual_output_text.clone()))
                                        .when(delta_text.is_some(), |popover_div| {
                                            popover_div.child(format!(
                                                "Estimation: {}",
                                                delta_text.clone().unwrap_or_default()
                                            ))
                                        }),
                                )
                            })
                            // Status alert
                            .when(status_label.is_some(), |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .pt_2()
                                        .border_t_1()
                                        .border_color(rgb(0xEF4444))
                                        .text_xs()
                                        .text_color(rgb(0xEF4444))
                                        .child(status_label.unwrap_or_default()),
                                )
                            })
                            // Session totals (unchanged from v1)
                            .when(has_session, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .pt_2()
                                        .border_t_1()
                                        .border_color(cx.theme().border)
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(cx.theme().foreground)
                                                .child("Session totals:"),
                                        )
                                        .child(format!("Input: {}", session_input_text))
                                        .child(format!("Output: {}", session_output_text))
                                        .when(has_cost, |popover_div| {
                                            popover_div.child(format!("Cost: {}", cost_text))
                                        }),
                                )
                            })
                    }),
            )
    }
}
