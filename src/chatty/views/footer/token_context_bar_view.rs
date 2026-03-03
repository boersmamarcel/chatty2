use crate::chatty::models::conversations_store::ConversationsStore;
use crate::chatty::models::token_usage::{format_cost, format_tokens};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::mcp_store::McpServersModel;
use crate::settings::models::models_store::ModelsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Sizable, button::*, h_flex};

const POPOVER_MIN_WIDTH: f32 = 240.0;
const POPOVER_MAX_WIDTH: f32 = 280.0;
const FOOTER_BAR_WIDTH: f32 = 80.0;

// Category dot colors
const COLOR_SYSTEM_PROMPT: u32 = 0x60A5FA; // Blue-400
const COLOR_TOOL_DEFS: u32 = 0xA78BFA; // Violet-400
const COLOR_MESSAGES: u32 = 0x34D399; // Emerald-400

// Average tokens per tool schema (JSON definition sent to LLM)
const TOKENS_PER_TOOL_SCHEMA: u32 = 300;

#[derive(IntoElement, Default)]
pub struct TokenContextBarView;

impl TokenContextBarView {
    pub fn new() -> Self {
        Self
    }
}

/// Pre-computed data for the popover, all Copy types so the closure can own it.
#[derive(Clone, Copy)]
struct TokenData {
    current_tokens: u32,
    max_context_window: u32,
    pct: f32,
    bar_color: Hsla,
    total_input_tokens: u32,
    total_output_tokens: u32,
    total_cost: f64,
    system_prompt_pct: f32,
    tool_definitions_pct: f32,
    messages_pct: f32,
}

fn gather_token_data(cx: &App) -> Option<TokenData> {
    let store = cx.global::<ConversationsStore>();
    let active_id = store.active_id()?;
    let conv = store.get_conversation(active_id)?;

    // Use estimated_context_tokens() to normalize rig-core's accumulated
    // input_tokens back to a per-turn average (closer to actual context fill).
    let current_tokens = conv
        .token_usage()
        .message_usages
        .last()
        .map(|u| u.estimated_context_tokens())
        .unwrap_or(0);

    let model_id = conv.model_id().to_string();
    let models = cx.global::<ModelsModel>();
    let model_config = models.get_model(&model_id)?;
    let max_context_window = model_config.max_context_window.map(|v| v as u32)?;

    let pct = (current_tokens as f32 / max_context_window as f32 * 100.0).clamp(0.0, 100.0);

    let bar_color: Hsla = if pct >= 85.0 {
        rgb(0xEF4444).into() // Red-500
    } else if pct >= 60.0 {
        rgb(0xF59E0B).into() // Amber-500
    } else {
        rgb(0x22C55E).into() // Green-500
    };

    let token_usage = conv.token_usage();
    let total_input_tokens = token_usage.total_input_tokens;
    let total_output_tokens = token_usage.total_output_tokens;
    let total_cost = token_usage.total_estimated_cost_usd;

    let (system_prompt_pct, tool_definitions_pct, messages_pct) =
        estimate_breakdown(current_tokens, max_context_window, model_config, cx);

    Some(TokenData {
        current_tokens,
        max_context_window,
        pct,
        bar_color,
        total_input_tokens,
        total_output_tokens,
        total_cost,
        system_prompt_pct,
        tool_definitions_pct,
        messages_pct,
    })
}

/// Estimate token breakdown as percentages of the context window.
fn estimate_breakdown(
    current_tokens: u32,
    max_context: u32,
    model_config: &crate::settings::models::models_store::ModelConfig,
    cx: &App,
) -> (f32, f32, f32) {
    if current_tokens == 0 || max_context == 0 {
        return (0.0, 0.0, 0.0);
    }

    // System prompt: user preamble + tool summary (~500 chars) + formatting guide (~800 chars)
    let augmentation_chars: usize = 1300;
    let system_tokens = (model_config.preamble.len() + augmentation_chars) as u32 / 4;

    // Tool definitions: count enabled tool schemas (mirrors agent_factory.rs gating)
    let exec = cx.global::<ExecutionSettingsModel>();
    let mut tool_count: u32 = 1; // list_tools always present
    let workspace = exec.workspace_dir.is_some();
    if exec.enabled {
        tool_count += 4; // shell tools
    }
    if workspace && exec.git_enabled {
        tool_count += 7; // git tools
    }
    if workspace && exec.filesystem_read_enabled {
        tool_count += 4 + 3; // fs_read + search tools
    }
    if workspace && exec.filesystem_write_enabled {
        tool_count += 5; // fs_write tools
    }
    if exec.fetch_enabled {
        tool_count += 1; // fetch tool
    }
    tool_count += 1; // add_attachment
    tool_count += 4; // MCP management tools

    // Estimated MCP tools (~3 tools per enabled server)
    let mcp_tool_count = cx.global::<McpServersModel>().enabled_count() as u32 * 3;
    tool_count += mcp_tool_count;

    let tool_tokens = tool_count * TOKENS_PER_TOOL_SCHEMA;

    // Messages = remainder of current context fill
    let overhead = system_tokens + tool_tokens;
    let messages_tokens = current_tokens.saturating_sub(overhead);

    let max_f = max_context as f32;
    (
        (system_tokens as f32 / max_f * 100.0).clamp(0.0, 100.0),
        (tool_tokens as f32 / max_f * 100.0).clamp(0.0, 100.0),
        (messages_tokens as f32 / max_f * 100.0).clamp(0.0, 100.0),
    )
}

impl RenderOnce for TokenContextBarView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(data) = gather_token_data(cx) else {
            return div().id("token-context-bar-hidden");
        };

        let pct = data.pct;
        let bar_color = data.bar_color;
        let pct_text = format!("{:.0}%", pct);

        // Pre-compute all strings for the popover (avoids lifetime issues in closure)
        let summary_text = format!(
            "{} / {} tokens \u{00B7} {:.0}%",
            format_tokens(data.current_tokens),
            format_tokens(data.max_context_window),
            data.pct,
        );
        let system_pct_text = format!("{:.1}%", data.system_prompt_pct);
        let tools_pct_text = format!("{:.1}%", data.tool_definitions_pct);
        let messages_pct_text = format!("{:.1}%", data.messages_pct);
        let input_text = format_tokens(data.total_input_tokens);
        let output_text = format_tokens(data.total_output_tokens);
        let cost_text = format_cost(data.total_cost);
        let has_cost = data.total_cost > 0.0;

        // Trigger button: small progress bar + percentage
        let trigger = Button::new("token-context-trigger").ghost().xsmall().child(
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .w(px(FOOTER_BAR_WIDTH))
                        .h(px(6.0))
                        .rounded_sm()
                        .bg(cx.theme().border)
                        .overflow_hidden()
                        .child(
                            div()
                                .w(px(FOOTER_BAR_WIDTH * pct / 100.0))
                                .h_full()
                                .rounded_sm()
                                .bg(bar_color),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(pct_text),
                ),
        );

        let dot_system: Hsla = rgb(COLOR_SYSTEM_PROMPT).into();
        let dot_tools: Hsla = rgb(COLOR_TOOL_DEFS).into();
        let dot_messages: Hsla = rgb(COLOR_MESSAGES).into();

        div().id("token-context-bar").child(
            Popover::new("token-context-popover")
                .trigger(trigger)
                .appearance(false)
                .content(move |_, _window, cx| {
                    let fg = cx.theme().foreground;
                    let muted = cx.theme().muted_foreground;
                    let bg = cx.theme().background;
                    let border = cx.theme().border;

                    div()
                        .flex()
                        .flex_col()
                        .bg(bg)
                        .border_1()
                        .border_color(border)
                        .rounded_md()
                        .shadow_md()
                        .p_2()
                        .min_w(px(POPOVER_MIN_WIDTH))
                        .max_w(px(POPOVER_MAX_WIDTH))
                        // Header
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::BOLD)
                                .text_color(fg)
                                .pb_1()
                                .child("Context Window"),
                        )
                        // Summary line
                        .child(
                            div()
                                .text_sm()
                                .text_color(fg)
                                .pb_1p5()
                                .child(summary_text.clone()),
                        )
                        // Progress bar (wider, inside popover)
                        .child(
                            div()
                                .w_full()
                                .h(px(8.0))
                                .rounded_sm()
                                .bg(border)
                                .overflow_hidden()
                                .mb_2()
                                .child(
                                    div()
                                        .w(relative(pct / 100.0))
                                        .h_full()
                                        .rounded_sm()
                                        .bg(bar_color),
                                ),
                        )
                        // Separator
                        .child(div().h(px(1.0)).w_full().bg(border).mb_2())
                        // System section
                        .child(section_header("System", muted))
                        .child(breakdown_row(
                            "System Prompt",
                            &system_pct_text,
                            dot_system,
                            fg,
                            muted,
                        ))
                        .child(breakdown_row(
                            "Tool Definitions",
                            &tools_pct_text,
                            dot_tools,
                            fg,
                            muted,
                        ))
                        // Separator
                        .child(div().h(px(1.0)).w_full().bg(border).mt_1().mb_2())
                        // Conversation section
                        .child(section_header("Conversation", muted))
                        .child(breakdown_row(
                            "Messages",
                            &messages_pct_text,
                            dot_messages,
                            fg,
                            muted,
                        ))
                        // Separator
                        .child(div().h(px(1.0)).w_full().bg(border).mt_1().mb_2())
                        // Session section
                        .child(section_header("Session", muted))
                        .child(stat_row("Input Tokens", &input_text, fg, muted))
                        .child(stat_row("Output Tokens", &output_text, fg, muted))
                        .when(has_cost, |this| {
                            this.child(stat_row("Cost", &cost_text, fg, muted))
                        })
                }),
        )
    }
}

fn section_header(label: &str, muted: Hsla) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(muted)
        .pb_1()
        .child(label.to_string())
}

fn breakdown_row(label: &str, pct_text: &str, dot_color: Hsla, fg: Hsla, muted: Hsla) -> Div {
    info_row(label, pct_text, Some(dot_color), fg, muted)
}

fn stat_row(label: &str, value: &str, fg: Hsla, muted: Hsla) -> Div {
    info_row(label, value, None, fg, muted)
}

fn info_row(label: &str, value: &str, dot: Option<Hsla>, fg: Hsla, muted: Hsla) -> Div {
    let label_content = h_flex()
        .gap_1p5()
        .items_center()
        .when_some(dot, |this, c| {
            this.child(div().w(px(8.0)).h(px(8.0)).rounded_sm().bg(c))
        })
        .child(div().text_sm().text_color(fg).child(label.to_string()));

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_1()
        .py_0p5()
        .child(label_content)
        .child(div().text_sm().text_color(muted).child(value.to_string()))
}
