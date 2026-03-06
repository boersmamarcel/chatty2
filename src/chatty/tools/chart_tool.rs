use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ChartToolError {
    #[error("Chart error: {0}")]
    ValidationError(String),
}

/// A single data point for bar, line, and pie charts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartDataPoint {
    pub label: String,
    pub value: f64,
}

/// The JSON schema the LLM sends as tool arguments.
#[derive(Deserialize, Serialize)]
pub struct CreateChartArgs {
    /// Chart type: "bar", "line", or "pie"
    pub chart_type: String,
    /// Optional title displayed above the chart
    pub title: Option<String>,
    /// Data points to plot
    pub data: Vec<ChartDataPoint>,
}

/// The validated chart specification returned as tool output.
/// Parsed by `message_component.rs` for inline rendering.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartSpec {
    pub chart_type: String,
    pub title: Option<String>,
    pub data: Vec<ChartDataPoint>,
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
            description: "Create and display a chart inline in the chat response. \
                         Supports bar charts, line charts, and pie charts.\n\
                         \n\
                         The chart will be rendered visually in your response using \
                         the application's theme colors.\n\
                         \n\
                         Chart types:\n\
                         - \"bar\": Vertical bar chart for comparing categories\n\
                         - \"line\": Line chart for trends and time series\n\
                         - \"pie\": Pie chart for proportions and distributions\n\
                         \n\
                         Examples:\n\
                         - Bar chart: {\"chart_type\": \"bar\", \"title\": \"Sales by Region\", \
                           \"data\": [{\"label\": \"North\", \"value\": 120}, {\"label\": \"South\", \"value\": 85}]}\n\
                         - Line chart: {\"chart_type\": \"line\", \"title\": \"Monthly Revenue\", \
                           \"data\": [{\"label\": \"Jan\", \"value\": 1000}, {\"label\": \"Feb\", \"value\": 1200}]}\n\
                         - Pie chart: {\"chart_type\": \"pie\", \"title\": \"Market Share\", \
                           \"data\": [{\"label\": \"Product A\", \"value\": 45}, {\"label\": \"Product B\", \"value\": 30}]}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "chart_type": {
                        "type": "string",
                        "enum": ["bar", "line", "pie"],
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
                        "description": "Array of data points to plot"
                    }
                },
                "required": ["chart_type", "data"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate chart type
        match args.chart_type.as_str() {
            "bar" | "line" | "pie" => {}
            other => {
                return Err(ChartToolError::ValidationError(format!(
                    "Unsupported chart type '{}'. Must be one of: bar, line, pie",
                    other
                )));
            }
        }

        // Validate data
        if args.data.is_empty() {
            return Err(ChartToolError::ValidationError(
                "Data array must not be empty".to_string(),
            ));
        }

        Ok(ChartSpec {
            chart_type: args.chart_type,
            title: args.title,
            data: args.data,
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
            })
            .await;
        assert!(result.is_ok());
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
        assert_eq!(def.parameters["required"][0], "chart_type");
        assert_eq!(def.parameters["required"][1], "data");
    }
}
