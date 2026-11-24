//! Proof pictures the host takes (AGE-2008).
//!
//! Not a model tool: the browser itself writes a PNG into `outputs/` when an
//! action stops at "Needs your OK" (`approval-<n>.png`, the target control
//! outlined), after a click that went through on an `ask_risky` or `any`
//! site (`after-<n>.png`, the 10 newest kept), and when the session closes
//! (`final.png`). Off unless the host sets `browser_proof_screenshots`.
//!
//! Two refusals, whatever the setting: a page on a site that is not granted
//! is never photographed, and neither is a page with a filled password field.
//! A picture that cannot be taken is logged and skipped; it never fails the
//! action.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams,
};
use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
use chromiumoxide::page::Page;
use tracing::{debug, warn};

use super::session::{BrowserSession, DEFAULT_TIMEOUT_SECS, with_deadline};
use super::snapshot::SnapshotNode;
use super::{BrowserError, BrowserManager, sites};

/// How many `after-*` files a session keeps; the oldest go first.
pub const MAX_AFTER_FILES: usize = 10;

/// How long a page gets to finish loading before the "after" picture.
const SETTLE_MAX_MS: u64 = 3_000;

/// Counters and the `after-*` files still on disk.
#[derive(Default)]
pub(super) struct ProofState {
    approvals: u32,
    afters: u32,
    kept: VecDeque<PathBuf>,
}

impl ProofState {
    fn next_approval(&mut self) -> u32 {
        self.approvals += 1;
        self.approvals
    }

    fn next_after(&mut self) -> u32 {
        self.afters += 1;
        self.afters
    }

    /// Record a new `after-*` file; returns the ones that now exceed the cap.
    fn keep_after(&mut self, path: PathBuf) -> Vec<PathBuf> {
        self.kept.push_back(path);
        let mut dropped = Vec::new();
        while self.kept.len() > MAX_AFTER_FILES {
            if let Some(old) = self.kept.pop_front() {
                dropped.push(old);
            }
        }
        dropped
    }
}

const OUTLINE_ON: &str = "function() { \
    this.__chattyProof = [this.style.outline, this.style.outlineOffset]; \
    this.style.setProperty('outline', '3px solid #e53935', 'important'); \
    this.style.setProperty('outline-offset', '2px', 'important'); \
    if (this.scrollIntoViewIfNeeded) { this.scrollIntoViewIfNeeded(); } }";

const OUTLINE_OFF: &str = "function() { const p = this.__chattyProof; \
    if (p) { this.style.outline = p[0]; this.style.outlineOffset = p[1]; \
    delete this.__chattyProof; } }";

const PASSWORD_FILLED: &str = "Array.from(document.querySelectorAll('input[type=password]'))\
    .some(i => i.value !== '')";

impl BrowserManager {
    /// Whether the host asked for proof pictures.
    pub fn proof_enabled(&self) -> bool {
        self.launch.proof_screenshots
    }

    /// The picture for an action that stops at "Needs your OK". Returns the
    /// file name (inside `outputs/`) to put in the `needs_approval` JSON.
    pub async fn proof_approval(
        &self,
        session: &BrowserSession,
        node: &SnapshotNode,
    ) -> Option<String> {
        let bytes = self.picture(session, Some(node)).await?;
        let n = self.proof.lock().next_approval();
        let name = format!("approval-{n}.png");
        self.write_proof(&name, &bytes).await?;
        Some(name)
    }

    /// The picture after a click that went through on an `ask_risky` or `any`
    /// site. Look-only sites and reads never get one.
    pub async fn proof_after(&self, session: &BrowserSession) {
        if !self.proof_enabled() {
            return;
        }
        settle(session).await;
        let Some(bytes) = self.picture(session, None).await else {
            return;
        };
        let n = self.proof.lock().next_after();
        let name = format!("after-{n}.png");
        let Some(path) = self.write_proof(&name, &bytes).await else {
            return;
        };
        let dropped = self.proof.lock().keep_after(path);
        for old in dropped {
            if let Err(e) = tokio::fs::remove_file(&old).await {
                warn!(path = %old.display(), error = %e, "browser: cannot remove an old proof picture");
            }
        }
    }

    /// The last page, when the session closes.
    pub(super) async fn proof_final(&self, session: &BrowserSession) {
        if let Some(bytes) = self.picture(session, None).await {
            self.write_proof("final.png", &bytes).await;
        }
    }

    /// A viewport PNG of the current page, or `None` when proof pictures are
    /// off, the page may not be photographed, or the capture failed.
    async fn picture(
        &self,
        session: &BrowserSession,
        outline: Option<&SnapshotNode>,
    ) -> Option<Vec<u8>> {
        if !self.proof_enabled() || self.workspace.is_none() {
            return None;
        }
        let page = session.page().ok()?;
        let url = page.url().await.ok().flatten()?;
        if sites::mode_for(&self.launch.act_on, &url).is_none() {
            debug!("browser: no proof picture, the site is not granted");
            return None;
        }
        // Fail closed: a page we cannot inspect is not photographed.
        let filled = match page.evaluate(PASSWORD_FILLED).await {
            Ok(v) => v.into_value::<bool>().unwrap_or(true),
            Err(_) => true,
        };
        if filled {
            debug!("browser: no proof picture, a password field is filled");
            return None;
        }
        match capture(&page, outline).await {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                warn!(error = %e, "browser: proof picture failed");
                None
            }
        }
    }

    async fn write_proof(&self, name: &str, bytes: &[u8]) -> Option<PathBuf> {
        let dir = self.workspace.as_ref()?.join("outputs");
        let path = dir.join(name);
        let result = async {
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(&path, bytes).await
        }
        .await;
        match result {
            Ok(()) => Some(path),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "browser: cannot write a proof picture");
                None
            }
        }
    }
}

/// Wait for the page to finish loading (bounded), then let it paint.
async fn settle(session: &BrowserSession) {
    let Ok(page) = session.page() else { return };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(SETTLE_MAX_MS);
    while tokio::time::Instant::now() < deadline {
        let ready = match page.evaluate("document.readyState === 'complete'").await {
            Ok(v) => v.into_value::<bool>().unwrap_or(false),
            Err(_) => false,
        };
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
}

async fn capture(page: &Page, outline: Option<&SnapshotNode>) -> Result<Vec<u8>, BrowserError> {
    let marked = match outline {
        Some(node) => set_outline(page, node, OUTLINE_ON).await.is_ok(),
        None => false,
    };
    let shot = with_deadline(DEFAULT_TIMEOUT_SECS, "capturing a proof picture", async {
        page.screenshot(
            CaptureScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .build(),
        )
        .await
        .map_err(|e| BrowserError::Protocol(format!("screenshot failed: {e}")))
    })
    .await;
    if let (true, Some(node)) = (marked, outline) {
        let _ = set_outline(page, node, OUTLINE_OFF).await;
    }
    shot
}

async fn set_outline(page: &Page, node: &SnapshotNode, function: &str) -> Result<(), BrowserError> {
    let id = node
        .backend_node_id
        .ok_or_else(|| BrowserError::Protocol("ref has no DOM node".to_string()))?;
    let resolved = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(BackendNodeId::new(id))
                .build(),
        )
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot reach the element: {e}")))?;
    let object_id = resolved
        .result
        .object
        .object_id
        .ok_or_else(|| BrowserError::Protocol("not a DOM node".to_string()))?;
    let params = CallFunctionOnParams::builder()
        .function_declaration(function)
        .object_id(object_id)
        .build()
        .map_err(|e| BrowserError::Protocol(format!("invalid script call: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot outline the element: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eleventh_after_picture_drops_the_oldest() {
        let mut state = ProofState::default();
        let mut dropped = Vec::new();
        for _ in 0..11 {
            let n = state.next_after();
            dropped.extend(state.keep_after(PathBuf::from(format!("after-{n}.png"))));
        }
        assert_eq!(dropped, vec![PathBuf::from("after-1.png")]);
        assert_eq!(state.kept.len(), MAX_AFTER_FILES);
        assert_eq!(state.kept.front(), Some(&PathBuf::from("after-2.png")));
    }

    #[test]
    fn approval_numbers_count_up_from_one() {
        let mut state = ProofState::default();
        assert_eq!(state.next_approval(), 1);
        assert_eq!(state.next_approval(), 2);
    }
}
