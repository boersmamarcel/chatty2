//! CDP session lifecycle: launch, drive, recover, tear down.
//!
//! One session owns one browser process and one page. Three consumers eventually
//! share it — the agent's tools (now), the artifact viewport (AGE-155), and
//! forwarded user input (AGE-156) — so the session holds the state all three
//! need rather than any one of them owning the browser.
//!
//! The rule that shapes everything here: **a browser that dies must surface as a
//! typed error, never as a hung tool call.** Every CDP round trip has a deadline,
//! and the event-handler task ending is the crash signal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::NavigateParams;
use chromiumoxide::page::Page;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::control::{ControlHolder, ControlLock};
use super::error::BrowserError;
use super::events::EventBuffers;
use super::input::{self, KeyInput, MouseInput};
use super::profile::{BrowserProfile, NavigationPolicy};
use super::screencast::{self, ScreencastState, ScreencastUpdate};

/// Default deadline for a CDP round trip.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Navigation gets longer — a cold dev server can be slow to first byte.
pub const NAVIGATE_TIMEOUT_SECS: u64 = 60;

/// Run a CDP future under a deadline, mapping the timeout to a typed error.
pub async fn with_deadline<T>(
    secs: u64,
    what: &str,
    fut: impl std::future::Future<Output = Result<T, BrowserError>>,
) -> Result<T, BrowserError> {
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(result) => result,
        Err(_) => Err(BrowserError::Timeout(secs, what.to_string())),
    }
}

/// A live browser session: one process, one page.
pub struct BrowserSession {
    browser: Mutex<Option<Browser>>,
    page: Page,
    policy: NavigationPolicy,
    profile: BrowserProfile,
    /// Console and network entries observed since the last drain.
    events: Arc<EventBuffers>,
    /// Bumped on every navigation; invalidates element refs from older snapshots.
    snapshot_generation: AtomicU64,
    /// Set when the handler task ends — the browser is gone.
    dead: Arc<AtomicBool>,
    /// Drives the CDP event stream. Nothing works if this is not polled.
    handler: Mutex<Option<JoinHandle<()>>>,
    /// Pumps CDP events into `events`.
    listeners: Mutex<Vec<JoinHandle<()>>>,
    /// Live `Page.startScreencast` state, when the artifact viewport
    /// (AGE-155) is watching this session. `tokio::sync::Mutex` because
    /// starting/retargeting holds the guard across CDP round trips.
    screencast: tokio::sync::Mutex<Option<ScreencastState>>,
    /// Who is driving (AGE-156). Agent by default; every fresh session
    /// starts here regardless of what a previous, now-dead session had.
    control: ControlLock,
    /// Temp user-data dir for an ephemeral profile; removed on drop.
    _user_data: Option<tempfile::TempDir>,
}

impl BrowserSession {
    /// Launch a browser and attach to a fresh page.
    pub async fn launch(
        chrome: std::path::PathBuf,
        profile: BrowserProfile,
        policy: NavigationPolicy,
    ) -> Result<Arc<Self>, BrowserError> {
        let mut builder = BrowserConfig::builder().chrome_executable(&chrome);

        // Lane A is headless. The viewport work (AGE-155) reads frames over CDP
        // rather than showing an OS window, so this stays headless there too.
        builder = builder.new_headless_mode();

        let user_data = match &profile {
            BrowserProfile::Ephemeral => {
                let dir = tempfile::Builder::new()
                    .prefix("chatty-browser-")
                    .tempdir()
                    .map_err(|e| BrowserError::Launch(format!("cannot create profile dir: {e}")))?;
                builder = builder.user_data_dir(dir.path());
                Some(dir)
            }
            BrowserProfile::Persistent { .. } => {
                // AGE-157. Nothing constructs this variant yet.
                return Err(BrowserError::Launch(
                    "persistent browser profiles are not implemented yet".into(),
                ));
            }
        };

        let config = builder
            .build()
            .map_err(|e| BrowserError::Launch(format!("invalid browser config: {e}")))?;

        let (browser, mut handler_stream) = tokio::time::timeout(
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            Browser::launch(config),
        )
        .await
        .map_err(|_| {
            BrowserError::Timeout(DEFAULT_TIMEOUT_SECS, "launching the browser".to_string())
        })?
        .map_err(|e| BrowserError::Launch(e.to_string()))?;

        // The handler stream must be driven or every command hangs forever.
        // Its ending is how we learn the browser died.
        let dead = Arc::new(AtomicBool::new(false));
        let handler = tokio::spawn({
            let dead = dead.clone();
            async move {
                while handler_stream.next().await.is_some() {}
                debug!("browser: CDP handler stream ended");
                dead.store(true, Ordering::SeqCst);
            }
        });

        let page = tokio::time::timeout(
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            browser.new_page("about:blank"),
        )
        .await
        .map_err(|_| BrowserError::Timeout(DEFAULT_TIMEOUT_SECS, "opening a page".to_string()))?
        .map_err(|e| BrowserError::Launch(format!("cannot open page: {e}")))?;

        let events = Arc::new(EventBuffers::default());
        let listeners = super::events::spawn_listeners(&page, events.clone()).await?;

        info!(
            profile = profile.label(),
            chrome = %chrome.display(),
            "browser: session ready"
        );

        Ok(Arc::new(Self {
            browser: Mutex::new(Some(browser)),
            page,
            policy,
            profile,
            events,
            snapshot_generation: AtomicU64::new(1),
            dead,
            handler: Mutex::new(Some(handler)),
            listeners: Mutex::new(listeners),
            screencast: tokio::sync::Mutex::new(None),
            control: ControlLock::new(),
            _user_data: user_data,
        }))
    }

    /// True once the browser process is gone.
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Fail fast rather than issuing a command that will never be answered.
    fn ensure_alive(&self) -> Result<(), BrowserError> {
        if self.is_dead() {
            Err(BrowserError::Crashed(format!(
                "the {} browser session ended unexpectedly",
                self.profile.label()
            )))
        } else {
            Ok(())
        }
    }

    /// The page this session drives.
    pub fn page(&self) -> Result<&Page, BrowserError> {
        self.ensure_alive()?;
        Ok(&self.page)
    }

    /// The navigation policy this session's profile carries.
    pub fn policy(&self) -> &NavigationPolicy {
        &self.policy
    }

    /// Buffered console and network entries.
    pub fn events(&self) -> &Arc<EventBuffers> {
        &self.events
    }

    /// The generation every element ref from the latest snapshot belongs to.
    pub fn snapshot_generation(&self) -> u64 {
        self.snapshot_generation.load(Ordering::SeqCst)
    }

    /// Invalidate every outstanding element ref. Called on navigation, and by
    /// the control-lock handback in AGE-156 — in both cases the page moved
    /// underneath whatever the agent last looked at.
    pub fn invalidate_snapshot(&self) -> u64 {
        self.snapshot_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Navigate, enforcing the profile's policy on the target and on every
    /// redirect hop the browser followed to get there.
    pub async fn navigate(&self, url: &str) -> Result<String, BrowserError> {
        self.ensure_alive()?;
        self.control.ensure_agent()?;
        self.policy.check(url)?;

        // Drop the previous page's entries *before* navigating, not after:
        // console output and requests from the page we are loading start
        // arriving while `wait_for_navigation` is still pending, and clearing
        // afterwards would throw them away.
        self.events.clear();

        with_deadline(NAVIGATE_TIMEOUT_SECS, "navigating", async {
            self.page
                .execute(NavigateParams::new(url.to_string()))
                .await
                .map_err(|e| BrowserError::Protocol(format!("navigate failed: {e}")))?;
            self.page
                .wait_for_navigation()
                .await
                .map_err(|e| BrowserError::Protocol(format!("navigation did not settle: {e}")))?;
            Ok(())
        })
        .await?;

        // A redirect can land us somewhere the policy would have refused, so
        // check where we actually ended up, not just where we aimed.
        let final_url = self
            .page
            .url()
            .await
            .map_err(|e| BrowserError::Protocol(format!("cannot read page URL: {e}")))?
            .unwrap_or_else(|| url.to_string());

        if final_url != url {
            self.policy.check(&final_url).map_err(|e| {
                BrowserError::NavigationRefused(format!(
                    "{url} redirected to {final_url}, which is not allowed: {e}"
                ))
            })?;
        }

        self.invalidate_snapshot();
        Ok(final_url)
    }

    /// Start (or resize) the live screencast the artifact viewport watches
    /// (AGE-155), returning a channel of frame updates.
    ///
    /// Backpressure is handled inside `screencast`: a slow receiver only
    /// ever sees the latest frame, never a backlog. Calling this again while
    /// a screencast is already running retargets it to the new size rather
    /// than starting a second one.
    pub async fn start_screencast(
        &self,
        width: u32,
        height: u32,
    ) -> Result<watch::Receiver<ScreencastUpdate>, BrowserError> {
        self.ensure_alive()?;
        let mut guard = self.screencast.lock().await;
        let existing = guard.take();
        let (state, rx) = screencast::start(&self.page, existing, width, height).await?;
        *guard = Some(state);
        Ok(rx)
    }

    /// Stop the screencast. Idle handling (AGE-155) calls this when the
    /// artifact window closes or the browser artifact stops being the
    /// active one — a screencast nobody is watching is pure CPU.
    pub async fn stop_screencast(&self) {
        let state = self.screencast.lock().await.take();
        if let Some(state) = state {
            screencast::stop(&self.page, state).await;
        }
    }

    /// Who is driving right now (AGE-156).
    pub fn control_holder(&self) -> ControlHolder {
        self.control.holder()
    }

    /// Err if the user currently holds control. `navigate` already checks
    /// this; other mutating tools (`browser_resize`) call it directly.
    pub fn ensure_agent_control(&self) -> Result<(), BrowserError> {
        self.control.ensure_agent()
    }

    /// The user takes control — never requested, always granted
    /// immediately. Returns the holder *before* the transition.
    pub fn take_control(&self) -> ControlHolder {
        self.control.take()
    }

    /// Hand control back to the agent. The page moved underneath whatever
    /// it last looked at, so every outstanding element ref is invalidated
    /// the same way a navigation invalidates them.
    pub fn release_control(&self) -> ControlHolder {
        let previous = self.control.release();
        if previous == ControlHolder::User {
            self.invalidate_snapshot();
        }
        previous
    }

    /// Forward a mouse event. A no-op — not an error — when the agent
    /// holds control: a stray event racing a handback must not steer a
    /// session nobody handed to the user.
    pub async fn dispatch_mouse(&self, event: MouseInput) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        if self.control_holder() != ControlHolder::User {
            return Ok(());
        }
        input::dispatch_mouse(&self.page, event).await
    }

    /// Forward a keyboard event. Same no-op-when-agent-owns-it rule as
    /// [`Self::dispatch_mouse`].
    pub async fn dispatch_key(&self, event: KeyInput) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        if self.control_holder() != ControlHolder::User {
            return Ok(());
        }
        input::dispatch_key(&self.page, event).await
    }

    /// Close the browser and stop every task this session owns.
    pub async fn shutdown(&self) {
        self.stop_screencast().await;
        for handle in self.listeners.lock().drain(..) {
            handle.abort();
        }
        let browser = self.browser.lock().take();
        if let Some(mut browser) = browser {
            if let Err(e) = browser.close().await {
                warn!(error = ?e, "browser: close failed");
            }
            if let Err(e) = browser.wait().await {
                warn!(error = ?e, "browser: wait failed");
            }
        }
        if let Some(handle) = self.handler.lock().take() {
            handle.abort();
        }
        self.dead.store(true, Ordering::SeqCst);
        debug!(profile = self.profile.label(), "browser: session shut down");
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        // `shutdown` is the graceful path. This is the backstop for a dropped
        // session: abort the tasks so we do not leak them, and let the child
        // process die with its pipes.
        for handle in self.listeners.lock().drain(..) {
            handle.abort();
        }
        if let Some(handle) = self.handler.lock().take() {
            handle.abort();
        }
        // No CDP round trip here — `try_lock` is sync, and stopping the
        // task is enough; the browser process going away ends the cast.
        if let Ok(mut guard) = self.screencast.try_lock()
            && let Some(state) = guard.take()
        {
            state.abort();
        }
    }
}
