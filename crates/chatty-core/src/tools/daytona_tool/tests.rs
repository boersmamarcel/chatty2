//! Tests for `daytona_tool` (extracted from the production file).

use super::*;

    use super::*;

    #[tokio::test]
    async fn test_daytona_tool_definition() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "daytona_run");
        assert!(def.description.contains("sandbox"));
    }

    #[tokio::test]
    async fn test_daytona_tool_empty_code() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let args = DaytonaToolArgs {
            code: "   ".to_string(),
            language: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_api_error_auth() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let body = r#"{"statusCode":401,"message":"Invalid API key"}"#;
        let err = tool.parse_api_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(matches!(err, DaytonaToolError::AuthenticationFailed(_)));
    }

    #[test]
    fn test_parse_api_error_quota() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let body = r#"{"statusCode":403,"message":"quota exceeded for your account"}"#;
        let err = tool.parse_api_error(reqwest::StatusCode::FORBIDDEN, body);
        assert!(matches!(err, DaytonaToolError::QuotaExceeded(_)));
    }

    #[test]
    fn test_parse_api_error_generic() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let body = "Internal Server Error";
        let err = tool.parse_api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, body);
        assert!(matches!(err, DaytonaToolError::ApiError(_)));
    }

    #[test]
    fn test_has_downloadable_extension() {
        assert!(has_downloadable_extension("chart.png"));
        assert!(has_downloadable_extension("PHOTO.JPG"));
        assert!(has_downloadable_extension("data.csv"));
        assert!(has_downloadable_extension("report.pdf"));
        assert!(!has_downloadable_extension("script.py"));
        assert!(!has_downloadable_extension("data.json"));
        assert!(!has_downloadable_extension("readme.txt"));
    }

    #[test]
    fn test_file_entry_deserialization() {
        let json = r#"[
            {"name": "chart.png", "isDir": false, "size": 12345},
            {"name": "subdir", "isDir": true, "size": 0}
        ]"#;
        let entries: Vec<FileEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "chart.png");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, 12345);
        assert!(entries[1].is_dir);
    }

    #[test]
    fn test_extract_python_imports() {
        let code = r#"
import matplotlib.pyplot as plt
import plotly.express as px
from numpy import array
import os
import json
import pandas as pd
from PIL import Image
"#;
        let imports = extract_python_imports(code);
        assert!(imports.contains(&"matplotlib".to_string()));
        assert!(imports.contains(&"plotly".to_string()));
        assert!(imports.contains(&"numpy".to_string()));
        assert!(imports.contains(&"pandas".to_string()));
        assert!(imports.contains(&"PIL".to_string()));
        // stdlib should be excluded
        assert!(!imports.contains(&"os".to_string()));
        assert!(!imports.contains(&"json".to_string()));
    }

    #[test]
    fn test_pip_package_name_mapping() {
        assert_eq!(pip_package_name("PIL"), "Pillow");
        assert_eq!(pip_package_name("sklearn"), "scikit-learn");
        assert_eq!(pip_package_name("cv2"), "opencv-python");
        assert_eq!(pip_package_name("yaml"), "pyyaml");
        // Unmapped names pass through
        assert_eq!(pip_package_name("plotly"), "plotly");
        assert_eq!(pip_package_name("pandas"), "pandas");
    }

    #[test]
    fn test_extract_output_paths() {
        let code = r#"
import plotly.graph_objects as go
fig = go.Figure()
fig.write_html("lorenz_attractor.html")
fig.write_image("/tmp/chart.png")
plt.savefig('population_chart.png')
df.to_csv('/tmp/results.csv')
"#;
        let paths = extract_output_paths(code);
        assert!(paths.contains(&"lorenz_attractor.html".to_string()));
        assert!(paths.contains(&"/tmp/chart.png".to_string()));
        assert!(paths.contains(&"population_chart.png".to_string()));
        assert!(paths.contains(&"/tmp/results.csv".to_string()));
    }
