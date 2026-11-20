//! Tests for `data_query_tool` (extracted from the production file).

use super::*;
use std::sync::Arc;

use rig_agent::tool::{Tool, ToolContext};

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
        .call(
            &mut ToolContext::new(),
            ProfileDataArgs {
                path: "data/sales.csv".to_string(),
                sample_rows: Some(2),
            },
        )
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

/// GAIA/DABstep traces: profile_data rejected .xlsx, so the model fell back
/// to pandas in the shell. The first sheet is profiled like a CSV.
#[cfg(feature = "excel")]
#[tokio::test]
async fn profile_data_reads_the_first_sheet_of_an_xlsx() {
    let dir = tempfile::tempdir().unwrap();
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Sales").unwrap();
    sheet.write_string(0, 0, "category").unwrap();
    sheet.write_string(0, 1, "amount").unwrap();
    for (row, (category, amount)) in [("book", 10.0), ("book, used", 20.0), ("game", 30.0)]
        .into_iter()
        .enumerate()
    {
        sheet.write_string(row as u32 + 1, 0, category).unwrap();
        sheet.write_number(row as u32 + 1, 1, amount).unwrap();
    }
    workbook.add_worksheet().set_name("Notes").unwrap();
    workbook.save(dir.path().join("sales.xlsx")).unwrap();

    let service = Arc::new(
        FileSystemService::new(dir.path().to_str().unwrap())
            .await
            .unwrap(),
    );
    let output = ProfileDataTool::new(service)
        .call(
            &mut ToolContext::new(),
            ProfileDataArgs {
                path: "sales.xlsx".to_string(),
                sample_rows: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(output.format, "excel");
    assert_eq!(output.row_count, 3);
    let amount = output
        .column_profiles
        .iter()
        .find(|profile| profile.name == "amount")
        .unwrap();
    assert_eq!(amount.sum, Some(60.0));
    assert!(output.notes[0].contains("'Sales'"), "{:?}", output.notes);
    assert!(output.notes[0].contains("Notes"), "{:?}", output.notes);
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
        .call(
            &mut ToolContext::new(),
            QueryDataArgs {
                query: "SELECT * FROM 'data/sales.csv' ORDER BY amount".to_string(),
                max_rows: Some(10),
            },
        )
        .await
        .unwrap();

    assert_eq!(output.row_count, 2);
    assert_eq!(output.column_count, 2);
    assert!(output.markdown_table.contains("book"));
    assert!(output.markdown_table.contains("game"));
    assert_eq!(output.preview.row_count, 2);
    assert_eq!(output.preview.column_count(), 2);
    assert!(
        output
            .preview
            .rows
            .iter()
            .flatten()
            .any(|c| c.contains("book"))
    );
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
        .call(
            &mut ToolContext::new(),
            QueryDataArgs {
                query: format!("SELECT * FROM read_csv('{outside_path}', header=true)"),
                max_rows: Some(10),
            },
        )
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

// ─── ranged delivery over HTTP (hive ADR-0089, AGE-1652) ───

/// What the local egress stand-in saw: each request's method, `Range` and
/// whether it carried the bearer, and the body bytes it sent in total.
#[derive(Default)]
struct RangeServerLog {
    requests: std::sync::Mutex<Vec<(String, Option<String>, bool)>>,
    body_bytes: std::sync::atomic::AtomicU64,
}

/// A stand-in for hive's `GET /api/egress/files/{resource}` (AGE-1648): a
/// bearer-checked file served whole, by a single `bytes=` range (206 with
/// `Content-Range`) or by `HEAD`, `Accept-Ranges: bytes`, on a loopback
/// port, until the returned guard drops.
fn range_server(
    bytes: Arc<Vec<u8>>,
    token: &'static str,
) -> (
    String,
    Arc<RangeServerLog>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::io::{BufRead, BufReader, Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/api/egress/files", listener.local_addr().unwrap());
    let log = Arc::new(RangeServerLog::default());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (log_t, stop_t) = (log.clone(), stop.clone());
    std::thread::spawn(move || {
        while !stop_t.load(std::sync::atomic::Ordering::Relaxed) {
            let (stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("accept: {e}"),
            };
            stream.set_nonblocking(false).unwrap();
            let mut reader = BufReader::new(stream);
            // One request per connection: `Connection: close` below.
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let _path = parts.next().unwrap_or_default().to_string();
            let (mut range, mut bearer_ok) = (None, false);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    let value = value.trim();
                    if name.eq_ignore_ascii_case("range") {
                        range = Some(value.to_string());
                    } else if name.eq_ignore_ascii_case("authorization") {
                        bearer_ok = value == format!("Bearer {token}");
                    }
                }
            }
            log_t
                .requests
                .lock()
                .unwrap()
                .push((method.clone(), range.clone(), bearer_ok));
            let mut stream = reader.into_inner();
            let len = bytes.len() as u64;
            let (status, start, end, content_range) = if !bearer_ok {
                (401, 0, 0, None)
            } else {
                match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
                    None => (200, 0, len, None),
                    Some(spec) => {
                        let (first, last) = spec.split_once('-').unwrap();
                        let (start, end) = if first.is_empty() {
                            let n: u64 = last.parse().unwrap();
                            (len.saturating_sub(n), len)
                        } else {
                            let start: u64 = first.parse().unwrap();
                            let end = match last.parse::<u64>() {
                                Ok(last) => (last + 1).min(len),
                                Err(_) => len,
                            };
                            (start, end)
                        };
                        (
                            206,
                            start,
                            end,
                            Some(format!("bytes {start}-{}/{len}", end - 1)),
                        )
                    }
                }
            };
            let body = &bytes[start as usize..end as usize];
            let mut head = format!(
                "HTTP/1.1 {status} {}\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\
                 Content-Type: application/octet-stream\r\nConnection: close\r\n",
                match status {
                    200 => "OK",
                    206 => "Partial Content",
                    _ => "Unauthorized",
                },
                body.len()
            );
            if let Some(content_range) = content_range {
                head.push_str(&format!("Content-Range: {content_range}\r\n"));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            if method != "HEAD" {
                stream.write_all(body).unwrap();
                log_t
                    .body_bytes
                    .fetch_add(body.len() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            let _ = stream.flush();
            // Drain whatever the client still sends before closing.
            let _ = stream.read(&mut [0u8; 1]);
        }
    });
    (base, log, stop)
}

/// A DABstep-shaped Parquet file: `rows` payments with the columns a
/// question touches, in row groups of 50 000, written by DuckDB itself.
fn payments_parquet(dir: &std::path::Path, rows: u64) -> std::path::PathBuf {
    let path = dir.join("payments.parquet");
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (
            SELECT
                20000000000 + i AS psp_reference,
                ['Crossfit_Hanna', 'Belles_cookbook_store', 'Golfclub_Baron_Friso',
                 'Martinis_Fine_Steakhouse', 'Rafa_AI'][1 + (i * 7919) % 5] AS merchant,
                ['NexPay', 'GlobalCard', 'SwiftCharge', 'TransactPlus'][1 + (i * 104729) % 4]
                    AS card_scheme,
                2023 AS year,
                (i % 24)::TINYINT AS hour_of_day,
                (1 + i % 365)::SMALLINT AS day_of_year,
                (i % 3 = 0) AS is_credit,
                round(((i * 2654435761) % 100000) / 100.0, 2) AS eur_amount,
                ['NL', 'SE', 'BE', 'FR', 'IT'][1 + (i * 31) % 5] AS ip_country,
                'shopper-' || (i % 50000) || '@example.test' AS email_address,
                md5(i::VARCHAR) AS card_number,
                (i % 97 = 0) AS has_fraudulent_dispute
            FROM range({rows}) t(i)
        ) TO '{}' (FORMAT parquet, ROW_GROUP_SIZE 50000);",
        escape_sql_string(&path.to_string_lossy())
    ))
    .unwrap();
    path
}

/// Hive's ranged delivery (ADR-0089, AGE-1652): a Parquet file served by a
/// bearer-checked, `Range`-serving egress stand-in is read in place by
/// `query_data` through `httpfs`, moving a fraction of its bytes for one
/// DABstep-style aggregate; every request carries the bearer and none asks
/// for the whole file; the same URL off the scope gets no bearer and
/// fails; a local file in the workspace still reads. Needs the `httpfs`
/// extension of this DuckDB version at `CHATTY_DUCKDB_HTTPFS` (the bundled
/// build has none; see `http.rs`), and skips without it. Prints the bytes
/// and times against the copy path (the whole file, then a local query).
#[tokio::test]
async fn ranged_parquet_over_http_moves_fewer_bytes_than_the_file() {
    let Some(httpfs) = std::env::var_os(HTTPFS_ENV).filter(|p| !p.is_empty()) else {
        eprintln!("skipped: set {HTTPFS_ENV} to DuckDB's httpfs extension file");
        return;
    };
    const TOKEN: &str = "lease-token-for-this-test";
    let workspace = tempfile::tempdir().unwrap();
    let parquet = payments_parquet(workspace.path(), 200_000);
    let bytes = Arc::new(std::fs::read(&parquet).unwrap());
    let file_size = bytes.len() as u64;
    let (base, log, stop) = range_server(bytes.clone(), TOKEN);
    let url = format!("{base}/0b8c4d1e-0000-4000-8000-000000000001");

    let service = Arc::new(
        FileSystemService::new(workspace.path().to_str().unwrap())
            .await
            .unwrap(),
    );
    let tool = QueryDataTool::new(service.clone())
        .with_http_reads(Some(HttpReads::new(&base, TOKEN).with_httpfs(&httpfs)));
    assert!(tool.description().contains("read in place over HTTP"));

    // One DABstep question (the average transaction value) over the range.
    let question = |path: &str| QueryDataArgs {
        query: format!("SELECT round(avg(eur_amount), 3) AS avg_eur FROM read_parquet('{path}')"),
        max_rows: Some(5),
    };
    let started = std::time::Instant::now();
    let ranged = tool
        .call(&mut ToolContext::new(), question(&url))
        .await
        .expect("the ranged query answers");
    let ranged_ms = started.elapsed().as_millis();
    let ranged_bytes = log.body_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let requests = log.requests.lock().unwrap().clone();
    assert_eq!(ranged.row_count, 1);
    assert_eq!(ranged.columns[0].name, "avg_eur");
    assert!(
        requests.iter().all(|(_, _, bearer)| *bearer),
        "every request carries the bearer: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|(method, range, _)| method == "HEAD" || range.is_some()),
        "no request fetched the whole file: {requests:?}"
    );
    assert!(
        ranged_bytes * 2 < file_size,
        "ranged moved {ranged_bytes} of {file_size} bytes: the ADR-0089 kill criterion"
    );

    // The copy path: the whole file over the same route, then the same
    // question on the local copy.
    let copied = workspace.path().join("copy.parquet");
    let started = std::time::Instant::now();
    let (copied_bytes, copy_ms) = tokio::task::spawn_blocking({
        let url = url.clone();
        let copied = copied.clone();
        move || {
            // The blocking client owns a runtime: made off the async thread.
            let body = reqwest::blocking::Client::new()
                .get(&url)
                .bearer_auth(TOKEN)
                .send()
                .unwrap()
                .bytes()
                .unwrap();
            std::fs::write(&copied, &body).unwrap();
            (body.len() as u64, started.elapsed().as_millis())
        }
    })
    .await
    .unwrap();
    let local = QueryDataTool::new(service.clone());
    let started = std::time::Instant::now();
    let on_copy = local
        .call(&mut ToolContext::new(), question("copy.parquet"))
        .await
        .expect("the local query answers");
    let copy_ms = copy_ms + started.elapsed().as_millis();
    assert_eq!(copied_bytes, file_size);
    assert_eq!(
        ranged.markdown_table, on_copy.markdown_table,
        "the same answer either way"
    );
    eprintln!(
        "ranged-delivery measurement: file {file_size} B; ranged {ranged_bytes} B in {} requests, \
         {ranged_ms} ms; copy {copied_bytes} B, {copy_ms} ms (fetch + query); answer {}\n\
         requests: {:?}",
        requests.len(),
        ranged.markdown_table.lines().last().unwrap_or_default(),
        requests
            .iter()
            .map(|(m, r, _)| format!("{m} {}", r.as_deref().unwrap_or("-")))
            .collect::<Vec<_>>()
    );

    // Off the scope — another path on the same host — is not an allowed
    // path: DuckDB refuses it before any request, so the bearer goes
    // nowhere else and nothing moves.
    let before = log.requests.lock().unwrap().len();
    let off_scope = format!(
        "http://{}/api/egress/seed",
        base.trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
    );
    let refused = tool
        .call(&mut ToolContext::new(), question(&off_scope))
        .await;
    match refused {
        Err(DataQueryError::QueryFailed(message)) => {
            assert!(message.contains("Permission"), "{message}");
            assert!(!message.contains(TOKEN), "{message}");
        }
        other => panic!("expected a permission failure, got {other:?}"),
    }
    assert_eq!(
        log.requests.lock().unwrap().len(),
        before,
        "no request left"
    );

    // Workspace files still read on the same tool.
    let on_local = tool
        .call(&mut ToolContext::new(), question("copy.parquet"))
        .await
        .expect("a workspace file still reads");
    assert_eq!(on_local.markdown_table, on_copy.markdown_table);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Without HTTP reads a URL literal is left to DuckDB, which cannot read it
/// (no `httpfs`, no external access): a clear failure, not a workspace
/// path error, and nothing is fetched.
#[tokio::test]
async fn a_url_is_not_a_workspace_path_and_is_refused_without_http_reads() {
    let workspace = tempfile::tempdir().unwrap();
    let service = Arc::new(
        FileSystemService::new(workspace.path().to_str().unwrap())
            .await
            .unwrap(),
    );
    let tool = QueryDataTool::new(service);
    assert!(!tool.description().contains("read in place over HTTP"));
    let result = tool
        .call(
            &mut ToolContext::new(),
            QueryDataArgs {
                query: "SELECT count(*) FROM read_parquet('http://127.0.0.1:9/x.parquet')"
                    .to_string(),
                max_rows: Some(5),
            },
        )
        .await;
    match result {
        Err(DataQueryError::QueryFailed(message)) => {
            assert!(!message.contains("outside the workspace root"), "{message}");
        }
        other => panic!("expected a DuckDB failure, got {other:?}"),
    }
    assert!(
        rewrite_query_file_literals(
            "SELECT 1 FROM read_parquet('HTTPS://h/a.parquet')",
            workspace.path()
        )
        .unwrap()
        .contains("'HTTPS://h/a.parquet'")
    );
}

/// Review probe (AGE-1652): the scope is a path prefix, not a string
/// prefix — a sibling route `…/files-evil/…` and the host's root are off
/// the scope and refused with no request; once set up the configuration
/// is locked (no `SET`/`RESET`, `LOAD`, `INSTALL`, no unredacted
/// secrets, no `http_proxy`); a quote in the bearer and in the scope is a
/// quoted string, not SQL, and the exact bearer reaches an in-scope URL;
/// local files outside the workspace stay refused. Skips without
/// `CHATTY_DUCKDB_HTTPFS`.
#[tokio::test]
async fn a_sibling_route_is_off_the_scope_and_the_locked_sandbox_holds() {
    let Some(httpfs) = std::env::var_os(HTTPFS_ENV).filter(|p| !p.is_empty()) else {
        eprintln!("skipped: set {HTTPFS_ENV} to DuckDB's httpfs extension file");
        return;
    };
    const TOKEN: &str = "tok'en); SET enable_external_access = true; --";
    let workspace = tempfile::tempdir().unwrap();
    let (base, log, stop) = range_server(Arc::new(b"not parquet".to_vec()), TOKEN);
    let reads = HttpReads::new(format!("{base}/"), TOKEN).with_httpfs(&httpfs);
    let conn = sandboxed_connection_with(workspace.path().to_str().unwrap(), Some(&reads))
        .expect("a quote in the bearer is quoted, not SQL");
    let requests = || log.requests.lock().unwrap().len();

    let probe = |sql: String| -> Result<String, String> {
        conn.query_row(&sql, [], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())
    };
    for off_scope in [
        format!("{base}-evil/x.parquet"),
        format!("{base}evil/x.parquet"),
        format!(
            "http://{}/x.parquet",
            base.trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap()
        ),
        "http://127.0.0.1:9/x.parquet".to_string(),
        "https://example.com/x.parquet".to_string(),
    ] {
        let error = probe(format!(
            "SELECT count(*)::VARCHAR FROM read_parquet('{off_scope}')"
        ))
        .expect_err(&off_scope);
        assert!(error.contains("Permission"), "{off_scope}: {error}");
        assert!(!error.contains("tok'en"), "{error}");
    }
    assert_eq!(requests(), 0, "no request for an off-scope URL");
    // DuckDB does not normalise dot segments: `…/files/../seed` passes the
    // prefix check and is requested, with the bearer, from the scope's own
    // host, where hive's router has no such route. Nothing new leaks: the
    // bearer goes only to the host that minted it.
    let dotted = format!("{base}/../seed");
    let _ = probe(format!(
        "SELECT count(*)::VARCHAR FROM read_parquet('{dotted}')"
    ));
    let seen = log.requests.lock().unwrap().clone();
    assert!(
        !seen.is_empty() && seen.iter().all(|(_, _, bearer)| *bearer),
        "{seen:?}"
    );
    log.requests.lock().unwrap().clear();
    let error = probe("SELECT content FROM read_text('/etc/hostname')".to_string()).unwrap_err();
    assert!(error.contains("Permission"), "{error}");

    for locked in [
        "SET enable_external_access = true",
        "RESET allowed_directories",
        "SET allowed_directories = ['/']",
        "SET allow_unredacted_secrets = true",
        "SET http_proxy = 'http://127.0.0.1:1'",
        "SET lock_configuration = false",
        "INSTALL spatial",
        // (`LOAD` of a built-in or already loaded extension is a no-op.)
        "LOAD '/tmp/other.duckdb_extension'",
    ] {
        let error = conn.execute_batch(locked).expect_err(locked).to_string();
        assert!(!error.contains("tok'en"), "{error}");
    }
    let shown =
        probe("SELECT string_agg(secret_string, ' ') FROM duckdb_secrets()".to_string()).unwrap();
    assert!(shown.contains("hive_files"), "{shown}");
    assert!(!shown.contains("tok'en"), "the secret is redacted: {shown}");
    assert!(
        probe(
            "SELECT string_agg(secret_string, ' ') FROM duckdb_secrets(redact := false)"
                .to_string()
        )
        .is_err(),
        "unredacted secrets are refused"
    );

    // The in-scope URL gets the exact bearer (the stand-in answers 401 to
    // any other); the body is not Parquet, so the read fails afterwards.
    let url = format!("{base}/0b8c4d1e-0000-4000-8000-000000000002");
    let error = probe(format!(
        "SELECT count(*)::VARCHAR FROM read_parquet('{url}')"
    ))
    .unwrap_err();
    assert!(!error.contains("tok'en"), "{error}");
    let seen = log.requests.lock().unwrap().clone();
    assert!(
        !seen.is_empty() && seen.iter().all(|(_, _, bearer)| *bearer),
        "{seen:?}"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn query_data_accepts_sql_as_alias_of_query() {
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
    let sql = "SELECT * FROM 'data/sales.csv' ORDER BY amount";

    let mut outputs = Vec::new();
    for key in ["query", "sql"] {
        let args: QueryDataArgs =
            serde_json::from_value(serde_json::json!({ key: sql, "max_rows": 10 })).unwrap();
        outputs.push(tool.call(&mut ToolContext::new(), args).await.unwrap());
    }
    assert_eq!(outputs[0].row_count, 2);
    assert_eq!(outputs[0].row_count, outputs[1].row_count);
    assert_eq!(outputs[0].markdown_table, outputs[1].markdown_table);
}
