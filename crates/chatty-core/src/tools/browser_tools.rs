//! Lane A browser tools: the self-review loop.
//!
//! The agent renders what it built, looks at it, critiques it, edits, reloads.
//! Localhost and workspace-local `file://` only at this stage — no credentials
//! are in play, so none of these tools needs an approval gate.
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
use rig_agent::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::tools::add_attachment_tool::PendingArtifacts;

use crate::services::browser::session::{DEFAULT_TIMEOUT_SECS, with_deadline};
use crate::services::browser::snapshot::{AxNode, flatten_ax_tree};
use crate::services::browser::{BrowserError, BrowserManager};
use crate::tools::ToolError;

/// Bundle of the six Lane A tools, so the agent factory moves one value.
pub type BrowserTools = (
    BrowserNavigateTool,
    BrowserSnapshotTool,
    BrowserScreenshotTool,
    BrowserConsoleTool,
    BrowserNetworkTool,
    BrowserResizeTool,
);

/// Build every Lane A tool over one shared manager.
pub fn build_browser_tools(
    manager: Arc<BrowserManager>,
    pending_artifacts: PendingArtifacts,
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
        BrowserResizeTool { manager },
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
        "Open a URL in the built-in browser. Only localhost URLs (http://localhost:PORT, \
         http://127.0.0.1:PORT) and file:// URLs inside the workspace are allowed — this \
         browser is for reviewing pages you built, not for browsing the web. Use search_web \
         or fetch for anything on the internet. Navigating invalidates every element ref \
         from a previous browser_snapshot."
            .to_string()
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

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let session = self.manager.session().await?;
        let url = session.navigate(&args.url).await?;
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
         element refs. This is the structural view — use it to find what is on the page. \
         For questions about how the page *looks* (spacing, alignment, colour), use \
         browser_screenshot instead; those are pixel judgements the tree cannot answer."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        empty_schema()
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
        let tree = snapshot.to_text();
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
            note: "The screenshot is attached to your next turn — end this turn to look at it. \
                   Do not try to read the PNG with read_file or read_binary; that returns \
                   base64 text you cannot see."
                .to_string(),
        })
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

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let dir = self.manager.output_dir().await?;
        let session = self.manager.session().await?;
        let (entries, dropped) = session.events().drain_console();

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

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let dir = self.manager.output_dir().await?;
        let session = self.manager.session().await?;
        let (entries, dropped) = session.events().drain_network();

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
        let (nav, snap, shot, console, net, resize) = build_browser_tools(manager(), artifacts());
        assert_eq!(tool_definition(&nav).name, "browser_navigate");
        assert_eq!(tool_definition(&snap).name, "browser_snapshot");
        assert_eq!(tool_definition(&shot).name, "browser_screenshot");
        assert_eq!(tool_definition(&console).name, "browser_console");
        assert_eq!(tool_definition(&net).name, "browser_network");
        assert_eq!(tool_definition(&resize).name, "browser_resize");
    }

    #[test]
    fn navigate_description_states_the_lane_a_restriction() {
        let (nav, ..) = build_browser_tools(manager(), artifacts());
        let description = tool_definition(&nav).description;
        assert!(description.contains("localhost"));
        assert!(description.contains("file://"));
    }

    #[tokio::test]
    async fn resize_rejects_out_of_range_viewports() {
        let (.., resize) = build_browser_tools(manager(), artifacts());
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
        let (_, _, shot, ..) = build_browser_tools(manager, artifacts());
        let err = shot
            .call(&mut ToolContext::new(), NoArgs {})
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("workspace"),
            "unexpected error: {err}"
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
