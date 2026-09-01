//! Inline table preview cards and artifact-panel table grid.

use chatty_core::tools::data_query_tool::{
    INLINE_TABLE_MAX_HEIGHT_PX, INLINE_TABLE_PREVIEW_ROWS, TablePreview, TableSource,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::OpenTable;
use chatty_core::models::message_types::ToolCallBlock;
use chatty_core::models::message_types::ToolCallState;
use chatty_core::tools::data_query_tool::QueryDataOutput;

/// Extract structured table preview from a successful `query_data` tool output.
pub fn extract_table_preview(tool_call: &ToolCallBlock) -> Option<TablePreview> {
    if tool_call.tool_name != "query_data" {
        return None;
    }
    if !matches!(tool_call.state, ToolCallState::Success) {
        return None;
    }
    let output = tool_call
        .output
        .as_deref()
        .or(tool_call.output_preview.as_deref())?;
    if let Ok(out) = serde_json::from_str::<QueryDataOutput>(output) {
        return Some(out.preview);
    }
    let value: serde_json::Value = serde_json::from_str(output).ok()?;
    value
        .get("preview")
        .and_then(|p| serde_json::from_value(p.clone()).ok())
}

/// Fixed height estimate for one inline table card (chrome + capped preview grid).
pub fn inline_table_card_height(preview: &TablePreview) -> f32 {
    const CHROME: f32 = 76.0;
    const HEADER: f32 = 28.0;
    const ROW: f32 = 24.0;
    let visible_rows = preview.rows.len().clamp(1, INLINE_TABLE_PREVIEW_ROWS);
    (CHROME + HEADER + visible_rows as f32 * ROW).min(CHROME + INLINE_TABLE_MAX_HEIGHT_PX)
}

fn source_hint(preview: &TablePreview) -> String {
    match &preview.source {
        TableSource::Query { .. } => "SQL query".into(),
        TableSource::File { path } => path.clone(),
    }
}

fn render_table_grid(
    id_prefix: &str,
    preview: &TablePreview,
    max_height_px: f32,
    cx: &App,
) -> impl IntoElement {
    let col_count = preview.columns.len().max(1);
    let min_col_w = px(88.0);
    let border = cx.theme().border;
    let muted = cx.theme().muted_foreground;
    let header_bg = cx.theme().secondary;

    div()
        .id(ElementId::Name(format!("{id_prefix}-grid").into()))
        .w_full()
        .max_h(px(max_height_px))
        .overflow_x_scroll()
        .overflow_y_scroll()
        .rounded_md()
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(min_col_w * col_count as f32)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .bg(header_bg)
                        .border_b_1()
                        .border_color(border)
                        .children(preview.columns.iter().enumerate().map(|(ci, col)| {
                            div()
                                .id(ElementId::Name(format!("{id_prefix}-th-{ci}").into()))
                                .flex_1()
                                .min_w(min_col_w)
                                .px_2()
                                .py_1()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(muted)
                                .child(col.name.clone())
                        })),
                )
                .children(preview.rows.iter().enumerate().map(|(ri, row)| {
                    div()
                        .id(ElementId::Name(format!("{id_prefix}-tr-{ri}").into()))
                        .flex()
                        .flex_row()
                        .when(ri % 2 == 1, |this| this.bg(header_bg.opacity(0.35)))
                        .border_b_1()
                        .border_color(border)
                        .children(row.iter().enumerate().map(|(ci, cell)| {
                            div()
                                .id(ElementId::Name(format!("{id_prefix}-td-{ri}-{ci}").into()))
                                .flex_1()
                                .min_w(min_col_w)
                                .px_2()
                                .py_1()
                                .text_xs()
                                .font_family("monospace")
                                .overflow_hidden()
                                .child(cell.clone())
                        }))
                })),
        )
}

/// Compact inline receipt after a successful `query_data` call.
pub fn render_table_preview_card(
    preview: TablePreview,
    msg_idx: usize,
    tool_idx: usize,
    on_open: Option<OpenTable>,
    cx: &App,
) -> impl IntoElement {
    let summary = preview.summary_label();
    let truncated = preview.truncated;
    let title = preview.title.clone();
    let hint = source_hint(&preview);
    let open_preview = preview.clone();

    div()
        .id(ElementId::Name(
            format!("table-card-{msg_idx}-{tool_idx}").into(),
        ))
        .w_full()
        .mt_2()
        .mb_2()
        .rounded_xl()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::LayoutDashboard).size_4())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{summary} · {hint}")),
                        ),
                )
                .when(truncated, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(cx.theme().secondary)
                            .text_color(cx.theme().muted_foreground)
                            .child("Truncated"),
                    )
                })
                .child(
                    Button::new(ElementId::Name(
                        format!("table-open-{msg_idx}-{tool_idx}").into(),
                    ))
                    .ghost()
                    .small()
                    .label("Open")
                    .when_some(on_open.clone(), |this, cb| {
                        let preview = open_preview.clone();
                        this.on_click(move |_, _, cx| {
                            cb(
                                super::TableOpen {
                                    preview: preview.clone(),
                                },
                                cx,
                            );
                        })
                    }),
                ),
        )
        .child(div().px_3().pb_2().child(render_table_grid(
            &format!("table-inline-{msg_idx}-{tool_idx}"),
            &preview,
            INLINE_TABLE_MAX_HEIGHT_PX,
            cx,
        )))
}

/// Full-width table body for the artifact panel.
pub fn render_table_preview_view(
    id_prefix: &str,
    preview: &TablePreview,
    cx: &App,
) -> impl IntoElement {
    let summary = preview.summary_label();
    let hint = source_hint(preview);
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{summary} · {hint}")),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(render_table_grid(id_prefix, preview, 480.0, cx)),
        )
}
