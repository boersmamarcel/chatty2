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

/// 5 chart colors matching gpui-component's default theme palette.
/// These are static hex values used for SVG export only (not themed).
const CHART_COLORS: [&str; 5] = ["#4e79a7", "#59a14f", "#f28e2b", "#e15759", "#76b7b2"];

// SVG fill colors for text elements (avoid raw string issues with `"#` sequences)
const FILL_TITLE: &str = "#333333";
const FILL_VALUE: &str = "#555555";
const FILL_LABEL: &str = "#666666";
const FILL_MUTED: &str = "#888888";
const STROKE_AXIS: &str = "#cccccc";
const STROKE_GRID: &str = "#eeeeee";

/// Generate an SVG string from a ChartSpec.
pub fn render_chart_svg(spec: &ChartSpec) -> String {
    match spec.chart_type.as_str() {
        "bar" => render_bar_svg(spec),
        "line" => render_line_svg(spec),
        "pie" => render_pie_svg(spec),
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

fn render_bar_svg(spec: &ChartSpec) -> String {
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
        let color = CHART_COLORS[i % CHART_COLORS.len()];

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

fn render_line_svg(spec: &ChartSpec) -> String {
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
    let min_val = spec
        .data
        .iter()
        .map(|d| d.value)
        .fold(f64::INFINITY, f64::min)
        .min(0.0);
    let range = max_val - min_val;
    let n = spec.data.len();
    if n == 0 || range == 0.0 {
        return empty_svg(spec);
    }

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

    // Axis
    elements.push(format!(
        r#"  <line x1="{chart_left}" y1="{chart_bottom}" x2="{chart_right}" y2="{chart_bottom}" stroke="{STROKE_AXIS}" stroke-width="1"/>"#,
    ));

    // Grid lines (4 horizontal)
    for i in 0..=4 {
        let y = chart_top + (i as f64 / 4.0) * chart_height;
        elements.push(format!(
            r#"  <line x1="{chart_left}" y1="{y}" x2="{chart_right}" y2="{y}" stroke="{STROKE_GRID}" stroke-width="1" stroke-dasharray="4,2"/>"#,
        ));
    }

    // Build points
    let points: Vec<(f64, f64)> = spec
        .data
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let x = if n == 1 {
                chart_left + chart_width / 2.0
            } else {
                chart_left + (i as f64 / (n - 1) as f64) * chart_width
            };
            let y = chart_bottom - ((d.value - min_val) / range) * chart_height;
            (x, y)
        })
        .collect();

    // Line path
    let path_d: String = points
        .iter()
        .enumerate()
        .map(|(i, (x, y))| {
            if i == 0 {
                format!("M{x},{y}")
            } else {
                format!(" L{x},{y}")
            }
        })
        .collect();

    let color = CHART_COLORS[0];
    elements.push(format!(
        r#"  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/>"#,
    ));

    // Dots and labels
    for (i, ((x, y), d)) in points.iter().zip(spec.data.iter()).enumerate() {
        elements.push(format!(
            r#"  <circle cx="{x}" cy="{y}" r="4" fill="{color}" stroke="white" stroke-width="2"/>"#,
        ));

        // X label
        elements.push(format!(
            r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{FILL_LABEL}">{}</text>"#,
            chart_bottom + 18.0,
            escape_xml(&d.label)
        ));

        // Value label (show for first, last, and every ~3rd point)
        if i == 0 || i == n - 1 || n <= 6 || i % 3 == 0 {
            elements.push(format!(
                r#"  <text x="{x}" y="{}" text-anchor="middle" font-family="sans-serif" font-size="10" fill="{FILL_VALUE}">{}</text>"#,
                y - 8.0,
                format_value(d.value)
            ));
        }
    }

    wrap_svg(&elements)
}

fn render_pie_svg(spec: &ChartSpec) -> String {
    let cx = SVG_WIDTH / 2.0;
    let cy = SVG_HEIGHT / 2.0 + if spec.title.is_some() { 15.0 } else { 0.0 };
    let radius = 140.0;

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
    let gap_angle = 0.03;

    for (i, d) in spec.data.iter().enumerate() {
        let fraction = d.value.max(0.0) / total;
        let sweep = fraction * 2.0 * std::f64::consts::PI - gap_angle;
        if sweep <= 0.0 {
            start_angle += fraction * 2.0 * std::f64::consts::PI;
            continue;
        }

        let inner_start = start_angle + gap_angle / 2.0;
        let end_angle = inner_start + sweep;

        let x1 = cx + radius * inner_start.cos();
        let y1 = cy + radius * inner_start.sin();
        let x2 = cx + radius * end_angle.cos();
        let y2 = cy + radius * end_angle.sin();

        let large_arc = if sweep > std::f64::consts::PI { 1 } else { 0 };
        let color = CHART_COLORS[i % CHART_COLORS.len()];

        elements.push(format!(
            r#"  <path d="M{cx},{cy} L{x1},{y1} A{radius},{radius} 0 {large_arc},1 {x2},{y2} Z" fill="{color}"/>"#,
        ));

        // Label at mid-angle, slightly outside
        let mid_angle = inner_start + sweep / 2.0;
        let label_r = radius + 20.0;
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
    use crate::chatty::tools::chart_tool::ChartDataPoint;

    #[test]
    fn bar_svg_contains_rects() {
        let spec = ChartSpec {
            chart_type: "bar".to_string(),
            title: Some("Test".to_string()),
            data: vec![
                ChartDataPoint {
                    label: "A".into(),
                    value: 10.0,
                },
                ChartDataPoint {
                    label: "B".into(),
                    value: 20.0,
                },
            ],
        };
        let svg = render_chart_svg(&spec);
        assert!(svg.contains("<rect"));
        assert!(svg.contains("Test"));
        assert!(svg.contains("A"));
        assert!(svg.contains("B"));
    }

    #[test]
    fn line_svg_contains_path() {
        let spec = ChartSpec {
            chart_type: "line".to_string(),
            title: None,
            data: vec![
                ChartDataPoint {
                    label: "Jan".into(),
                    value: 5.0,
                },
                ChartDataPoint {
                    label: "Feb".into(),
                    value: 15.0,
                },
            ],
        };
        let svg = render_chart_svg(&spec);
        assert!(svg.contains("<path"));
        assert!(svg.contains("<circle"));
    }

    #[test]
    fn pie_svg_contains_arcs() {
        let spec = ChartSpec {
            chart_type: "pie".to_string(),
            title: Some("Share".to_string()),
            data: vec![
                ChartDataPoint {
                    label: "X".into(),
                    value: 60.0,
                },
                ChartDataPoint {
                    label: "Y".into(),
                    value: 40.0,
                },
            ],
        };
        let svg = render_chart_svg(&spec);
        assert!(svg.contains("<path"));
        assert!(svg.contains("60%"));
        assert!(svg.contains("40%"));
    }

    #[test]
    fn empty_data_produces_valid_svg() {
        let spec = ChartSpec {
            chart_type: "bar".to_string(),
            title: None,
            data: vec![],
        };
        let svg = render_chart_svg(&spec);
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn xml_escaping_works() {
        let spec = ChartSpec {
            chart_type: "bar".to_string(),
            title: Some("A & B <test>".to_string()),
            data: vec![ChartDataPoint {
                label: "x\"y".into(),
                value: 10.0,
            }],
        };
        let svg = render_chart_svg(&spec);
        assert!(svg.contains("A &amp; B &lt;test&gt;"));
        assert!(svg.contains("x&quot;y"));
    }
}
