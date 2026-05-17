//! Tests for `data_query_tool` (extracted from the production file).

use super::*;

    use super::*;
    use std::sync::Arc;

    use rig::tool::Tool;

    use crate::services::filesystem_service::FileSystemService;

    #[tokio::test]
    async fn profile_data_returns_compact_generic_summary() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            data_dir.join("sales.csv"),
            "category,amount,flag\nbook,10,true\nbook,20,false\ngame,30,true\n",
        )
        .unwrap();

        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ProfileDataTool::new(service);

        let output = tool
            .call(ProfileDataArgs {
                path: "data/sales.csv".to_string(),
                sample_rows: Some(2),
            })
            .await
            .unwrap();

        assert_eq!(output.file_name, "sales.csv");
        assert_eq!(output.row_count, 3);
        assert_eq!(output.columns.len(), 3);
        assert!(output.sample_rows_markdown.contains("book"));

        let amount = output
            .column_profiles
            .iter()
            .find(|profile| profile.name == "amount")
            .unwrap();
        assert_eq!(amount.min.as_deref(), Some("10"));
        assert_eq!(amount.max.as_deref(), Some("30"));
        assert_eq!(amount.sum, Some(60.0));

        let category = output
            .column_profiles
            .iter()
            .find(|profile| profile.name == "category")
            .unwrap();
        assert!(
            category
                .top_values
                .iter()
                .any(|value| value.value == "book" && value.count == 2)
        );
    }

    #[test]
    fn profile_skips_top_values_for_complex_types() {
        assert!(!should_collect_top_values("VARCHAR[]"));
        assert!(!should_collect_top_values("STRUCT(name VARCHAR)"));
        assert!(!is_numeric_type("BIGINT[]"));
        assert!(should_collect_top_values("VARCHAR"));
        assert!(is_numeric_type("BIGINT"));
    }

    #[test]
    fn profile_selects_important_columns_beyond_first_eight() {
        let columns = vec![
            ("psp_reference", "BIGINT"),
            ("merchant", "VARCHAR"),
            ("card_scheme", "VARCHAR"),
            ("year", "BIGINT"),
            ("hour_of_day", "BIGINT"),
            ("minute_of_hour", "BIGINT"),
            ("day_of_year", "BIGINT"),
            ("is_credit", "BOOLEAN"),
            ("eur_amount", "DOUBLE"),
            ("email_address", "VARCHAR"),
            ("has_fraudulent_dispute", "BOOLEAN"),
            ("is_refused_by_adyen", "BOOLEAN"),
            ("aci", "VARCHAR"),
            ("acquirer_country", "VARCHAR"),
        ]
        .into_iter()
        .map(|(name, data_type)| ColumnInfo {
            name: name.to_string(),
            data_type: data_type.to_string(),
        })
        .collect::<Vec<_>>();

        let selected = select_profile_columns(&columns);
        let names = selected
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"has_fraudulent_dispute"));
        assert!(names.contains(&"is_refused_by_adyen"));
        assert!(names.contains(&"aci"));
        assert!(names.contains(&"acquirer_country"));
        assert!(!names.contains(&"email_address"));
    }

    #[tokio::test]
    async fn query_data_reads_workspace_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            data_dir.join("sales.csv"),
            "category,amount\nbook,10\ngame,30\n",
        )
        .unwrap();

        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = QueryDataTool::new(service);

        let output = tool
            .call(QueryDataArgs {
                query: "SELECT * FROM 'data/sales.csv' ORDER BY amount".to_string(),
                max_rows: Some(10),
            })
            .await
            .unwrap();

        assert_eq!(output.row_count, 2);
        assert_eq!(output.column_count, 2);
        assert!(output.markdown_table.contains("book"));
        assert!(output.markdown_table.contains("game"));
    }

    #[tokio::test]
    async fn query_data_rejects_files_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "secret\nvalue\n").unwrap();

        let service = Arc::new(
            FileSystemService::new(workspace.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = QueryDataTool::new(service);
        let outside_path = escape_sql_string(&outside.path().to_string_lossy());

        let result = tool
            .call(QueryDataArgs {
                query: format!("SELECT * FROM read_csv('{outside_path}', header=true)"),
                max_rows: Some(10),
            })
            .await;

        match result {
            Err(DataQueryError::PathNotAllowed(message)) => {
                assert!(message.contains("outside the workspace root"));
            }
            Err(DataQueryError::QueryFailed(message)) => {
                assert!(
                    message.contains("Permission")
                        || message.contains("disabled")
                        || message.contains("external")
                );
            }
            other => panic!("expected permission failure, got {other:?}"),
        }
    }
