//! Built-in browser control (AGE-142).
//!
//! One CDP session with three eventual consumers: the agent's tools, the artifact
//! viewport (AGE-155), and forwarded user input (AGE-156). The layer is headless.
//! Tools default to Lane A (localhost and workspace-local `file://` URLs, no
//! credentials in play); when the app's internet-access setting is on, the
//! agent factory instead builds an open-web manager (`BrowserManager::open_web`)
//! that also allows any public http(s) host, gated by the same SSRF denylist
//! `fetch_tool` uses.
//!
//! Layout:
//! - [`error`] — every failure the agent can observe
//! - [`profile`] — profiles and the navigation policy each carries
//! - [`provisioning`] — resolving a Chrome binary: cache, then system, then download
//! - [`session`] — CDP session lifecycle
//! - [`events`] — bounded console and network capture
//! - [`snapshot`] — accessibility tree with generation-guarded element refs
//! - [`screencast`] — live frames for the artifact viewport (AGE-155)
//! - [`registry`] — per-conversation lookup so the UI can find a running
//!   session's manager (AGE-155)
//! - [`control`] — the control lock: who is driving (AGE-156)
//! - [`input`] — forwarded mouse/keyboard input over CDP (AGE-156)

pub mod control;
pub mod error;
pub mod events;
pub mod input;
pub mod profile;
pub mod provisioning;
pub mod registry;
pub mod screencast;
pub mod session;
pub mod snapshot;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{info, warn};

pub use control::ControlHolder;
pub use error::BrowserError;
pub use events::{ConsoleEntry, EventBuffers, NetworkEntry};
pub use input::{InputModifiers, KeyInput, MouseAction, MouseButtonKind, MouseInput};
pub use profile::{BrowserProfile, NavigationPolicy};
pub use screencast::{ScreencastFrame, ScreencastUpdate};
pub use session::BrowserSession;
pub use snapshot::{Snapshot, SnapshotNode};

/// The handle the browser tools hold.
///
/// Constructed at agent-build time and cheap — it launches nothing. The browser
/// starts on the first tool call that needs it, so a user who never asks the
/// agent to look at a page never pays for a Chrome process or a download.
pub struct BrowserManager {
    profile: BrowserProfile,
    policy: NavigationPolicy,
    workspace: Option<PathBuf>,
    session: Mutex<Option<Arc<BrowserSession>>>,
    /// Latest snapshot, so refs survive between tool calls.
    snapshot: Mutex<Option<Snapshot>>,
}

impl BrowserManager {
    /// A Lane A manager: ephemeral profile, localhost and workspace files only.
    pub fn lane_a(workspace: Option<PathBuf>) -> Self {
        Self {
            profile: BrowserProfile::Ephemeral,
            policy: NavigationPolicy::local_only(workspace.clone()),
            workspace,
            session: Mutex::new(None),
            snapshot: Mutex::new(None),
        }
    }

    /// An ephemeral manager with the open-web policy: everything Lane A allows,
    /// plus any public http(s) host. Gated by the caller on the app's
    /// internet-access setting — this constructor itself does not check it.
    pub fn open_web(workspace: Option<PathBuf>) -> Self {
        Self {
            profile: BrowserProfile::Ephemeral,
            policy: NavigationPolicy::open(workspace.clone()),
            workspace,
            session: Mutex::new(None),
            snapshot: Mutex::new(None),
        }
    }

    /// Whether this manager's policy allows navigating off-machine, so tool
    /// descriptions can tell the model what is actually allowed right now.
    pub fn allows_open_web(&self) -> bool {
        matches!(self.policy, NavigationPolicy::Open { .. })
    }

    /// The configured workspace, which the tools need somewhere to write into.
    pub fn workspace(&self) -> Result<&std::path::Path, BrowserError> {
        self.workspace.as_deref().ok_or(BrowserError::NoWorkspace)
    }

    /// Directory screenshots and dumps are written to.
    pub async fn output_dir(&self) -> Result<PathBuf, BrowserError> {
        let dir = self.workspace()?.join(".chatty").join("browser");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| BrowserError::Io(format!("cannot create {}: {e}", dir.display())))?;
        Ok(dir)
    }

    /// The live session, launching or relaunching one if needed.
    ///
    /// A session that died is replaced exactly once per call. If the replacement
    /// also fails, the caller gets a typed error rather than a retry loop.
    pub async fn session(&self) -> Result<Arc<BrowserSession>, BrowserError> {
        let mut guard = self.session.lock().await;

        if let Some(existing) = guard.as_ref() {
            if !existing.is_dead() {
                return Ok(existing.clone());
            }
            warn!("browser: previous session died, relaunching");
            *guard = None;
            // Refs from the dead session's page mean nothing now.
            *self.snapshot.lock().await = None;
        }

        let chrome = provisioning::resolve_chrome(None).await?;
        let session =
            BrowserSession::launch(chrome, self.profile.clone(), self.policy.clone()).await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    /// Store the snapshot a `browser_snapshot` call produced.
    pub async fn set_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.lock().await = Some(snapshot);
    }

    /// The most recent snapshot, if one was taken.
    pub async fn snapshot(&self) -> Option<Snapshot> {
        self.snapshot.lock().await.clone()
    }

    /// Stop the screencast if a session is running (AGE-155 idle handling).
    /// Never launches a browser — closing an artifact window that never
    /// showed one must not start a session just to immediately stop it.
    pub async fn stop_screencast(&self) {
        let session = self.session.lock().await.clone();
        if let Some(session) = session {
            session.stop_screencast().await;
        }
    }

    /// Close the browser if one is running. Called on app shutdown.
    pub async fn shutdown(&self) {
        let session = self.session.lock().await.take();
        if let Some(session) = session {
            info!("browser: shutting down session");
            session.shutdown().await;
        }
    }
}
