use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ChartToolError {
    #[error("Chart error: {0}")]
    ValidationError(String),
}

/// A single data point for bar, line, pie, donut, and area charts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartDataPoint {
    pub label: String,
    pub value: f64,
}

/// A named data series for multi-series line and area charts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeriesData {
    pub name: String,
    pub data: Vec<ChartDataPoint>,
}

/// A single data point for candlestick charts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandlestickDataPoint {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// The JSON schema the LLM sends as tool arguments.
#[derive(Deserialize, Serialize)]
pub struct CreateChartArgs {
    /// Chart type: "bar", "line", "pie", "donut", "area", or "candlestick"
    pub chart_type: String,
    /// Optional title displayed above the chart
    pub title: Option<String>,
    /// Data points for bar, line, pie, donut, and area charts (single series)
    #[serde(default)]
    pub data: Vec<ChartDataPoint>,
    /// Multiple named series for line and area charts (use instead of `data` for multi-line/area)
    pub series: Option<Vec<SeriesData>>,
    /// Data points for candlestick charts (date, open, high, low, close)
    pub candlestick_data: Option<Vec<CandlestickDataPoint>>,
    /// Inner radius for donut charts (default: 50). Also works on pie charts.
    pub inner_radius: Option<f32>,
    /// Angle gap between slices in radians (default: 0.03 for pie/donut)
    pub pad_angle: Option<f32>,
    /// Optional absolute file path to save the chart as a PNG (e.g. "/home/user/charts/revenue.png").
    /// If omitted the chart is only shown inline. Use this when you need to reference the image
    /// later (e.g. in a Markdown report).
    pub save_path: Option<String>,
}

/// The validated chart specification returned as tool output.
/// Parsed by `message_component.rs` for inline rendering.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartSpec {
    pub chart_type: String,
    pub title: Option<String>,
    pub data: Vec<ChartDataPoint>,
    pub series: Option<Vec<SeriesData>>,
    pub candlestick_data: Option<Vec<CandlestickDataPoint>>,
    pub inner_radius: Option<f32>,
    pub pad_angle: Option<f32>,
    /// Absolute path where the chart PNG was saved, if `save_path` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
}

#[derive(Clone)]
pub struct CreateChartTool {
    /// The configured workspace directory, used as base for relative save paths.
    pub workspace_dir: Option<String>,
}

impl CreateChartTool {
    pub fn new(workspace_dir: Option<String>) -> Self {
        Self { workspace_dir }
    }
}

impl Tool for CreateChartTool {
    const NAME: &'static str = "create_chart";
    type Error = ChartToolError;
    type Args = CreateChartArgs;
    type Output = ChartSpec;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "create_chart".to_string(),
            description: "Create and display a chart inline in the chat response.\n\
                         \n\
                         Chart types:\n\
                         - \"bar\": Vertical bar chart for comparing categories (shows value labels on bars)\n\
                         - \"line\": Line chart for trends and time series. Supports multiple series via 'series' field.\n\
                         - \"pie\": Pie chart for proportions (use 'pad_angle' for gaps between slices)\n\
                         - \"donut\": Donut chart — like pie but with a hole (use 'inner_radius' to control hole size, default 50)\n\
                         - \"area\": Area chart for trends with filled area below the line. Supports multiple series via 'series' field.\n\
                         - \"candlestick\": OHLC candlestick chart for financial/stock data\n\
                         \n\
                         Multi-series line/area: use 'series' instead of 'data' to plot multiple lines or areas.\n\
                         \n\
                         Saving to disk: pass 'save_path' with an absolute file path (e.g. \"/home/user/report/chart.png\") \
                         to save the chart as a PNG file. The tool returns the saved path in 'saved_path' so you can \
                         reference it in Markdown reports or other files. Parent directories are created automatically.\n\
                         \n\
                         Note on y-axis numbers: Bar charts show values on top of each bar. \
                         For line/area charts, a value table is shown below. \
                         True y-axis tick marks are not supported.\n\
                         \n\
                         Examples:\n\
                         - Bar: {\"chart_type\": \"bar\", \"title\": \"Sales by Region\", \
                           \"data\": [{\"label\": \"North\", \"value\": 120}, {\"label\": \"South\", \"value\": 85}]}\n\
                         - Line (single): {\"chart_type\": \"line\", \"title\": \"Monthly Revenue\", \
                           \"data\": [{\"label\": \"Jan\", \"value\": 1000}, {\"label\": \"Feb\", \"value\": 1200}]}\n\
                         - Line (multi): {\"chart_type\": \"line\", \"title\": \"Revenue vs Expenses\", \
                           \"series\": [{\"name\": \"Revenue\", \"data\": [{\"label\": \"Jan\", \"value\": 1000}, {\"label\": \"Feb\", \"value\": 1200}]}, \
                           {\"name\": \"Expenses\", \"data\": [{\"label\": \"Jan\", \"value\": 800}, {\"label\": \"Feb\", \"value\": 950}]}]}\n\
                         - Pie: {\"chart_type\": \"pie\", \"title\": \"Market Share\", \"pad_angle\": 0.05, \
                           \"data\": [{\"label\": \"A\", \"value\": 45}, {\"label\": \"B\", \"value\": 30}]}\n\
                         - Donut: {\"chart_type\": \"donut\", \"title\": \"Budget\", \"inner_radius\": 60, \
                           \"data\": [{\"label\": \"Dev\", \"value\": 50}, {\"label\": \"Marketing\", \"value\": 30}]}\n\
                         - Area: {\"chart_type\": \"area\", \"title\": \"Visitors\", \
                           \"data\": [{\"label\": \"Mon\", \"value\": 400}, {\"label\": \"Tue\", \"value\": 520}]}\n\
                         - Candlestick: {\"chart_type\": \"candlestick\", \"title\": \"AAPL\", \
                           \"candlestick_data\": [{\"date\": \"2024-01\", \"open\": 150, \"high\": 160, \"low\": 145, \"close\": 158}]}\n\
                         - Save to disk: {\"chart_type\": \"bar\", \"title\": \"Sales\", \
                           \"data\": [{\"label\": \"A\", \"value\": 100}], \
                           \"save_path\": \"/home/user/report/sales.png\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "chart_type": {
                        "type": "string",
                        "enum": ["bar", "line", "pie", "donut", "area", "candlestick"],
                        "description": "The type of chart to create"
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional title displayed above the chart"
                    },
                    "data": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {
                                    "type": "string",
                                    "description": "Category label or x-axis value"
                                },
                                "value": {
                                    "type": "number",
                                    "description": "Numeric value for this data point"
                                }
                            },
                            "required": ["label", "value"]
                        },
                        "description": "Data points for single-series charts (bar, pie, donut, area, line). Use 'series' instead for multi-line/area."
                    },
                    "series": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "description": "Series name shown in the legend"
                                },
                                "data": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string", "description": "X-axis label" },
                                            "value": { "type": "number", "description": "Y value" }
                                        },
                                        "required": ["label", "value"]
                                    }
                                }
                            },
                            "required": ["name", "data"]
                        },
                        "description": "Multiple named series for line and area charts. Each series has a name and its own data array. All series should share the same x-axis labels."
                    },
                    "candlestick_data": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "date": { "type": "string", "description": "Date or time label" },
                                "open": { "type": "number", "description": "Opening price" },
                                "high": { "type": "number", "description": "Highest price" },
                                "low": { "type": "number", "description": "Lowest price" },
                                "close": { "type": "number", "description": "Closing price" }
                            },
                            "required": ["date", "open", "high", "low", "close"]
                        },
                        "description": "OHLC data points for candlestick charts"
                    },
                    "inner_radius": {
                        "type": "number",
                        "description": "Inner radius for donut charts (default: 50). Larger = bigger hole."
                    },
                    "pad_angle": {
                        "type": "number",
                        "description": "Gap between pie/donut slices in radians (e.g. 0.03 to 0.1)"
                    },
                    "save_path": {
                        "type": "string",
                        "description": "Absolute file path to save the chart as a PNG file (e.g. \"/home/user/report/chart.png\"). \
                                        Parent directories are created automatically. \
                                        The saved path is returned in 'saved_path' so you can reference it in Markdown or other files."
                    }
                },
                "required": ["chart_type"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.chart_type.as_str() {
            "bar" | "pie" | "donut" => {
                if args.data.is_empty() {
                    return Err(ChartToolError::ValidationError(
                        "Data array must not be empty".to_string(),
                    ));
                }
            }
            "line" | "area" => {
                let has_data = !args.data.is_empty();
                let has_series = args
                    .series
                    .as_ref()
                    .is_some_and(|s| !s.is_empty() && s.iter().all(|s| !s.data.is_empty()));
                if !has_data && !has_series {
                    return Err(ChartToolError::ValidationError(
                        "Either 'data' or 'series' (with non-empty data) must be provided"
                            .to_string(),
                    ));
                }
            }
            "candlestick" => {
                let empty = args.candlestick_data.as_ref().is_none_or(|d| d.is_empty());
                if empty {
                    return Err(ChartToolError::ValidationError(
                        "candlestick_data must not be empty for candlestick charts".to_string(),
                    ));
                }
            }
            other => {
                return Err(ChartToolError::ValidationError(format!(
                    "Unsupported chart type '{}'. Must be one of: bar, line, pie, donut, area, candlestick",
                    other
                )));
            }
        }

        let spec = ChartSpec {
            chart_type: args.chart_type,
            title: args.title,
            data: args.data,
            series: args.series,
            candlestick_data: args.candlestick_data,
            inner_radius: args.inner_radius,
            pad_angle: args.pad_angle,
            saved_path: None,
        };

        // Save to disk if the caller requested it.
        if let Some(save_path) = args.save_path {
            match save_chart_png(&spec, &save_path, self.workspace_dir.as_deref()) {
                Ok(resolved) => {
                    return Ok(ChartSpec {
                        saved_path: Some(resolved),
                        ..spec
                    });
                }
                Err(e) => {
                    return Err(ChartToolError::ValidationError(format!(
                        "Chart created but failed to save PNG to '{save_path}': {e}"
                    )));
                }
            }
        }

        Ok(spec)
    }
}

/// Render `spec` to a PNG file at `save_path`.
///
/// Uses the default palette (no theme colors available in the tool layer).
/// Creates parent directories if they don't exist.
/// Path resolution priority for relative paths:
///   1. `workspace_dir` if set (the user's configured working directory)
///   2. User's home directory as fallback
///
/// `~` is always expanded to the home directory.
/// Returns the resolved absolute path on success.
fn save_chart_png(
    spec: &ChartSpec,
    save_path: &str,
    workspace_dir: Option<&str>,
) -> Result<String, String> {
    use crate::chatty::services::chart_svg_renderer::{DEFAULT_CHART_COLORS, render_chart_svg};
    use crate::chatty::services::mermaid_renderer_service::MermaidRendererService;

    let colors: [String; 5] = DEFAULT_CHART_COLORS.map(str::to_owned);
    let svg = render_chart_svg(spec, &colors);

    // Expand `~` and resolve relative paths
    let resolved: std::path::PathBuf = {
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

        if save_path.starts_with("~/") || save_path == "~" {
            home.join(&save_path[2..])
        } else {
            let p = std::path::Path::new(save_path);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                // Relative path: prefer workspace_dir, fall back to home
                let base = workspace_dir.map(std::path::PathBuf::from).unwrap_or(home);
                base.join(p)
            }
        }
    };

    let path = resolved.as_path();

    // Ensure parent directory exists
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create directory '{}': {e}", parent.display()))?;
    }

    // Write SVG to a temp file, then convert to PNG via resvg
    let tmp_svg = std::env::temp_dir().join(format!(
        "chatty_chart_export_{}.svg",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp_svg, &svg).map_err(|e| format!("Failed to write temp SVG: {e}"))?;

    let png_bytes = MermaidRendererService::render_svg_to_png(&tmp_svg)
        .map_err(|e| format!("SVG→PNG render failed: {e}"))?;

    let _ = std::fs::remove_file(&tmp_svg);

    std::fs::write(path, &png_bytes)
        .map_err(|e| format!("Failed to write PNG to '{save_path}': {e}"))?;

    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;

    #[tokio::test]
    async fn test_bar_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "bar".to_string(),
                title: Some("Test".to_string()),
                data: vec![
                    ChartDataPoint {
                        label: "A".to_string(),
                        value: 10.0,
                    },
                    ChartDataPoint {
                        label: "B".to_string(),
                        value: 20.0,
                    },
                ],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "bar");
        assert_eq!(spec.data.len(), 2);
    }

    #[tokio::test]
    async fn test_line_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "line".to_string(),
                title: None,
                data: vec![ChartDataPoint {
                    label: "Jan".to_string(),
                    value: 100.0,
                }],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multi_series_line_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "line".to_string(),
                title: Some("Revenue vs Expenses".to_string()),
                data: vec![],
                series: Some(vec![
                    SeriesData {
                        name: "Revenue".to_string(),
                        data: vec![
                            ChartDataPoint {
                                label: "Jan".to_string(),
                                value: 1000.0,
                            },
                            ChartDataPoint {
                                label: "Feb".to_string(),
                                value: 1200.0,
                            },
                        ],
                    },
                    SeriesData {
                        name: "Expenses".to_string(),
                        data: vec![
                            ChartDataPoint {
                                label: "Jan".to_string(),
                                value: 800.0,
                            },
                            ChartDataPoint {
                                label: "Feb".to_string(),
                                value: 950.0,
                            },
                        ],
                    },
                ]),
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "line");
        assert_eq!(spec.series.as_ref().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_line_chart_requires_data_or_series() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "line".to_string(),
                title: None,
                data: vec![],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pie_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "pie".to_string(),
                title: Some("Share".to_string()),
                data: vec![
                    ChartDataPoint {
                        label: "X".to_string(),
                        value: 60.0,
                    },
                    ChartDataPoint {
                        label: "Y".to_string(),
                        value: 40.0,
                    },
                ],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_donut_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "donut".to_string(),
                title: Some("Budget".to_string()),
                data: vec![
                    ChartDataPoint {
                        label: "Dev".to_string(),
                        value: 50.0,
                    },
                    ChartDataPoint {
                        label: "Marketing".to_string(),
                        value: 30.0,
                    },
                ],
                series: None,
                candlestick_data: None,
                inner_radius: Some(60.0),
                pad_angle: Some(0.05),
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "donut");
        assert_eq!(spec.inner_radius, Some(60.0));
    }

    #[tokio::test]
    async fn test_area_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "area".to_string(),
                title: None,
                data: vec![ChartDataPoint {
                    label: "Mon".to_string(),
                    value: 400.0,
                }],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candlestick_chart() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "candlestick".to_string(),
                title: Some("AAPL".to_string()),
                data: vec![],
                series: None,
                candlestick_data: Some(vec![CandlestickDataPoint {
                    date: "2024-01".to_string(),
                    open: 150.0,
                    high: 160.0,
                    low: 145.0,
                    close: 158.0,
                }]),
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candlestick_requires_data() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "candlestick".to_string(),
                title: None,
                data: vec![],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_chart_type() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "scatter".to_string(),
                title: None,
                data: vec![ChartDataPoint {
                    label: "A".to_string(),
                    value: 1.0,
                }],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_data() {
        let tool = CreateChartTool::new(None);
        let result = tool
            .call(CreateChartArgs {
                chart_type: "bar".to_string(),
                title: None,
                data: vec![],
                series: None,
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
                save_path: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let tool = CreateChartTool::new(None);
        let def = tool.definition("test".into()).await;
        assert_eq!(def.name, "create_chart");
        assert!(def.description.contains("bar"));
        assert!(def.description.contains("line"));
        assert!(def.description.contains("pie"));
        assert!(def.description.contains("donut"));
        assert!(def.description.contains("area"));
        assert!(def.description.contains("candlestick"));
        assert_eq!(def.parameters["required"][0], "chart_type");
    }
}
