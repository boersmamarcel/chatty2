use crate::assets::CustomIcon;
use crate::chatty::services::MermaidRendererService;
use crate::chatty::services::chart_svg_renderer;
use crate::chatty::tools::chart_tool::{CandlestickDataPoint, ChartSpec, SeriesData};
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::chart::{AreaChart, BarChart, CandlestickChart, LineChart, PieChart};
use gpui_component::{Icon, Sizable};

use super::message_types::ToolCallBlock;

/// Extract chart specification from a `create_chart` tool call output JSON.
pub(super) fn extract_chart_spec(tool_call: &ToolCallBlock) -> Option<ChartSpec> {
    let output = tool_call.output.as_ref()?;
    serde_json::from_str(output).ok()
}

/// A chart data point with a pre-assigned color index for themed rendering.
struct IndexedDataPoint {
    label: String,
    value: f64,
    color_index: usize,
}

/// A multi-series data point: one label with one value per series (aligned by index).
struct MultiSeriesPoint {
    label: String,
    values: Vec<f64>,
}

/// Build a "Copy as PNG" button for a chart.
///
/// Generates an SVG from the chart spec, converts to PNG via resvg,
/// and copies the result to the system clipboard.
fn build_chart_copy_png_button(
    spec: &ChartSpec,
    msg_idx: usize,
    tool_idx: usize,
    colors: [String; 5],
) -> Button {
    let spec_clone = spec.clone();
    Button::new(ElementId::Name(
        format!("copy-chart-png-{msg_idx}-{tool_idx}").into(),
    ))
    .ghost()
    .xsmall()
    .icon(Icon::new(CustomIcon::Image))
    .tooltip("Copy as PNG")
    .on_click(move |_event, _window, _cx| {
        let svg_str = chart_svg_renderer::render_chart_svg(&spec_clone, &colors);

        // Write SVG to a temp file so render_svg_to_png can read it
        let tmp_path = std::env::temp_dir().join(format!("chatty_chart_{msg_idx}_{tool_idx}.svg"));
        if let Err(e) = std::fs::write(&tmp_path, &svg_str) {
            tracing::warn!(error = ?e, "Failed to write chart SVG to temp file");
            return;
        }

        match MermaidRendererService::render_svg_to_png(&tmp_path) {
            Ok(png_bytes) => {
                #[cfg(target_os = "linux")]
                {
                    if !super::mermaid_component::copy_png_to_linux_clipboard(&png_bytes) {
                        tracing::warn!("No clipboard tool found (install wl-clipboard or xclip)");
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, png_bytes);
                    _cx.write_to_clipboard(ClipboardItem::new_image(&image));
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to render chart PNG for clipboard");
            }
        }

        // Clean up temp file
        let _ = std::fs::remove_file(&tmp_path);
    })
}

/// Build a `Vec<MultiSeriesPoint>` aligned by index across all series.
/// All series are assumed to share the same x-axis labels in the same order.
fn build_multi_series_points(series: &[SeriesData]) -> Vec<MultiSeriesPoint> {
    if series.is_empty() {
        return vec![];
    }
    let n = series[0].data.len();
    (0..n)
        .map(|i| MultiSeriesPoint {
            label: series[0].data[i].label.clone(),
            values: series
                .iter()
                .map(|s| s.data.get(i).map(|d| d.value).unwrap_or(0.0))
                .collect(),
        })
        .collect()
}

/// Render a colored legend row for multi-series charts.
/// `series_meta` is a list of (name, color_index) pairs.
fn render_series_legend(
    series_meta: &[(String, usize)],
    chart_colors: &[gpui::Hsla; 5],
    cx: &App,
) -> impl gpui::IntoElement {
    div()
        .flex()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .mt_2()
        .pt_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .children(series_meta.iter().map(|(name, color_idx)| {
            let color = chart_colors[color_idx % 5];
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().w(px(10.0)).h(px(10.0)).rounded_sm().bg(color))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(name.clone()),
                )
        }))
}

/// Render a chart inline from the validated ChartSpec.
pub(super) fn render_chart(
    spec: ChartSpec,
    msg_idx: usize,
    tool_idx: usize,
    cx: &App,
) -> Stateful<Div> {
    let element_id = ElementId::Name(format!("chart-{msg_idx}-{tool_idx}").into());
    let border_color = cx.theme().border;

    // Extract theme colors as hex strings for the SVG renderer
    let theme_chart_colors = {
        let colors = [
            cx.theme().chart_1,
            cx.theme().chart_2,
            cx.theme().chart_3,
            cx.theme().chart_4,
            cx.theme().chart_5,
        ];
        colors.map(|c| {
            let r = c.to_rgb();
            format!(
                "#{:02x}{:02x}{:02x}",
                (r.r * 255.0) as u8,
                (r.g * 255.0) as u8,
                (r.b * 255.0) as u8
            )
        })
    };

    // Build copy button before we move spec.data into indexed_data
    let copy_png_button =
        build_chart_copy_png_button(&spec, msg_idx, tool_idx, theme_chart_colors);

    let mut chart_container = div()
        .id(element_id)
        .relative()
        .w_full()
        .max_w(px(600.0))
        .rounded_lg()
        .border_1()
        .border_color(border_color)
        .bg(cx.theme().background)
        .p_4()
        .mt_2()
        .mb_2();

    // Optional title
    if let Some(title) = &spec.title {
        chart_container = chart_container.child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .mb_2()
                .child(title.clone()),
        );
    }

    // Pre-build indexed data with color assignments
    let indexed_data: Vec<IndexedDataPoint> = spec
        .data
        .into_iter()
        .enumerate()
        .map(|(i, d)| IndexedDataPoint {
            label: d.label,
            value: d.value,
            color_index: i,
        })
        .collect();

    // Capture all 5 theme chart colors for closures
    let chart_colors = [
        cx.theme().chart_1,
        cx.theme().chart_2,
        cx.theme().chart_3,
        cx.theme().chart_4,
        cx.theme().chart_5,
    ];

    match spec.chart_type.as_str() {
        "bar" => {
            chart_container = chart_container.child(
                div().h(px(250.0)).child(
                    BarChart::new(indexed_data)
                        .x(|d| SharedString::from(d.label.clone()))
                        .y(|d| d.value)
                        .fill(move |d| chart_colors[d.color_index % 5])
                        .label(|d| {
                            SharedString::from(if d.value.fract() == 0.0 {
                                format!("{}", d.value as i64)
                            } else {
                                format!("{:.1}", d.value)
                            })
                        }),
                ),
            );
        }
        "line" => {
            let line_series = spec.series;
            let is_multi = line_series.as_ref().is_some_and(|s| s.len() > 1);
            if is_multi {
                // Multi-series: use AreaChart with transparent fill (renders as pure lines)
                let series = line_series.unwrap();
                let series_meta: Vec<(String, usize)> = series
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.name.clone(), i))
                    .collect();
                let multi_points = build_multi_series_points(&series);
                let num_series = series_meta.len();
                let mut area_chart = AreaChart::new(multi_points)
                    .x(|d: &MultiSeriesPoint| SharedString::from(d.label.clone()))
                    .natural();
                for i in 0..num_series {
                    let color = chart_colors[i % 5];
                    area_chart = area_chart
                        .y(move |d: &MultiSeriesPoint| d.values[i])
                        .stroke(color)
                        .fill(color.opacity(0.0)); // transparent — line chart look
                }
                chart_container = chart_container
                    .child(div().h(px(250.0)).child(area_chart))
                    .child(render_series_legend(&series_meta, &chart_colors, cx));
            } else {
                // Single series: use LineChart (from spec.data or the single series entry)
                let source_data = if let Some(mut s) = line_series {
                    s.pop()
                        .map(|sd| {
                            sd.data
                                .into_iter()
                                .enumerate()
                                .map(|(i, d)| IndexedDataPoint {
                                    label: d.label,
                                    value: d.value,
                                    color_index: i,
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or(indexed_data)
                } else {
                    indexed_data
                };
                let stroke_color = chart_colors[0];
                let value_labels: Vec<(String, f64)> = source_data
                    .iter()
                    .map(|d| (d.label.clone(), d.value))
                    .collect();
                chart_container = chart_container
                    .child(
                        div().h(px(250.0)).child(
                            LineChart::new(source_data)
                                .x(|d| SharedString::from(d.label.clone()))
                                .y(|d| d.value)
                                .stroke(stroke_color)
                                .natural()
                                .dot(),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x_4()
                            .gap_y_1()
                            .mt_2()
                            .pt_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .children(value_labels.into_iter().map(|(label, value)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(stroke_color)
                                            .child(if value.fract() == 0.0 {
                                                format!("{}", value as i64)
                                            } else {
                                                format!("{:.1}", value)
                                            }),
                                    )
                            })),
                    );
            }
        }
        "pie" | "donut" => {
            let is_donut = spec.chart_type == "donut";
            let pad_angle = spec.pad_angle.unwrap_or(0.03);
            let total: f64 = indexed_data.iter().map(|d| d.value).sum();
            // Build legend data before moving indexed_data into the chart
            let legend: Vec<(String, f64, usize)> = indexed_data
                .iter()
                .map(|d| (d.label.clone(), d.value, d.color_index))
                .collect();

            let mut pie = PieChart::new(indexed_data)
                .value(|d| d.value as f32)
                .color(move |d| chart_colors[d.color_index % 5])
                // Workaround: gpui-component PieChart bug passes Some(0.0) for
                // outer_radius per-arc, overriding the bounds-computed value.
                // Explicitly set outer_radius = div_height * 0.4 = 300 * 0.4.
                .outer_radius(120.0)
                .pad_angle(pad_angle);

            if is_donut {
                pie = pie.inner_radius(spec.inner_radius.unwrap_or(50.0));
            }

            chart_container = chart_container.child(div().h(px(300.0)).child(pie)).child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_1()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .children(legend.into_iter().map(|(label, value, color_idx)| {
                        let color = chart_colors[color_idx % 5];
                        let pct = if total > 0.0 {
                            format!(" ({:.1}%)", value / total * 100.0)
                        } else {
                            String::new()
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().w(px(10.0)).h(px(10.0)).rounded_sm().bg(color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(format!(
                                        "{}{}",
                                        if value.fract() == 0.0 {
                                            format!("{}", value as i64)
                                        } else {
                                            format!("{:.1}", value)
                                        },
                                        pct
                                    )),
                            )
                    })),
            );
        }
        "area" => {
            let area_series = spec.series;
            let is_multi = area_series.as_ref().is_some_and(|s| s.len() > 1);
            if is_multi {
                // Multi-series area chart
                let series = area_series.unwrap();
                let series_meta: Vec<(String, usize)> = series
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.name.clone(), i))
                    .collect();
                let multi_points = build_multi_series_points(&series);
                let num_series = series_meta.len();
                let mut area_chart = AreaChart::new(multi_points)
                    .x(|d: &MultiSeriesPoint| SharedString::from(d.label.clone()))
                    .natural();
                for i in 0..num_series {
                    let color = chart_colors[i % 5];
                    area_chart = area_chart
                        .y(move |d: &MultiSeriesPoint| d.values[i])
                        .stroke(color)
                        .fill(color.opacity(0.2)); // subtle fill per series
                }
                chart_container = chart_container
                    .child(div().h(px(250.0)).child(area_chart))
                    .child(render_series_legend(&series_meta, &chart_colors, cx));
            } else {
                let stroke_color = chart_colors[0];
                let value_labels: Vec<(String, f64)> = indexed_data
                    .iter()
                    .map(|d| (d.label.clone(), d.value))
                    .collect();
                chart_container = chart_container
                    .child(
                        div().h(px(250.0)).child(
                            AreaChart::new(indexed_data)
                                .x(|d| SharedString::from(d.label.clone()))
                                .y(|d| d.value)
                                .stroke(stroke_color)
                                .natural(),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x_4()
                            .gap_y_1()
                            .mt_2()
                            .pt_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .children(value_labels.into_iter().map(|(label, value)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(stroke_color)
                                            .child(if value.fract() == 0.0 {
                                                format!("{}", value as i64)
                                            } else {
                                                format!("{:.1}", value)
                                            }),
                                    )
                            })),
                    );
            }
        }
        "candlestick" => {
            if let Some(cs_data) = spec.candlestick_data {
                chart_container = chart_container.child(
                    div().h(px(280.0)).child(
                        CandlestickChart::new(cs_data)
                            .x(|d: &CandlestickDataPoint| SharedString::from(d.date.clone()))
                            .open(|d: &CandlestickDataPoint| d.open)
                            .high(|d: &CandlestickDataPoint| d.high)
                            .low(|d: &CandlestickDataPoint| d.low)
                            .close(|d: &CandlestickDataPoint| d.close),
                    ),
                );
            } else {
                chart_container = chart_container.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No candlestick data provided"),
                );
            }
        }
        _ => {
            chart_container = chart_container.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Unsupported chart type: {}", spec.chart_type)),
            );
        }
    }

    // Copy as PNG button overlay (top-right corner)
    chart_container =
        chart_container.child(div().absolute().top_1().right_1().child(copy_png_button));

    chart_container
}
