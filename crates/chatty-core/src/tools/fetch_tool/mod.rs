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

/// Most of a non-success response's body that is read (AGE-548).
const MAX_ERROR_BODY_BYTES: usize = 256 * 1024;
/// Most of a page's text body that is read (AGE-1094).
const MAX_TEXT_BYTES: usize = 5 * 1024 * 1024;
/// Note added to a page whose body went on past [`MAX_TEXT_BYTES`].
const TEXT_CUT_NOTE: &str = "Only the first 5 MiB of this page were read.";

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
    /// Hosted guests read pages through the lease's managed Tavily Extract
    /// endpoint instead of connecting out (AGE-1183). `None` on desktop and
    /// TUI, where nothing below changes.
    managed_extract: Option<ManagedExtract>,
}

/// The managed extract endpoint, its bearer token and the client that
/// reaches it. The client is a plain one: the endpoint is the lease proxy
/// on a private address, which the SSRF-guarded web client refuses.
#[derive(Clone)]
struct ManagedExtract {
    endpoint: String,
    token: String,
    client: reqwest::Client,
}

impl FetchTool {
    /// Read pages through the managed extract `endpoint` with `token`
    /// instead of fetching them directly.
    pub fn with_managed_extract(mut self, endpoint: &str, token: &str) -> Self {
        self.managed_extract = Some(ManagedExtract {
            endpoint: endpoint.to_string(),
            token: token.to_string(),
            client: crate::services::http_client::default_client(REQUEST_TIMEOUT_SECS),
        });
        self
    }

    pub fn new(workspace_dir: Option<PathBuf>) -> Self {
        let client = crate::services::http_client::open_web_client(REQUEST_TIMEOUT_SECS);
        Self {
            client,
            workspace_dir,
            pages: PageCache::default(),
            wayback_available_url: WAYBACK_AVAILABLE_URL.to_string(),
            wayback_web_url: WAYBACK_WEB_URL.to_string(),
            wayback_lookup_timeout: WAYBACK_LOOKUP_TIMEOUT,
            managed_extract: None,
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
        let mut description =
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
                .to_string();
        if self.managed_extract.is_some() {
            description.push_str(
                " In this hosted environment pages are read through a managed extraction \
                 service: binary files are not saved, redirects and the Wayback Machine \
                 fallback do not apply, and `user_agent` is ignored. A page the service \
                 cannot read comes back with status 0 and an explanation.",
            );
        }
        description
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
                .download_or_extract(
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

/// Tavily Extract's answer, reduced to what `fetch` reads.
#[derive(Deserialize)]
struct ExtractResponse {
    #[serde(default)]
    results: Vec<ExtractResult>,
    #[serde(default)]
    failed_results: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ExtractResult {
    #[serde(default)]
    raw_content: Option<String>,
}

impl FetchTool {
    /// The managed extract path when hosted, else the direct download.
    async fn download_or_extract(
        &self,
        url: &str,
        user_agent: Option<&str>,
        fragment: Option<&str>,
        start_index: usize,
        max_length: usize,
    ) -> Result<Download, ToolError> {
        match &self.managed_extract {
            Some(managed) => self.extract(managed, url).await,
            None => {
                self.download(url, user_agent, fragment, start_index, max_length)
                    .await
            }
        }
    }

    /// Read `url` through the managed extract endpoint: one POST, no direct
    /// connection to the page's host.
    async fn extract(&self, managed: &ManagedExtract, url: &str) -> Result<Download, ToolError> {
        info!(url = %url, "Reading URL through the managed extract endpoint");
        let response = managed
            .client
            .post(format!("{}/extract", managed.endpoint))
            .bearer_auth(&managed.token)
            .json(&serde_json::json!({ "urls": [url], "format": "markdown" }))
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Extract request failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::OperationFailed(format!(
                "The managed extract service returned {}",
                status.as_u16()
            )));
        }
        let answer: ExtractResponse = response.json().await.map_err(|e| {
            ToolError::OperationFailed(format!("Unreadable extract service answer: {e}"))
        })?;
        let text = answer
            .results
            .into_iter()
            .find_map(|r| r.raw_content.filter(|c| !c.trim().is_empty()));
        match text {
            Some(text) => Ok(Download::Page(CachedPage {
                status: 200,
                content_type: "text/markdown".to_string(),
                text,
                note: None,
            })),
            None => {
                let reason = answer
                    .failed_results
                    .iter()
                    .find_map(|f| f.get("error").and_then(|e| e.as_str()).map(str::to_string));
                Ok(Download::Done(FetchToolOutput {
                    status: 0,
                    content: format!(
                        "Couldn't read this page: the extraction service returned no content{}.",
                        reason.map(|r| format!(" ({r})")).unwrap_or_default()
                    ),
                    content_type: "unknown".to_string(),
                    truncated: false,
                    total_length: None,
                    saved_to: None,
                    note: Some("Read through the managed extract service.".to_string()),
                }))
            }
        }
    }

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
            let body = read_capped(response, MAX_ERROR_BODY_BYTES)
                .await
                .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned())
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
        let (body, cut) = read_text_capped(response, MAX_TEXT_BYTES)
            .await
            .map_err(|e| {
                ToolError::OperationFailed(format!("Failed to read response body: {}", e))
            })?;
        let note = if cut {
            Some(match note {
                Some(n) => format!("{n} {TEXT_CUT_NOTE}"),
                None => TEXT_CUT_NOTE.to_string(),
            })
        } else {
            note
        };

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

        // Refuse an oversized declared length before reading any body
        if let Some(n) = response.content_length()
            && n > MAX_BINARY_BYTES as u64
        {
            return Err(ToolError::OperationFailed(format!(
                "Response too large: {} bytes (max {} bytes / {} MB)",
                n,
                MAX_BINARY_BYTES,
                MAX_BINARY_BYTES / 1024 / 1024,
            )));
        }

        // Read binary body, capped
        let (bytes, cut) = read_capped(response, MAX_BINARY_BYTES).await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to read response body: {}", e))
        })?;

        if cut {
            return Err(ToolError::OperationFailed(format!(
                "Response too large: more than {} bytes (max {} MB)",
                MAX_BINARY_BYTES,
                MAX_BINARY_BYTES / 1024 / 1024,
            )));
        }

        // Extract filename from URL or Content-Disposition header
        let filename = extract_filename(url, content_type);
        let save_path = workspace.join(&filename);

        // create_new picks a free name and never follows a planted symlink
        let size = bytes.len();
        let save_path = tokio::task::spawn_blocking(move || save_new_file(save_path, &bytes))
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Task join error: {e}")))?
            .map_err(|e| {
                ToolError::OperationFailed(format!(
                    "Failed to save file in {}: {}",
                    workspace.display(),
                    e
                ))
            })?;
        info!(path = %save_path.display(), size, "Saved binary content to workspace");

        Ok(FetchToolOutput {
            status,
            content: format!(
                "Downloaded {} ({} bytes) and saved to: {}",
                content_type,
                size,
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

/// Read at most `cap` bytes of `response`'s body, chunk by chunk, so an
/// oversized body is never held in memory whole (AGE-548). Returns the
/// bytes read and whether the body went on past `cap`; the rest is not read.
async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = cap - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

/// Decode `bytes` in the charset `content_type` names (`charset=`), or
/// UTF-8 when it names none or one encoding_rs does not know: what
/// reqwest's `Response::text()` does (AGE-1094).
fn decode_text(bytes: &[u8], content_type: &str) -> String {
    let encoding = content_type
        .split(';')
        .skip(1)
        .filter_map(|param| param.split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("charset"))
        .and_then(|(_, value)| {
            encoding_rs::Encoding::for_label(value.trim().trim_matches('"').as_bytes())
        })
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Read at most `cap` bytes of a text body and decode them in the
/// response's charset. Returns the text and whether the body went on past
/// `cap`; the rest is not read (AGE-1094).
pub(crate) async fn read_text_capped(
    response: reqwest::Response,
    cap: usize,
) -> Result<(String, bool), reqwest::Error> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let (bytes, cut) = read_capped(response, cap).await?;
    Ok((decode_text(&bytes, &content_type), cut))
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

/// Create `path` as a new file holding `bytes`, or `stem-1.ext`,
/// `stem-2.ext`, … when that name is taken. `create_new` refuses any
/// existing path, a dangling symlink included, so a download never
/// replaces a file or writes through a planted link (AGE-1097).
fn save_new_file(path: PathBuf, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_string();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin")
        .to_string();
    let parent = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let mut last_err = None;
    for i in 0..1000u32 {
        let candidate = if i == 0 {
            path.clone()
        } else {
            parent.join(format!("{stem}-{i}.{ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::ErrorKind::AlreadyExists.into()))
}

/// Validate that a URL does not target private, internal, or reserved network hosts.
///
/// Delegates to the shared SSRF guard (`services::ssrf_guard`) so this tool and the
/// browser's internet-enabled navigation policy enforce the same denylist.
/// Needs no DNS: a hostname is checked where it is resolved, by the guarded
/// resolver of this tool's client, so the check and the connection share one
/// lookup (AGE-537).
fn validate_url_host(url: &str) -> Result<(), ToolError> {
    crate::services::ssrf_guard::check_public_url_without_lookup(url)
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

mod html;
mod passages;

pub(crate) use html::html_to_text;
#[cfg(test)]
use html::{SOFT_BREAK, attr_value, resolve_href};
use passages::{WINDOW_HINT, error_body_window, find_passages, window};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_net;
