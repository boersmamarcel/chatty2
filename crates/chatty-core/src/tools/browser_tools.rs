//! Browser tools: the self-review loop, plus reading the open web.
//!
//! The agent renders what it built, looks at it, critiques it, edits, reloads.
//! Localhost and workspace-local `file://` by default (Lane A); with internet
//! access on, navigation also allows public http(s) hosts (still refusing
//! private/internal network targets via the shared SSRF guard). Either way
//! the profile is ephemeral — no stored credentials are ever in play, so
//! none of the reading tools needs an approval gate.
//!
//! `browser_click` is the one tool that *acts* (AGE-489). On a Lane A page
//! it is the agent clicking through its own output and runs unattended. On
//! anything else the user may have logged in under take-control (AGE-156),
//! so every click is an approval card the user answers — per action, never
//! generalised, and never auto-approved by the shell's approval mode.
//!
//! Two deliberate choices about tokens, made here rather than retrofitted:
//!
//! - `browser_screenshot` writes a PNG and queues it as a pending artifact, the
//!   same path `add_attachment` uses. It is deliberately *not* returned as
//!   tool-result image content: none of this app's providers accept that.
//! - `browser_console` and `browser_network` write the full dump to a file and
//!   return a summary plus that path. Dumping every line into context after
//!   every action is how a browser loop becomes unaffordable.

use std::sync::Arc;

use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams,
};
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::models::execution_approval_store::{PendingApprovals, request_execution_approval};
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::add_attachment_tool::PendingArtifacts;

use crate::services::browser::gate::RiskyApproval;
use crate::services::browser::session::{DEFAULT_TIMEOUT_SECS, with_deadline};
use crate::services::browser::sites;
use crate::services::browser::snapshot::{AxNode, flatten_ax_tree};
use crate::services::browser::{BrowserError, BrowserManager, MAX_TEXT_LEN, NavigationPolicy};
use crate::tools::ToolError;

/// Bundle of the nine browser tools, so the agent factory moves one value.
pub type BrowserTools = (
    BrowserNavigateTool,
    BrowserSnapshotTool,
    BrowserScreenshotTool,
    BrowserConsoleTool,
    BrowserNetworkTool,
    BrowserResizeTool,
    BrowserClickTool,
    BrowserTypeTool,
    BrowserSelectTool,
);

/// Build every browser tool over one shared manager. `pending_approvals` is
/// the channel `browser_click` asks the user through when the page is not a
/// Lane A origin; without one, such clicks are refused rather than assumed.
pub fn build_browser_tools(
    manager: Arc<BrowserManager>,
    pending_artifacts: PendingArtifacts,
    pending_approvals: Option<PendingApprovals>,
) -> BrowserTools {
    (
        BrowserNavigateTool {
            manager: manager.clone(),
        },
        BrowserSnapshotTool {
            manager: manager.clone(),
        },
        BrowserScreenshotTool {
            manager: manager.clone(),
            pending_artifacts,
        },
        BrowserConsoleTool {
            manager: manager.clone(),
        },
        BrowserNetworkTool {
            manager: manager.clone(),
        },
        BrowserResizeTool {
            manager: manager.clone(),
        },
        BrowserClickTool {
            manager: manager.clone(),
            pending_approvals: pending_approvals.clone(),
        },
        BrowserTypeTool {
            manager: manager.clone(),
            pending_approvals: pending_approvals.clone(),
        },
        BrowserSelectTool {
            manager,
            pending_approvals,
        },
    )
}

/// Tools that take no arguments still need a struct for the schema.
#[derive(Deserialize, Serialize)]
pub struct NoArgs {}

fn empty_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object", "properties": {}, "required": [] })
}

// ── navigate ────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct NavigateArgs {
    /// The URL to open.
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct NavigateOutput {
    /// Where the browser actually ended up after any redirects.
    pub url: String,
    /// Refs from earlier snapshots are dead; this is the new generation.
    pub snapshot_generation: u64,
    pub note: String,
}

#[derive(Clone)]
pub struct BrowserNavigateTool {
    manager: Arc<BrowserManager>,
}

impl Tool for BrowserNavigateTool {
    const NAME: &'static str = "browser_navigate";
    type Error = ToolError;
    type Args = NavigateArgs;
    type Output = NavigateOutput;

    fn description(&self) -> String {
        if self.manager.allows_open_web() && self.manager.allows_private_network_access() {
            "Open a URL in the built-in browser. Internet access is enabled, so any public \
             http(s) URL is allowed, plus localhost URLs (http://localhost:PORT, \
             http://127.0.0.1:PORT), file:// URLs inside the workspace, and private/internal \
             network targets (e.g. 192.168.x.x, 10.x.x.x) on the user's own network — only \
             link-local/cloud-metadata addresses (169.254.x.x) stay refused. Only http(s) \
             and file:// URLs: to press a button or follow a link, use browser_click with \
             a ref from browser_snapshot, never a javascript: URL. Navigating invalidates \
             every element ref from a previous browser_snapshot."
                .to_string()
        } else if self.manager.allows_open_web() {
            "Open a URL in the built-in browser. Internet access is enabled, so any public \
             http(s) URL is allowed, plus localhost URLs (http://localhost:PORT, \
             http://127.0.0.1:PORT) and file:// URLs inside the workspace — private/internal \
             network targets (RFC-1918, link-local, cloud metadata) stay refused either way. \
             Only http(s) and file:// URLs: to press a button or follow a link, use \
             browser_click with a ref from browser_snapshot, never a javascript: URL. \
             Navigating invalidates every element ref from a previous browser_snapshot."
                .to_string()
        } else {
            "Open a URL in the built-in browser. Only localhost URLs (http://localhost:PORT, \
             http://127.0.0.1:PORT) and file:// URLs inside the workspace are allowed — enable \
             internet access in Settings to browse the open web here too. Until then, use \
             search_web or fetch for anything on the internet. To press a button or follow \
             a link, use browser_click with a ref from browser_snapshot, never a \
             javascript: URL. Navigating invalidates every element ref from a previous \
             browser_snapshot."
                .to_string()
        }
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to open, e.g. 'http://localhost:3000/' or \
                                    'file:///path/inside/workspace/index.html'."
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
        let session = self.manager.session().await?;
        // No approved action carries over into a navigation (AGE-2154).
        session.gate().reset_risky();
        let url = navigate_checked(
            self.manager.is_unattended(),
            || async { session.navigate(&args.url).await.map_err(ToolError::from) },
            || inspect_page(&session),
        )
        .await?;
        info!(url = %url, "browser: navigated");
        Ok(NavigateOutput {
            url,
            snapshot_generation: session.snapshot_generation(),
            note: "Call browser_snapshot to see the page structure, or browser_screenshot \
                   to look at it."
                .to_string(),
        })
    }
}

// ── snapshot ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SnapshotOutput {
    /// Indented accessibility tree with `[eN]` refs.
    pub tree: String,
    pub node_count: usize,
    pub snapshot_generation: u64,
}

#[derive(Clone)]
pub struct BrowserSnapshotTool {
    manager: Arc<BrowserManager>,
}

impl Tool for BrowserSnapshotTool {
    const NAME: &'static str = "browser_snapshot";
    type Error = ToolError;
    type Args = NoArgs;
    type Output = SnapshotOutput;

    fn description(&self) -> String {
        "Read the current page as an accessibility tree: roles, names, and stable [eN] \
         element refs. This is the structural view — use it to find what is on the page, \
         and to get the ref browser_click needs. For questions about how the page *looks* \
         (spacing, alignment, colour), use browser_screenshot instead; those are pixel \
         judgements the tree cannot answer."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        empty_schema()
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let session = self.manager.session().await?;
        let page = session.page()?;

        let response = with_deadline(
            DEFAULT_TIMEOUT_SECS,
            "reading the accessibility tree",
            async {
                page.execute(
                chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeParams::default(),
            )
            .await
            .map_err(|e| BrowserError::Protocol(format!("cannot read accessibility tree: {e}")))
            },
        )
        .await?;

        // Round-trip through JSON so the flattening logic stays testable against
        // a captured tree without a browser in the loop.
        let nodes: Vec<AxNode> = serde_json::to_value(&response.result.nodes)
            .and_then(serde_json::from_value)
            .map_err(|e| {
                ToolError::OperationFailed(format!("cannot read accessibility tree: {e}"))
            })?;

        let snapshot = flatten_ax_tree(&nodes, session.snapshot_generation());
        // A dropdown's options are not in the tree; add them so the agent
        // knows it is a dropdown and what browser_select can pick (AGE-2009).
        // A page that will not answer leaves the plain tree, not an error.
        let notes = match session.describe_selects(&snapshot.nodes).await {
            Ok(notes) => notes,
            Err(e) => {
                warn!(error = %e, "browser: cannot describe dropdowns for the snapshot");
                Default::default()
            }
        };
        let tree = snapshot.to_text_with(&notes);
        let node_count = snapshot.nodes.len();
        let generation = snapshot.generation;
        self.manager.set_snapshot(snapshot).await;

        Ok(SnapshotOutput {
            tree,
            node_count,
            snapshot_generation: generation,
        })
    }
}

// ── screenshot ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ScreenshotOutput {
    /// Where the PNG was written.
    pub path: String,
    pub note: String,
}

#[derive(Clone)]
pub struct BrowserScreenshotTool {
    manager: Arc<BrowserManager>,
    /// Queue that surfaces the image to the user and carries it into the next
    /// turn's content. See `call` for why it is not returned as tool-result
    /// image content.
    pending_artifacts: PendingArtifacts,
}

impl Tool for BrowserScreenshotTool {
    const NAME: &'static str = "browser_screenshot";
    type Error = ToolError;
    type Args = NoArgs;
    type Output = ScreenshotOutput;

    fn description(&self) -> String {
        "Capture the current page as a PNG. The screenshot is shown in the chat and \
         attached to your next turn, so you can judge spacing, alignment, overlap and \
         colour from it — pixel questions the accessibility tree cannot answer. You will \
         not see it inside this tool result; end your turn to look at it. Do not call \
         add_attachment on the screenshot — it is already shown, and attaching it again \
         renders it twice."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        empty_schema()
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let dir = self.manager.output_dir().await?;
        let session = self.manager.session().await?;
        let page = session.page()?;

        let bytes = with_deadline(DEFAULT_TIMEOUT_SECS, "capturing a screenshot", async {
            page.screenshot(
                CaptureScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .build(),
            )
            .await
            .map_err(|e| BrowserError::Protocol(format!("screenshot failed: {e}")))
        })
        .await?;

        let path = dir.join(format!("screenshot-{}.png", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| BrowserError::Io(format!("cannot write {}: {e}", path.display())))?;

        info!(path = %path.display(), bytes = bytes.len(), "browser: screenshot captured");

        // Not returned as `ToolResultContent::Image`: none of the providers this
        // app ships with accept images in tool results — rig rejects them for
        // OpenRouter, Ollama and OpenAI Chat Completions alike, and the whole
        // stream dies on the conversion. Queueing the artifact is the path that
        // works: the user sees the screenshot immediately, and it is attached as
        // image content on the next turn, where the model can actually look at it.
        match self.pending_artifacts.lock() {
            Ok(mut artifacts) => artifacts.push(path.clone()),
            Err(e) => warn!(
                error = ?e,
                path = %path.display(),
                "Failed to lock pending_artifacts; screenshot saved but not queued for display"
            ),
        }

        Ok(ScreenshotOutput {
            path: path.display().to_string(),
            note: screenshot_note(self.manager.is_unattended()).to_string(),
        })
    }
}

/// What the screenshot result tells the model about seeing it. The desktop
/// attaches the image to the next turn; an unattended (hosted, headless) run
/// has no next turn — ending it ends the job (AGE-2008) — so the image is
/// saved for the user and the model is told to read the page instead.
fn screenshot_note(unattended: bool) -> &'static str {
    if unattended {
        "You cannot see this screenshot in this job. It is saved for the user; use \
         browser_snapshot to read the page."
    } else {
        "The screenshot is attached to your next turn — end this turn to look at it. \
         Do not try to read the PNG with read_file or read_binary; that returns \
         base64 text you cannot see."
    }
}

// ── console ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ConsoleOutput {
    pub total: usize,
    pub errors: usize,
    /// The problem lines, inline — capped, because these are what matter.
    pub problems: Vec<String>,
    /// Full dump, for grepping when the summary is not enough.
    pub dump_path: Option<String>,
    pub note: String,
}

/// How many problem lines we inline before deferring to the dump file.
const MAX_INLINE_PROBLEMS: usize = 20;

#[derive(Clone)]
pub struct BrowserConsoleTool {
    manager: Arc<BrowserManager>,
}

impl Tool for BrowserConsoleTool {
    const NAME: &'static str = "browser_console";
    type Error = ToolError;
    type Args = NoArgs;
    type Output = ConsoleOutput;

    fn description(&self) -> String {
        "Drain console output and uncaught exceptions captured since the last call. \
         Returns a count plus the error and warning lines; the full log is written to a \
         file you can grep if you need more than the summary."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        empty_schema()
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let dir = self.manager.output_dir().await?;
        let session = self.manager.session().await?;
        let (entries, dropped) = session.events()?.drain_console();

        let problems: Vec<String> = entries
            .iter()
            .filter(|e| e.is_problem())
            .map(|e| format!("[{}] {}", e.level, e.text))
            .collect();
        let errors = problems.len();

        let dump_path = write_dump(
            &dir,
            "console",
            &entries
                .iter()
                .map(|e| format!("[{}] {}", e.level, e.text))
                .collect::<Vec<_>>(),
        )
        .await?;

        Ok(ConsoleOutput {
            total: entries.len(),
            errors,
            problems: problems.into_iter().take(MAX_INLINE_PROBLEMS).collect(),
            dump_path,
            note: summary_note(entries.len(), errors, dropped, MAX_INLINE_PROBLEMS),
        })
    }
}

// ── network ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct NetworkOutput {
    pub total: usize,
    pub failures: usize,
    pub problems: Vec<String>,
    pub dump_path: Option<String>,
    pub note: String,
}

#[derive(Clone)]
pub struct BrowserNetworkTool {
    manager: Arc<BrowserManager>,
}

impl Tool for BrowserNetworkTool {
    const NAME: &'static str = "browser_network";
    type Error = ToolError;
    type Args = NoArgs;
    type Output = NetworkOutput;

    fn description(&self) -> String {
        "Drain network activity captured since the last call. Returns a count plus the \
         failed and 4xx/5xx requests; the full list is written to a file you can grep."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        empty_schema()
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let dir = self.manager.output_dir().await?;
        let session = self.manager.session().await?;
        let (entries, dropped) = session.events()?.drain_network();

        let describe = |e: &crate::services::browser::NetworkEntry| match (&e.error, e.status) {
            (Some(err), _) => format!("FAILED {} — {}", e.url, err),
            (None, Some(status)) => format!("{} {}", status, e.url),
            (None, None) => e.url.clone(),
        };

        let problems: Vec<String> = entries
            .iter()
            .filter(|e| e.is_problem())
            .map(describe)
            .collect();
        let failures = problems.len();

        let dump_path = write_dump(
            &dir,
            "network",
            &entries.iter().map(describe).collect::<Vec<_>>(),
        )
        .await?;

        Ok(NetworkOutput {
            total: entries.len(),
            failures,
            problems: problems.into_iter().take(MAX_INLINE_PROBLEMS).collect(),
            dump_path,
            note: summary_note(entries.len(), failures, dropped, MAX_INLINE_PROBLEMS),
        })
    }
}

// ── resize ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ResizeArgs {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize)]
pub struct ResizeOutput {
    pub width: u32,
    pub height: u32,
}

/// Chrome refuses absurd viewports, and a huge one is also a huge screenshot.
const MAX_VIEWPORT: u32 = 10_000;

#[derive(Clone)]
pub struct BrowserResizeTool {
    manager: Arc<BrowserManager>,
}

impl Tool for BrowserResizeTool {
    const NAME: &'static str = "browser_resize";
    type Error = ToolError;
    type Args = ResizeArgs;
    type Output = ResizeOutput;

    fn description(&self) -> String {
        "Resize the browser viewport, then re-screenshot. Responsive checks catch most of \
         what a design review is for — try a phone width (390x844) and a desktop width \
         (1440x900)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "width": { "type": "integer", "description": "Viewport width in CSS pixels." },
                "height": { "type": "integer", "description": "Viewport height in CSS pixels." }
            },
            "required": ["width", "height"]
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
        if args.width == 0
            || args.height == 0
            || args.width > MAX_VIEWPORT
            || args.height > MAX_VIEWPORT
        {
            return Err(ToolError::OperationFailed(format!(
                "viewport must be between 1x1 and {MAX_VIEWPORT}x{MAX_VIEWPORT}, got {}x{}",
                args.width, args.height
            )));
        }

        let session = self.manager.session().await?;
        session.ensure_agent_control()?;
        let page = session.page()?;

        with_deadline(DEFAULT_TIMEOUT_SECS, "resizing the viewport", async {
            page.execute(SetDeviceMetricsOverrideParams::new(
                args.width as i64,
                args.height as i64,
                1.0,
                false,
            ))
            .await
            .map_err(|e| BrowserError::Protocol(format!("resize failed: {e}")))
        })
        .await?;

        Ok(ResizeOutput {
            width: args.width,
            height: args.height,
        })
    }
}

// ── click ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ClickArgs {
    /// An element ref from the latest `browser_snapshot`, e.g. `e12`.
    pub r#ref: String,
}

#[derive(Debug, Serialize)]
pub struct ClickOutput {
    /// What was clicked, as the snapshot described it.
    pub clicked: String,
    /// Where the page is after the click settled.
    pub url: String,
    /// Whether the click moved the page. If so, every ref is dead.
    pub navigated: bool,
    pub snapshot_generation: u64,
    pub note: String,
}

/// Left-click one element by snapshot ref (AGE-489).
///
/// The ref is the only input, and it is checked three times over before a
/// pointer event is sent: it must belong to the current snapshot generation,
/// the element must still be what the snapshot showed, and the click point
/// must land on it — see `services::browser::click`. Off Lane A origins the
/// click is also an approval card the user answers first.
#[derive(Clone)]
pub struct BrowserClickTool {
    manager: Arc<BrowserManager>,
    pending_approvals: Option<PendingApprovals>,
}

/// Whether an action on a page at `url` needs the user's approval: anything
/// that is not a Lane A origin (loopback http(s) or workspace `file://`).
/// Decided by the URL, not the manager's policy — with internet access on,
/// the same session serves both the agent's own dev server and the open
/// web, and only the latter can carry a session the user logged into.
fn action_needs_approval(url: &str, workspace: Option<&std::path::Path>) -> bool {
    NavigationPolicy::local_only(workspace.map(|w| w.to_path_buf()))
        .check(url)
        .is_err()
}

/// `s` on one line, cut to `max` characters with an ellipsis.
fn short(s: &str, max: usize) -> String {
    let one_line: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if one_line.chars().count() > max {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    } else {
        one_line
    }
}

/// A page URL as the approval card shows it: the host in full — that is
/// what tells the user which site is being acted on — then the path cut
/// short, and never the query or fragment, which may carry session tokens
/// and would otherwise be persisted as the card's text (AGE-492).
fn display_url(url: &str) -> String {
    const MAX_PATH: usize = 40;
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return short(url, MAX_PATH);
    };
    let host = parsed.host_str().unwrap_or_default();
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = if parsed.path() == "/" {
        String::new()
    } else {
        short(parsed.path(), MAX_PATH)
    };
    let query = if parsed.query().is_some() { "?…" } else { "" };
    if host.is_empty() {
        // file:// — the path is all there is; keep its tail, where the name is.
        let tail: String = parsed.path().chars().rev().take(MAX_PATH).collect();
        let tail: String = tail.chars().rev().collect();
        return if tail.len() < parsed.path().len() {
            format!("{}:…{tail}", parsed.scheme())
        } else {
            format!("{}:{tail}", parsed.scheme())
        };
    }
    format!("{host}{port}{path}{query}")
}

/// The approval-card text for a click: what would be clicked, where.
fn click_approval_label(role: &str, name: &str, url: &str) -> String {
    format!(
        "[browser] click {role} \"{}\" on {}",
        short(name, 80),
        display_url(url)
    )
}

/// The approval-card text for typing: what would be typed, into what, where.
fn type_approval_label(role: &str, name: &str, text: &str, url: &str) -> String {
    format!(
        "[browser] type \"{}\" into {role} \"{}\" on {}",
        short(text, 60),
        short(name, 60),
        display_url(url)
    )
}

/// `role "name"`, or just the role for an unnamed element.
fn describe(node: &crate::services::browser::SnapshotNode) -> String {
    if node.name.is_empty() {
        node.role.clone()
    } else {
        format!("{} \"{}\"", node.role, node.name)
    }
}

/// Whether a person can answer an approval card: there is a channel and the
/// host has not said nobody is behind it (AGE-2038).
fn is_attended(manager: &BrowserManager, pending_approvals: Option<&PendingApprovals>) -> bool {
    pending_approvals.is_some() && !manager.is_unattended()
}

/// Everything an acting tool does before it acts (AGE-489): resolve the ref
/// against the current generation and, off Lane A origins, put the action to
/// the user as an approval card first. `label` builds that card's text from
/// the element and the page URL; `verb` names the action in refusals.
async fn prepare_action(
    manager: &BrowserManager,
    pending_approvals: Option<&PendingApprovals>,
    r#ref: &str,
    verb: &str,
    is_click: bool,
    label: impl FnOnce(&crate::services::browser::SnapshotNode, &str) -> String,
) -> Result<
    (
        Arc<crate::services::browser::BrowserSession>,
        crate::services::browser::SnapshotNode,
        bool,
        Option<RiskyApproval>,
    ),
    ToolError,
> {
    // Before touching the session: a missing snapshot is a model mistake
    // and must not launch a browser to be reported.
    let snapshot = manager.snapshot().await.ok_or_else(|| {
        ToolError::OperationFailed(
            "no snapshot to take refs from; call browser_snapshot first".to_string(),
        )
    })?;
    let session = manager.session().await?;
    session.ensure_agent_control()?;
    let page = session.page()?;
    let node = snapshot
        .resolve(r#ref, session.snapshot_generation())?
        .clone();

    let url = page
        .url()
        .await
        .ok()
        .flatten()
        .ok_or_else(|| ToolError::OperationFailed("cannot read the page URL".into()))?;
    let act_on = manager.act_on();
    let off_lane_a = action_needs_approval(&url, manager.workspace().ok());
    // Anything the interceptor refused before this action is not this action's.
    session.gate().take_outcome();
    session.gate().reset_risky();
    let mut approval = None;
    let gate = sites::decide(
        act_on,
        &url,
        off_lane_a,
        is_click,
        &node.role,
        &node.name,
        is_attended(manager, pending_approvals),
    );
    let look = match gate {
        sites::ActionGate::Proceed { look } => look,
        sites::ActionGate::Refuse => {
            return Err(ToolError::OperationFailed(if act_on.is_empty() {
                format!(
                    "{verb} on {} needs the user's approval and no approval channel is available \
                     in this session",
                    display_url(&url)
                )
            } else {
                format!(
                    "this run may act only on: {}",
                    sites::granted_origins(act_on).join(", ")
                )
            }));
        }
        sites::ActionGate::NeedsApproval => {
            // A person approved exactly this action (AGE-2043): once. It is
            // spent in `after_action`, when the action went through, so a
            // click refused for a covered button keeps it for the retry.
            let origin = sites::origin_of(&url).unwrap_or_default();
            if manager.approved_action_matches(&origin, &bare_url(&url), &describe(&node)) {
                approval = Some(session.gate().approve_risky_for(&origin));
                false
            } else {
                let title = page_title(&page).await;
                let screenshot = manager.proof_approval(&session, &node).await;
                return Err(needs_approval_error(
                    &url,
                    &describe(&node),
                    &title,
                    screenshot.as_deref(),
                ));
            }
        }
        sites::ActionGate::Ask => {
            let pending = pending_approvals.expect("Ask only with an approval channel");
            // Always asks: the shell's auto-approve modes are about commands
            // the user chose to trust, not about acting inside their web
            // sessions (AGE-158: per action, never generalised).
            let approved = request_execution_approval(
                pending,
                &ApprovalMode::AlwaysAsk,
                &label(&node, &url),
                false,
            )
            .await
            .map_err(|e| ToolError::OperationFailed(format!("approval failed: {e}")))?;
            if !approved {
                return Err(ToolError::OperationFailed(format!(
                    "the user declined to {verb} {} on {}",
                    describe(&node),
                    display_url(&url)
                )));
            }
            // The card may have been open for minutes; the page the user
            // approved must be the page that gets acted on.
            if session.snapshot_generation() != snapshot.generation {
                return Err(BrowserError::StaleRef(
                    r#ref.to_string(),
                    snapshot.generation,
                    session.snapshot_generation(),
                )
                .into());
            }
            false
        }
    };
    Ok((session, node, look, approval))
}

/// The page's title, or empty.
async fn page_title(page: &chromiumoxide::page::Page) -> String {
    page.get_title().await.ok().flatten().unwrap_or_default()
}

/// The page address without query or fragment.
fn bare_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) => {
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// The structured error an unattended run gets for a risky action
/// (ADR-0108); hive turns it into "Needs your OK". The URL carries no query.
/// `screenshot` names the proof picture in `outputs/` when the host asked for
/// one (AGE-2008).
fn needs_approval_error(
    url: &str,
    control: &str,
    title: &str,
    screenshot: Option<&str>,
) -> ToolError {
    let bare = bare_url(url);
    let mut body = serde_json::json!({
        "error": "needs_approval",
        "origin": sites::origin_of(url).unwrap_or_default(),
        "action": format!("{control} on {}", short(title, 80)),
        "url": bare,
    });
    if let Some(name) = screenshot {
        body["screenshot"] = name.into();
    }
    ToolError::OperationFailed(body.to_string())
}

/// The main document as the site-check detector sees it, with its verdict.
struct CheckedPage {
    url: String,
    title: String,
    vendor: &'static str,
}

/// Longest slice of the document the detector reads.
const CHECK_HTML_CAP: usize = 200_000;

/// Read the main document and say whether it is a site's bot check
/// (AGE-2047). A page that cannot be read is not a check.
async fn inspect_page(session: &crate::services::browser::BrowserSession) -> Option<CheckedPage> {
    let page = session.page().ok()?;
    let status = page
        .evaluate("(performance.getEntriesByType('navigation')[0] || {}).responseStatus || 0")
        .await
        .ok()
        .and_then(|v| v.into_value::<u32>().ok())
        .filter(|s| *s > 0)
        .map(|s| s as u16);
    let html = page
        .evaluate("document.documentElement ? document.documentElement.outerHTML : ''")
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .unwrap_or_default();
    let html: String = html.chars().take(CHECK_HTML_CAP).collect();
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page_title(&page).await;
    let check = crate::services::browser::site_check::detect(
        &crate::services::browser::site_check::PageFacts {
            status,
            url: &url,
            title: &title,
            html: &html,
        },
    )?;
    Some(CheckedPage {
        url,
        title,
        vendor: check.vendor,
    })
}

/// Navigate exactly once, then look at the page. A site check ends the call
/// with an error; the browser never navigates again on its own.
async fn navigate_checked<N, NF, I, IF>(
    unattended: bool,
    navigate: N,
    inspect: I,
) -> Result<String, ToolError>
where
    N: FnOnce() -> NF,
    NF: std::future::Future<Output = Result<String, ToolError>>,
    I: FnOnce() -> IF,
    IF: std::future::Future<Output = Option<CheckedPage>>,
{
    let url = navigate().await?;
    match inspect().await {
        Some(check) => Err(site_check_error(&check, unattended)),
        None => Ok(url),
    }
}

/// What the agent is told when the page is a site's bot check. Unattended it
/// is the structured `needs_human` error (same shape and bare URL as
/// `needs_approval_error`); attended it asks the user in words.
fn site_check_error(check: &CheckedPage, unattended: bool) -> ToolError {
    let origin = sites::origin_of(&check.url).unwrap_or_default();
    let host = origin
        .split_once("://")
        .map_or(origin.as_str(), |(_, h)| h)
        .trim_start_matches("www.")
        .to_string();
    if !unattended {
        return ToolError::OperationFailed(format!(
            "{host} shows a check that a person must pass. Open it in the browser view and \
             tell me when done. This site wants a person to pass a check. Stop and report; \
             do not retry."
        ));
    }
    let body = serde_json::json!({
        "error": "needs_human",
        "kind": "site_check",
        "origin": origin,
        "url": bare_url(&check.url),
        "title": check.title,
        "vendor": check.vendor,
        "note": SITE_CHECK_NOTE,
    });
    ToolError::OperationFailed(body.to_string())
}

/// The agent's instruction when a site wants a person.
const SITE_CHECK_NOTE: &str =
    "This site wants a person to pass a check. Stop and report; do not retry.";

/// After an action: look-only sites say nothing was changed; a risky request
/// refused while unattended becomes `needs_approval`.
async fn after_action(
    manager: &BrowserManager,
    session: &crate::services::browser::BrowserSession,
    node: &crate::services::browser::SnapshotNode,
    look: bool,
    approval: Option<RiskyApproval>,
    control: &str,
) -> Result<(), ToolError> {
    let gate = session.gate();
    // The action went through: an approval it used is spent (AGE-2043).
    if approval.is_some() {
        manager.spend_approved_action();
    }
    // The interceptor answers asynchronously; give a submit time to be seen.
    if look || !gate.act_on().is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let outcome = gate.take_outcome();
    // Whatever happens next, page requests are refused again (AGE-2154).
    drop(approval);
    if let Some((_, url)) = outcome.risky_blocked {
        let title = match session.page() {
            Ok(page) => page_title(&page).await,
            Err(_) => String::new(),
        };
        let screenshot = manager.proof_approval(session, node).await;
        return Err(needs_approval_error(
            &url,
            control,
            &title,
            screenshot.as_deref(),
        ));
    }
    Ok(())
}

/// What the tool result says on a look-only site.
const LOOK_NOTE: &str = "look only: the site was not changed";

impl Tool for BrowserClickTool {
    const NAME: &'static str = "browser_click";
    type Error = ToolError;
    type Args = ClickArgs;
    type Output = ClickOutput;

    fn description(&self) -> String {
        "Left-click one element on the current page, by its [eN] ref from the latest \
         browser_snapshot. Refs die when the page navigates or the user takes control — \
         take a fresh snapshot after either. The click is refused if the element changed \
         since the snapshot, is off screen, or is covered by a dialog or overlay. On pages \
         outside localhost and the workspace, every click first asks the user for \
         approval, so expect to wait; never try to work around that with a URL."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Element ref from the latest browser_snapshot, e.g. 'e12'."
                }
            },
            "required": ["ref"]
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
        let (session, node, look, approval) = prepare_action(
            &self.manager,
            self.pending_approvals.as_ref(),
            &args.r#ref,
            "click",
            true,
            |node, url| click_approval_label(&node.role, &node.name, url),
        )
        .await?;
        let clicked = describe(&node);

        let result = session.click(&node).await?;
        after_action(&self.manager, &session, &node, look, approval, &clicked).await?;
        if let Some(check) = inspect_page(&session).await {
            return Err(site_check_error(&check, self.manager.is_unattended()));
        }
        if !look {
            // A click that went through on an ask_risky or any site (AGE-2008).
            self.manager.proof_after(&session).await;
        }
        info!(r#ref = %args.r#ref, clicked = %clicked, url = %result.url, navigated = result.navigated, "browser: clicked");
        let note = if look {
            LOOK_NOTE.to_string()
        } else if result.navigated {
            "The page navigated; every ref is invalid. Call browser_snapshot before clicking \
             anything else."
                .to_string()
        } else {
            "Take a new browser_snapshot to see what changed; refs to unchanged elements \
             remain valid."
                .to_string()
        };
        Ok(ClickOutput {
            clicked,
            url: result.url,
            navigated: result.navigated,
            snapshot_generation: result.snapshot_generation,
            note,
        })
    }
}

// ── type ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct TypeArgs {
    /// An element ref from the latest `browser_snapshot`, e.g. `e12`.
    pub r#ref: String,
    /// What the field should contain afterwards.
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct TypeOutput {
    /// The field, as the snapshot described it.
    pub typed_into: String,
    /// What the field reads now, from the accessibility tree.
    pub value: String,
    pub snapshot_generation: u64,
    pub note: String,
}

/// Replace a text field's contents by snapshot ref (AGE-492).
///
/// Same locate-and-verify path and the same approval gate as
/// [`BrowserClickTool`]; on top, password and payment-card fields are
/// refused outright — see `services::browser::typing`.
#[derive(Clone)]
pub struct BrowserTypeTool {
    manager: Arc<BrowserManager>,
    pending_approvals: Option<PendingApprovals>,
}

impl Tool for BrowserTypeTool {
    const NAME: &'static str = "browser_type";
    type Error = ToolError;
    type Args = TypeArgs;
    type Output = TypeOutput;

    fn description(&self) -> String {
        "Replace the contents of a text field (input, textarea, search box, editable area) \
         with the given text, by its [eN] ref from the latest browser_snapshot. Whatever the \
         field held before is replaced, not appended to. Password and payment-card fields \
         are refused — ask the user to take control and fill those in. Does not press \
         Enter; to submit, browser_click the form's button. On pages outside localhost and \
         the workspace, every call first asks the user for approval."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Element ref from the latest browser_snapshot, e.g. 'e12'."
                },
                "text": {
                    "type": "string",
                    "description": "The text the field should contain afterwards."
                }
            },
            "required": ["ref", "text"]
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
        if args.text.chars().count() > MAX_TEXT_LEN {
            return Err(ToolError::OperationFailed(format!(
                "text is longer than {MAX_TEXT_LEN} characters; split it up"
            )));
        }
        let (session, node, look, approval) = prepare_action(
            &self.manager,
            self.pending_approvals.as_ref(),
            &args.r#ref,
            "type into",
            false,
            |node, url| type_approval_label(&node.role, &node.name, &args.text, url),
        )
        .await?;
        let typed_into = describe(&node);

        let value = session.type_text(&node, &args.text).await?;
        after_action(&self.manager, &session, &node, look, approval, &typed_into).await?;
        info!(r#ref = %args.r#ref, typed_into = %typed_into, chars = args.text.chars().count(), "browser: typed");
        Ok(TypeOutput {
            typed_into,
            value,
            snapshot_generation: session.snapshot_generation(),
            note: if look {
                LOOK_NOTE.to_string()
            } else {
                "The field was not submitted. Refs stay valid; browser_click a button or \
                 take a new browser_snapshot to continue."
                    .to_string()
            },
        })
    }
}

// ── select ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct SelectArgs {
    /// Element ref of the dropdown from the latest browser_snapshot, e.g. `e12`.
    pub r#ref: String,
    /// The option's visible label or its value.
    pub option: String,
}

#[derive(Debug, Serialize)]
pub struct SelectOutput {
    /// The dropdown, as the snapshot described it.
    pub selected_in: String,
    /// The label selected now.
    pub selected: String,
    pub snapshot_generation: u64,
    pub note: String,
}

/// Set a native dropdown (`<select>`) by snapshot ref (AGE-2009).
///
/// Same locate-and-verify path and the same approval gate as
/// [`BrowserTypeTool`]: pages outside localhost and the workspace ask first,
/// and the risky-action rules of a site grant apply.
#[derive(Clone)]
pub struct BrowserSelectTool {
    manager: Arc<BrowserManager>,
    pending_approvals: Option<PendingApprovals>,
}

/// The approval-card text for choosing a dropdown option.
fn select_approval_label(role: &str, name: &str, option: &str, url: &str) -> String {
    format!(
        "[browser] select \"{}\" in {role} \"{}\" on {}",
        short(option, 60),
        short(name, 60),
        display_url(url)
    )
}

impl Tool for BrowserSelectTool {
    const NAME: &'static str = "browser_select";
    type Error = ToolError;
    type Args = SelectArgs;
    type Output = SelectOutput;

    fn description(&self) -> String {
        "Choose an option in a dropdown (<select>), by its [eN] ref from the latest \
         browser_snapshot and the option's visible label or its value. Use this instead of \
         browser_click or browser_type for dropdowns: a click cannot open a native dropdown \
         here, and typing into a nearby box sets nothing. It fires the input and change \
         events a user's choice would, and returns the label now selected. An unknown \
         option is an error that lists the available labels. On pages outside localhost and \
         the workspace, every call first asks the user for approval."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Element ref of the dropdown from the latest browser_snapshot, e.g. 'e12'."
                },
                "option": {
                    "type": "string",
                    "description": "The option's visible label or its value."
                }
            },
            "required": ["ref", "option"]
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
        if args.option.chars().count() > MAX_TEXT_LEN {
            return Err(ToolError::OperationFailed(format!(
                "option is longer than {MAX_TEXT_LEN} characters"
            )));
        }
        let (session, node, look, approval) = prepare_action(
            &self.manager,
            self.pending_approvals.as_ref(),
            &args.r#ref,
            "select in",
            false,
            |node, url| select_approval_label(&node.role, &node.name, &args.option, url),
        )
        .await?;
        let selected_in = describe(&node);

        let selected = session.select_option(&node, &args.option).await?;
        after_action(&self.manager, &session, &node, look, approval, &selected_in).await?;
        info!(r#ref = %args.r#ref, selected_in = %selected_in, selected = %selected, "browser: selected");
        Ok(SelectOutput {
            selected_in,
            selected,
            snapshot_generation: session.snapshot_generation(),
            note: if look {
                LOOK_NOTE.to_string()
            } else {
                "The form was not submitted. Refs stay valid; browser_click a button or \
                 take a new browser_snapshot to continue."
                    .to_string()
            },
        })
    }
}

// ── shared helpers ──────────────────────────────────────────────────────────

/// Write the full dump to a file, returning its path. `None` when there is
/// nothing to write — an empty file is just noise for the agent to open.
async fn write_dump(
    dir: &std::path::Path,
    kind: &str,
    lines: &[String],
) -> Result<Option<String>, BrowserError> {
    if lines.is_empty() {
        return Ok(None);
    }
    let path = dir.join(format!("{kind}-{}.log", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, lines.join("\n"))
        .await
        .map_err(|e| BrowserError::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(Some(path.display().to_string()))
}

/// One sentence telling the agent whether the summary is the whole story.
fn summary_note(total: usize, problems: usize, dropped: usize, inline_cap: usize) -> String {
    let mut note = if total == 0 {
        "Nothing captured since the last call.".to_string()
    } else if problems == 0 {
        format!("{total} entries, none of them problems.")
    } else {
        format!("{total} entries, {problems} of them problems.")
    };
    if problems > inline_cap {
        note.push_str(&format!(
            " Only the first {inline_cap} are listed — grep dump_path for the rest."
        ));
    }
    if dropped > 0 {
        note.push_str(&format!(
            " {dropped} older entries were dropped because the buffer filled."
        ));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_agent::tool::tool_definition;

    fn artifacts() -> PendingArtifacts {
        Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    fn manager() -> Arc<BrowserManager> {
        Arc::new(BrowserManager::lane_a(Some(std::path::PathBuf::from(
            "/ws",
        ))))
    }

    #[test]
    fn every_tool_has_a_definition_naming_the_browser() {
        let (nav, snap, shot, console, net, resize, click, r#type, select) =
            build_browser_tools(manager(), artifacts(), None);
        assert_eq!(tool_definition(&nav).name, "browser_navigate");
        assert_eq!(tool_definition(&snap).name, "browser_snapshot");
        assert_eq!(tool_definition(&shot).name, "browser_screenshot");
        assert_eq!(tool_definition(&console).name, "browser_console");
        assert_eq!(tool_definition(&net).name, "browser_network");
        assert_eq!(tool_definition(&resize).name, "browser_resize");
        assert_eq!(tool_definition(&click).name, "browser_click");
        assert_eq!(tool_definition(&r#type).name, "browser_type");
        assert_eq!(tool_definition(&select).name, "browser_select");
    }

    /// The model reached for `javascript:` URLs because nothing told it how
    /// to press a button; every navigate variant now points at the click.
    #[test]
    fn navigate_descriptions_point_at_browser_click() {
        for manager in [
            manager(),
            Arc::new(BrowserManager::open_web(Some("/ws".into()), false)),
            Arc::new(BrowserManager::open_web(Some("/ws".into()), true)),
        ] {
            let (nav, ..) = build_browser_tools(manager, artifacts(), None);
            let description = tool_definition(&nav).description;
            assert!(description.contains("browser_click"), "{description}");
            assert!(description.contains("javascript:"), "{description}");
        }
    }

    #[test]
    fn click_schema_takes_exactly_one_ref() {
        let (.., click, _, _) = build_browser_tools(manager(), artifacts(), None);
        let definition = tool_definition(&click);
        assert_eq!(
            definition.parameters["required"],
            serde_json::json!(["ref"])
        );
        assert_eq!(
            definition.parameters["properties"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
    }

    /// The gate is decided by where the page is, not by which policy the
    /// session runs under: with internet access on, the agent's own dev
    /// server is still ungated and the open web is still gated.
    #[test]
    fn click_is_gated_exactly_off_lane_a_origins() {
        let ws = std::path::Path::new("/ws");
        for url in [
            "http://localhost:3000/",
            "http://127.0.0.1:5173/app",
            "https://app.localhost/",
            "file:///ws/dist/index.html",
        ] {
            assert!(
                !action_needs_approval(url, Some(ws)),
                "{url} should be free"
            );
        }
        for url in [
            "https://example.com/",
            "http://192.168.1.10/admin",
            "file:///etc/passwd",
            "about:blank",
        ] {
            assert!(action_needs_approval(url, Some(ws)), "{url} should ask");
        }
        // No workspace: file:// has nothing to be inside of.
        assert!(action_needs_approval("file:///ws/index.html", None));
    }

    #[test]
    fn click_approval_label_names_the_element_and_the_page() {
        assert_eq!(
            click_approval_label("button", "Buy now", "https://shop.example/cart"),
            "[browser] click button \"Buy now\" on shop.example/cart"
        );
        let long = "x".repeat(200);
        let label = click_approval_label("link", &long, "https://e.com/");
        assert!(label.contains(&format!("{}…", "x".repeat(80))));
        assert!(!label.contains(&"x".repeat(81)));
    }

    /// AGE-492: the card must stay readable, and must never carry a query
    /// string — that is where session tokens live.
    #[test]
    fn display_url_keeps_the_host_and_drops_the_rest() {
        assert_eq!(display_url("https://shop.example/"), "shop.example");
        assert_eq!(display_url("http://localhost:3000/"), "localhost:3000");
        assert_eq!(
            display_url("https://shop.example/cart?session=abc&token=verysecret#top"),
            "shop.example/cart?…"
        );
        let long_path = format!("https://app.example.com/{}", "segment/".repeat(20));
        let shown = display_url(&long_path);
        assert!(shown.starts_with("app.example.com/segment/"), "{shown}");
        assert!(shown.ends_with('…'), "{shown}");
        assert!(shown.chars().count() < 60, "{shown}");
        assert!(!shown.contains("segment/".repeat(8).as_str()), "{shown}");
        // A very long host is never cut: it is the one thing the user must see whole.
        let host = format!("https://{}.example.com/x", "sub.".repeat(20));
        assert!(display_url(&host).contains(&"sub.".repeat(20)));
        // file:// keeps the tail of the path, where the file name is.
        let file = display_url("file:///ws/very/deep/directory/tree/dist/index.html");
        assert!(file.starts_with("file:…"), "{file}");
        assert!(file.ends_with("dist/index.html"), "{file}");
        assert_eq!(display_url("file:///ws/index.html"), "file:/ws/index.html");
        assert_eq!(display_url("not a url"), "not a url");
    }

    #[test]
    fn type_approval_label_shows_the_text_on_one_line_and_cut() {
        assert_eq!(
            type_approval_label(
                "textbox",
                "Email",
                "me@example.com",
                "https://a.example/login"
            ),
            "[browser] type \"me@example.com\" into textbox \"Email\" on a.example/login"
        );
        let label = type_approval_label(
            "textbox",
            "Body",
            "line one\nline two",
            "https://a.example/",
        );
        assert!(label.contains("\"line one line two\""), "{label}");
        let label = type_approval_label("textbox", "Body", &"y".repeat(100), "https://a.example/");
        assert!(label.contains(&format!("{}…", "y".repeat(60))));
    }

    #[test]
    fn type_schema_takes_a_ref_and_text() {
        let (.., r#type, _) = build_browser_tools(manager(), artifacts(), None);
        let definition = tool_definition(&r#type);
        assert_eq!(
            definition.parameters["required"],
            serde_json::json!(["ref", "text"])
        );
        assert!(
            definition.description.contains("Password"),
            "{}",
            definition.description
        );
    }

    #[tokio::test]
    async fn type_refuses_oversized_text_before_touching_the_browser() {
        let (.., r#type, _) = build_browser_tools(manager(), artifacts(), None);
        let args = TypeArgs {
            r#ref: "e1".to_string(),
            text: "z".repeat(MAX_TEXT_LEN + 1),
        };
        let err = r#type
            .call(&mut ToolContext::new(), args)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("longer than"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn click_without_a_snapshot_says_so_rather_than_launching_a_browser() {
        // `session()` would provision and launch Chrome; the snapshot check
        // comes first so a model mistake never costs a browser process.
        let (.., click, _, _) = build_browser_tools(manager(), artifacts(), None);
        let args = ClickArgs {
            r#ref: "e1".to_string(),
        };
        let err = click.call(&mut ToolContext::new(), args).await.unwrap_err();
        assert!(
            err.to_string().contains("browser_snapshot first"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn navigate_description_states_the_lane_a_restriction() {
        let (nav, ..) = build_browser_tools(manager(), artifacts(), None);
        let description = tool_definition(&nav).description;
        assert!(description.contains("localhost"));
        assert!(description.contains("file://"));
    }

    /// AGE-459: the model is told when the workspace toggle has opened up
    /// private/internal targets, so it will actually try them instead of
    /// assuming they are refused.
    #[test]
    fn navigate_description_reflects_private_network_toggle() {
        let open_manager = Arc::new(BrowserManager::open_web(
            Some(std::path::PathBuf::from("/ws")),
            false,
        ));
        let (nav, ..) = build_browser_tools(open_manager, artifacts(), None);
        let description = tool_definition(&nav).description;
        assert!(description.contains("stay refused either way"));

        let private_manager = Arc::new(BrowserManager::open_web(
            Some(std::path::PathBuf::from("/ws")),
            true,
        ));
        let (nav, ..) = build_browser_tools(private_manager, artifacts(), None);
        let description = tool_definition(&nav).description;
        assert!(description.contains("192.168.x.x"));
        assert!(!description.contains("stay refused either way"));
    }

    #[tokio::test]
    async fn resize_rejects_out_of_range_viewports() {
        let (_, _, _, _, _, resize, _, _, _) = build_browser_tools(manager(), artifacts(), None);
        for (width, height) in [(0, 800), (800, 0), (MAX_VIEWPORT + 1, 800)] {
            let result = resize
                .call(&mut ToolContext::new(), ResizeArgs { width, height })
                .await;
            assert!(result.is_err(), "{width}x{height} should be rejected");
        }
    }

    #[tokio::test]
    async fn tools_needing_a_workspace_say_so_rather_than_launching_a_browser() {
        let manager = Arc::new(BrowserManager::lane_a(None));
        let (_, _, shot, ..) = build_browser_tools(manager, artifacts(), None);
        let err = shot
            .call(&mut ToolContext::new(), NoArgs {})
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("workspace"),
            "unexpected error: {err}"
        );
    }

    /// AGE-2009: a hosted (unattended) run has no next turn to see the image
    /// in, so it is told so; the desktop text stays as it was.
    #[test]
    fn the_screenshot_note_differs_between_a_headless_and_a_desktop_session() {
        use crate::services::browser::LaunchOptions;

        let desktop = manager();
        let headless = Arc::new(
            BrowserManager::lane_a(Some(std::path::PathBuf::from("/ws"))).with_launch_options(
                LaunchOptions {
                    unattended: true,
                    ..Default::default()
                },
            ),
        );
        let on_desktop = screenshot_note(desktop.is_unattended());
        let in_job = screenshot_note(headless.is_unattended());
        assert_ne!(on_desktop, in_job);
        assert_eq!(
            in_job,
            "You cannot see this screenshot in this job. It is saved for the user; use \
             browser_snapshot to read the page."
        );
        assert!(
            on_desktop.contains("end this turn to look at it"),
            "{on_desktop}"
        );
        assert!(
            !on_desktop.contains("cannot see this screenshot"),
            "{on_desktop}"
        );
    }

    fn risky_gate(unattended: bool, channel: bool) -> sites::ActionGate {
        use crate::services::browser::LaunchOptions;
        let m = BrowserManager::lane_a(Some(std::path::PathBuf::from("/ws"))).with_launch_options(
            LaunchOptions {
                unattended,
                ..Default::default()
            },
        );
        let pending: PendingApprovals = Arc::new(parking_lot::Mutex::new(
            crate::models::execution_approval_store::PendingApprovalsState::new(),
        ));
        let act_on = [crate::settings::models::execution_settings::ActOnSite {
            origin: "https://shop.example".into(),
            mode: crate::settings::models::execution_settings::SiteMode::AskRisky,
        }];
        sites::decide(
            &act_on,
            "https://shop.example/cart",
            true,
            true,
            "button",
            "Delete account",
            is_attended(&m, channel.then_some(&pending)),
        )
    }

    /// AGE-2038: a host that says nobody is attached gets `needs_approval`
    /// at once even with an approval channel; no card, no wait.
    fn checked(url: &str) -> CheckedPage {
        CheckedPage {
            url: url.to_string(),
            title: "Access Denied".to_string(),
            vendor: "akamai",
        }
    }

    #[test]
    fn unattended_site_check_is_needs_human_with_a_bare_url() {
        let ToolError::OperationFailed(msg) =
            site_check_error(&checked("https://www.ah.nl/x?q=1#f"), true);
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["error"], "needs_human");
        assert_eq!(v["kind"], "site_check");
        assert_eq!(v["origin"], "https://www.ah.nl");
        assert_eq!(v["url"], "https://www.ah.nl/x");
        assert_eq!(v["title"], "Access Denied");
        assert_eq!(v["vendor"], "akamai");
        assert!(msg.contains("do not retry"));
    }

    #[test]
    fn attended_site_check_asks_the_user() {
        let ToolError::OperationFailed(msg) =
            site_check_error(&checked("https://www.ah.nl/"), false);
        assert!(msg.contains("ah.nl shows a check that a person must pass"));
        assert!(msg.contains("do not retry"));
    }

    #[tokio::test]
    async fn needs_human_is_not_retried_by_the_browser() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let navigations = AtomicUsize::new(0);
        let r = navigate_checked(
            true,
            || async {
                navigations.fetch_add(1, Ordering::SeqCst);
                Ok("https://www.ah.nl/".to_string())
            },
            || async { Some(checked("https://www.ah.nl/")) },
        )
        .await;
        let ToolError::OperationFailed(msg) = r.unwrap_err();
        assert!(msg.contains("needs_human"));
        assert_eq!(navigations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_normal_page_navigates_through() {
        let r = navigate_checked(
            true,
            || async { Ok("http://localhost/".to_string()) },
            || async { None },
        )
        .await;
        assert_eq!(r.unwrap(), "http://localhost/");
    }

    #[test]
    fn an_unattended_host_with_a_channel_gets_needs_approval() {
        assert_eq!(risky_gate(true, true), sites::ActionGate::NeedsApproval);
    }

    /// The desktop (channel, not unattended) still asks.
    #[test]
    fn an_attended_session_with_a_channel_still_asks() {
        assert_eq!(risky_gate(false, true), sites::ActionGate::Ask);
        assert_eq!(risky_gate(false, false), sites::ActionGate::NeedsApproval);
    }

    fn manager_with_approval() -> BrowserManager {
        use crate::services::browser::LaunchOptions;
        use crate::settings::models::execution_settings::ApprovedBrowserAction;
        BrowserManager::lane_a(Some(std::path::PathBuf::from("/ws"))).with_launch_options(
            LaunchOptions {
                unattended: true,
                approved_action: Some(ApprovedBrowserAction {
                    origin: "https://shop.example".into(),
                    url: "https://shop.example/orders/1037".into(),
                    action: "button \"Refund $84.50\" on Order 1037".into(),
                }),
                ..Default::default()
            },
        )
    }

    /// What `needs_approval_error` reports is what the approval matches on.
    fn spend(m: &BrowserManager, url: &str, control: &str) -> bool {
        m.take_approved_action(
            &sites::origin_of(url).unwrap_or_default(),
            &bare_url(url),
            control,
        )
    }

    /// AGE-2043: the approved click goes through once; the same click again
    /// is `needs_approval`.
    #[test]
    fn an_approved_action_is_spent_once() {
        let m = manager_with_approval();
        let url = "https://shop.example/orders/1037?tab=1#x";
        assert!(spend(&m, url, "button \"Refund $84.50\""));
        assert!(!spend(&m, url, "button \"Refund $84.50\""));
    }

    /// A different button, page or origin is not the approved action, and
    /// does not spend the approval.
    #[test]
    fn a_different_action_is_not_approved() {
        let m = manager_with_approval();
        let url = "https://shop.example/orders/1037";
        assert!(!spend(&m, url, "button \"Delete order\""));
        assert!(!spend(
            &m,
            "https://shop.example/orders/1038",
            "button \"Refund $84.50\""
        ));
        assert!(!spend(
            &m,
            "http://shop.example/orders/1037",
            "button \"Refund $84.50\""
        ));
        assert!(!spend(&m, url, "button \"Refund $84.5\""));
        assert!(spend(&m, url, "button \"Refund $84.50\""));
    }

    /// A matching action that then fails (a covered button) keeps the
    /// approval for the retry; it is spent only when the action goes through.
    #[test]
    fn a_failed_approved_action_keeps_the_approval() {
        let m = manager_with_approval();
        let url = "https://shop.example/orders/1037";
        let origin = sites::origin_of(url).unwrap_or_default();
        let control = "button \"Refund $84.50\"";
        assert!(m.approved_action_matches(&origin, url, control));
        assert!(m.approved_action_matches(&origin, url, control));
        m.spend_approved_action();
        assert!(!m.approved_action_matches(&origin, url, control));
    }

    #[test]
    fn nothing_is_approved_by_default() {
        let m = BrowserManager::lane_a(Some(std::path::PathBuf::from("/ws")));
        assert!(!spend(&m, "https://shop.example/", "button \"Refund\""));
    }

    #[test]
    fn select_schema_takes_a_ref_and_an_option() {
        let (.., select) = build_browser_tools(manager(), artifacts(), None);
        let definition = tool_definition(&select);
        assert_eq!(
            definition.parameters["required"],
            serde_json::json!(["ref", "option"])
        );
    }

    #[test]
    fn summary_note_reports_nothing_captured() {
        assert!(summary_note(0, 0, 0, 20).contains("Nothing captured"));
    }

    #[test]
    fn summary_note_flags_truncation_and_drops() {
        let note = summary_note(100, 50, 7, 20);
        assert!(note.contains("100 entries, 50 of them problems"));
        assert!(note.contains("grep dump_path"));
        assert!(note.contains("7 older entries were dropped"));
    }

    #[test]
    fn summary_note_stays_quiet_when_there_is_nothing_to_flag() {
        let note = summary_note(5, 0, 0, 20);
        assert!(note.contains("none of them problems"));
        assert!(!note.contains("grep"));
        assert!(!note.contains("dropped"));
    }

    #[tokio::test]
    async fn empty_dump_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            write_dump(dir.path(), "console", &[])
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn dump_is_written_when_there_is_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_dump(dir.path(), "console", &["[error] boom".to_string()])
            .await
            .unwrap()
            .expect("a dump path");
        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(written, "[error] boom");
    }
}
