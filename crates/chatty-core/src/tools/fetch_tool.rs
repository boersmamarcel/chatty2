#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use crate::tools::ToolError;

/// Default and largest window of page text one call returns, in bytes of
/// UTF-8 text.
///
/// Sized like `read_file`'s cap: on a 32k-token window the context shaper
/// records a tool result whole only up to two fifths of the history budget
/// (~7k tokens), and cuts anything larger from the tail — which is where the
/// "continue with start_index" note lives. The old 50 KB default (~13k
/// tokens) was always cut there, so the model got a third of the window and
/// no way to page on; it re-downloaded the page with `curl` instead. 20k is
/// ~5-6k tokens of prose, a little more for number-dense tables.
const MAX_WINDOW: usize = 20_000;

/// Text kept around each `find` match: less before (the lead-in), more after
/// (the figures a heading or label introduces usually follow it).
const FIND_CONTEXT_BEFORE: usize = 300;
const FIND_CONTEXT_AFTER: usize = 900;

/// Passages shown when `find` has no exact match and falls back to ranking.
const FIND_FALLBACK_PASSAGES: usize = 3;

/// Pages kept in a session's page cache, and the most text they may hold
/// together; the oldest page is dropped first.
const MAX_CACHED_PAGES: usize = 16;
const MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;

/// Maximum binary response size in bytes (10 MB)
const MAX_BINARY_BYTES: usize = 10 * 1024 * 1024;

/// Request timeout in seconds
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maximum length of a non-2xx error body, in bytes of UTF-8 text.
///
/// An error page's useful content — the message a 404 or 429 actually wants
/// to convey — is rarely more than a short paragraph; the rest is markup the
/// model never needed (AGE-508). Applied on top of `max_length` (the smaller
/// of the two wins) rather than replacing it, so a caller who explicitly
/// asks for a smaller window still gets it.
const ERROR_MAX_LENGTH: usize = 2_048;

/// Times a 429 is retried before its answer is returned as is.
const MAX_RATE_LIMIT_RETRIES: u32 = 2;

/// Longest `Retry-After` the tool waits out; a server asking for longer gets
/// its 429 passed back to the model instead of stalling the turn.
const MAX_RETRY_AFTER_SECS: u64 = 10;

/// Where the Wayback Machine fallback looks up and reads archived copies.
const WAYBACK_AVAILABLE_URL: &str = "https://archive.org/wayback/available";
const WAYBACK_WEB_URL: &str = "https://web.archive.org";

/// How long the Wayback availability lookup may take. It runs on every
/// blocked or dead page, and archive.org can be slow: a lookup that has not
/// answered by then is dropped and the live answer returned, so a guessed
/// URL that 404s costs the turn seconds, not the 30 s request timeout.
const WAYBACK_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Arguments for the fetch tool
#[derive(Deserialize, Serialize)]
pub struct FetchToolArgs {
    /// The URL to fetch
    pub url: String,
    /// Maximum length of the returned content, in bytes of UTF-8 text
    /// (default and maximum: [`MAX_WINDOW`])
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Byte offset into the *extracted* text to start the returned window at
    /// (default: 0). Continues a fetch that came back truncated without
    /// re-fetching from the top (AGE-507).
    #[serde(default)]
    pub start_index: Option<usize>,
    /// A word or phrase to look for: return the passages that contain it,
    /// each with the `start_index` to read on from, instead of a window.
    #[serde(default)]
    pub find: Option<String>,
    /// Override the User-Agent header for this request. Useful when a site's
    /// automated-traffic policy asks for a specific declared identity (e.g.
    /// SEC EDGAR — AGE-496) that the default Chatty user-agent doesn't satisfy.
    #[serde(default)]
    pub user_agent: Option<String>,
}

/// Output from the fetch tool
#[derive(Debug, Serialize)]
pub struct FetchToolOutput {
    /// HTTP status code
    pub status: u16,
    /// The readable text content of the response (empty for binary responses that were saved to disk)
    pub content: String,
    /// The content type of the response
    pub content_type: String,
    /// Whether the content was truncated due to max_length
    pub truncated: bool,
    /// Length of the page's whole extracted text, so the model can tell how
    /// much a window or a `find` covered (absent for saved binary files)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_length: Option<usize>,
    /// Path to the saved file (only present for binary content like images, PDFs, zips)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_to: Option<String>,
    /// What the tool did on the way to this answer: waited out a rate
    /// limit, retried with a browser User-Agent, or served an archived copy
    /// (with its capture date) because the live page was blocked or gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Native fetch tool that provides read-only HTTP GET access to web content.
///
/// Converts HTML responses to readable plain text, preserving non-HTML content as-is.
/// Binary content (images, PDFs, zip files) is saved to the workspace directory.
/// Enforces timeouts, size limits, and HTTPS preference for safety.
#[derive(Clone)]
pub struct FetchTool {
    /// Reusable HTTP client with connection pooling, timeout, and SSRF-safe redirect policy.
    client: reqwest::Client,
    /// Optional workspace directory for saving downloaded binary files.
    /// When None, binary content returns an error asking the user to configure a workspace.
    workspace_dir: Option<PathBuf>,
    /// Extracted text of the pages this session fetched, so paging and
    /// `find` over a long document don't download it again. Shared by
    /// clones, i.e. by the one agent the tool was built for.
    pages: PageCache,
    /// Wayback Machine endpoints: the availability lookup and the snapshot
    /// host. Fields so tests can point them at a local server.
    wayback_available_url: String,
    wayback_web_url: String,
    wayback_lookup_timeout: std::time::Duration,
}

impl FetchTool {
    pub fn new(workspace_dir: Option<PathBuf>) -> Self {
        let client = crate::services::http_client::no_redirect_client(REQUEST_TIMEOUT_SECS);
        Self {
            client,
            workspace_dir,
            pages: PageCache::default(),
            wayback_available_url: WAYBACK_AVAILABLE_URL.to_string(),
            wayback_web_url: WAYBACK_WEB_URL.to_string(),
            wayback_lookup_timeout: WAYBACK_LOOKUP_TIMEOUT,
        }
    }

    /// Build a GET request, overriding the client's default User-Agent when
    /// the caller declared one (AGE-496).
    fn request(&self, url: &str, user_agent: Option<&str>) -> reqwest::RequestBuilder {
        let request = self.client.get(url);
        match user_agent {
            Some(ua) => request.header(reqwest::header::USER_AGENT, ua),
            None => request,
        }
    }
}

impl Tool for FetchTool {
    const NAME: &'static str = "fetch";
    type Error = ToolError;
    type Args = FetchToolArgs;
    type Output = FetchToolOutput;

    fn description(&self) -> String {
        "Fetch a URL and return its content. HTML is converted to readable text, \
         with links kept as `text (url)` and table cells separated by ` | `. \
         A long page comes back one window at a time: continue with the `start_index` \
         the result names, or pass `find` to get only the passages that mention a word \
         or phrase, each with the start_index to read on from. Pages are cached for the \
         session, so paging and find don't download again. \
         A URL with a #fragment starts at that section. \
         Binary content (images, PDFs, zip files, etc.) is saved to the workspace directory. \
         Rate limits are waited out; a page that is blocked or gone is served from the \
         Wayback Machine when an archived copy exists, and the result's `note` says so. \
         Only performs GET requests (read-only)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch. HTTPS is preferred for security."
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum length of returned content, in bytes of UTF-8 text. Defaults to (and is capped at) 20000 — leave it unset unless you deliberately want a smaller window."
                },
                "start_index": {
                    "type": "integer",
                    "description": "Where to start the returned window in the extracted text, as a byte offset into it. Defaults to 0. When a response comes back truncated it names the start_index to continue from, so pass that instead of re-fetching the page. With `find`, only text from here on is searched."
                },
                "find": {
                    "type": "string",
                    "description": "A word or phrase to look for in the page (case-insensitive). Returns only the passages that contain it, each labelled with its start_index. Use it on long documents instead of paging through them."
                },
                "user_agent": {
                    "type": "string",
                    "description": "Override the User-Agent header for this request. Use this when a site rejects the default identity and states what it expects (e.g. \"declare an automated tool with contact info\")."
                }
            },
            "required": ["url"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let url = args.url.trim().to_string();
        let max_length = args.max_length.unwrap_or(MAX_WINDOW).min(MAX_WINDOW);
        let start_index = args.start_index.unwrap_or(0);
        let find = args
            .find
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty());
        // The fragment is a client-side instruction — it never reaches the
        // server, and a redirect can drop it — so read it off what was asked
        // for, not off the URL we ended up at.
        let fragment = url
            .split_once('#')
            .map(|(_, fragment)| fragment.to_string())
            .filter(|fragment| !fragment.is_empty());

        // Validate URL scheme
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ToolError::OperationFailed(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        // SSRF protection: block requests to private/internal networks
        validate_url_host(&url)?;

        // A page is read from the cache only to page through it or search
        // it (`start_index` or `find`); a plain fetch downloads it again, so
        // a page that changes (a status endpoint, a live feed) is never
        // served stale. The User-Agent is part of the key: a retry with the
        // identity a site asked for must not get back the page it refused.
        let cache_key = format!("{url}\n{}", args.user_agent.as_deref().unwrap_or(""));
        let reuse = args.start_index.is_some() || find.is_some();
        let cached = if reuse {
            self.pages.get(&cache_key)
        } else {
            None
        };
        let page = match cached {
            Some(page) => {
                info!(url = %url, "Serving fetch from the session page cache");
                page
            }
            None => match self
                .download(
                    &url,
                    args.user_agent.as_deref(),
                    fragment.as_deref(),
                    start_index,
                    max_length,
                )
                .await?
            {
                Download::Page(page) => {
                    let page = Arc::new(page);
                    self.pages.insert(cache_key, Arc::clone(&page));
                    page
                }
                Download::Done(output) => return Ok(output),
            },
        };

        let total_length = page.text.len();
        let (content, truncated) = match find {
            Some(find) => find_passages(&page.text, find, start_index, max_length),
            None => window(&page.text, start_index, max_length, WINDOW_HINT),
        };
        if truncated {
            warn!(
                original_len = total_length,
                start_index = start_index,
                max_length = max_length,
                "Truncating response content"
            );
        }

        Ok(FetchToolOutput {
            status: page.status,
            content,
            content_type: page.content_type.clone(),
            truncated,
            total_length: Some(total_length),
            saved_to: None,
            note: page.note.clone(),
        })
    }
}

/// A successful text response, reduced to what paging and `find` read.
struct CachedPage {
    status: u16,
    content_type: String,
    /// The extracted text: HTML already converted, starting at the
    /// requested #fragment when the page had it.
    text: String,
    /// How the page was obtained when that was not a plain GET (see
    /// [`FetchToolOutput::note`]).
    note: Option<String>,
}

/// What one download produced: a text page to window (and cache), or an
/// answer that is already final — an error status or a saved binary file.
enum Download {
    Page(CachedPage),
    Done(FetchToolOutput),
}

/// The session's recently fetched pages, keyed by the URL as asked for
/// (fragment included, since it decides where the text starts) and the
/// User-Agent override, least recently used first. Bounded by [`MAX_CACHED_PAGES`] and [`MAX_CACHED_BYTES`].
#[derive(Clone, Default)]
struct PageCache(Arc<Mutex<CachedPages>>);

/// `(url, page)`, least recently used first.
type CachedPages = VecDeque<(String, Arc<CachedPage>)>;

impl PageCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, CachedPages> {
        // The data is a plain cache; a panic mid-insert leaves nothing a
        // later reader can't use.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The cached page for `url`, marked most recently used.
    fn get(&self, url: &str) -> Option<Arc<CachedPage>> {
        let mut pages = self.lock();
        let ix = pages.iter().position(|(key, _)| key == url)?;
        let entry = pages.remove(ix)?;
        let page = Arc::clone(&entry.1);
        pages.push_back(entry);
        Some(page)
    }

    fn insert(&self, url: String, page: Arc<CachedPage>) {
        if page.text.len() > MAX_CACHED_BYTES {
            return;
        }
        let mut pages = self.lock();
        pages.retain(|(key, _)| *key != url);
        pages.push_back((url, page));
        let mut bytes: usize = pages.iter().map(|(_, page)| page.text.len()).sum();
        while pages.len() > MAX_CACHED_PAGES || bytes > MAX_CACHED_BYTES {
            let Some((_, dropped)) = pages.pop_front() else {
                break;
            };
            bytes -= dropped.text.len();
        }
    }
}

impl FetchTool {
    /// GET `url` and turn the response into either a page of extracted
    /// text or a final answer (an error status's body, or a binary file
    /// saved to the workspace).
    ///
    /// Recovers from the failures a person at a browser would work around:
    /// 1. 429: wait out `Retry-After` (or a short backoff) and try again.
    /// 2. 403 with the default User-Agent: retry once as a browser.
    /// 3. Still blocked, or 404/410: serve the Wayback Machine's closest
    ///    archived copy, labelled with its capture date.
    async fn download(
        &self,
        url: &str,
        user_agent: Option<&str>,
        fragment: Option<&str>,
        start_index: usize,
        max_length: usize,
    ) -> Result<Download, ToolError> {
        info!(url = %url, max_length = max_length, "Fetching URL");

        let mut notes: Vec<String> = Vec::new();
        let mut agent = user_agent.map(str::to_string);
        let mut rate_limit_retries = 0;
        let (response, current_url) = loop {
            let (response, current_url) = self.send(url, agent.as_deref()).await?;
            let status = response.status().as_u16();
            if status == 429 && rate_limit_retries < MAX_RATE_LIMIT_RETRIES {
                match retry_delay(response.headers(), rate_limit_retries) {
                    Ok(delay) => {
                        info!(url = %url, delay_secs = delay.as_secs(), "Rate limited; retrying");
                        notes.push(format!(
                            "Rate limited (429); waited {} s and retried.",
                            delay.as_secs()
                        ));
                        tokio::time::sleep(delay).await;
                        rate_limit_retries += 1;
                        continue;
                    }
                    Err(asked) => notes.push(format!(
                        "Rate limited (429); the server asks to wait {asked} s, longer than \
                         this tool waits. Try again later or use another source."
                    )),
                }
            }
            if status == 403 && user_agent.is_none() && agent.is_none() {
                info!(url = %url, "Forbidden with the default User-Agent; retrying as a browser");
                notes.push(
                    "The default User-Agent got 403; retried with a browser User-Agent."
                        .to_string(),
                );
                agent = Some(crate::services::http_client::BROWSER_USER_AGENT.to_string());
                continue;
            }
            break (response, current_url);
        };

        let status = response.status().as_u16();
        if matches!(status, 403 | 404 | 410) && !is_archive_url(url) {
            match self.archived_copy(url).await {
                Some((snapshot_url, captured)) => {
                    notes.push(format!(
                        "The live page returned {status}. This is an ARCHIVED copy from the \
                         Wayback Machine, captured {captured} ({snapshot_url}); it may differ \
                         from the current page."
                    ));
                    let (snapshot, snapshot_final) = self.send(&snapshot_url, None).await?;
                    if snapshot.status().is_success() {
                        return self
                            .read_response(
                                snapshot,
                                &snapshot_final,
                                url,
                                fragment,
                                start_index,
                                max_length,
                                notes,
                            )
                            .await;
                    }
                    notes.pop();
                    notes.push(format!(
                        "The Wayback Machine lists a copy captured {captured}, but reading it \
                         returned {}.",
                        snapshot.status().as_u16()
                    ));
                }
                None => notes.push(format!(
                    "The live page returned {status} and the Wayback Machine has no archived copy."
                )),
            }
        }

        self.read_response(
            response,
            &current_url,
            url,
            fragment,
            start_index,
            max_length,
            notes,
        )
        .await
    }

    /// GET `url`, following redirects manually (max 10 hops) to validate
    /// each redirect target against the private-host denylist. Returns the
    /// final response and the URL it came from.
    async fn send(
        &self,
        url: &str,
        user_agent: Option<&str>,
    ) -> Result<(reqwest::Response, String), ToolError> {
        let mut current_url = url.to_string();
        let mut response = self
            .request(&current_url, user_agent)
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Request failed: {}", e)))?;

        for _ in 0..10 {
            if !response.status().is_redirection() {
                break;
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    ToolError::OperationFailed(
                        "Redirect response missing Location header".to_string(),
                    )
                })?
                .to_string();

            // Resolve relative redirects against the current URL
            let next_url = if location.starts_with("http://") || location.starts_with("https://") {
                location
            } else {
                // Relative URL — resolve against current
                let base = reqwest::Url::parse(&current_url).map_err(|e| {
                    ToolError::OperationFailed(format!("Invalid base URL for redirect: {}", e))
                })?;
                base.join(&location)
                    .map_err(|e| {
                        ToolError::OperationFailed(format!("Invalid redirect URL: {}", e))
                    })?
                    .to_string()
            };

            // SSRF protection: validate redirect target
            validate_url_host(&next_url)?;

            info!(from = %current_url, to = %next_url, "Following redirect");
            current_url = next_url;

            response = self
                .request(&current_url, user_agent)
                .send()
                .await
                .map_err(|e| ToolError::OperationFailed(format!("Redirect failed: {}", e)))?;
        }
        Ok((response, current_url))
    }

    /// The Wayback Machine's closest good capture of `url`, as the snapshot
    /// URL to read (raw, without the archive's toolbar) and a readable
    /// capture date. `None` when there is none or the lookup fails — the
    /// fallback is best-effort and never hides the live answer.
    async fn archived_copy(&self, url: &str) -> Option<(String, String)> {
        #[derive(Deserialize)]
        struct Available {
            archived_snapshots: Snapshots,
        }
        #[derive(Deserialize)]
        struct Snapshots {
            closest: Option<Closest>,
        }
        #[derive(Deserialize)]
        struct Closest {
            available: bool,
            status: String,
            timestamp: String,
        }

        let lookup =
            reqwest::Url::parse_with_params(&self.wayback_available_url, &[("url", url)]).ok()?;
        let response = self
            .client
            .get(lookup)
            .timeout(self.wayback_lookup_timeout)
            .send()
            .await
            .map_err(|e| warn!(error = %e, "Wayback availability lookup failed"))
            .ok()?;
        if !response.status().is_success() {
            warn!(
                status = response.status().as_u16(),
                "Wayback availability lookup failed"
            );
            return None;
        }
        let available: Available = response
            .json()
            .await
            .map_err(|e| warn!(error = %e, "Unreadable Wayback availability answer"))
            .ok()?;
        let closest = available.archived_snapshots.closest?;
        if !closest.available || closest.status != "200" {
            return None;
        }
        let timestamp = closest.timestamp;
        let snapshot = format!("{}/web/{timestamp}id_/{url}", self.wayback_web_url);
        Some((snapshot, capture_date(&timestamp)))
    }

    /// Turn a final response into a [`Download`], attaching `notes`.
    #[allow(clippy::too_many_arguments)]
    async fn read_response(
        &self,
        response: reqwest::Response,
        current_url: &str,
        url: &str,
        fragment: Option<&str>,
        start_index: usize,
        max_length: usize,
        notes: Vec<String>,
    ) -> Result<Download, ToolError> {
        let note = (!notes.is_empty()).then(|| notes.join(" "));
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        info!(status = status, content_type = %content_type, "Received response");

        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            let (content, truncated) = error_body_window(
                &body,
                &content_type,
                Some(current_url),
                fragment,
                start_index,
                max_length,
            );
            return Ok(Download::Done(FetchToolOutput {
                status,
                content,
                content_type,
                truncated,
                total_length: None,
                saved_to: None,
                note,
            }));
        }

        // Determine if this is binary content that should be saved to disk
        if is_binary_content_type(&content_type) {
            return self
                .handle_binary_response(response, url, status, &content_type)
                .await
                .map(|output| Download::Done(FetchToolOutput { note, ..output }));
        }

        // Read body text
        let body = response.text().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to read response body: {}", e))
        })?;

        // Convert HTML to readable text if appropriate. Relative links resolve
        // against the URL the body actually came from, i.e. after redirects.
        let is_html = content_type.contains("text/html") || looks_like_html(&body);
        let text = if is_html {
            html_to_text(&body, Some(current_url), fragment)
        } else {
            body
        };

        Ok(Download::Page(CachedPage {
            status,
            content_type,
            text,
            note,
        }))
    }
}

/// How long to wait before retrying a 429: the server's `Retry-After` in
/// seconds when it gives one, else 2 s, 4 s, ... by attempt. `Err` carries
/// a `Retry-After` longer than [`MAX_RETRY_AFTER_SECS`].
fn retry_delay(
    headers: &reqwest::header::HeaderMap,
    attempt: u32,
) -> Result<std::time::Duration, u64> {
    let asked = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    match asked {
        Some(secs) if secs > MAX_RETRY_AFTER_SECS => Err(secs),
        Some(secs) => Ok(std::time::Duration::from_secs(secs)),
        None => Ok(std::time::Duration::from_secs(2u64 << attempt)),
    }
}

/// Whether `url` already points into the Internet Archive, where a Wayback
/// fallback would only look itself up.
fn is_archive_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|host| host == "archive.org" || host.ends_with(".archive.org"))
}

/// A Wayback timestamp (`YYYYMMDDhhmmss`) as `YYYY-MM-DD hh:mm:ss UTC`;
/// anything else is returned as is.
fn capture_date(timestamp: &str) -> String {
    if timestamp.len() == 14 && timestamp.bytes().all(|b| b.is_ascii_digit()) {
        let t = timestamp;
        format!(
            "{}-{}-{} {}:{}:{} UTC",
            &t[0..4],
            &t[4..6],
            &t[6..8],
            &t[8..10],
            &t[10..12],
            &t[12..14]
        )
    } else {
        timestamp.to_string()
    }
}

impl FetchTool {
    /// Handle binary responses by saving them to the workspace directory.
    async fn handle_binary_response(
        &self,
        response: reqwest::Response,
        url: &str,
        status: u16,
        content_type: &str,
    ) -> Result<FetchToolOutput, ToolError> {
        let workspace = self.workspace_dir.as_ref().ok_or_else(|| {
            ToolError::OperationFailed(
                "Cannot download binary files: no workspace directory configured. \
                 Set a workspace directory in Settings > Code Execution to enable file downloads."
                    .to_string(),
            )
        })?;

        // Read binary body
        let bytes = response.bytes().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to read response body: {}", e))
        })?;

        if bytes.len() > MAX_BINARY_BYTES {
            return Err(ToolError::OperationFailed(format!(
                "Response too large: {} bytes (max {} bytes / {} MB)",
                bytes.len(),
                MAX_BINARY_BYTES,
                MAX_BINARY_BYTES / 1024 / 1024,
            )));
        }

        // Extract filename from URL or Content-Disposition header
        let filename = extract_filename(url, content_type);
        let save_path = workspace.join(&filename);

        // Ensure we don't overwrite existing files — add a numeric suffix if needed
        let save_path = unique_path(save_path);

        info!(
            path = %save_path.display(),
            size = bytes.len(),
            "Saving binary content to workspace"
        );

        tokio::fs::write(&save_path, &bytes).await.map_err(|e| {
            ToolError::OperationFailed(format!(
                "Failed to save file to {}: {}",
                save_path.display(),
                e
            ))
        })?;

        Ok(FetchToolOutput {
            status,
            content: format!(
                "Downloaded {} ({} bytes) and saved to: {}",
                content_type,
                bytes.len(),
                save_path.display()
            ),
            content_type: content_type.to_string(),
            truncated: false,
            total_length: None,
            saved_to: Some(save_path.to_string_lossy().to_string()),
            note: None,
        })
    }
}

/// Check if a content type indicates binary content that should be saved to disk.
fn is_binary_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.contains("application/pdf")
        || ct.contains("application/zip")
        || ct.contains("application/gzip")
        || ct.contains("application/x-tar")
        || ct.contains("application/x-gzip")
        || ct.contains("application/x-bzip2")
        || ct.contains("application/x-7z")
        || ct.contains("application/x-rar")
        || ct.contains("application/octet-stream")
        || ct.contains("application/vnd.openxmlformats") // docx, xlsx, pptx
        || ct.contains("application/msword")
        || ct.contains("application/vnd.ms-")
        || ct.contains("application/wasm")
}

/// Extract a reasonable filename from a URL and content type.
fn extract_filename(url: &str, content_type: &str) -> String {
    // Try to get filename from the URL path
    if let Some(path_segment) = url.split('?').next().and_then(|u| u.rsplit('/').next()) {
        let decoded = path_segment.to_string();
        if !decoded.is_empty() && decoded.contains('.') && decoded.len() <= 255 {
            // Sanitize: only keep alphanumeric, dots, hyphens, underscores
            let sanitized: String = decoded
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if !sanitized.is_empty() && sanitized != "." && sanitized != ".." {
                return sanitized;
            }
        }
    }

    // Fallback: generate name from content type
    let extension = match content_type.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "audio/mpeg" => "mp3",
        "video/mp4" => "mp4",
        _ => "bin",
    };
    format!("download.{}", extension)
}

/// Generate a unique file path by appending a numeric suffix if the file already exists.
fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let parent = path.parent().unwrap_or(std::path::Path::new("."));

    for i in 1..1000 {
        let candidate = parent.join(format!("{}-{}.{}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
    }

    // Extremely unlikely fallback
    parent.join(format!("{}-{}.{}", stem, uuid::Uuid::new_v4(), ext))
}

/// Validate that a URL does not target private, internal, or reserved network hosts.
///
/// Delegates to the shared SSRF guard (`services::ssrf_guard`) so this tool and the
/// browser's internet-enabled navigation policy enforce the same denylist.
fn validate_url_host(url: &str) -> Result<(), ToolError> {
    crate::services::ssrf_guard::check_public_host(url)
        .map_err(|reason| ToolError::OperationFailed(format!("Access denied: {reason}")))
}

/// Simple heuristic to detect HTML content when content-type is missing or ambiguous
fn looks_like_html(body: &str) -> bool {
    let trimmed = body.trim_start();
    trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
}

/// The largest char boundary at or before `index`, so a window never splits a
/// multi-byte character. Both edges of a window use this one rule, which is
/// what makes consecutive windows meet exactly (see `window`).
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut end = index.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Appended to the truncation note of a page window: the page is cached, so
/// `find` is a cheap way to the part that matters.
const WINDOW_HINT: &str = ", or pass `find` to jump to the passage you need";

/// Take the `max_length` window of `s` that starts at `start_index`, returning
/// it with a flag saying whether anything was left over.
///
/// Truncation is not a dead end: the note says where the window sits in the
/// whole text and names the `start_index` to continue from (then `hint`), and
/// because both the end of one window and the start of the next are snapped
/// with `floor_char_boundary`, `(0, n)` followed by `(n, n)` reproduce `s`
/// with no gap and no overlap (AGE-507).
fn window(s: &str, start_index: usize, max_length: usize, hint: &str) -> (String, bool) {
    let start = floor_char_boundary(s, start_index);
    let rest = &s[start..];
    if rest.len() <= max_length {
        return (rest.to_string(), false);
    }
    let end = floor_char_boundary(rest, max_length);
    // A window narrower than the character at its start floors to 0, which
    // would return nothing and name the same start_index again — a model
    // following the note would page forever. Emit one whole character instead.
    let end = if end == 0 {
        rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)
    } else {
        end
    };
    let mut result = rest[..end].to_string();
    result.push_str(&format!(
        "\n\n[Content truncated: showing {start}-{} of {}. Continue with start_index={}{hint}.]",
        start + end,
        s.len(),
        start + end
    ));
    (result, true)
}

/// Turn a non-2xx response body into the text the error path returns
/// (AGE-508): run it through the same HTML-to-text extraction a success
/// response gets — an error body is frequently a full HTML error page, and
/// none of that markup is ever what the model needed — then window it to
/// `max_length`, capped at [`ERROR_MAX_LENGTH`] (the smaller of the two
/// wins, so a caller who explicitly asks for a smaller window still gets
/// it).
fn error_body_window(
    body: &str,
    content_type: &str,
    base_url: Option<&str>,
    fragment: Option<&str>,
    start_index: usize,
    max_length: usize,
) -> (String, bool) {
    let is_html = content_type.contains("text/html") || looks_like_html(body);
    let content = if is_html {
        html_to_text(body, base_url, fragment)
    } else {
        body.to_string()
    };
    window(&content, start_index, max_length.min(ERROR_MAX_LENGTH), "")
}

/// The passages of `text` from `start_index` on that contain `query`, each
/// headed by the `start_index` it begins at, within `max_length` bytes.
///
/// The query's words match case-insensitively with any whitespace between
/// them, so a phrase split across a line or a table cell still matches.
/// Neighbouring matches share one passage. When the passages don't all fit,
/// the summary line names the `start_index` to repeat the `find` from.
/// With no exact match, the closest passages by word overlap (BM25, as
/// `search_web` ranks page text) are shown instead, marked as such.
///
/// Returns the content and whether matches were left out.
fn find_passages(text: &str, query: &str, start_index: usize, max_length: usize) -> (String, bool) {
    let from = floor_char_boundary(text, start_index);
    let pattern = query
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+");
    let hits: Vec<(usize, usize)> = regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .map(|re| {
            re.find_iter(&text[from..])
                .map(|m| (from + m.start(), from + m.end()))
                .collect()
        })
        .unwrap_or_default();

    if hits.is_empty() {
        return (closest_passages(text, query, from, max_length), false);
    }

    // One passage per run of nearby matches.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in &hits {
        let span_start = floor_char_boundary(text, start.saturating_sub(FIND_CONTEXT_BEFORE));
        let span_end = floor_char_boundary(text, end + FIND_CONTEXT_AFTER);
        match spans.last_mut() {
            Some(last) if span_start <= last.1 => last.1 = last.1.max(span_end),
            _ => spans.push((span_start, span_end)),
        }
    }

    let mut body = String::new();
    // Where the next `find` should resume, when not every passage fit.
    let mut resume_at: Option<usize> = None;
    for &(start, end) in &spans {
        let label = format!("\n--- start_index={start} ---\n");
        let room = max_length.saturating_sub(body.len() + label.len());
        if end - start <= room {
            body.push_str(&label);
            body.push_str(&text[start..end]);
            continue;
        }
        if body.is_empty() {
            // Not even the first passage fits: show its head, resume after.
            // Always make progress, or a model following the note would
            // repeat the same call forever.
            let cut = floor_char_boundary(text, start + room.max(1));
            let cut = if cut <= start {
                text[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(text.len(), |(i, _)| start + i)
            } else {
                cut
            };
            body.push_str(&label);
            body.push_str(&text[start..cut]);
            resume_at = Some(cut);
        } else {
            resume_at = Some(start);
        }
        break;
    }

    let shown = hits
        .iter()
        .filter(|(start, _)| resume_at.is_none_or(|resume| *start < resume))
        .count();
    let mut summary = format!(
        "[find \"{query}\": {} match{} from start_index={from} of {}",
        hits.len(),
        if hits.len() == 1 { "" } else { "es" },
        text.len()
    );
    match resume_at {
        Some(resume) => summary.push_str(&format!(
            ", showing the first {shown}. For the rest, repeat this find with start_index={resume}. \
             To read on from a passage, fetch without find at its start_index.]"
        )),
        None => summary.push_str(". To read on from a passage, fetch without find at its start_index.]"),
    }
    (format!("{summary}\n{body}"), resume_at.is_some())
}

/// The passages of `text` from `from` on that share the most words with
/// `query` (BM25), for a `find` with no exact match: a model's phrasing of
/// what it is looking for is rarely the document's own.
fn closest_passages(text: &str, query: &str, from: usize, max_length: usize) -> String {
    use crate::tools::passages::{PASSAGE_STRIDE, PASSAGE_WORDS, score_passages, tokenize};

    let no_match = format!(
        "[find \"{query}\": no match from start_index={from} of {}. \
         Try other words, or page through with start_index.]",
        text.len()
    );
    let query_terms = tokenize(query);
    let words: Vec<(usize, usize)> = regex::Regex::new(r"\S+")
        .map(|re| {
            re.find_iter(&text[from..])
                .map(|m| (from + m.start(), from + m.end()))
                .collect()
        })
        .unwrap_or_default();
    if query_terms.is_empty() || words.is_empty() {
        return no_match;
    }

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut first = 0;
    loop {
        let last = (first + PASSAGE_WORDS).min(words.len()) - 1;
        spans.push((words[first].0, words[last].1));
        if last + 1 == words.len() {
            break;
        }
        first += PASSAGE_STRIDE;
    }
    let tokens: Vec<Vec<String>> = spans
        .iter()
        .map(|&(start, end)| tokenize(&text[start..end]))
        .collect();
    let scores = score_passages(&query_terms, &tokens);
    let mut ranked: Vec<usize> = (0..spans.len()).filter(|&i| scores[i] > 0.0).collect();
    ranked.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));

    let mut chosen: Vec<(usize, usize)> = Vec::new();
    for i in ranked {
        let (start, end) = spans[i];
        if chosen.iter().all(|&(s, e)| end <= s || start >= e) {
            chosen.push((start, end));
        }
        if chosen.len() == FIND_FALLBACK_PASSAGES {
            break;
        }
    }
    if chosen.is_empty() {
        return no_match;
    }

    let mut out = format!(
        "[find \"{query}\": no exact match from start_index={from} of {}. \
         Closest passages by shared words, best first:]\n",
        text.len()
    );
    let share = max_length.saturating_sub(out.len()) / chosen.len();
    for (start, end) in chosen {
        let label = format!("\n--- start_index={start} ---\n");
        let end = (end + FIND_CONTEXT_AFTER).min(start + share.saturating_sub(label.len()));
        let end = floor_char_boundary(text, end);
        out.push_str(&label);
        out.push_str(&text[start..end]);
    }
    out
}

/// Elements whose text is site furniture rather than page content. Skipped
/// whole, the way `<script>`/`<style>` are: on a Wikipedia article they are the
/// "Jump to content / Main menu / …" preamble that used to fill a small
/// `max_length` window before the article body was ever reached (AGE-507).
const CHROME_TAGS: [&str; 4] = ["nav", "header", "footer", "aside"];

/// Elements that never have a closing tag, so can't open a skipped block.
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Placeholder between table cells while extracting; `tidy_table_rows`
/// turns it into ` | ` once the row's cells are known.
const CELL_BREAK: char = '\u{1F}';

/// Whether tag `element` (a closing tag when `closing`) ends an open
/// `hidden` element whose end tag may be omitted, per HTML's implied end
/// tags: a `<p>` ends at the next block, an `<li>` at the next item or the
/// end of its list, a cell at the next cell or row, and so on.
fn implicitly_closes(hidden: &str, element: &str, closing: bool) -> bool {
    const P_ENDERS: [&str; 22] = [
        "p",
        "div",
        "table",
        "ul",
        "ol",
        "dl",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "section",
        "article",
        "blockquote",
        "pre",
        "hr",
        "main",
        "header",
        "footer",
        "nav",
        "form",
    ];
    const P_CONTAINERS: [&str; 10] = [
        "div",
        "td",
        "th",
        "li",
        "dd",
        "section",
        "article",
        "blockquote",
        "main",
        "body",
    ];
    match hidden {
        "p" if closing => P_CONTAINERS.contains(&element),
        "p" => P_ENDERS.contains(&element),
        "li" if closing => matches!(element, "ul" | "ol" | "menu" | "body"),
        "li" => element == "li",
        "td" | "th" if closing => matches!(
            element,
            "tr" | "table" | "tbody" | "thead" | "tfoot" | "body"
        ),
        "td" | "th" => matches!(element, "td" | "th" | "tr"),
        "tr" if closing => matches!(element, "table" | "tbody" | "thead" | "tfoot" | "body"),
        "tr" => element == "tr",
        "dt" | "dd" if closing => matches!(element, "dl" | "body"),
        "dt" | "dd" => matches!(element, "dt" | "dd"),
        "option" if closing => matches!(element, "select" | "datalist" | "optgroup" | "body"),
        "option" => matches!(element, "option" | "optgroup"),
        _ => false,
    }
}

/// Placeholder for a block element inside a table cell; see
/// `resolve_cell_breaks`.
const SOFT_BREAK: char = '\u{1E}';

/// A line of extracted text longer than this is not a table row but a page
/// laid out as a table (a Paul Graham essay, Hacker News, older sites): its
/// block elements go back to being line breaks.
const MAX_ROW_BYTES: usize = 2_000;

/// Settle the [`SOFT_BREAK`]s block elements inside table cells left behind.
/// On a table row they become spaces, so a filing that wraps each cell's
/// value in a `<p>` still reads as one row; on a line longer than
/// [`MAX_ROW_BYTES`] they become newlines, or a page laid out as one big
/// cell would come out as a single line of tens of KB. Neighbouring
/// whitespace folds into the break either way.
fn resolve_cell_breaks(text: &str) -> String {
    if !text.contains(SOFT_BREAK) {
        return text.to_string();
    }
    text.split('\n')
        .map(|line| {
            if !line.contains(SOFT_BREAK) {
                return line.to_string();
            }
            let brk = if line.len() > MAX_ROW_BYTES {
                '\n'
            } else {
                ' '
            };
            let mut out = String::with_capacity(line.len());
            let mut pending_break = false;
            for ch in line.chars() {
                if ch == SOFT_BREAK {
                    pending_break = true;
                    continue;
                }
                if pending_break && ch == ' ' {
                    continue;
                }
                if pending_break {
                    let kept = out.trim_end_matches(' ').len();
                    out.truncate(kept);
                    if !out.is_empty() && !out.ends_with(CELL_BREAK) {
                        out.push(brk);
                    }
                    pending_break = false;
                }
                out.push(ch);
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a raw opening tag hides its element (`style="display: none"`):
/// a browser shows none of that text, and documents use such blocks for
/// machine-readable data — an inline-XBRL filing opens with tens of KB of
/// it — that would otherwise fill the first window.
fn hides_element(tag: &str) -> bool {
    attr_value(tag, "style").is_some_and(|style| {
        style
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
            .contains("display:none")
    })
}

/// Rebuild the table rows of extracted text: drop empty cells (layout
/// spacers), glue a cell holding only a currency sign or bracket onto its
/// neighbour (`$` and `(` open the number after them, `)` and `%` close the
/// one before), and separate what is left with ` | `. Any other cell stays
/// its own, a lone `—` included: tables use it for a nil value. Financial tables put each of those
/// in a cell of its own, which read as `$ | | 1,234 | ) | |` before.
fn tidy_table_rows(text: &str) -> String {
    if !text.contains(CELL_BREAK) {
        return text.to_string();
    }
    let opens_next = |cell: &str| cell.chars().all(|ch| "$(€£¥".contains(ch));
    let closes_previous = |cell: &str| cell.chars().all(|ch| ")%".contains(ch));
    text.split('\n')
        .map(|line| {
            if !line.contains(CELL_BREAK) {
                return line.to_string();
            }
            let mut cells: Vec<String> = Vec::new();
            let mut pending = String::new();
            for cell in line
                .split(CELL_BREAK)
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                if opens_next(cell) {
                    pending.push_str(cell);
                } else if closes_previous(cell) && pending.is_empty() && !cells.is_empty() {
                    if let Some(last) = cells.last_mut() {
                        last.push_str(cell);
                    }
                } else {
                    cells.push(format!("{}{cell}", std::mem::take(&mut pending)));
                }
            }
            if !pending.is_empty() {
                cells.push(pending);
            }
            cells.join(" | ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lowercase tag name of a raw tag body (`a href="/x"` → `a`,
/// `/div ` → `/div`), keeping the leading slash of a closing tag.
fn tag_name(tag: &str) -> String {
    tag.chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '/')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// The value of attribute `name` in a raw tag body, quoted or not.
///
/// Attribute names are matched whole and case-insensitively, so looking for
/// `id` doesn't match `data-id` and looking for `href` doesn't match `xhref`.
/// Entity references in the value are left alone — the whole extracted text is
/// decoded once at the end, and decoding here too would decode twice.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    // Every index below lands on a char boundary: the scan only ever stops on
    // an ASCII delimiter, and a UTF-8 continuation byte is never one.
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1; // the tag name itself
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let key = &tag[key_start..i];
        if key.is_empty() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue; // valueless attribute, e.g. `disabled`
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let value = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
            let quote = bytes[j];
            j += 1;
            let value_start = j;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            let value = &tag[value_start..j];
            j = (j + 1).min(bytes.len());
            value
        } else {
            let value_start = j;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            &tag[value_start..j]
        };
        i = j;
        if key.eq_ignore_ascii_case(name) {
            return Some(value.to_string());
        }
    }
    None
}

/// Turn an `href` into an absolute URL to print next to its link text.
///
/// Returns `None` for links that would only add noise: empty hrefs, in-page
/// `#fragment` links (the model can build those from the page URL itself), and
/// `javascript:` handlers.
fn resolve_href(base: Option<&reqwest::Url>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    // `get` rather than `href[..11]`: an href whose 11th byte falls inside a
    // multi-byte character (`αααααα.html`) would panic the whole fetch call.
    if href
        .get(..11)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("javascript:"))
    {
        return None;
    }
    match base {
        Some(base) => base.join(href).ok().map(String::from),
        None => Some(href.to_string()),
    }
}

/// Convert HTML to readable plain text.
///
/// Comprehensive HTML-to-text converter: strips tags, skips script/style and
/// navigation-chrome blocks, keeps hyperlinks as `text (absolute-url)` and
/// table cells separated by ` | `, inserts newlines for block-level elements,
/// normalises whitespace, and decodes entities. Contrast with
/// `search_web_tool::strip_html_tags`, which is a lightweight tag stripper for
/// short search-result snippets.
///
/// `base_url` is the URL the body came from; relative `href`s resolve against
/// it. `fragment` is the `#fragment` that was asked for: if an element carries
/// a matching `id`, the text starts there instead of at the top of the page.
pub(crate) fn html_to_text(html: &str, base_url: Option<&str>, fragment: Option<&str>) -> String {
    let base = base_url.and_then(|url| reqwest::Url::parse(url).ok());
    let mut result = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut chrome_depth: usize = 0;
    let mut last_was_whitespace = false;
    let mut tag = String::new();
    // Open `<a href>` elements: the URL to print, and how much text had been
    // written when the link opened — a link with no text gets no URL.
    let mut open_links: Vec<(String, usize)> = Vec::new();
    let mut fragment_start: Option<usize> = None;
    // The hidden element being skipped, and how deeply elements of its name
    // nest inside it, so the skip ends at its own closing tag.
    let mut hidden: Option<(String, usize)> = None;
    // Inside a table cell, block elements (a `<p>` or `<div>` per cell is
    // common) must not break the row onto one line per value.
    let mut in_cell = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag.clear();
            continue;
        }

        if in_tag {
            if ch != '>' {
                tag.push(ch);
                continue;
            }
            in_tag = false;

            let name = tag_name(&tag);
            let closing = name.starts_with('/');
            let element = name.trim_start_matches('/');

            // Inside a hidden element, only track where it ends. One whose
            // end tag HTML lets a page leave out also ends where a browser
            // would close it, and this tag is then read as usual; otherwise
            // an unclosed hidden `<p>` or `<li>` would hide the rest of the
            // page.
            if let Some((hidden_name, 1)) = hidden.as_ref()
                && implicitly_closes(hidden_name, element, closing)
            {
                hidden = None;
            }
            if let Some((hidden_name, depth)) = hidden.as_mut() {
                if element == hidden_name.as_str() {
                    if closing {
                        *depth -= 1;
                    } else if !tag.ends_with('/') {
                        *depth += 1;
                    }
                }
                if *depth == 0 {
                    hidden = None;
                }
                continue;
            }

            // Blocks whose content is skipped entirely
            match element {
                "script" => in_script = !closing,
                "style" => in_style = !closing,
                _ if CHROME_TAGS.contains(&element) => {
                    chrome_depth = if closing {
                        chrome_depth.saturating_sub(1)
                    } else {
                        chrome_depth + 1
                    };
                }
                _ => {}
            }
            if in_script || in_style || chrome_depth > 0 {
                continue;
            }
            if !closing
                && !VOID_TAGS.contains(&element)
                && !tag.ends_with('/')
                && hides_element(&tag)
            {
                hidden = Some((element.to_string(), 1));
                continue;
            }

            // Where the requested #fragment starts, if this is its element
            if let Some(fragment) = fragment
                && fragment_start.is_none()
                && !closing
                && attr_value(&tag, "id").as_deref() == Some(fragment)
            {
                fragment_start = Some(result.len());
            }

            // Hyperlinks: `text (absolute-url)`
            if element == "a" {
                if closing {
                    if let Some((href, opened_at)) = open_links.pop()
                        && !result[opened_at..].trim().is_empty()
                    {
                        result.truncate(result.trim_end_matches(' ').len());
                        result.push_str(&format!(" ({href})"));
                        last_was_whitespace = false;
                    }
                } else if let Some(href) =
                    attr_value(&tag, "href").and_then(|href| resolve_href(base.as_ref(), &href))
                {
                    open_links.push((href, result.len()));
                }
            }

            // Table cells: keep neighbouring cells apart whatever whitespace
            // the source HTML happens to have between them
            match element {
                "td" | "th" => in_cell = !closing && !tag.ends_with('/'),
                "tr" | "table" => in_cell = false,
                _ => {}
            }
            if !closing && matches!(element, "td" | "th") {
                let line = result.trim_end_matches(' ');
                if !line.is_empty() && !line.ends_with('\n') {
                    result.truncate(line.len());
                    result.push(CELL_BREAK);
                    last_was_whitespace = true;
                }
            }

            // Add line breaks for block-level elements
            let is_block = matches!(
                element,
                "p" | "div"
                    | "br"
                    | "br/"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "li"
                    | "tr"
                    | "blockquote"
                    | "pre"
                    | "hr"
                    | "header"
                    | "footer"
                    | "section"
                    | "article"
                    | "nav"
                    | "main"
            );
            if is_block && in_cell {
                // A break that `resolve_cell_breaks` settles once the whole
                // row is known: a space on a table row, a newline on a page
                // laid out as one big cell.
                if !result.ends_with([SOFT_BREAK, CELL_BREAK, '\n']) {
                    result.push(SOFT_BREAK);
                    last_was_whitespace = true;
                }
            } else if is_block && !result.ends_with('\n') {
                result.push('\n');
                last_was_whitespace = true;
            }
            continue;
        }

        // Skip content inside script, style, navigation-chrome and hidden blocks
        if in_script || in_style || chrome_depth > 0 || hidden.is_some() {
            continue;
        }

        // Normalize whitespace
        if ch.is_whitespace() {
            if !last_was_whitespace {
                result.push(' ');
                last_was_whitespace = true;
            }
        } else {
            result.push(ch);
            last_was_whitespace = false;
        }
    }

    // Start at the requested #fragment when the page had it
    if let Some(offset) = fragment_start {
        result.drain(..offset);
    }

    // Decode HTML entities, once (`html_entities::decode_html_entities`)
    let result = crate::tools::html_entities::decode_html_entities(&result);
    let result = resolve_cell_breaks(&result);
    let result = tidy_table_rows(&result);

    // Trim each line (a decoded `&nbsp;` spacer leaves lines of only
    // whitespace) and keep at most one blank line in a row
    let mut cleaned = String::with_capacity(result.len());
    let mut blank_run = 0;
    for line in result.split('\n').map(str::trim) {
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }

    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract with no URL context — the shape most of these tests want.
    fn extract(html: &str) -> String {
        html_to_text(html, None, None)
    }

    #[test]
    fn test_html_to_text_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = extract(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_html_to_text_strips_script() {
        let html = "<p>Before</p><script>alert('xss')</script><p>After</p>";
        let text = extract(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_html_to_text_strips_style() {
        let html = "<p>Text</p><style>body { color: red; }</style><p>More</p>";
        let text = extract(html);
        assert!(text.contains("Text"));
        assert!(text.contains("More"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn test_html_to_text_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D &quot;E&quot;</p>";
        let text = extract(html);
        assert!(text.contains("A & B < C > D \"E\""));
    }

    // --- AGE-507 criterion 1: max_length is not required ---

    #[test]
    fn test_schema_does_not_require_max_length() {
        let schema = FetchTool::new(None).parameters();
        assert_eq!(
            schema["required"],
            serde_json::json!(["url"]),
            "only the URL is required; requiring max_length pushes the model to invent small values"
        );
        // …and it still has a default when omitted.
        let args: FetchToolArgs =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert_eq!(args.max_length, None);
        assert_eq!(args.max_length.unwrap_or(MAX_WINDOW), 20_000);
    }

    // --- AGE-507 criterion 2: start_index continues a truncated fetch ---

    #[test]
    fn test_schema_offers_start_index() {
        let schema = FetchTool::new(None).parameters();
        assert!(schema["properties"]["start_index"].is_object());
        let args: FetchToolArgs = serde_json::from_value(
            serde_json::json!({ "url": "https://example.com", "start_index": 4000 }),
        )
        .unwrap();
        assert_eq!(args.start_index, Some(4000));
    }

    /// Two windows of the same size, the second continuing where the first
    /// stopped, must rebuild the text exactly: no gap, no overlap.
    ///
    /// This is the seam `call()` uses; an end-to-end `call()` test can't be
    /// written here because the SSRF guard blocks loopback by design, so a
    /// local test server is unreachable.
    #[test]
    fn test_window_chunks_join_without_gap_or_overlap() {
        let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let size = 1000;

        let (first, truncated) = window(&text, 0, size, "");
        assert!(truncated);
        let first_body = first.split("\n\n[Content truncated").next().unwrap();
        assert_eq!(first_body.len(), size);

        let (second, _) = window(&text, size, size, "");
        let second_body = second.split("\n\n[Content truncated").next().unwrap();

        assert_eq!(format!("{first_body}{second_body}"), text[..2 * size]);
        assert!(text.starts_with(&format!("{first_body}{second_body}")));
    }

    /// The whole text comes back when the windows are walked to the end.
    #[test]
    fn test_window_walks_the_whole_text() {
        let text: String = (0..200).map(|i| format!("row {i} • ")).collect();
        let size = 97; // deliberately lands mid-multibyte-character
        let mut rebuilt = String::new();
        let mut start = 0;
        loop {
            let (chunk, truncated) = window(&text, start, size, "");
            let body = chunk.split("\n\n[Content truncated").next().unwrap();
            rebuilt.push_str(body);
            if !truncated {
                break;
            }
            start += body.len();
        }
        assert_eq!(rebuilt, text);
    }

    /// The truncation note names the offset to continue from, so the model
    /// doesn't have to work it out (or give up, which is what it did).
    #[test]
    fn test_window_note_names_the_next_start_index() {
        let text = "abcdefghij";
        let (chunk, truncated) = window(text, 0, 4, "");
        assert!(truncated);
        assert_eq!(
            chunk,
            "abcd\n\n[Content truncated: showing 0-4 of 10. Continue with start_index=4.]"
        );
        let (rest, truncated) = window(text, 4, 4, "");
        assert!(truncated);
        assert!(rest.starts_with("efgh"));
    }

    /// A window narrower than the character it starts on must still make
    /// progress: returning nothing while naming the same `start_index` would
    /// send a model that follows the note round the same call forever.
    #[test]
    fn test_window_narrower_than_a_character_still_advances() {
        let text = "a🌍béc日本語d";
        for size in 1..=4 {
            let mut start = 0;
            let mut rebuilt = String::new();
            let mut steps = 0;
            loop {
                let (chunk, truncated) = window(text, start, size, "");
                let body = chunk.split("\n\n[Content truncated").next().unwrap();
                assert!(
                    !body.is_empty(),
                    "empty window at start={start} size={size} would stall paging"
                );
                rebuilt.push_str(body);
                if !truncated {
                    break;
                }
                start += body.len();
                steps += 1;
                assert!(steps < 100, "paging did not terminate at size={size}");
            }
            assert_eq!(rebuilt, text, "size={size} lost or duplicated text");
        }
    }

    #[test]
    fn test_window_past_the_end_is_empty() {
        let (chunk, truncated) = window("short", 100, 10, "");
        assert_eq!(chunk, "");
        assert!(!truncated);
    }

    // --- AGE-508 criterion 3: non-2xx bodies go through html_to_text and a small cap ---

    #[test]
    fn test_error_body_window_strips_html_error_page() {
        let body = "<html><body><nav>Site nav</nav>\
                    <h1>404 Not Found</h1>\
                    <p>The page you requested does not exist.</p>\
                    <footer>Copyright 2026</footer></body></html>";
        let (content, truncated) = error_body_window(body, "text/html", None, None, 0, 50_000);
        assert!(!truncated);
        assert!(content.contains("404 Not Found"), "got {content:?}");
        assert!(
            content.contains("The page you requested does not exist"),
            "got {content:?}"
        );
        assert!(!content.contains("Site nav"), "chrome leaked: {content:?}");
        assert!(
            !content.contains("<html>"),
            "raw markup leaked: {content:?}"
        );
    }

    #[test]
    fn test_error_body_window_leaves_non_html_bodies_alone() {
        let body = r#"{"error":"rate limited"}"#;
        let (content, truncated) =
            error_body_window(body, "application/json", None, None, 0, 50_000);
        assert!(!truncated);
        assert_eq!(content, body);
    }

    /// The error cap (2KB) applies even when the caller's `max_length` is
    /// larger (the default is 20000) — an error page's useful content is
    /// rarely more than a short message.
    #[test]
    fn test_error_body_window_caps_below_default_max_length() {
        let long_message: String = std::iter::repeat_n('a', ERROR_MAX_LENGTH * 5).collect();
        let body = format!("<p>{long_message}</p>");
        let (content, truncated) = error_body_window(&body, "text/html", None, None, 0, MAX_WINDOW);
        assert!(truncated);
        assert!(
            content.len() <= ERROR_MAX_LENGTH + 100, // + the truncation note
            "error body was not capped to ~{ERROR_MAX_LENGTH} bytes: {} bytes",
            content.len()
        );
    }

    /// A caller-supplied `max_length` smaller than the error cap still wins
    /// (the smaller of the two applies).
    #[test]
    fn test_error_body_window_respects_smaller_caller_max_length() {
        let body = "<p>Not found, sorry about that.</p>";
        let (content, truncated) = error_body_window(body, "text/html", None, None, 0, 10);
        assert!(truncated);
        let text = content.split("\n\n[Content truncated").next().unwrap();
        assert_eq!(text.len(), 10);
    }

    /// The error path resolves the requested `#fragment` the same way a
    /// success response does, since it reuses `html_to_text` directly.
    #[test]
    fn test_error_body_window_honours_fragment() {
        let body = "<p>Lead</p><h2 id=\"detail\">Detail</h2><p>More detail text.</p>";
        let (content, _) = error_body_window(
            body,
            "text/html",
            Some("https://example.com/error"),
            Some("detail"),
            0,
            50_000,
        );
        assert!(content.starts_with("Detail"), "got {content:?}");
        assert!(!content.contains("Lead"), "got {content:?}");
    }

    // --- AGE-507 criterion 3: navigation chrome is skipped ---

    #[test]
    fn test_html_to_text_skips_navigation_chrome() {
        let html = "<body><nav><a href=\"/menu\">Jump to content</a> Main menu</nav>\
                    <header>Site header</header>\
                    <p>Article body</p>\
                    <aside>Related pages</aside>\
                    <footer>Privacy policy</footer></body>";
        let text = extract(html);
        assert!(text.contains("Article body"));
        for chrome in [
            "Jump to content",
            "Main menu",
            "Site header",
            "Related pages",
            "Privacy policy",
            "/menu",
        ] {
            assert!(!text.contains(chrome), "chrome leaked into text: {text:?}");
        }
    }

    #[test]
    fn test_html_to_text_resumes_after_nested_chrome() {
        let html = "<nav>Menu <nav>Submenu</nav> More menu</nav><p>Body</p>";
        let text = extract(html);
        assert_eq!(text, "Body");
    }

    // --- AGE-507 criterion 4: #fragment starts the text at that element ---

    #[test]
    fn test_html_to_text_starts_at_fragment() {
        let html = "<body><p>Lead paragraph</p>\
                    <h2 id=\"route-description\">Route description</h2>\
                    <p>The route begins…</p></body>";
        let text = html_to_text(
            html,
            Some("https://example.com/road"),
            Some("route-description"),
        );
        assert!(text.starts_with("Route description"), "got {text:?}");
        assert!(!text.contains("Lead paragraph"));
        assert!(text.contains("The route begins"));
    }

    #[test]
    fn test_html_to_text_fragment_not_found_keeps_whole_page() {
        let html = "<p>Lead paragraph</p><h2 id=\"other\">Other</h2>";
        let text = html_to_text(html, Some("https://example.com/road"), Some("missing"));
        assert!(text.starts_with("Lead paragraph"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_fragment_matches_whole_id_only() {
        let html = "<p>Lead</p><span data-id=\"sec\">decoy</span><span id=\"sec\">Target</span>";
        let text = html_to_text(html, None, Some("sec"));
        assert!(text.starts_with("Target"), "got {text:?}");
    }

    // --- AGE-507 criterion 5: links and table structure survive extraction ---

    #[test]
    fn test_html_to_text_emits_links_with_absolute_urls() {
        let html = "<p>See <a href=\"/wiki/Route_66\">Route 66</a> and \
                    <a href=\"https://example.org/spec\">the spec</a>.</p>";
        let text = html_to_text(html, Some("https://en.wikipedia.org/wiki/Roads"), None);
        assert!(
            text.contains("Route 66 (https://en.wikipedia.org/wiki/Route_66)"),
            "got {text:?}"
        );
        assert!(
            text.contains("the spec (https://example.org/spec)"),
            "got {text:?}"
        );
    }

    #[test]
    fn test_html_to_text_resolves_relative_links_against_the_page_path() {
        let html = "<a href=\"sibling.html\">sibling</a> <a href=\"../up.html\">up</a>";
        let text = html_to_text(html, Some("https://example.com/docs/guide/page.html"), None);
        assert!(
            text.contains("sibling (https://example.com/docs/guide/sibling.html)"),
            "got {text:?}"
        );
        assert!(
            text.contains("up (https://example.com/docs/up.html)"),
            "got {text:?}"
        );
    }

    #[test]
    fn test_html_to_text_link_urls_are_entity_decoded_once() {
        let html = "<a href=\"/s?q=a&amp;p=2\">results</a>";
        let text = html_to_text(html, Some("https://example.com/"), None);
        assert!(
            text.contains("results (https://example.com/s?q=a&p=2)"),
            "got {text:?}"
        );
    }

    #[test]
    fn test_html_to_text_skips_noise_links() {
        let html = "<a href=\"#cite_note_1\">[1]</a> <a href=\"javascript:void(0)\">Toggle</a> \
                    <a href=\"/real\">Real</a>";
        let text = html_to_text(html, Some("https://example.com/page"), None);
        assert!(!text.contains("#cite_note_1"), "got {text:?}");
        assert!(!text.contains("javascript:"), "got {text:?}");
        assert!(
            text.contains("Real (https://example.com/real)"),
            "got {text:?}"
        );
    }

    /// A non-ASCII href used to panic the whole `fetch` call: the
    /// `javascript:` check sliced 11 raw bytes out of the href, and the 11th
    /// byte of `αααααα.html` is inside a character.
    #[test]
    fn test_html_to_text_handles_non_ascii_hrefs() {
        for (html, expect) in [
            ("<a href=\"αααααα.html\">Greek</a>", "Greek ("),
            ("<a href=\"日本語ペ.html\">Japanese</a>", "Japanese ("),
            ("<a href=\"ü.html\">Short</a>", "Short ("),
        ] {
            let text = html_to_text(html, Some("https://example.com/docs/"), None);
            assert!(text.contains(expect), "got {text:?}");
        }
        // …and the same href straight through `resolve_href`.
        let base = reqwest::Url::parse("https://example.com/docs/").unwrap();
        assert!(resolve_href(Some(&base), "αααααα.html").is_some());
        assert!(resolve_href(Some(&base), "日本語ペ.html").is_some());
        assert!(resolve_href(None, "αααααα.html").is_some());
        // A genuine javascript: href is still dropped.
        assert_eq!(resolve_href(Some(&base), "JavaScript:void(0)"), None);
    }

    #[test]
    fn test_html_to_text_link_without_text_emits_no_url() {
        let html = "<p>Before<a href=\"/icon\"><img src=\"i.png\"></a>After</p>";
        let text = html_to_text(html, Some("https://example.com/"), None);
        assert!(!text.contains("/icon"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_separates_table_cells() {
        // No whitespace at all between the cells: the separation has to come
        // from the tags, not from incidental whitespace in the source.
        let html = "<table><tr><th>Year</th><th>Winner</th></tr>\
                    <tr><td>1994</td><td>Brazil</td></tr></table>";
        let text = extract(html);
        assert!(text.contains("Year | Winner"), "got {text:?}");
        assert!(text.contains("1994 | Brazil"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_table_rows_stay_on_their_own_lines() {
        let html = "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>";
        let text = extract(html);
        assert_eq!(text, "a | b\nc | d");
    }

    // --- AGE-507 criterion 6: one entity decoder, no double decoding ---

    #[test]
    fn test_html_to_text_does_not_double_decode_entities() {
        // A page showing the literal text `&lt;` writes it as `&amp;lt;`.
        let html = "<p>Write &amp;lt; for a less-than sign</p>";
        let text = extract(html);
        assert_eq!(text, "Write &lt; for a less-than sign");
    }

    #[test]
    fn test_html_to_text_decodes_numeric_entities() {
        let html = "<p>it&#39;s &#x2014; fine</p>";
        let text = extract(html);
        assert_eq!(text, "it's — fine");
    }

    // --- long documents: conversion, windows, find, cache ---

    /// A browser shows nothing of a `display:none` block; an inline-XBRL
    /// filing opens with tens of KB of such data, which filled the first
    /// window before any of the document was reached.
    #[test]
    fn test_html_to_text_skips_hidden_elements() {
        let html = "<body><div style=\"display: none\"><div>0001973266 2025-01-01</div>\
                    <ix:header>facts</ix:header></div>\
                    <img style=\"display:none\" src=\"x.png\"><p>Visible text</p>\
                    <div STYLE='DISPLAY:NONE'/><p>Still visible</p></body>";
        let text = extract(html);
        assert!(!text.contains("0001973266"), "got {text:?}");
        assert!(!text.contains("facts"), "got {text:?}");
        assert!(text.contains("Visible text"), "got {text:?}");
        assert!(text.contains("Still visible"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_financial_table_cells_read_as_values() {
        let html = "<table><tr><td>Revenue</td><td></td><td>$</td><td>1,234</td><td>&#160;</td>\
                    <td>(</td><td>56</td><td>)</td><td>12.5</td><td>%</td><td>—</td></tr>\
                    <tr><td></td><td>Total</td><td>$</td><td>(</td><td>7</td><td>)</td></tr></table>";
        let text = extract(html);
        assert_eq!(text, "Revenue | $1,234 | (56) | 12.5% | —\nTotal | $(7)");
    }

    /// Filings wrap every cell's value in a `<p>`; a block element inside a
    /// cell must not put each value on a line of its own.
    #[test]
    fn test_html_to_text_block_elements_inside_cells_stay_on_the_row() {
        let html = "<table><tr><td><p>Total revenue</p></td><td><p>$</p></td>\
                    <td><div>359,747</div></td></tr>\
                    <tr><td><p>Net loss</p></td><td><p>(</p></td><td><p>12</p></td>\
                    <td><p>)</p></td></tr></table><p>After</p><p>the table</p>";
        let text = extract(html);
        assert_eq!(
            text,
            "Total revenue | $359,747\nNet loss | (12)\nAfter\nthe table"
        );
    }

    /// A page laid out as one big table cell (Paul Graham's essays, Hacker
    /// News, older sites) keeps its paragraphs: only a row short enough to
    /// be a table row reads its blocks as spaces.
    #[test]
    fn test_html_to_text_page_laid_out_in_a_table_keeps_its_paragraphs() {
        let paragraph = "word ".repeat(150);
        let html = format!(
            "<table><tr><td><img src=\"x.gif\"></td><td><font>Title<br><br>{p}<br><br>{p}\
             <p>{p}</p><p>Last paragraph.</p></font></td></tr></table>",
            p = paragraph
        );
        let text = extract(&html);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "got {text:?}");
        assert_eq!(lines[0], "Title");
        assert_eq!(lines[1], paragraph.trim());
        assert_eq!(lines[4], "Last paragraph.");
        assert!(!text.contains(SOFT_BREAK));
    }

    /// A hidden element whose end tag the page left out ends where a
    /// browser would close it, instead of hiding the rest of the page.
    #[test]
    fn test_html_to_text_unclosed_hidden_element_does_not_hide_the_page() {
        let html = "<body><p style=\"display:none\">secret<p>First visible\
                    <ul><li style=\"display:none\">hidden item<li>Shown item</ul>\
                    <table><tr><td style=\"display:none\">x<td>Cell</table>\
                    <div>After</div></body>";
        let text = extract(html);
        assert!(!text.contains("secret"), "got {text:?}");
        assert!(!text.contains("hidden item"), "got {text:?}");
        assert!(text.contains("First visible"), "got {text:?}");
        assert!(text.contains("Shown item"), "got {text:?}");
        assert!(text.contains("Cell"), "got {text:?}");
        assert!(text.contains("After"), "got {text:?}");
        // A closed one still hides all of itself, nested tags included.
        let closed = extract(
            "<p style=\"display:none\">a <b>b</b> c</p><p>Shown</p>\
             <div style=\"display:none\"><p>x<div>y</div>z</div><p>Also shown</p>",
        );
        assert_eq!(closed, "Shown\nAlso shown");
    }

    /// A long page made of numbered lines, for the find tests.
    fn long_page() -> String {
        (0..2000)
            .map(|i| match i {
                700 => "Note 3. The total\nconsideration was $3.25 billion.\n".to_string(),
                1500 => "Later the TOTAL CONSIDERATION was adjusted.\n".to_string(),
                _ => format!("Filler line {i} about nothing in particular.\n"),
            })
            .collect()
    }

    /// Each passage is labelled with the offset its text starts at, so
    /// `start_index` from a label reads on from exactly that passage.
    #[test]
    fn test_find_returns_labelled_passages_across_whitespace_and_case() {
        let text = long_page();
        let (content, more) = find_passages(&text, "total consideration", 0, MAX_WINDOW);
        assert!(!more);
        assert!(
            content.starts_with("[find \"total consideration\": 2 matches from start_index=0 of "),
            "got {content:?}"
        );
        assert!(content.contains("$3.25 billion"), "got {content:?}");
        assert!(content.contains("TOTAL CONSIDERATION was adjusted"));
        assert!(
            content.len() < 4_000,
            "passages should be local, got {}",
            content.len()
        );
        let labels: Vec<usize> = content
            .split("--- start_index=")
            .skip(1)
            .map(|rest| rest.split(' ').next().unwrap().parse().unwrap())
            .collect();
        assert_eq!(labels.len(), 2);
        for (label, passage) in labels.iter().zip(content.split(" ---\n").skip(1)) {
            let passage = passage.split("\n--- start_index=").next().unwrap();
            assert!(text[*label..].starts_with(passage), "label {label} is off");
        }
    }

    #[test]
    fn test_find_searches_from_start_index() {
        let text = long_page();
        let second = text.find("Later the TOTAL").unwrap();
        let (content, _) = find_passages(&text, "total consideration", second - 10, MAX_WINDOW);
        assert!(
            content.contains(": 1 match from start_index="),
            "got {content:?}"
        );
        assert!(!content.contains("$3.25 billion"));
    }

    /// Matches that don't fit the window are not lost: the summary names
    /// where to repeat the find, and repeating it walks every match.
    #[test]
    fn test_find_pages_through_matches_that_do_not_fit() {
        let text: String = (0..300)
            .map(|i| {
                format!(
                    "Row {i}: segment revenue was {i} million.\n{}\n",
                    "x ".repeat(700)
                )
            })
            .collect();
        let mut start = 0;
        let mut seen = 0;
        for _ in 0..300 {
            let (content, more) = find_passages(&text, "segment revenue", start, 5_000);
            assert!(
                content.len() <= 5_000 + 300,
                "window overrun: {}",
                content.len()
            );
            seen += content.matches("segment revenue was").count();
            if !more {
                break;
            }
            let resume: usize = content
                .split("repeat this find with start_index=")
                .nth(1)
                .unwrap()
                .split('.')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(resume > start, "find did not advance");
            start = resume;
        }
        assert_eq!(seen, 300);
    }

    /// With no exact match, the passages sharing the most words come back,
    /// marked as such — the model's phrasing is rarely the document's own.
    #[test]
    fn test_find_without_exact_match_ranks_passages() {
        let text = long_page();
        let (content, more) =
            find_passages(&text, "consideration measured at closing", 0, MAX_WINDOW);
        assert!(!more);
        assert!(content.contains("no exact match"), "got {content:?}");
        assert!(
            content.contains("consideration was $3.25 billion"),
            "got {content:?}"
        );
        assert!(content.len() <= MAX_WINDOW);

        let (content, _) = find_passages(&text, "zebra quokka", 0, MAX_WINDOW);
        assert!(content.contains("no match"), "got {content:?}");
        assert!(content.contains("page through with start_index"));
    }

    #[test]
    fn test_find_handles_regex_metacharacters_and_tiny_windows() {
        let text = "Price (USD): $3.25 [approx.]\n".repeat(3);
        let (content, _) = find_passages(&text, "$3.25 [approx.]", 0, MAX_WINDOW);
        assert!(content.contains(": 3 matches"), "got {content:?}");
        let (content, more) = find_passages(&text, "(USD)", 0, 1);
        assert!(more);
        assert!(content.contains("start_index=0 ---\nP"), "got {content:?}");
    }

    /// The truncation note of a page says where the window sits and how to
    /// get the rest — the model gave up on pages whose note it never saw.
    #[test]
    fn test_window_note_offers_find() {
        let text = "a".repeat(100);
        let (content, truncated) = window(&text, 10, 20, WINDOW_HINT);
        assert!(truncated);
        assert!(
            content.ends_with(
                "[Content truncated: showing 10-30 of 100. Continue with start_index=30, \
                 or pass `find` to jump to the passage you need.]"
            ),
            "got {content:?}"
        );
    }

    fn page(text: &str) -> Arc<CachedPage> {
        Arc::new(CachedPage {
            status: 200,
            content_type: "text/html".to_string(),
            text: text.to_string(),
            note: None,
        })
    }

    #[test]
    fn test_page_cache_evicts_least_recently_used() {
        let cache = PageCache::default();
        for i in 0..MAX_CACHED_PAGES {
            cache.insert(format!("https://example.com/{i}"), page("x"));
        }
        // Touch the oldest so the second-oldest goes first.
        assert!(cache.get("https://example.com/0").is_some());
        cache.insert("https://example.com/new".to_string(), page("y"));
        assert!(cache.get("https://example.com/0").is_some());
        assert!(cache.get("https://example.com/1").is_none());
        assert!(cache.get("https://example.com/new").is_some());
        assert_eq!(cache.lock().len(), MAX_CACHED_PAGES);
    }

    #[test]
    fn test_page_cache_is_bounded_by_bytes() {
        let cache = PageCache::default();
        let big = "x".repeat(MAX_CACHED_BYTES / 2 + 1);
        cache.insert("https://example.com/a".to_string(), page(&big));
        cache.insert("https://example.com/b".to_string(), page(&big));
        assert!(cache.get("https://example.com/a").is_none());
        assert!(cache.get("https://example.com/b").is_some());
        let huge = "x".repeat(MAX_CACHED_BYTES + 1);
        cache.insert("https://example.com/huge".to_string(), page(&huge));
        assert!(cache.get("https://example.com/huge").is_none());
    }

    /// Paging and find over a cached page never touch the network: the URL
    /// here does not resolve, so any download would fail the call.
    #[tokio::test]
    async fn test_call_pages_and_finds_in_the_session_cache() {
        let tool = FetchTool::new(None);
        let url = "https://doc.example.invalid/10-q.htm";
        tool.pages.insert(format!("{url}\n"), page(&long_page()));
        let try_call = |start_index: Option<usize>,
                        max_length: Option<usize>,
                        find: Option<&str>,
                        user_agent: Option<&str>| {
            let tool = tool.clone();
            let find = find.map(str::to_string);
            let user_agent = user_agent.map(str::to_string);
            async move {
                tool.call(
                    &mut ToolContext::new(),
                    FetchToolArgs {
                        url: url.to_string(),
                        max_length,
                        start_index,
                        find,
                        user_agent,
                    },
                )
                .await
            }
        };
        let call = |start_index: Option<usize>, max_length: Option<usize>, find: Option<&str>| {
            let call = try_call(start_index, max_length, find, None);
            async move { call.await.expect("served from the cache") }
        };

        // A plain fetch downloads again (here: fails, the host doesn't
        // resolve), so a changing page is never served stale; so does a
        // different User-Agent, even when paging.
        assert!(try_call(None, None, None, None).await.is_err());
        assert!(
            try_call(Some(0), None, None, Some("Research bot admin@example.com"))
                .await
                .is_err()
        );

        let first = call(Some(0), None, None).await;
        assert!(first.truncated);
        assert_eq!(first.total_length, Some(long_page().len()));
        assert!(
            first
                .content
                .contains("Continue with start_index=20000, or pass `find`")
        );

        // max_length can't take a window past what the context shaper keeps.
        let clamped = call(Some(0), Some(200_000), None).await;
        assert_eq!(clamped.content, first.content);

        let found = call(None, None, Some("total consideration")).await;
        assert!(
            found.content.contains("$3.25 billion"),
            "got {:?}",
            found.content
        );
        assert!(!found.truncated);

        let next = call(Some(20_000), None, None).await;
        assert!(
            long_page()[20_000..].starts_with(next.content.split("\n\n[Content").next().unwrap())
        );
    }

    // --- helpers ---

    #[test]
    fn test_attr_value_reads_quoted_and_unquoted_values() {
        assert_eq!(
            attr_value("a href=\"/x\" id='y'", "href").as_deref(),
            Some("/x")
        );
        assert_eq!(
            attr_value("a href=\"/x\" id='y'", "id").as_deref(),
            Some("y")
        );
        assert_eq!(
            attr_value("a href=/x class=b", "href").as_deref(),
            Some("/x")
        );
        assert_eq!(attr_value("a HREF=\"/x\"", "href").as_deref(), Some("/x"));
        assert_eq!(attr_value("span data-id=\"n\"", "id"), None);
        assert_eq!(
            attr_value("a download href=\"/x\"", "href").as_deref(),
            Some("/x")
        );
        assert_eq!(attr_value("br/", "href"), None);
        assert_eq!(
            attr_value("a title=\"héllo wörld\" href=\"/x\"", "href").as_deref(),
            Some("/x")
        );
    }

    #[test]
    fn test_looks_like_html() {
        assert!(looks_like_html("<!DOCTYPE html><html>"));
        assert!(looks_like_html("  <!doctype html>"));
        assert!(looks_like_html("<html><head>"));
        assert!(!looks_like_html("{\"key\": \"value\"}"));
        assert!(!looks_like_html("plain text"));
    }

    #[test]
    fn test_truncate_at_char_boundary() {
        let (truncated, flagged) = window("Hello, World!", 0, 5, "");
        assert!(truncated.starts_with("Hello"));
        assert!(truncated.contains("[Content truncated"));
        assert!(flagged);
    }

    #[test]
    fn test_truncate_no_truncation_needed() {
        let (result, truncated) = window("short", 0, 100, "");
        assert_eq!(result, "short");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_multibyte_chars() {
        // Ensure we don't split in the middle of a multi-byte character
        let (truncated, _) = window("Hello 🌍 World", 0, 7, "");
        // Should not panic and should produce valid UTF-8
        assert!(truncated.starts_with("Hello "));
        assert!(truncated.contains("[Content truncated"));
    }

    /// A window that starts mid-character starts at the character instead —
    /// the same rounding the previous window's end used, which is what makes
    /// the two meet exactly.
    #[test]
    fn test_window_start_snaps_to_char_boundary() {
        let text = "a🌍b";
        let (first, _) = window(text, 0, 3, "");
        let first_body = first.split("\n\n[Content truncated").next().unwrap();
        assert_eq!(first_body, "a");
        let (second, _) = window(text, 3, 10, "");
        assert_eq!(format!("{first_body}{second}"), text);
    }

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
        let (base, _) =
            mock_server(|_, _, _| ("404 Not Found", String::new(), "gone".into())).await;
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
        // The client's own default User-Agent (set once in `no_redirect_client`)
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
            url: "https://en.wikipedia.org/wiki/Special:This_page_does_not_exist_af8s7d6f"
                .to_string(),
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
    fn test_unique_path_no_conflict() {
        let path = PathBuf::from("/tmp/nonexistent_test_file_12345.txt");
        assert_eq!(unique_path(path.clone()), path);
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
}
