#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::tools::ToolError;

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
    /// Theme chart colors captured at agent creation time (hex strings, e.g. "#4e79a7").
    /// Used when saving charts to disk so the file matches the inline chart appearance.
    /// Falls back to `DEFAULT_CHART_COLORS` when not set.
    pub theme_colors: Option<[String; 5]>,
}

impl CreateChartTool {
    pub fn new(workspace_dir: Option<String>, theme_colors: Option<[String; 5]>) -> Self {
        Self {
            workspace_dir,
            theme_colors,
        }
    }
}

impl Tool for CreateChartTool {
    const NAME: &'static str = "create_chart";
    type Error = ToolError;
    type Args = CreateChartArgs;
    type Output = ChartSpec;

    fn description(&self) -> String {
        "Create and display a chart inline. Prefer this over matplotlib/shell.\n\
                         Keep the title short plain text. Always send one complete JSON object with a full \
                         `data` (or `series`) array — never put JSON inside the title.\n\
                         Types: bar, line, pie, donut, area, candlestick.\n\
                         Save with workspace-relative save_path, e.g. \"charts/sales.png\".\n\
                         Example: {\"chart_type\":\"bar\",\"title\":\"Sales\",\"save_path\":\"charts/sales.png\",\
                         \"data\":[{\"label\":\"Jan\",\"value\":45},{\"label\":\"Feb\",\"value\":52}]}"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "chart_type": {
                    "type": "string",
                    "enum": ["bar", "line", "pie", "donut", "area", "candlestick"],
                    "description": "The type of chart to create"
                },
                "title": {
                    "type": "string",
                    "description": "Short plain-text title (do not embed JSON here)"
                },
                "data": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string" },
                            "value": { "type": "number" }
                        },
                        "required": ["label", "value"]
                    },
                    "description": "Points for bar/pie/donut/line/area. Use series for multi-line/area."
                },
                "series": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "data": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "value": { "type": "number" }
                                    },
                                    "required": ["label", "value"]
                                }
                            }
                        },
                        "required": ["name", "data"]
                    },
                    "description": "Named series for multi-line/area charts"
                },
                "candlestick_data": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "date": { "type": "string" },
                            "open": { "type": "number" },
                            "high": { "type": "number" },
                            "low": { "type": "number" },
                            "close": { "type": "number" }
                        },
                        "required": ["date", "open", "high", "low", "close"]
                    }
                },
                "inner_radius": { "type": "number" },
                "pad_angle": { "type": "number" },
                "save_path": {
                    "type": "string",
                    "description": "Workspace-relative PNG path (e.g. charts/sales.png)"
                }
            },
            "required": ["chart_type"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    ///
    /// This tool already surfaced its message; routing it through the shared
    /// helper adds the tool name and the retryability classification the rest
    /// of the tools now get.
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        match args.chart_type.as_str() {
            "bar" | "pie" | "donut" => {
                if args.data.is_empty() {
                    return Err(ToolError::OperationFailed(
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
                    return Err(ToolError::OperationFailed(
                        "Either 'data' or 'series' (with non-empty data) must be provided"
                            .to_string(),
                    ));
                }
            }
            "candlestick" => {
                let empty = args.candlestick_data.as_ref().is_none_or(|d| d.is_empty());
                if empty {
                    return Err(ToolError::OperationFailed(
                        "candlestick_data must not be empty for candlestick charts".to_string(),
                    ));
                }
            }
            other => {
                return Err(ToolError::OperationFailed(format!(
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
            match save_chart_png(
                &spec,
                &save_path,
                self.workspace_dir.as_deref(),
                self.theme_colors.as_ref(),
            ) {
                Ok(resolved) => {
                    return Ok(ChartSpec {
                        saved_path: Some(resolved),
                        ..spec
                    });
                }
                Err(e) => {
                    return Err(ToolError::OperationFailed(format!(
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
/// Uses `theme_colors` when provided (captured at agent-creation time so the saved
/// file matches the inline chart the user sees). Falls back to `DEFAULT_CHART_COLORS`
/// when no theme is available.
///
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
    theme_colors: Option<&[String; 5]>,
) -> Result<String, String> {
    use crate::services::chart_svg_renderer::{DEFAULT_CHART_COLORS, render_chart_svg};

    let fallback: [String; 5] = DEFAULT_CHART_COLORS.map(str::to_owned);
    let colors = theme_colors.unwrap_or(&fallback);
    let svg = render_chart_svg(spec, colors);

    let resolved = super::path_utils::resolve_output_path(save_path, workspace_dir)?;
    let path = resolved.as_path();

    // Ensure parent directory exists
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create directory '{}': {e}", parent.display()))?;
    }

    // Write SVG to a temp file, then convert to PNG via resvg (mermaid feature)
    #[cfg(feature = "mermaid")]
    {
        use crate::services::mermaid_renderer_service::MermaidRendererService;

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
    #[cfg(not(feature = "mermaid"))]
    {
        let _ = (svg, path);
        Err(
            "PNG chart export requires the `mermaid` feature (resvg). Rebuild with mermaid enabled."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_agent::tool::{Tool, ToolContext};

    #[tokio::test]
    async fn test_bar_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "bar");
        assert_eq!(spec.data.len(), 2);
    }

    #[tokio::test]
    async fn test_line_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multi_series_line_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "line");
        assert_eq!(spec.series.as_ref().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_line_chart_requires_data_or_series() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
                    chart_type: "line".to_string(),
                    title: None,
                    data: vec![],
                    series: None,
                    candlestick_data: None,
                    inner_radius: None,
                    pad_angle: None,
                    save_path: None,
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pie_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_donut_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "donut");
        assert_eq!(spec.inner_radius, Some(60.0));
    }

    #[tokio::test]
    async fn test_area_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candlestick_chart() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candlestick_requires_data() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
                    chart_type: "candlestick".to_string(),
                    title: None,
                    data: vec![],
                    series: None,
                    candlestick_data: None,
                    inner_radius: None,
                    pad_angle: None,
                    save_path: None,
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_chart_type() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
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
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_data() {
        let tool = CreateChartTool::new(None, None);
        let result = tool
            .call(
                &mut ToolContext::new(),
                CreateChartArgs {
                    chart_type: "bar".to_string(),
                    title: None,
                    data: vec![],
                    series: None,
                    candlestick_data: None,
                    inner_radius: None,
                    pad_angle: None,
                    save_path: None,
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let tool = CreateChartTool::new(None, None);
        let def = tool_definition(&tool);
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
