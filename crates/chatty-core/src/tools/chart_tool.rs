#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::write_approval_store::PendingWriteApprovals;
use crate::services::filesystem_service::FileSystemService;
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::ToolError;
use crate::tools::path_utils::{approve_output_write, resolve_workspace_output, write_output_file};

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
    /// Optional workspace-relative file path to save the chart as a PNG (e.g. "charts/revenue.png").
    /// If omitted the chart is only shown inline. Use this when you need to reference the image
    /// later (e.g. in a Markdown report). Needs filesystem writes (AGE-534).
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
    /// Theme chart colors captured at agent creation time (hex strings, e.g. "#4e79a7").
    /// Used when saving charts to disk so the file matches the inline chart appearance.
    /// Falls back to `DEFAULT_CHART_COLORS` when not set.
    pub theme_colors: Option<[String; 5]>,
    /// The workspace `save_path` writes into. `None` (filesystem writes off,
    /// or no workspace) refuses `save_path`; the chart still shows inline.
    save_service: Option<Arc<FileSystemService>>,
    write_approval: Option<(ApprovalMode, PendingWriteApprovals)>,
}

impl CreateChartTool {
    pub fn new(theme_colors: Option<[String; 5]>) -> Self {
        Self {
            theme_colors,
            save_service: None,
            write_approval: None,
        }
    }

    /// Let `save_path` write PNGs into `service`'s workspace (AGE-534). The
    /// agent factory sets this only when filesystem writes are on, and
    /// always together with [`with_write_approval`](Self::with_write_approval).
    pub fn with_save_path(mut self, service: Arc<FileSystemService>) -> Self {
        self.save_service = Some(service);
        self
    }

    /// Ask for write approval under `mode` before every `save_path` write
    /// (AGE-534, as AGE-1096 did for `write_docx`).
    pub fn with_write_approval(
        mut self,
        mode: ApprovalMode,
        pending: PendingWriteApprovals,
    ) -> Self {
        self.write_approval = Some((mode, pending));
        self
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
            let Some(service) = &self.save_service else {
                return Err(ToolError::OperationFailed(format!(
                    "Cannot save the chart to '{save_path}': filesystem writes are disabled \
                     (or no workspace is set). Call create_chart again without save_path \
                     to show the chart inline."
                )));
            };
            let root = service.workspace_root();
            let failed = |e: String| {
                ToolError::OperationFailed(format!(
                    "Chart created but failed to save PNG to '{save_path}': {e}"
                ))
            };
            let resolved = resolve_workspace_output(root, &save_path).map_err(failed)?;
            let png = render_chart_png(&spec, self.theme_colors.as_ref()).map_err(failed)?;
            let approved = approve_output_write(
                self.write_approval.as_ref(),
                &resolved,
                format!("PNG chart '{}', {} bytes", spec.chart_type, png.len()),
            )
            .await
            .map_err(failed)?;
            if !approved {
                return Err(ToolError::OperationFailed(
                    "Write denied by user".to_string(),
                ));
            }
            write_output_file(root, &resolved, &png).map_err(failed)?;
            return Ok(ChartSpec {
                saved_path: Some(resolved.to_string_lossy().into_owned()),
                ..spec
            });
        }

        Ok(spec)
    }
}

/// Render `spec` to PNG bytes.
///
/// Uses `theme_colors` when provided (captured at agent-creation time so the saved
/// file matches the inline chart the user sees). Falls back to `DEFAULT_CHART_COLORS`
/// when no theme is available.
fn render_chart_png(
    spec: &ChartSpec,
    theme_colors: Option<&[String; 5]>,
) -> Result<Vec<u8>, String> {
    use crate::services::chart_svg_renderer::{DEFAULT_CHART_COLORS, render_chart_svg};

    let fallback: [String; 5] = DEFAULT_CHART_COLORS.map(str::to_owned);
    let colors = theme_colors.unwrap_or(&fallback);
    let svg = render_chart_svg(spec, colors);

    // Convert the SVG to PNG in memory via resvg (mermaid feature); no temp
    // file, so nothing on a shared /tmp can be raced or clobbered (AGE-1114).
    #[cfg(feature = "mermaid")]
    {
        use crate::services::mermaid_renderer_service::MermaidRendererService;

        MermaidRendererService::render_svg_data_to_png(svg.as_bytes())
            .map_err(|e| format!("SVG→PNG render failed: {e}"))
    }
    #[cfg(not(feature = "mermaid"))]
    {
        let _ = svg;
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

    /// AGE-1114: a chart renders to PNG in memory. It needs no writable temp
    /// dir (a predictable name there could be raced or symlinked), and the PNG
    /// is the same as rendering the same SVG from a file.
    #[cfg(all(feature = "mermaid", unix))]
    #[test]
    fn a_chart_renders_to_png_without_a_temp_file() {
        use crate::services::chart_svg_renderer::{DEFAULT_CHART_COLORS, render_chart_svg};
        use crate::services::mermaid_renderer_service::MermaidRendererService;
        use std::os::unix::fs::PermissionsExt;

        let spec = ChartSpec {
            chart_type: "bar".to_string(),
            title: Some("AGE-1114".to_string()),
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
            saved_path: None,
        };
        // Point the temp dir at a read-only directory: a render that still
        // wrote a temp SVG there fails ("Failed to write temp SVG"), and one
        // that renders in memory doesn't notice. CI runs tests on one thread
        // (`--test-threads=1`), so changing TMPDIR here is not raced.
        let readonly = tempfile::tempdir().unwrap();
        std::fs::set_permissions(readonly.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let old_tmpdir = std::env::var_os("TMPDIR");
        // SAFETY: tests run single-threaded in CI; the value is restored below.
        unsafe { std::env::set_var("TMPDIR", readonly.path()) };
        let rendered = render_chart_png(&spec, None);
        // SAFETY: as above.
        unsafe {
            match old_tmpdir {
                Some(value) => std::env::set_var("TMPDIR", value),
                None => std::env::remove_var("TMPDIR"),
            }
        }
        std::fs::set_permissions(readonly.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let png = rendered.expect("the chart renders with no writable temp dir");
        assert!(png.starts_with(b"\x89PNG"), "PNG bytes");

        // Same output as the file path the desktop's Mermaid view still uses.
        let colors: [String; 5] = DEFAULT_CHART_COLORS.map(str::to_owned);
        let svg = render_chart_svg(&spec, &colors);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chart.svg");
        std::fs::write(&file, &svg).unwrap();
        assert_eq!(
            MermaidRendererService::render_svg_to_png(&file).unwrap(),
            png,
            "in-memory and file rendering give the same PNG"
        );
    }

    #[tokio::test]
    async fn test_bar_chart() {
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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
        let tool = CreateChartTool::new(None);
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

    fn bar_with_save_path(save_path: &str) -> CreateChartArgs {
        CreateChartArgs {
            chart_type: "bar".to_string(),
            title: Some("Sales".to_string()),
            data: vec![ChartDataPoint {
                label: "Jan".to_string(),
                value: 45.0,
            }],
            series: None,
            candlestick_data: None,
            inner_radius: None,
            pad_angle: None,
            save_path: Some(save_path.to_string()),
        }
    }

    #[tokio::test]
    async fn save_path_writes_nothing_when_writes_are_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sales.png");
        // No `with_save_path`: what the factory builds with writes off.
        let tool = CreateChartTool::new(None);
        let err = tool
            .call(
                &mut ToolContext::new(),
                bar_with_save_path(target.to_str().unwrap()),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("filesystem writes are disabled"),
            "{err}"
        );
        assert!(!target.exists(), "nothing is written with writes off");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn save_path_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = CreateChartTool::new(None).with_save_path(service);
        for path in ["~", "~/../../x.png", "../x.png", "/etc/x.png"] {
            let err = tool
                .call(&mut ToolContext::new(), bar_with_save_path(path))
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("not allowed")
                    || err.to_string().contains("outside the workspace"),
                "{path}: {err}"
            );
        }
    }

    #[cfg(feature = "mermaid")]
    #[tokio::test]
    async fn save_path_asks_before_writing_under_always_ask() {
        use std::time::Duration;

        use crate::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("charts/sales.png");
        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );

        let mut store = WriteApprovalStore::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (resolution_tx, _resolution_rx) = tokio::sync::mpsc::unbounded_channel();
        store.set_notifiers(tx, resolution_tx);
        let tool = CreateChartTool::new(None)
            .with_save_path(service)
            .with_write_approval(ApprovalMode::AlwaysAsk, store.get_pending_approvals());
        let spawn = |tool: CreateChartTool| {
            tokio::spawn(async move {
                tool.call(
                    &mut ToolContext::new(),
                    bar_with_save_path("charts/sales.png"),
                )
                .await
            })
        };

        // Denied: the call fails and nothing is written.
        let denied = spawn(tool.clone());
        let asked = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the save is asked about")
            .expect("notifier open");
        assert!(
            asked.command.starts_with("Create file:"),
            "{}",
            asked.command
        );
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Denied));
        assert!(denied.await.unwrap().is_err(), "a denied save fails");
        assert!(!target.exists(), "a denied save writes nothing");

        // Approved: the PNG is written and its path returned.
        let approved = spawn(tool);
        let asked = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the second save is asked about")
            .expect("notifier open");
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Approved));
        let spec = approved.await.unwrap().unwrap();
        assert!(target.exists(), "an approved save is written");
        assert_eq!(
            spec.saved_path.as_deref(),
            Some(std::fs::canonicalize(&target).unwrap().to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let tool = CreateChartTool::new(None);
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
