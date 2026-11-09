//! Tests for `daytona_tool` (extracted from the production file).

use super::*;
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext};

#[tokio::test]
async fn test_daytona_tool_definition() {
    let tool = DaytonaTool::new("test-key".into(), None);
    let def = tool_definition(&tool);
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
    let result = tool.call(&mut ToolContext::new(), args).await;
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

#[test]
fn download_dir_needs_a_workspace() {
    assert_eq!(download_dir(None), None);
    assert_eq!(
        download_dir(Some("/w")),
        Some(PathBuf::from("/w/daytona_outputs"))
    );
}

#[test]
fn numbered_name_keeps_the_extension() {
    assert_eq!(numbered_name("data.csv", 1), "data-1.csv");
    assert_eq!(numbered_name("chart.png", 2), "chart-2.png");
    assert_eq!(numbered_name(".bashrc", 1), ".bashrc-1");
}

#[test]
fn save_new_file_never_replaces_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("data.csv"), b"old").unwrap();

    let first = save_new_file(dir, "data.csv", b"new").unwrap();
    assert_eq!(first, dir.join("data-1.csv"));
    assert_eq!(std::fs::read(dir.join("data.csv")).unwrap(), b"old");
    assert_eq!(std::fs::read(&first).unwrap(), b"new");

    let second = save_new_file(dir, "data.csv", b"newer").unwrap();
    assert_eq!(second, dir.join("data-2.csv"));
}

#[test]
fn save_new_file_writes_a_dotfile_inside_the_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = save_new_file(dir, ".bashrc", b"x").unwrap();
    assert_eq!(path, dir.join(".bashrc"));
}

#[test]
fn prepare_download_dir_creates_a_missing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("daytona_outputs");
    prepare_download_dir(&dir).unwrap();
    assert!(dir.is_dir());
}

#[test]
fn prepare_download_dir_accepts_an_existing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("daytona_outputs");
    std::fs::create_dir(&dir).unwrap();
    assert!(prepare_download_dir(&dir).is_ok());
}

#[cfg(unix)]
#[test]
fn prepare_download_dir_refuses_a_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let dir = tmp.path().join("daytona_outputs");
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
    let err = prepare_download_dir(&dir).unwrap_err();
    assert!(err.to_string().contains("symlink"));
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn prepare_download_dir_refuses_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("daytona_outputs");
    std::fs::write(&dir, b"x").unwrap();
    let err = prepare_download_dir(&dir).unwrap_err();
    assert!(err.to_string().contains("not a directory"));
}
