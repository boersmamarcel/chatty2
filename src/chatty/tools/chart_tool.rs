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
    /// Data points for bar, line, pie, donut, and area charts
    #[serde(default)]
    pub data: Vec<ChartDataPoint>,
    /// Data points for candlestick charts (date, open, high, low, close)
    pub candlestick_data: Option<Vec<CandlestickDataPoint>>,
    /// Inner radius for donut charts (default: 50). Also works on pie charts.
    pub inner_radius: Option<f32>,
    /// Angle gap between slices in radians (default: 0.03 for pie/donut)
    pub pad_angle: Option<f32>,
}

/// The validated chart specification returned as tool output.
/// Parsed by `message_component.rs` for inline rendering.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartSpec {
    pub chart_type: String,
    pub title: Option<String>,
    pub data: Vec<ChartDataPoint>,
    pub candlestick_data: Option<Vec<CandlestickDataPoint>>,
    pub inner_radius: Option<f32>,
    pub pad_angle: Option<f32>,
}

#[derive(Clone)]
pub struct CreateChartTool;

impl CreateChartTool {
    pub fn new() -> Self {
        Self
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
                         - \"line\": Line chart for trends and time series\n\
                         - \"pie\": Pie chart for proportions (use 'pad_angle' for gaps between slices)\n\
                         - \"donut\": Donut chart — like pie but with a hole (use 'inner_radius' to control hole size, default 50)\n\
                         - \"area\": Area chart for trends with filled area below the line\n\
                         - \"candlestick\": OHLC candlestick chart for financial/stock data\n\
                         \n\
                         Note on y-axis numbers: Bar charts show values on top of each bar. \
                         For line/area charts, a value table is shown below. \
                         True y-axis tick marks are not supported.\n\
                         \n\
                         Examples:\n\
                         - Bar: {\"chart_type\": \"bar\", \"title\": \"Sales by Region\", \
                           \"data\": [{\"label\": \"North\", \"value\": 120}, {\"label\": \"South\", \"value\": 85}]}\n\
                         - Line: {\"chart_type\": \"line\", \"title\": \"Monthly Revenue\", \
                           \"data\": [{\"label\": \"Jan\", \"value\": 1000}, {\"label\": \"Feb\", \"value\": 1200}]}\n\
                         - Pie: {\"chart_type\": \"pie\", \"title\": \"Market Share\", \"pad_angle\": 0.05, \
                           \"data\": [{\"label\": \"A\", \"value\": 45}, {\"label\": \"B\", \"value\": 30}]}\n\
                         - Donut: {\"chart_type\": \"donut\", \"title\": \"Budget\", \"inner_radius\": 60, \
                           \"data\": [{\"label\": \"Dev\", \"value\": 50}, {\"label\": \"Marketing\", \"value\": 30}]}\n\
                         - Area: {\"chart_type\": \"area\", \"title\": \"Visitors\", \
                           \"data\": [{\"label\": \"Mon\", \"value\": 400}, {\"label\": \"Tue\", \"value\": 520}]}\n\
                         - Candlestick: {\"chart_type\": \"candlestick\", \"title\": \"AAPL\", \
                           \"candlestick_data\": [{\"date\": \"2024-01\", \"open\": 150, \"high\": 160, \"low\": 145, \"close\": 158}]}"
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
                        "description": "Data points for bar, line, pie, donut, and area charts"
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
                    }
                },
                "required": ["chart_type"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.chart_type.as_str() {
            "bar" | "line" | "pie" | "donut" | "area" => {
                if args.data.is_empty() {
                    return Err(ChartToolError::ValidationError(
                        "Data array must not be empty".to_string(),
                    ));
                }
            }
            "candlestick" => {
                let empty = args
                    .candlestick_data
                    .as_ref()
                    .map_or(true, |d| d.is_empty());
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

        Ok(ChartSpec {
            chart_type: args.chart_type,
            title: args.title,
            data: args.data,
            candlestick_data: args.candlestick_data,
            inner_radius: args.inner_radius,
            pad_angle: args.pad_angle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;

    #[tokio::test]
    async fn test_bar_chart() {
        let tool = CreateChartTool::new();
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
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "bar");
        assert_eq!(spec.data.len(), 2);
    }

    #[tokio::test]
    async fn test_line_chart() {
        let tool = CreateChartTool::new();
        let result = tool
            .call(CreateChartArgs {
                chart_type: "line".to_string(),
                title: None,
                data: vec![ChartDataPoint {
                    label: "Jan".to_string(),
                    value: 100.0,
                }],
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_pie_chart() {
        let tool = CreateChartTool::new();
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
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_donut_chart() {
        let tool = CreateChartTool::new();
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
                candlestick_data: None,
                inner_radius: Some(60.0),
                pad_angle: Some(0.05),
            })
            .await;
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.chart_type, "donut");
        assert_eq!(spec.inner_radius, Some(60.0));
    }

    #[tokio::test]
    async fn test_area_chart() {
        let tool = CreateChartTool::new();
        let result = tool
            .call(CreateChartArgs {
                chart_type: "area".to_string(),
                title: None,
                data: vec![ChartDataPoint {
                    label: "Mon".to_string(),
                    value: 400.0,
                }],
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candlestick_chart() {
        let tool = CreateChartTool::new();
        let result = tool
            .call(CreateChartArgs {
                chart_type: "candlestick".to_string(),
                title: Some("AAPL".to_string()),
                data: vec![],
                candlestick_data: Some(vec![CandlestickDataPoint {
                    date: "2024-01".to_string(),
                    open: 150.0,
                    high: 160.0,
                    low: 145.0,
                    close: 158.0,
                }]),
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candlestick_requires_data() {
        let tool = CreateChartTool::new();
        let result = tool
            .call(CreateChartArgs {
                chart_type: "candlestick".to_string(),
                title: None,
                data: vec![],
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_chart_type() {
        let tool = CreateChartTool::new();
        let result = tool
            .call(CreateChartArgs {
                chart_type: "scatter".to_string(),
                title: None,
                data: vec![ChartDataPoint {
                    label: "A".to_string(),
                    value: 1.0,
                }],
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_data() {
        let tool = CreateChartTool::new();
        let result = tool
            .call(CreateChartArgs {
                chart_type: "bar".to_string(),
                title: None,
                data: vec![],
                candlestick_data: None,
                inner_radius: None,
                pad_angle: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let tool = CreateChartTool::new();
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
