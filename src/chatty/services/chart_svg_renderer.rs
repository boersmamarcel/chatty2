//! Generates SVG representations of chart data for PNG export.
//!
//! This module produces standalone SVG strings from `ChartSpec` data.
//! The SVGs are designed for clipboard export (via resvg → PNG), not for
//! in-app display (which uses gpui-component charts instead).

use crate::chatty::tools::chart_tool::ChartSpec;

/// Chart dimensions for SVG rendering
const SVG_WIDTH: f64 = 600.0;
const SVG_HEIGHT: f64 = 400.0;
const PADDING: f64 = 60.0;
const TITLE_HEIGHT: f64 = 30.0;

/// Default chart colors used when no theme colors are available (e.g. file export).
pub const DEFAULT_CHART_COLORS: [&str; 5] = ["#4e79a7", "#59a14f", "#f28e2b", "#e15759", "#76b7b2"];

#[cfg(test)]
const FALLBACK_CHART_COLORS: [&str; 5] = DEFAULT_CHART_COLORS;

// SVG fill colors for text elements (avoid raw string issues with `"#` sequences)
const FILL_TITLE: &str = "#333333";
const FILL_VALUE: &str = "#555555";
const FILL_LABEL: &str = "#666666";
const FILL_MUTED: &str = "#888888";
const STROKE_AXIS: &str = "#cccccc";
const STROKE_GRID: &str = "#eeeeee";
const BULLISH_COLOR: &str = "#22c55e";
const BEARISH_COLOR: &str = "#ef4444";

/// Generate an SVG string from a ChartSpec using the provided theme colors.
pub fn render_chart_svg(spec: &ChartSpec, colors: &[String; 5]) -> String {
    let colors: [&str; 5] = colors.each_ref().map(|s| s.as_str());
    match spec.chart_type.as_str() {
        "bar" => render_bar_svg(spec, &colors),
        "line" => render_line_svg(spec, &colors),
        "pie" => render_pie_svg(spec, false, &colors),
        "donut" => render_pie_svg(spec, true, &colors),
        "area" => render_area_svg(spec, &colors),
        "candlestick" => render_candlestick_svg(spec),
        _ => format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SVG_WIDTH}" height="{SVG_HEIGHT}">
  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="14" fill="{FILL_MUTED}">Unsupported chart type: {}</text>
</svg>"#,
            SVG_WIDTH / 2.0,
            SVG_HEIGHT / 2.0,
            spec.chart_type
        ),
    }
}

fn render_bar_svg(spec: &ChartSpec, colors: &[&str; 5]) -> String {
    let title_offset = if spec.title.is_some() {
        TITLE_HEIGHT
    } else {
        0.0
    };
    let chart_top = PADDING + title_offset;
    let chart_bottom = SVG_HEIGHT - PADDING;
    let chart_left = PADDING;
    let chart_right = SVG_WIDTH - PADDING;
    let chart_width = chart_right - chart_left;
    let chart_height = chart_bottom - chart_top;

    let max_val = spec
        .data
        .iter()
        .map(|d| d.value)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.0);
    let n = spec.data.len();
    if n == 0 || max_val == 0.0 {
        return empty_svg(spec);
    }

    let bar_gap = 8.0;
    let bar_width = (chart_width - bar_gap * (n as f64 + 1.0)) / n as f64;

    let mut elements = Vec::new();

    // Title
    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0,
            PADDING - 5.0 + title_offset / 2.0,
            escape_xml(title)
        ));
    }

    // Axis lines
    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));

    // Bars and labels
    for (i, d) in spec.data.iter().enumerate() {
        let x = chart_left + bar_gap + i as f64 * (bar_width + bar_gap);
        let bar_height = (d.value / max_val) * chart_height;
        let y = chart_bottom - bar_height;
        let color = colors[i % colors.len()];

        // Bar
        elements.push(format!(
            r#"  <rect x="{x}" y="{y}" width="{bar_width}" height="{bar_height}" fill="{color}" rx="2"/>"#,
        ));

        // Value label above bar
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_VALUE}">{}</text>"#,
            x + bar_width / 2.0,
            y - 5.0,
            format_value(d.value)
        ));

        // Category label below axis
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
            x + bar_width / 2.0,
            chart_bottom + 18.0,
            escape_xml(&d.label)
        ));
    }

    wrap_svg(&elements)
}

fn render_line_svg(spec: &ChartSpec, colors: &[&str; 5]) -> String {
    if let Some(series) = &spec.series {
        if series.len() > 1 {
            return render_multi_line_svg(spec, series, colors);
        }
    }

    // Single-series fallback: use spec.data (or the single series entry)
    let data: std::borrow::Cow<[crate::chatty::tools::chart_tool::ChartDataPoint]> =
        if let Some(series) = &spec.series {
            if let Some(s) = series.first() {
                std::borrow::Cow::Borrowed(&s.data)
            } else {
                std::borrow::Cow::Borrowed(&spec.data)
            }
        } else {
            std::borrow::Cow::Borrowed(&spec.data)
        };

    let title_offset = if spec.title.is_some() { TITLE_HEIGHT } else { 0.0 };
    let chart_top = PADDING + title_offset;
    let chart_bottom = SVG_HEIGHT - PADDING;
    let chart_left = PADDING;
    let chart_right = SVG_WIDTH - PADDING;
    let chart_width = chart_right - chart_left;
    let chart_height = chart_bottom - chart_top;

    let max_val = data.iter().map(|d| d.value).fold(f64::NEG_INFINITY, f64::max).max(0.0);
    let min_val = data.iter().map(|d| d.value).fold(f64::INFINITY, f64::min).min(0.0);
    let range = max_val - min_val;
    let n = data.len();
    if n == 0 || range == 0.0 {
        return empty_svg(spec);
    }

    let mut elements = Vec::new();

    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0, PADDING - 5.0 + title_offset / 2.0, escape_xml(title)
        ));
    }

    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));
    for i in 0..=4 {
        let y = chart_top + (i as f64 / 4.0) * chart_height;
        elements.push(format!(
            r#"  <line x1="{chart_left}" y1="{y}" x2="{chart_right}" y2="{y}" stroke="{STROKE_GRID}" stroke-width="1" stroke-dasharray="4,2"/>"#,
        ));
    }

    let points: Vec<(f64, f64)> = data.iter().enumerate().map(|(i, d)| {
        let x = if n == 1 { chart_left + chart_width / 2.0 } else { chart_left + (i as f64 / (n - 1) as f64) * chart_width };
        let y = chart_bottom - ((d.value - min_val) / range) * chart_height;
        (x, y)
    }).collect();

    let path_d: String = points.iter().enumerate().map(|(i, (x, y))| {
        if i == 0 { format!("M{x},{y}") } else { format!(" L{x},{y}") }
    }).collect();

    let color = colors[0];
    elements.push(format!(
        r#"  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/>"#,
    ));

    for (i, ((x, y), d)) in points.iter().zip(data.iter()).enumerate() {
        elements.push(format!(r#"  <circle cx="{x}" cy="{y}" r="4" fill="{color}" stroke="white" stroke-width="2"/>"#));
        elements.push(format!(
            r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
            chart_bottom + 18.0, escape_xml(&d.label)
        ));
        if i == 0 || i == n - 1 || n <= 6 || i % 3 == 0 {
            elements.push(format!(
                r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="10" fill="{FILL_VALUE}">{}</text>"#,
                y - 8.0, format_value(d.value)
            ));
        }
    }

    wrap_svg(&elements)
}

fn render_multi_line_svg(
    spec: &ChartSpec,
    series: &[crate::chatty::tools::chart_tool::SeriesData],
    colors: &[&str; 5],
) -> String {
    // Compute global min/max across all series
    let max_val = series
        .iter()
        .flat_map(|s| s.data.iter().map(|d| d.value))
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.0);
    let min_val = series
        .iter()
        .flat_map(|s| s.data.iter().map(|d| d.value))
        .fold(f64::INFINITY, f64::min)
        .min(0.0);
    let range = max_val - min_val;
    let n = series.first().map(|s| s.data.len()).unwrap_or(0);
    if n == 0 || range == 0.0 {
        return empty_svg(spec);
    }

    // Reserve space at bottom for legend
    let legend_height = 24.0 * ((series.len() as f64 / 3.0).ceil()).max(1.0);
    let title_offset = if spec.title.is_some() { TITLE_HEIGHT } else { 0.0 };
    let chart_top = PADDING + title_offset;
    let chart_bottom = SVG_HEIGHT - PADDING - legend_height;
    let chart_left = PADDING;
    let chart_right = SVG_WIDTH - PADDING;
    let chart_width = chart_right - chart_left;
    let chart_height = chart_bottom - chart_top;

    let mut elements = Vec::new();

    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0, PADDING - 5.0 + title_offset / 2.0, escape_xml(title)
        ));
    }

    // Axes and grid
    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));
    for i in 0..=4 {
        let y = chart_top + (i as f64 / 4.0) * chart_height;
        elements.push(format!(
            r#"  <line x1="{chart_left}" y1="{y}" x2="{chart_right}" y2="{y}" stroke="{STROKE_GRID}" stroke-width="1" stroke-dasharray="4,2"/>"#,
        ));
    }

    // Draw each series
    let x_fn = |i: usize| -> f64 {
        if n == 1 { chart_left + chart_width / 2.0 } else { chart_left + (i as f64 / (n - 1) as f64) * chart_width }
    };
    let y_fn = |v: f64| -> f64 { chart_bottom - ((v - min_val) / range) * chart_height };

    for (si, s) in series.iter().enumerate() {
        let color = colors[si % 5];
        let points: Vec<(f64, f64)> = s.data.iter().enumerate()
            .map(|(i, d)| (x_fn(i), y_fn(d.value)))
            .collect();
        let path_d: String = points.iter().enumerate()
            .map(|(i, (x, y))| if i == 0 { format!("M{x},{y}") } else { format!(" L{x},{y}") })
            .collect();
        elements.push(format!(
            r#"  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/>"#,
        ));
        for (x, y) in &points {
            elements.push(format!(r#"  <circle cx="{x}" cy="{y}" r="4" fill="{color}" stroke="white" stroke-width="1.5"/>"#));
        }
    }

    // X-axis labels from the first series
    if let Some(first) = series.first() {
        for (i, d) in first.data.iter().enumerate() {
            let x = x_fn(i);
            elements.push(format!(
                r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
                chart_bottom + 18.0, escape_xml(&d.label)
            ));
        }
    }

    // Legend at bottom
    let legend_y = SVG_HEIGHT - legend_height + 8.0;
    let items_per_row = 3usize;
    let col_width = (SVG_WIDTH - PADDING * 2.0) / items_per_row as f64;
    for (si, s) in series.iter().enumerate() {
        let col = si % items_per_row;
        let row = si / items_per_row;
        let lx = PADDING + col as f64 * col_width;
        let ly = legend_y + row as f64 * 20.0;
        let color = colors[si % 5];
        elements.push(format!(
            r#"  <rect x="{lx}" y="{}" width="12" height="12" rx="2" fill="{color}"/>"#,
            ly - 9.0
        ));
        elements.push(format!(
            r#"  <text x="{}" y="{ly}" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
            lx + 16.0, escape_xml(&s.name)
        ));
    }

    wrap_svg(&elements)
}

fn render_pie_svg(spec: &ChartSpec, is_donut: bool, colors: &[&str; 5]) -> String {
    let cx = SVG_WIDTH / 2.0;
    let cy = SVG_HEIGHT / 2.0 + if spec.title.is_some() { 15.0 } else { 0.0 };
    let outer_radius = 140.0;
    let inner_radius = if is_donut {
        spec.inner_radius.unwrap_or(50.0) as f64 * (outer_radius / 120.0)
    } else {
        0.0
    };
    let gap_angle = spec.pad_angle.unwrap_or(0.03) as f64;

    let total: f64 = spec.data.iter().map(|d| d.value.max(0.0)).sum();
    if total == 0.0 || spec.data.is_empty() {
        return empty_svg(spec);
    }

    let mut elements = Vec::new();

    // Title
    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0,
            35.0,
            escape_xml(title)
        ));
    }

    let mut start_angle: f64 = -std::f64::consts::FRAC_PI_2; // Start at top

    for (i, d) in spec.data.iter().enumerate() {
        let fraction = d.value.max(0.0) / total;
        let sweep = fraction * 2.0 * std::f64::consts::PI - gap_angle;
        if sweep <= 0.0 {
            start_angle += fraction * 2.0 * std::f64::consts::PI;
            continue;
        }

        let inner_start = start_angle + gap_angle / 2.0;
        let end_angle = inner_start + sweep;
        let large_arc = if sweep > std::f64::consts::PI { 1 } else { 0 };
        let color = colors[i % colors.len()];

        if is_donut && inner_radius > 0.0 {
            // Donut slice: outer arc + inner arc (reverse)
            let ox1 = cx + outer_radius * inner_start.cos();
            let oy1 = cy + outer_radius * inner_start.sin();
            let ox2 = cx + outer_radius * end_angle.cos();
            let oy2 = cy + outer_radius * end_angle.sin();
            let ix1 = cx + inner_radius * end_angle.cos();
            let iy1 = cy + inner_radius * end_angle.sin();
            let ix2 = cx + inner_radius * inner_start.cos();
            let iy2 = cy + inner_radius * inner_start.sin();
            elements.push(format!(
                r#"  <path d="M{ox1},{oy1} A{outer_radius},{outer_radius} 0 {large_arc},1 {ox2},{oy2} L{ix1},{iy1} A{inner_radius},{inner_radius} 0 {large_arc},0 {ix2},{iy2} Z" fill="{color}"/>"#,
            ));
        } else {
            // Full pie slice
            let x1 = cx + outer_radius * inner_start.cos();
            let y1 = cy + outer_radius * inner_start.sin();
            let x2 = cx + outer_radius * end_angle.cos();
            let y2 = cy + outer_radius * end_angle.sin();
            elements.push(format!(
                r#"  <path d="M{cx},{cy} L{x1},{y1} A{outer_radius},{outer_radius} 0 {large_arc},1 {x2},{y2} Z" fill="{color}"/>"#,
            ));
        }

        // Label at mid-angle, slightly outside
        let mid_angle = inner_start + sweep / 2.0;
        let label_r = outer_radius + 20.0;
        let lx = cx + label_r * mid_angle.cos();
        let ly = cy + label_r * mid_angle.sin();

        let pct = (fraction * 100.0).round() as u32;
        elements.push(format!(
            r#"  <text x="{lx}" y="{ly}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_VALUE}">{} ({pct}%)</text>"#,
            escape_xml(&d.label)
        ));

        start_angle += fraction * 2.0 * std::f64::consts::PI;
    }

    wrap_svg(&elements)
}

fn render_area_svg(spec: &ChartSpec, colors: &[&str; 5]) -> String {
    if let Some(series) = &spec.series {
        if series.len() > 1 {
            return render_multi_area_svg(spec, series, colors);
        }
    }

    // Single-series fallback
    let data: std::borrow::Cow<[crate::chatty::tools::chart_tool::ChartDataPoint]> =
        if let Some(series) = &spec.series {
            if let Some(s) = series.first() {
                std::borrow::Cow::Borrowed(&s.data)
            } else {
                std::borrow::Cow::Borrowed(&spec.data)
            }
        } else {
            std::borrow::Cow::Borrowed(&spec.data)
        };

    let title_offset = if spec.title.is_some() { TITLE_HEIGHT } else { 0.0 };
    let chart_top = PADDING + title_offset;
    let chart_bottom = SVG_HEIGHT - PADDING;
    let chart_left = PADDING;
    let chart_right = SVG_WIDTH - PADDING;
    let chart_width = chart_right - chart_left;
    let chart_height = chart_bottom - chart_top;

    let max_val = data.iter().map(|d| d.value).fold(f64::NEG_INFINITY, f64::max).max(0.0);
    let min_val = data.iter().map(|d| d.value).fold(f64::INFINITY, f64::min).min(0.0);
    let range = (max_val - min_val).max(1.0);
    let n = data.len();
    if n == 0 {
        return empty_svg(spec);
    }

    let mut elements = Vec::new();

    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0, PADDING - 5.0 + title_offset / 2.0, escape_xml(title)
        ));
    }

    for i in 0..=4 {
        let y = chart_top + (i as f64 / 4.0) * chart_height;
        elements.push(format!(
            r#"  <line x1="{chart_left}" y1="{y}" x2="{chart_right}" y2="{y}" stroke="{STROKE_GRID}" stroke-width="1" stroke-dasharray="4,2"/>"#,
        ));
    }

    let points: Vec<(f64, f64)> = data.iter().enumerate().map(|(i, d)| {
        let x = if n == 1 { chart_left + chart_width / 2.0 } else { chart_left + (i as f64 / (n - 1) as f64) * chart_width };
        let y = chart_bottom - ((d.value - min_val) / range) * chart_height;
        (x, y)
    }).collect();

    let color = colors[0];
    let fill_color = format!("{}40", color);

    let first_x = points[0].0;
    let last_x = points[points.len() - 1].0;
    let area_d: String = {
        let mut d = format!("M{first_x},{chart_bottom}");
        for (x, y) in &points { d.push_str(&format!(" L{x},{y}")); }
        d.push_str(&format!(" L{last_x},{chart_bottom} Z"));
        d
    };
    elements.push(format!(r#"  <path d="{area_d}" fill="{fill_color}"/>"#));

    let line_d: String = points.iter().enumerate()
        .map(|(i, (x, y))| if i == 0 { format!("M{x},{y}") } else { format!(" L{x},{y}") })
        .collect();
    elements.push(format!(
        r#"  <path d="{line_d}" fill="none" stroke="{color}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/>"#,
    ));

    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));

    for (i, ((x, y), d)) in points.iter().zip(data.iter()).enumerate() {
        elements.push(format!(
            r#"  <circle cx="{x}" cy="{y}" r="3" fill="{color}" stroke="white" stroke-width="1.5"/>"#,
        ));
        if i == 0 || i == n - 1 || n <= 8 || i % (n / 6 + 1) == 0 {
            elements.push(format!(
                r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
                chart_bottom + 18.0, escape_xml(&d.label)
            ));
        }
    }

    wrap_svg(&elements)
}

fn render_multi_area_svg(
    spec: &ChartSpec,
    series: &[crate::chatty::tools::chart_tool::SeriesData],
    colors: &[&str; 5],
) -> String {
    let max_val = series.iter().flat_map(|s| s.data.iter().map(|d| d.value)).fold(f64::NEG_INFINITY, f64::max).max(0.0);
    let min_val = series.iter().flat_map(|s| s.data.iter().map(|d| d.value)).fold(f64::INFINITY, f64::min).min(0.0);
    let range = (max_val - min_val).max(1.0);
    let n = series.first().map(|s| s.data.len()).unwrap_or(0);
    if n == 0 { return empty_svg(spec); }

    let legend_height = 24.0 * ((series.len() as f64 / 3.0).ceil()).max(1.0);
    let title_offset = if spec.title.is_some() { TITLE_HEIGHT } else { 0.0 };
    let chart_top = PADDING + title_offset;
    let chart_bottom = SVG_HEIGHT - PADDING - legend_height;
    let chart_left = PADDING;
    let chart_right = SVG_WIDTH - PADDING;
    let chart_width = chart_right - chart_left;
    let chart_height = chart_bottom - chart_top;

    let mut elements = Vec::new();

    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0, PADDING - 5.0 + title_offset / 2.0, escape_xml(title)
        ));
    }

    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));
    for i in 0..=4 {
        let y = chart_top + (i as f64 / 4.0) * chart_height;
        elements.push(format!(
            r#"  <line x1="{chart_left}" y1="{y}" x2="{chart_right}" y2="{y}" stroke="{STROKE_GRID}" stroke-width="1" stroke-dasharray="4,2"/>"#,
        ));
    }

    let x_fn = |i: usize| -> f64 {
        if n == 1 { chart_left + chart_width / 2.0 } else { chart_left + (i as f64 / (n - 1) as f64) * chart_width }
    };
    let y_fn = |v: f64| -> f64 { chart_bottom - ((v - min_val) / range) * chart_height };

    // Draw fills first (back to front), then lines
    for (si, s) in series.iter().enumerate() {
        let color = colors[si % 5];
        let points: Vec<(f64, f64)> = s.data.iter().enumerate().map(|(i, d)| (x_fn(i), y_fn(d.value))).collect();
        let first_x = points[0].0;
        let last_x = points[points.len() - 1].0;
        let area_d: String = {
            let mut d = format!("M{first_x},{chart_bottom}");
            for (x, y) in &points { d.push_str(&format!(" L{x},{y}")); }
            d.push_str(&format!(" L{last_x},{chart_bottom} Z"));
            d
        };
        elements.push(format!(r#"  <path d="{area_d}" fill="{color}" fill-opacity="0.15"/>"#));
    }

    for (si, s) in series.iter().enumerate() {
        let color = colors[si % 5];
        let points: Vec<(f64, f64)> = s.data.iter().enumerate().map(|(i, d)| (x_fn(i), y_fn(d.value))).collect();
        let path_d: String = points.iter().enumerate()
            .map(|(i, (x, y))| if i == 0 { format!("M{x},{y}") } else { format!(" L{x},{y}") })
            .collect();
        elements.push(format!(
            r#"  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/>"#,
        ));
        for (x, y) in &points {
            elements.push(format!(r#"  <circle cx="{x}" cy="{y}" r="3" fill="{color}" stroke="white" stroke-width="1.5"/>"#));
        }
    }

    if let Some(first) = series.first() {
        for (i, d) in first.data.iter().enumerate() {
            let x = x_fn(i);
            elements.push(format!(
                r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
                chart_bottom + 18.0, escape_xml(&d.label)
            ));
        }
    }

    let legend_y = SVG_HEIGHT - legend_height + 8.0;
    let items_per_row = 3usize;
    let col_width = (SVG_WIDTH - PADDING * 2.0) / items_per_row as f64;
    for (si, s) in series.iter().enumerate() {
        let col = si % items_per_row;
        let row = si / items_per_row;
        let lx = PADDING + col as f64 * col_width;
        let ly = legend_y + row as f64 * 20.0;
        let color = colors[si % 5];
        elements.push(format!(r#"  <rect x="{lx}" y="{}" width="12" height="12" rx="2" fill="{color}"/>"#, ly - 9.0));
        elements.push(format!(
            r#"  <text x="{}" y="{ly}" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
            lx + 16.0, escape_xml(&s.name)
        ));
    }

    wrap_svg(&elements)
}

fn render_candlestick_svg(spec: &ChartSpec) -> String {
    let Some(cs_data) = &spec.candlestick_data else {
        return empty_svg(spec);
    };
    if cs_data.is_empty() {
        return empty_svg(spec);
    }

    let title_offset = if spec.title.is_some() {
        TITLE_HEIGHT
    } else {
        0.0
    };
    let chart_top = PADDING + title_offset;
    let chart_bottom = SVG_HEIGHT - PADDING;
    let chart_left = PADDING;
    let chart_right = SVG_WIDTH - PADDING;
    let chart_width = chart_right - chart_left;
    let chart_height = chart_bottom - chart_top;

    let all_prices: Vec<f64> = cs_data
        .iter()
        .flat_map(|d| [d.open, d.high, d.low, d.close])
        .collect();
    let max_val = all_prices.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min_val = all_prices.iter().cloned().fold(f64::INFINITY, f64::min);
    let range = (max_val - min_val).max(1.0);

    let n = cs_data.len();
    let candle_total_width = chart_width / n as f64;
    let candle_body_width = (candle_total_width * 0.6).max(2.0);
    let wick_width = 1.5_f64;

    let price_to_y =
        |price: f64| -> f64 { chart_bottom - ((price - min_val) / range) * chart_height };

    let mut elements = Vec::new();

    // Title
    if let Some(title) = &spec.title {
        elements.push(format!(
            r#"  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="bold" fill="{FILL_TITLE}">{}</text>"#,
            SVG_WIDTH / 2.0,
            PADDING - 5.0 + title_offset / 2.0,
            escape_xml(title)
        ));
    }

    // Grid lines
    for i in 0..=4 {
        let y = chart_top + (i as f64 / 4.0) * chart_height;
        elements.push(format!(
            r#"  <line x1="{chart_left}" y1="{y}" x2="{chart_right}" y2="{y}" stroke="{STROKE_GRID}" stroke-width="1" stroke-dasharray="4,2"/>"#,
        ));
    }

    // Axis
    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));

    // Candles
    for (i, d) in cs_data.iter().enumerate() {
        let center_x = chart_left + (i as f64 + 0.5) * candle_total_width;
        let is_bullish = d.close >= d.open;
        let color = if is_bullish {
            BULLISH_COLOR
        } else {
            BEARISH_COLOR
        };

        let body_top = price_to_y(d.open.max(d.close));
        let body_bottom = price_to_y(d.open.min(d.close));
        let body_height = (body_bottom - body_top).max(1.0);
        let body_x = center_x - candle_body_width / 2.0;

        // High wick
        let high_y = price_to_y(d.high);
        elements.push(format!(
            r#"  <line x1="{center_x}" y1="{high_y}" x2="{center_x}" y2="{body_top}" stroke="{color}" stroke-width="{wick_width}"/>"#,
        ));

        // Low wick
        let low_y = price_to_y(d.low);
        elements.push(format!(
            r#"  <line x1="{center_x}" y1="{body_bottom}" x2="{center_x}" y2="{low_y}" stroke="{color}" stroke-width="{wick_width}"/>"#,
        ));

        // Body
        elements.push(format!(
            r#"  <rect x="{body_x}" y="{body_top}" width="{candle_body_width}" height="{body_height}" fill="{color}" rx="1"/>"#,
        ));

        // X label (sparse to avoid overlap)
        let show_label = n <= 10 || i == 0 || i == n - 1 || i % (n / 5).max(1) == 0;
        if show_label {
            elements.push(format!(
                r#"  <text x="{center_x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="10" fill="{FILL_LABEL}">{}</text>"#,
                chart_bottom + 16.0,
                escape_xml(&d.date)
            ));
        }
    }

    wrap_svg(&elements)
}

fn wrap_svg(elements: &[String]) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SVG_WIDTH}" height="{SVG_HEIGHT}" viewBox="0 0 {SVG_WIDTH} {SVG_HEIGHT}">
  <rect width="100%" height="100%" fill="white"/>
{}
</svg>"#,
        elements.join("\n")
    )
}

fn empty_svg(spec: &ChartSpec) -> String {
    let title = spec.title.as_deref().unwrap_or("Chart");
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SVG_WIDTH}" height="{SVG_HEIGHT}">
  <rect width="100%" height="100%" fill="white"/>
  <text x="{}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="14" fill="{FILL_MUTED}">{} — no data</text>
</svg>"#,
        SVG_WIDTH / 2.0,
        SVG_HEIGHT / 2.0,
        escape_xml(title)
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn format_value(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{:.1}", v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::tools::chart_tool::{CandlestickDataPoint, ChartDataPoint};

    fn make_spec(chart_type: &str, data: Vec<ChartDataPoint>) -> ChartSpec {
        ChartSpec {
            chart_type: chart_type.to_string(),
            title: None,
            data,
            series: None,
            candlestick_data: None,
            inner_radius: None,
            pad_angle: None,
        }
    }

    #[test]
    fn bar_svg_contains_rects() {
        let spec = ChartSpec {
            title: Some("Test".to_string()),
            ..make_spec(
                "bar",
                vec![
                    ChartDataPoint {
                        label: "A".into(),
                        value: 10.0,
                    },
                    ChartDataPoint {
                        label: "B".into(),
                        value: 20.0,
                    },
                ],
            )
        };
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<rect"));
        assert!(svg.contains("Test"));
        assert!(svg.contains("A"));
        assert!(svg.contains("B"));
    }

    #[test]
    fn line_svg_contains_path() {
        let spec = make_spec(
            "line",
            vec![
                ChartDataPoint {
                    label: "Jan".into(),
                    value: 5.0,
                },
                ChartDataPoint {
                    label: "Feb".into(),
                    value: 15.0,
                },
            ],
        );
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<path"));
        assert!(svg.contains("<circle"));
    }

    #[test]
    fn pie_svg_contains_arcs() {
        let spec = ChartSpec {
            title: Some("Share".to_string()),
            ..make_spec(
                "pie",
                vec![
                    ChartDataPoint {
                        label: "X".into(),
                        value: 60.0,
                    },
                    ChartDataPoint {
                        label: "Y".into(),
                        value: 40.0,
                    },
                ],
            )
        };
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<path"));
        assert!(svg.contains("60%"));
        assert!(svg.contains("40%"));
    }

    #[test]
    fn donut_svg_contains_arcs() {
        let spec = ChartSpec {
            inner_radius: Some(50.0),
            ..make_spec(
                "donut",
                vec![
                    ChartDataPoint {
                        label: "A".into(),
                        value: 70.0,
                    },
                    ChartDataPoint {
                        label: "B".into(),
                        value: 30.0,
                    },
                ],
            )
        };
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<path"));
        assert!(svg.contains("A"));
    }

    #[test]
    fn area_svg_contains_filled_path() {
        let spec = make_spec(
            "area",
            vec![
                ChartDataPoint {
                    label: "Mon".into(),
                    value: 100.0,
                },
                ChartDataPoint {
                    label: "Tue".into(),
                    value: 200.0,
                },
            ],
        );
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<path"));
        // Filled area uses semi-transparent fill
        assert!(svg.contains("fill"));
    }

    #[test]
    fn candlestick_svg_contains_candles() {
        let spec = ChartSpec {
            candlestick_data: Some(vec![CandlestickDataPoint {
                date: "2024-01".into(),
                open: 100.0,
                high: 120.0,
                low: 90.0,
                close: 110.0,
            }]),
            ..make_spec("candlestick", vec![])
        };
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<rect"));
        assert!(svg.contains("<line"));
        assert!(svg.contains("2024-01"));
    }

    #[test]
    fn empty_data_produces_valid_svg() {
        let spec = make_spec("bar", vec![]);
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn xml_escaping_works() {
        let spec = ChartSpec {
            title: Some("A & B <test>".to_string()),
            ..make_spec(
                "bar",
                vec![ChartDataPoint {
                    label: "x\"y".into(),
                    value: 10.0,
                }],
            )
        };
        let svg = render_chart_svg(&spec, &FALLBACK_CHART_COLORS.map(str::to_owned));
        assert!(svg.contains("A &amp; B &lt;test&gt;"));
        assert!(svg.contains("x&quot;y"));
    }
}
