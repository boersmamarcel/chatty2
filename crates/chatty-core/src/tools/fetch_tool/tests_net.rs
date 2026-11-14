use super::*;

/// A loopback HTTP server that answers each request with
/// `respond(path, user_agent, request_number)` = (status line, extra
/// headers, body), and records the `(path, user_agent)` it saw.
type Respond = fn(&str, &str, usize) -> (&'static str, String, String);
type Seen = Arc<Mutex<Vec<(String, String)>>>;

async fn mock_server(respond: Respond) -> (String, Seen) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 16 * 1024];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let agent = request
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.eq_ignore_ascii_case("user-agent")
                        .then(|| value.trim().to_string())
                })
                .unwrap_or_default();
            let number = {
                let mut log = log.lock().unwrap();
                log.push((path.clone(), agent.clone()));
                log.len()
            };
            let (status, headers, body) = respond(&path, &agent, number);
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (base, seen)
}

fn tool_with_archive(base: &str) -> FetchTool {
    FetchTool {
        wayback_available_url: format!("{base}/wayback/available"),
        wayback_web_url: base.to_string(),
        ..FetchTool::new(None)
    }
}

async fn fetch_page(tool: &FetchTool, url: &str) -> CachedPage {
    match tool.download(url, None, None, 0, MAX_WINDOW).await.unwrap() {
        Download::Page(page) => page,
        Download::Done(output) => panic!("expected a page, got {output:?}"),
    }
}

#[tokio::test]
async fn test_rate_limit_is_waited_out_and_retried() {
    let (base, seen) = mock_server(|_, _, n| match n {
        1 => (
            "429 Too Many Requests",
            "retry-after: 0\r\n".into(),
            "slow down".into(),
        ),
        _ => (
            "200 OK",
            "content-type: text/plain\r\n".into(),
            "the data".into(),
        ),
    })
    .await;
    let page = fetch_page(&tool_with_archive(&base), &format!("{base}/data")).await;
    assert_eq!(page.text, "the data");
    assert!(page.note.unwrap().contains("Rate limited (429)"));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn test_rate_limit_longer_than_the_cap_is_returned() {
    let (base, seen) = mock_server(|_, _, _| {
        (
            "429 Too Many Requests",
            "retry-after: 3600\r\n".into(),
            "later".into(),
        )
    })
    .await;
    let tool = tool_with_archive(&base);
    let Download::Done(output) = tool
        .download(&format!("{base}/data"), None, None, 0, MAX_WINDOW)
        .await
        .unwrap()
    else {
        panic!("expected the 429 back");
    };
    assert_eq!(output.status, 429);
    assert!(output.note.unwrap().contains("3600 s"));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn test_forbidden_default_agent_retries_as_a_browser() {
    let (base, seen) = mock_server(|_, agent, _| {
        if agent.starts_with("Mozilla/") {
            (
                "200 OK",
                "content-type: text/plain\r\n".into(),
                "welcome".into(),
            )
        } else {
            ("403 Forbidden", String::new(), "bots not allowed".into())
        }
    })
    .await;
    let page = fetch_page(&tool_with_archive(&base), &format!("{base}/page")).await;
    assert_eq!(page.text, "welcome");
    assert!(page.note.unwrap().contains("browser User-Agent"));
    let seen = seen.lock().unwrap();
    assert!(seen[0].1.starts_with("Chatty/"));
    assert!(seen[1].1.starts_with("Mozilla/"));
}

#[tokio::test]
async fn test_gone_page_is_served_from_the_archive_with_its_date() {
    let (base, seen) = mock_server(|path, _, _| {
        if path.starts_with("/wayback/available") {
            (
                "200 OK",
                "content-type: application/json\r\n".into(),
                r#"{"archived_snapshots":{"closest":{"available":true,"status":"200","timestamp":"20190304120000","url":"x"}}}"#.into(),
            )
        } else if path.starts_with("/web/") {
            ("200 OK", "content-type: text/plain\r\n".into(), "old text".into())
        } else {
            ("404 Not Found", String::new(), "gone".into())
        }
    })
    .await;
    let url = format!("{base}/old");
    let page = fetch_page(&tool_with_archive(&base), &url).await;
    assert_eq!(page.text, "old text");
    let note = page.note.unwrap();
    assert!(note.contains("ARCHIVED copy"), "{note}");
    assert!(note.contains("2019-03-04 12:00:00 UTC"), "{note}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen[2].0, format!("/web/20190304120000id_/{url}"));
}

#[tokio::test]
async fn test_missing_page_without_archive_copy_says_so() {
    let (base, _) = mock_server(|path, _, _| {
        if path.starts_with("/wayback/available") {
            (
                "200 OK",
                String::new(),
                r#"{"archived_snapshots":{}}"#.into(),
            )
        } else {
            ("404 Not Found", String::new(), "no such page".into())
        }
    })
    .await;
    let tool = tool_with_archive(&base);
    let Download::Done(output) = tool
        .download(&format!("{base}/nope"), None, None, 0, MAX_WINDOW)
        .await
        .unwrap()
    else {
        panic!("expected the 404 back");
    };
    assert_eq!(output.status, 404);
    assert_eq!(output.content, "no such page");
    assert!(output.note.unwrap().contains("no archived copy"));
}

/// A Wayback lookup that never answers is dropped after the lookup
/// timeout and the live 404 comes back, instead of stalling the turn.
#[tokio::test]
async fn test_slow_archive_lookup_does_not_stall_the_answer() {
    let (base, _) = mock_server(|_, _, _| ("404 Not Found", String::new(), "gone".into())).await;
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_base = format!("http://{}", silent.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    let tool = FetchTool {
        wayback_available_url: format!("{silent_base}/wayback/available"),
        wayback_web_url: silent_base,
        wayback_lookup_timeout: std::time::Duration::from_millis(200),
        ..FetchTool::new(None)
    };
    let started = std::time::Instant::now();
    let Download::Done(output) = tool
        .download(&format!("{base}/nope"), None, None, 0, MAX_WINDOW)
        .await
        .unwrap()
    else {
        panic!("expected the 404 back");
    };
    assert_eq!(output.status, 404);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn test_capture_date_and_archive_hosts() {
    assert_eq!(capture_date("20190304120000"), "2019-03-04 12:00:00 UTC");
    assert_eq!(capture_date("2019"), "2019");
    assert!(is_archive_url("https://web.archive.org/web/2019/x"));
    assert!(!is_archive_url("https://example.org/archive.org"));
}

#[tokio::test]
async fn test_fetch_tool_definition() {
    let tool = FetchTool::new(None);
    let def = tool_definition(&tool);
    assert_eq!(def.name, "fetch");
    assert!(def.description.contains("Fetch a URL"));
}

/// AGE-496: SEC EDGAR (and similar automated-traffic policies) reject
/// requests that don't declare a contact; the default UA must keep one.
#[test]
fn test_default_user_agent_declares_a_contact() {
    assert!(crate::services::http_client::USER_AGENT.contains('@'));
}

#[tokio::test]
async fn test_fetch_request_overrides_user_agent_when_declared() {
    let tool = FetchTool::new(None);
    let built = tool
        .request(
            "https://example.com",
            Some("Custom/1.0 contact@example.com"),
        )
        .build()
        .unwrap();
    assert_eq!(
        built.headers().get(reqwest::header::USER_AGENT).unwrap(),
        "Custom/1.0 contact@example.com"
    );
}

#[tokio::test]
async fn test_fetch_request_uses_default_user_agent_when_not_declared() {
    // The client's own default User-Agent (set once in `open_web_client`)
    // is applied by reqwest at send time, not baked into `build()`'s
    // `Request` — so "no override" means no per-request header at all.
    let tool = FetchTool::new(None);
    let built = tool.request("https://example.com", None).build().unwrap();
    assert!(built.headers().get(reqwest::header::USER_AGENT).is_none());
}

/// AGE-496: confirms the EDGAR-compliant default UA actually gets past
/// SEC's automated-traffic block, against the live API.
///
/// ```
/// cargo test -p chatty-core --lib fetches_sec_edgar_with_default_user_agent -- --ignored
/// ```
#[tokio::test]
#[ignore = "hits the live data.sec.gov API"]
async fn fetches_sec_edgar_with_default_user_agent() {
    let tool = FetchTool::new(None);
    let args = FetchToolArgs {
        url: "https://data.sec.gov/submissions/CIK0001408198.json".to_string(),
        max_length: None,
        start_index: None,
        find: None,
        user_agent: None,
    };
    let result = tool
        .call(&mut ToolContext::new(), args)
        .await
        .expect("EDGAR should accept the default declared user-agent");
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn test_fetch_tool_invalid_url() {
    let tool = FetchTool::new(None);
    let args = FetchToolArgs {
        url: "not-a-url".to_string(),
        max_length: None,
        start_index: None,
        find: None,
        user_agent: None,
    };
    let result = tool.call(&mut ToolContext::new(), args).await;
    assert!(result.is_err());
}

/// AGE-507 criterion 2, end to end: a truncated fetch continued with
/// `start_index` rebuilds the page with no gap and no overlap, and the
/// `#fragment` lands on the section.
///
/// This is the one criterion the in-process tests can only cover at the
/// `window()` seam — `chatty-core` has no mock-server dev-dependency, and
/// a local test server is unreachable on purpose (the SSRF guard blocks
/// loopback, which `test_fetch_tool_blocks_ssrf` asserts). So it is an
/// ignored live test, like `fetches_sec_edgar_with_default_user_agent`.
///
/// The three calls are three separate requests: if the article is edited
/// between them the seam shifts and this fails. That is inherent to paging
/// a live page statelessly, not a bug in `start_index`.
///
/// ```
/// cargo test -p chatty-core --lib continues_a_truncated_fetch_with_start_index -- --ignored
/// ```
#[tokio::test]
#[ignore = "hits the live en.wikipedia.org site"]
async fn continues_a_truncated_fetch_with_start_index() {
    const URL: &str = "https://en.wikipedia.org/wiki/U.S._Route_66#Route_description";
    let tool = FetchTool::new(None);
    let fetch = async |start_index: Option<usize>, max_length: Option<usize>| {
        tool.call(
            &mut ToolContext::new(),
            FetchToolArgs {
                url: URL.to_string(),
                max_length,
                start_index,
                find: None,
                user_agent: None,
            },
        )
        .await
        .expect("Wikipedia should answer")
    };
    let strip = |s: &str| {
        s.split("\n\n[Content truncated")
            .next()
            .unwrap()
            .to_string()
    };

    let whole = fetch(None, Some(6000)).await;
    let first = fetch(Some(0), Some(3000)).await;
    let second = fetch(Some(3000), Some(3000)).await;

    // Criterion 4, live: the #fragment starts the text at that section.
    assert!(
        strip(&whole.content).starts_with("Route description"),
        "got {:?}",
        &whole.content[..80.min(whole.content.len())]
    );
    assert!(whole.truncated);
    assert_eq!(
        format!("{}{}", strip(&first.content), strip(&second.content)),
        strip(&whole.content),
        "two 3000-byte windows must rebuild one 6000-byte window exactly"
    );
}

/// AGE-508 criterion 3, end to end: a live 404 HTML error page comes back
/// through `call()` as extracted text, not raw markup, and capped well
/// under the 20000-byte default.
///
/// ```
/// cargo test -p chatty-core --lib fetch_of_a_404_page_returns_extracted_text -- --ignored
/// ```
#[tokio::test]
#[ignore = "hits the live en.wikipedia.org site"]
async fn fetch_of_a_404_page_returns_extracted_text() {
    let tool = FetchTool::new(None);
    let args = FetchToolArgs {
        url: "https://en.wikipedia.org/wiki/Special:This_page_does_not_exist_af8s7d6f".to_string(),
        max_length: None,
        start_index: None,
        find: None,
        user_agent: None,
    };
    let result = tool
        .call(&mut ToolContext::new(), args)
        .await
        .expect("a 404 is still Ok(..) with the error status recorded, not an Err");
    assert_eq!(result.status, 404);
    assert!(
        !result.content.contains("<html"),
        "raw markup leaked into a 404 body: {:?}",
        &result.content[..200.min(result.content.len())]
    );
    assert!(
        result.content.len() <= ERROR_MAX_LENGTH + 100,
        "404 body was not capped: {} bytes",
        result.content.len()
    );
}

#[test]
fn test_is_binary_content_type() {
    assert!(is_binary_content_type("image/png"));
    assert!(is_binary_content_type("image/jpeg"));
    assert!(is_binary_content_type("application/pdf"));
    assert!(is_binary_content_type("application/zip"));
    assert!(is_binary_content_type("application/octet-stream"));
    assert!(is_binary_content_type(
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    ));
    assert!(!is_binary_content_type("text/html"));
    assert!(!is_binary_content_type("text/plain"));
    assert!(!is_binary_content_type("application/json"));
}

#[test]
fn test_extract_filename_from_url() {
    assert_eq!(
        extract_filename("https://example.com/photo.png", "image/png"),
        "photo.png"
    );
    assert_eq!(
        extract_filename("https://example.com/docs/report.pdf?v=2", "application/pdf"),
        "report.pdf"
    );
    assert_eq!(
        extract_filename("https://example.com/", "image/png"),
        "download.png"
    );
    assert_eq!(
        extract_filename("https://example.com/archive", "application/zip"),
        "download.zip"
    );
}

#[test]
fn save_new_file_takes_the_name_when_free() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    assert_eq!(save_new_file(path.clone(), b"x").unwrap(), path);
    assert_eq!(std::fs::read(&path).unwrap(), b"x");
}

#[test]
fn save_new_file_never_replaces_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    std::fs::write(&path, b"old").unwrap();
    let first = save_new_file(path.clone(), b"new").unwrap();
    assert_eq!(first, dir.path().join("a-1.png"));
    assert_eq!(std::fs::read(&first).unwrap(), b"new");
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    let second = save_new_file(path, b"new").unwrap();
    assert_eq!(second, dir.path().join("a-2.png"));
}

#[cfg(unix)]
#[test]
fn save_new_file_does_not_write_through_a_dangling_symlink() {
    let ws = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target.pdf");
    std::os::unix::fs::symlink(&target, ws.path().join("report.pdf")).unwrap();
    let saved = save_new_file(ws.path().join("report.pdf"), b"x").unwrap();
    assert_eq!(saved, ws.path().join("report-1.pdf"));
    assert!(!target.exists());
}

// --- SSRF protection tests ---
// Low-level is_private_ip/is_blocked_hostname coverage lives in
// services::ssrf_guard, which this tool's validate_url_host delegates to.

#[test]
fn test_validate_url_host_blocks_private() {
    assert!(validate_url_host("http://127.0.0.1/secret").is_err());
    assert!(validate_url_host("http://localhost:8080/admin").is_err());
    assert!(validate_url_host("http://169.254.169.254/latest/meta-data/").is_err());
    assert!(validate_url_host("http://10.0.0.1/internal").is_err());
    assert!(validate_url_host("http://192.168.1.1/router").is_err());
    assert!(validate_url_host("http://172.16.0.5/service").is_err());
    assert!(validate_url_host("http://[::1]/secret").is_err());
    assert!(validate_url_host("http://metadata.google.internal/computeMetadata/v1/").is_err());
}

#[test]
fn test_validate_url_host_allows_public() {
    assert!(validate_url_host("https://example.com").is_ok());
    assert!(validate_url_host("https://docs.rs/rig-core/latest").is_ok());
}

/// AGE-537: the name is resolved once, by the client that connects, and
/// that answer is checked. A rebinding name whose answer turns private is
/// refused at connect time: the private server never sees a request.
#[tokio::test]
async fn fetch_connects_only_to_the_checked_address() {
    use crate::services::ssrf_guard::HostLookup;
    use std::future::Future;
    use std::net::IpAddr;
    use std::pin::Pin;

    struct Rebinds;
    impl HostLookup for Rebinds {
        fn lookup(
            &self,
            _host: &str,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
            Box::pin(async { Ok(vec!["127.0.0.1".parse().unwrap()]) })
        }
    }
    struct Fails;
    impl HostLookup for Fails {
        fn lookup(
            &self,
            _host: &str,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
            Box::pin(async { Err(std::io::Error::other("stub: resolver down")) })
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let url = format!("http://rebind.test:{port}/admin");
    for lookup in [
        std::sync::Arc::new(Rebinds) as std::sync::Arc<dyn HostLookup>,
        std::sync::Arc::new(Fails),
    ] {
        let tool = FetchTool {
            client: crate::services::http_client::open_web_client_with(5, lookup),
            ..FetchTool::new(None)
        };
        let err = tool
            .send(&url, None)
            .await
            .expect_err("a private or unresolvable answer must be refused");
        let message = format!("{err}");
        assert!(message.contains("Request failed"), "{message}");
    }
    // A connection, had one been made, would be waiting in the backlog.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "the private server must never be connected to"
    );
}

/// A fake extract endpoint: answers every request with `body` and logs
/// the raw requests it got.
async fn fake_extract(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/managed/tavily_extract",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let want = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if rest.len() >= want {
                            break;
                        }
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).into_owned());
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

/// A tool whose direct client can only reach `direct` (for
/// `pages.example.test`), so a direct fetch is observable.
fn tool_with_direct_listener(direct: std::net::SocketAddr) -> FetchTool {
    FetchTool {
        client: reqwest::Client::builder()
            .resolve("pages.example.test", direct)
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap(),
        ..FetchTool::new(None)
    }
}

fn fetch_args(url: &str) -> FetchToolArgs {
    FetchToolArgs {
        url: url.to_string(),
        max_length: None,
        start_index: None,
        find: None,
        user_agent: None,
    }
}

#[tokio::test]
async fn hosted_fetch_uses_managed_extract() {
    let (base, seen) = fake_extract(
        r##"{"results":[{"url":"https://pages.example.test/a","raw_content":"# Title\n\nThe needle is here."}],"failed_results":[]}"##,
    )
    .await;
    let direct = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = direct.local_addr().unwrap().port();
    let tool = tool_with_direct_listener(direct.local_addr().unwrap())
        .with_managed_extract(&base, "lease-token-for-test");
    let url = format!("https://pages.example.test:{port}/a");
    let out = tool
        .call(&mut ToolContext::new(), fetch_args(&url))
        .await
        .unwrap();
    assert_eq!(out.status, 200);
    assert!(out.content.contains("The needle is here."));
    // Paging and find read the cached text without another request.
    let found = tool
        .call(
            &mut ToolContext::new(),
            FetchToolArgs {
                find: Some("needle".into()),
                ..fetch_args(&url)
            },
        )
        .await
        .unwrap();
    assert!(found.content.contains("needle"));
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "exactly one extract request");
    let request = requests[0].to_ascii_lowercase();
    assert!(request.starts_with("post /managed/tavily_extract/extract "));
    assert!(request.contains("authorization: bearer lease-token-for-test"));
    assert!(request.contains(r#""urls":["#) && request.contains(r#""format":"markdown""#));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), direct.accept())
            .await
            .is_err(),
        "no direct connection to the page's host"
    );
}

/// The hosted guest has no resolver: the page's host is never looked up.
#[tokio::test]
async fn hosted_fetch_needs_no_dns() {
    let (base, seen) = fake_extract(
        r#"{"results":[{"url":"https://example.invalid/page","raw_content":"hello"}],"failed_results":[]}"#,
    )
    .await;
    let tool = FetchTool::new(None).with_managed_extract(&base, "t");
    let out = tool
        .call(
            &mut ToolContext::new(),
            fetch_args("https://example.invalid/page"),
        )
        .await
        .unwrap();
    assert_eq!(out.content, "hello");
    assert_eq!(seen.lock().unwrap().len(), 1, "exactly one POST");
}

#[tokio::test]
async fn hosted_fetch_reports_failed_result() {
    let (base, _) = fake_extract(
        r#"{"results":[],"failed_results":[{"url":"https://pages.example.test/x","error":"timed out"}]}"#,
    )
    .await;
    let tool = FetchTool::new(None).with_managed_extract(&base, "t");
    let out = tool
        .call(
            &mut ToolContext::new(),
            fetch_args("https://pages.example.test/x"),
        )
        .await
        .unwrap();
    assert_eq!(out.status, 0);
    assert!(
        out.content.contains("Couldn't read this page"),
        "{}",
        out.content
    );
    assert!(out.content.contains("timed out"));
}

#[tokio::test]
async fn desktop_fetch_unchanged() {
    let (base, seen) = mock_server(|_, _, _| {
        (
            "200 OK",
            "content-type: text/plain\r\n".into(),
            "the data".into(),
        )
    })
    .await;
    let tool = tool_with_archive(&base);
    assert!(tool.managed_extract.is_none());
    assert!(!tool.description().contains("hosted"));
    let page = fetch_page(&tool, &format!("{base}/data")).await;
    assert_eq!(page.text, "the data");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/data");
}

#[tokio::test]
async fn test_fetch_tool_blocks_ssrf() {
    let tool = FetchTool::new(None);
    let args = FetchToolArgs {
        url: "http://169.254.169.254/latest/meta-data/".to_string(),
        max_length: None,
        start_index: None,
        find: None,
        user_agent: None,
    };
    let result = tool.call(&mut ToolContext::new(), args).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("SSRF"),
        "Error should mention SSRF: {}",
        err_msg
    );
}

/// Serve one raw HTTP response (`head`, then `body_len` bytes of `a`) on a
/// loopback listener and return the client's `reqwest::Response`. The
/// server ignores write errors: the client may stop reading early. With
/// `hold`, the socket stays open afterwards instead of closing.
async fn raw_response(
    head: &'static str,
    body_len: usize,
    hold: bool,
) -> (reqwest::Response, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 16 * 1024];
        let _ = socket.read(&mut buf).await;
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&vec![b'a'; body_len]).await;
        if hold {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    let response = reqwest::Client::new().get(&url).send().await.unwrap();
    (response, url)
}

#[tokio::test]
async fn read_capped_stops_at_the_cap() {
    let (response, _) =
        raw_response("HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n", 65536, false).await;
    let (body, cut) = read_capped(response, 1024).await.unwrap();
    assert_eq!(body.len(), 1024);
    assert!(cut);
}

/// Serve one canned response (`head` then `body`) on a local port.
async fn one_response_server(head: &'static str, body: Vec<u8>, hold_open: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&body).await;
        if hold_open {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    format!("http://{addr}/page")
}

#[test]
fn decode_text_follows_the_declared_charset() {
    assert_eq!(
        decode_text(&[b'c', b'a', b'f', 0xE9], "text/html; charset=ISO-8859-1"),
        "café"
    );
    assert_eq!(decode_text("café".as_bytes(), "text/html"), "café");
    assert_eq!(
        decode_text("café".as_bytes(), "text/plain; charset=\"UTF-8\""),
        "café"
    );
    assert_eq!(
        decode_text(b"plain", "text/html; charset=no-such-charset"),
        "plain"
    );
}

#[tokio::test]
async fn read_text_capped_stops_at_the_cap_without_waiting_for_the_end() {
    let url = one_response_server(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n",
        vec![b'a'; 65536],
        true,
    )
    .await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client.get(&url).send().await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_text_capped(response, 1024),
    )
    .await;
    match result {
        Ok(Ok((text, true))) => assert_eq!(text, "a".repeat(1024)),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn read_text_capped_reads_a_short_body_whole() {
    let url = one_response_server(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: 6\r\nconnection: close\r\n\r\n",
        "héllo".as_bytes().to_vec(),
        false,
    )
    .await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client.get(&url).send().await.unwrap();
    let result = read_text_capped(response, 1024).await.unwrap();
    assert_eq!(result, ("héllo".to_string(), false));
}

#[tokio::test]
async fn read_capped_keeps_a_body_under_the_cap() {
    let (response, _) =
        raw_response("HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n", 512, false).await;
    let (body, cut) = read_capped(response, 1024).await.unwrap();
    assert_eq!(body, vec![b'a'; 512]);
    assert!(!cut);
}

#[tokio::test]
async fn binary_download_refuses_a_declared_oversize_body_without_reading_it() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = FetchTool::new(Some(tmp.path().to_path_buf()));
    let (response, url) = raw_response(
        "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: 10485761\r\n\r\n",
        0,
        true,
    )
    .await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.handle_binary_response(response, &url, 200, "application/octet-stream"),
    )
    .await
    .expect("must refuse without waiting for the body");
    let err = result.expect_err("oversize declared length must be refused");
    assert!(err.to_string().contains("too large"), "{err}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn binary_download_refuses_an_oversize_body_with_no_length() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = FetchTool::new(Some(tmp.path().to_path_buf()));
    let (response, url) = raw_response(
        "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n",
        MAX_BINARY_BYTES + 1,
        false,
    )
    .await;
    let result = tool
        .handle_binary_response(response, &url, 200, "application/octet-stream")
        .await;
    let err = result.expect_err("oversize body must be refused");
    assert!(err.to_string().contains("too large"), "{err}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}
