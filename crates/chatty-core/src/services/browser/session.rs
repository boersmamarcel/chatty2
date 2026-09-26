//! CDP session lifecycle: launch, drive, recover, tear down.
//!
//! One session owns one browser process and every page target it knows about
//! (AGE-473). Three consumers share it — the agent's tools, the artifact
//! viewport (AGE-155), and forwarded user input (AGE-156) — so the session
//! holds the state all three need rather than any one of them owning the
//! browser.
//!
//! Exactly one tab is *active* at a time: screencast, forwarded input and the
//! agent's tools all drive it. A page that opens a new tab or window
//! (`target="_blank"`, `window.open`, an OAuth popup) hands the new target to
//! [`BrowserSession::track_target`], which adds it to the tab list and makes it
//! active; the human switches between open tabs with
//! [`BrowserSession::select_tab`] and closes them with
//! [`BrowserSession::close_tab`]. See [`super::targets`] for the watcher that
//! feeds this and the per-tab navigation guard.
//!
//! Two rules shape everything here.
//!
//! **A browser that dies must surface as a typed error, never as a hung tool
//! call.** Every CDP round trip has a deadline, and the event-handler task
//! ending is the crash signal.
//!
//! **Nothing is shown or read that the navigation policy would refuse.** The
//! policy is not a check `browser_navigate` performs once; it is a property of
//! every tab this session tracks, re-asserted on every navigation that tab
//! makes, active or not — see [`BrowserSession::ensure_allowed`] and
//! [`BrowserSession::page_navigated`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::NavigateParams;
use chromiumoxide::cdp::browser_protocol::target::{
    EventTargetCreated, EventTargetDestroyed, TargetId,
};
use chromiumoxide::cdp::js_protocol::runtime::RunIfWaitingForDebuggerParams;
use chromiumoxide::page::Page;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::click::{self, ClickPoint};
use super::control::{ControlHolder, ControlLock};
use super::error::BrowserError;
use super::events::EventBuffers;
use super::input::{self, KeyInput, MouseInput};
use super::profile::{BrowserProfile, NavigationPolicy};
use super::screencast::{self, Screencast, ScreencastUpdate};
use super::snapshot::SnapshotNode;
use super::targets;
use super::typing;

/// Default deadline for a CDP round trip.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Navigation gets longer — a cold dev server can be slow to first byte.
pub const NAVIGATE_TIMEOUT_SECS: u64 = 60;
/// How long a new tab gets to become drivable before we give up on it.
const NEW_TAB_TIMEOUT_SECS: u64 = 10;
/// How long a new tab gets to land on its real URL before the policy decides
/// whether to show it. `window.open` reports `about:blank` until then.
const NEW_TAB_SETTLE_SECS: u64 = 2;
/// How long a click gets to start a navigation before the result reports
/// whether it did. Long enough for a same-process handler to call
/// `location.assign`; not a page-load wait.
const CLICK_SETTLE_MS: u64 = 300;

/// The URL a newly opened tab settles on.
///
/// A tab is created before it navigates, and reports `about:blank` until then
/// — which is both the wrong thing to show in the address bar and the wrong
/// thing to hand the navigation policy. Wait briefly for the real one; a tab
/// that genuinely stays blank (an opener writing into it directly) costs the
/// wait once and is tracked anyway.
async fn settled_url(page: &Page) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(NEW_TAB_SETTLE_SECS);
    loop {
        let url = page.url().await.ok().flatten().unwrap_or_default();
        if !targets::is_exempt_url(&url) || tokio::time::Instant::now() >= deadline {
            return url;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

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

/// One open tab as the artifact panel's tab strip shows it (AGE-473).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserTab {
    /// The CDP target id — what [`BrowserSession::select_tab`] and
    /// [`BrowserSession::close_tab`] take.
    pub id: String,
    /// Best-known title: `document.title` as of the last document load.
    /// Empty until then, and never read off a blocked tab.
    pub title: String,
    /// The URL the tab last navigated to.
    pub url: String,
    /// Whether this is the tab every consumer drives right now.
    pub active: bool,
    /// Set while the tab sits somewhere the navigation policy refuses — the
    /// URL it is blocked at. A blocked tab is never shown, read or driven.
    pub blocked: Option<String>,
}

/// What a [`BrowserSession::click`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct ClickResult {
    /// Where the pointer went, in viewport CSS pixels.
    pub point: ClickPoint,
    /// The active tab's URL once the click settled.
    pub url: String,
    /// Whether the click moved the page — if so every element ref is dead.
    pub navigated: bool,
    /// The generation refs must belong to from now on.
    pub snapshot_generation: u64,
}

/// A tracked page target: the handle, what the strip shows for it, and the
/// per-tab state the policy needs.
struct Tab {
    page: Page,
    title: String,
    url: String,
    /// Per tab, not per session: one tab sitting somewhere refused must not
    /// make another unreadable.
    blocked: Option<String>,
    /// Console/network pumps into the session's buffers. Running only while
    /// this is the active tab: a tab nobody is driving must not go on
    /// writing into what the tools read.
    listeners: Vec<JoinHandle<()>>,
}

/// Every page target the session knows about, and which one is active.
struct Tabs {
    /// In the order they were opened; the page the session launched with is
    /// first until it closes.
    open: Vec<Tab>,
    /// The page every consumer drives. A clone of one of `open`'s pages —
    /// except between the last tab closing and its replacement opening,
    /// when it is the page that just died and every command on it fails.
    active: Page,
}

impl Tabs {
    fn find(&self, target: &TargetId) -> Option<&Tab> {
        self.open.iter().find(|tab| tab.page.target_id() == target)
    }

    fn find_mut(&mut self, target: &TargetId) -> Option<&mut Tab> {
        self.open
            .iter_mut()
            .find(|tab| tab.page.target_id() == target)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.find(self.active.target_id())
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let target = self.active.target_id().clone();
        self.find_mut(&target)
    }

    fn snapshot(&self) -> Vec<BrowserTab> {
        let active = self.active.target_id();
        self.open
            .iter()
            .map(|tab| BrowserTab {
                id: tab.page.target_id().inner().clone(),
                title: tab.title.clone(),
                url: tab.url.clone(),
                active: tab.page.target_id() == active,
                blocked: tab.blocked.clone(),
            })
            .collect()
    }
}

/// What the panel is told while a tab it is looking at is refused.
fn refusal_reason(url: &str) -> String {
    format!("the page navigated to {url}, which this browser profile does not allow")
}

/// A live browser session: one process, one active page among the open tabs.
pub struct BrowserSession {
    /// The Tokio runtime the session was launched on. Every task the session
    /// spawns and every timer it arms goes through this rather than the
    /// caller's ambient context: the artifact panel calls `select_tab`,
    /// `close_tab` and `start_screencast` from gpui threads that have no
    /// Tokio runtime entered, and a `tokio::spawn` there panics (AGE-473).
    runtime: tokio::runtime::Handle,
    /// `tokio::sync::Mutex` because the target watcher resolves new tabs
    /// through the browser handle, which is a CDP round trip.
    browser: tokio::sync::Mutex<Option<Browser>>,
    /// Every tab the session tracks and the one it drives (AGE-473).
    tabs: Mutex<Tabs>,
    /// The tab list, broadcast to the artifact panel's tab strip on every
    /// change — same shape as `current_url`, so the panel runs one task per
    /// channel rather than polling the session on every frame.
    tab_list: watch::Sender<Vec<BrowserTab>>,
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
    /// The target watcher and one navigation guard per tracked tab, alive
    /// for the tab's whole lifetime (AGE-473). Guards end on their own when
    /// their tab does; finished handles are pruned as new ones are added.
    listeners: Mutex<Vec<JoinHandle<()>>>,
    /// Live `Page.startScreencast` state, when the artifact viewport
    /// (AGE-155) is watching this session. `tokio::sync::Mutex` because
    /// starting/retargeting holds the guard across CDP round trips — and
    /// because which page is active must be read *under* it (AGE-458), or a
    /// tab activated between the read and the lock leaves the cast behind.
    screencast: tokio::sync::Mutex<Option<Screencast>>,
    /// Who is driving (AGE-156). Agent by default; every fresh session
    /// starts here regardless of what a previous, now-dead session had.
    control: ControlLock,
    /// Current page URL, broadcast so the artifact viewport's address bar
    /// (AGE-156) can mirror navigation from either side — the agent's
    /// `browser_navigate` tool or the user typing a new URL.
    current_url: watch::Sender<String>,
    /// Temp user-data dir for an ephemeral profile; removed on drop.
    _user_data: Option<tempfile::TempDir>,
}

/// Chrome's sandbox cannot run as root, and Chrome then exits before the
/// DevTools socket is up with only its stderr to explain why. Say so plainly
/// instead (AGE-564). The sandbox is deliberately *not* turned off for root:
/// with internet access on the browser renders open-web content, and that
/// is the case the sandbox exists for.
pub(crate) fn refuse_root() -> Result<(), BrowserError> {
    #[cfg(unix)]
    if nix::unistd::geteuid().is_root() {
        return Err(BrowserError::Launch(
            "Chrome cannot run as root: its sandbox is unavailable to the root user. \
             Run Chatty as a regular user to use the browser."
                .into(),
        ));
    }
    Ok(())
}

impl BrowserSession {
    /// Launch a browser and attach to a fresh page.
    pub async fn launch(
        chrome: std::path::PathBuf,
        profile: BrowserProfile,
        policy: NavigationPolicy,
    ) -> Result<Arc<Self>, BrowserError> {
        // Launching is always under Tokio (chromiumoxide needs it to drive
        // the process); what is captured here is what later calls from
        // Tokio-less threads borrow.
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            BrowserError::Launch("the browser must be launched from a Tokio runtime".into())
        })?;
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
        let handler = runtime.spawn({
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
        let listeners = super::events::spawn_listeners(&runtime, &page, events.clone()).await?;

        // Subscribe *after* opening our own page, so its creation is not
        // reported as a new tab (tracking is keyed by target id anyway).
        let created = browser
            .event_listener::<EventTargetCreated>()
            .await
            .map_err(|e| BrowserError::Launch(format!("cannot watch for new tabs: {e}")))?;
        let destroyed = browser
            .event_listener::<EventTargetDestroyed>()
            .await
            .map_err(|e| BrowserError::Launch(format!("cannot watch for closed tabs: {e}")))?;
        info!(
            profile = profile.label(),
            chrome = %chrome.display(),
            "browser: session ready"
        );

        let tabs = Tabs {
            open: vec![Tab {
                page: page.clone(),
                title: String::new(),
                url: "about:blank".to_string(),
                blocked: None,
                listeners,
            }],
            active: page.clone(),
        };
        let session = Arc::new(Self {
            runtime,
            browser: tokio::sync::Mutex::new(Some(browser)),
            tab_list: watch::channel(tabs.snapshot()).0,
            tabs: Mutex::new(tabs),
            policy,
            profile,
            events,
            snapshot_generation: AtomicU64::new(1),
            dead,
            handler: Mutex::new(Some(handler)),
            listeners: Mutex::new(Vec::new()),
            screencast: tokio::sync::Mutex::new(None),
            control: ControlLock::new(),
            current_url: watch::channel(String::from("about:blank")).0,
            _user_data: user_data,
        });

        let watcher = targets::spawn_watcher(&session, created, destroyed);
        session.listeners.lock().push(watcher);

        // The policy is checked again on every navigation this page makes on
        // its own, not only on the ones we asked for (AGE-458).
        match targets::spawn_navigation_guard(&session, &page).await {
            Ok(guard) => session.listeners.lock().push(guard),
            Err(e) => {
                session.shutdown().await;
                return Err(e);
            }
        }

        Ok(session)
    }

    /// The Tokio runtime every task of this session runs on.
    pub(super) fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
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

    /// Err while the active tab is one the navigation policy refuses
    /// (AGE-458). `browser_navigate` vets the URL it is given and every
    /// redirect hop, but a page can move on its own afterwards — a script, a
    /// meta refresh, an opener setting `popup.location` — and a page the
    /// policy would have refused must not become readable just because the
    /// page, rather than the agent, is what navigated.
    fn ensure_allowed(&self) -> Result<(), BrowserError> {
        let tabs = self.tabs.lock();
        match tabs.active_tab().and_then(|tab| tab.blocked.as_deref()) {
            Some(url) => Err(BrowserError::NavigationRefused(format!(
                "the page navigated itself to {url}, which this browser profile does not \
                 allow; it is not readable from here — navigate somewhere allowed to continue"
            ))),
            None => Ok(()),
        }
    }

    /// The page this session drives right now. Cloned rather than borrowed
    /// because it can change under the caller: a tab the page opened is
    /// activated, or the user picks another one, while a tool call is in
    /// flight (AGE-458, AGE-473).
    pub fn page(&self) -> Result<Page, BrowserError> {
        self.ensure_alive()?;
        self.ensure_allowed()?;
        Ok(self.active_page())
    }

    /// The active page, without the liveness check.
    fn active_page(&self) -> Page {
        self.tabs.lock().active.clone()
    }

    /// The navigation policy this session's profile carries.
    pub fn policy(&self) -> &NavigationPolicy {
        &self.policy
    }

    /// Every open tab, in strip order (AGE-473).
    pub fn tabs(&self) -> Vec<BrowserTab> {
        self.tabs.lock().snapshot()
    }

    /// Subscribe to the tab list (AGE-473's tab strip) — fires whenever a
    /// tab opens, closes, changes title, is blocked or unblocked, or becomes
    /// active. The initial value on a fresh receiver is the list as of
    /// subscription time.
    pub fn watch_tabs(&self) -> watch::Receiver<Vec<BrowserTab>> {
        self.tab_list.subscribe()
    }

    /// Broadcast the tab list after a change. `send_replace`, not `send`: a
    /// panel that subscribes later must see the current list, not the one
    /// from when a receiver last existed.
    fn publish_tabs(&self) {
        let snapshot = self.tabs.lock().snapshot();
        self.tab_list.send_replace(snapshot);
    }

    /// Buffered console and network entries.
    ///
    /// Refused while the page on screen is one the policy refuses (AGE-458):
    /// its console text and request URLs are content from that origin too, so
    /// `browser_console` must not be a way around `browser_snapshot` being
    /// refused. Whatever it produced is dropped when the block goes up.
    pub fn events(&self) -> Result<&Arc<EventBuffers>, BrowserError> {
        self.ensure_allowed()?;
        Ok(&self.events)
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
        self.do_navigate(url).await
    }

    /// The user navigates directly (AGE-156's address bar) — takes control
    /// first, exactly like reaching for the mouse does, then navigates the
    /// same as the agent would. Never refused for being user-held control;
    /// the whole point is the user driving.
    pub async fn navigate_as_user(&self, url: &str) -> Result<String, BrowserError> {
        self.ensure_alive()?;
        self.control.take();
        self.do_navigate(url).await
    }

    /// Shared navigation body once the caller has settled who's allowed to
    /// drive: policy check, the CDP round trip, the redirect-landing check,
    /// snapshot invalidation, and broadcasting the new URL to the address bar.
    async fn do_navigate(&self, url: &str) -> Result<String, BrowserError> {
        self.policy.check(url)?;
        let page = self.active_page();

        // Drop the previous page's entries *before* navigating, not after:
        // console output and requests from the page we are loading start
        // arriving while `wait_for_navigation` is still pending, and clearing
        // afterwards would throw them away.
        self.events.clear();

        with_deadline(NAVIGATE_TIMEOUT_SECS, "navigating", async {
            page.execute(NavigateParams::new(url.to_string()))
                .await
                .map_err(|e| BrowserError::Protocol(format!("navigate failed: {e}")))?;
            page.wait_for_navigation()
                .await
                .map_err(|e| BrowserError::Protocol(format!("navigation did not settle: {e}")))?;
            Ok(())
        })
        .await?;

        // A redirect can land us somewhere the policy would have refused, so
        // check where we actually ended up, not just where we aimed.
        let final_url = page
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
        // Navigating somewhere allowed is the way out of a block (AGE-458):
        // the URL was checked above, and the page is readable again.
        let was_blocked = {
            let mut tabs = self.tabs.lock();
            match tabs.find_mut(page.target_id()) {
                Some(tab) => {
                    tab.url = final_url.clone();
                    tab.blocked.take().is_some()
                }
                None => false,
            }
        };
        self.publish_tabs();
        if was_blocked {
            self.unblocked(&page).await;
        }
        let _ = self.current_url.send(final_url.clone());
        Ok(final_url)
    }

    /// Reload the current page (AGE-156's refresh button) — takes control
    /// first, the same as any other user-initiated action, then reloads.
    /// The URL does not change, so unlike `do_navigate` this skips the
    /// policy check (already satisfied when the page first loaded) and
    /// does not touch `current_url`.
    pub async fn reload_as_user(&self) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        // Reloading a page the policy refuses would just load it again.
        self.ensure_allowed()?;
        self.control.take();
        self.events.clear();
        let page = self.active_page();
        with_deadline(NAVIGATE_TIMEOUT_SECS, "reloading", async {
            page.reload()
                .await
                .map_err(|e| BrowserError::Protocol(format!("reload failed: {e}")))?;
            Ok(())
        })
        .await?;
        self.invalidate_snapshot();
        Ok(())
    }

    /// Subscribe to page-URL changes (AGE-156's address bar) — fires once
    /// per successful navigation, whichever side initiated it. The initial
    /// value on a fresh receiver is the URL as of subscription time.
    pub fn watch_url(&self) -> watch::Receiver<String> {
        self.current_url.subscribe()
    }

    /// Start (or resize) the live screencast the artifact viewport watches
    /// (AGE-155), returning a channel of frame updates.
    ///
    /// Backpressure is handled inside `screencast`: a slow receiver only
    /// ever sees the latest frame, never a backlog. Calling this again while
    /// a screencast is already running retargets it — to the new size, or to
    /// whatever tab is active now (AGE-458) — rather than starting a second
    /// one. While the active tab is blocked the cast is created suspended,
    /// carrying the reason (AGE-473): the channel has to exist for a switch
    /// to another tab to have something to revive.
    pub async fn start_screencast(
        &self,
        width: u32,
        height: u32,
    ) -> Result<watch::Receiver<ScreencastUpdate>, BrowserError> {
        self.ensure_alive()?;
        let mut guard = self.screencast.lock().await;
        // Read the active page *under* the lock: a tab activated between the
        // read and the lock would otherwise leave the cast on the old page
        // while input and tools drive the new one (AGE-458).
        let (page, blocked) = self.active_view();
        if let Some(url) = blocked {
            return screencast::hold(
                &self.runtime,
                &mut guard,
                width,
                height,
                &refusal_reason(&url),
            )
            .await;
        }
        // The state is handed over by mutable borrow, never taken out: a
        // failed retarget leaves the previous screencast — which Chrome is
        // still encoding — in place, so a later call retargets or stops it
        // instead of asking Chrome to start a second one (AGE-457).
        screencast::start(&self.runtime, &page, &mut guard, width, height).await
    }

    /// Stop the screencast. Idle handling (AGE-155) calls this when the
    /// artifact window closes or the browser artifact stops being the
    /// active one — a screencast nobody is watching is pure CPU.
    pub async fn stop_screencast(&self) {
        let screencast = self.screencast.lock().await.take();
        if let Some(screencast) = screencast {
            screencast.stop().await;
        }
    }

    /// The active page and, if it is blocked, the URL it is blocked at.
    fn active_view(&self) -> (Page, Option<String>) {
        let tabs = self.tabs.lock();
        let blocked = tabs.active_tab().and_then(|tab| tab.blocked.clone());
        (tabs.active.clone(), blocked)
    }

    /// Make the cast show whatever tab is active now, keeping the channel its
    /// consumer is already watching: moved onto the page (AGE-458), or
    /// suspended with the reason while that tab is blocked. A no-op when
    /// nothing is casting — switching tabs must not start a cast nobody
    /// asked for.
    ///
    /// Failing here leaves the cast suspended with its channel alive, so the
    /// panel says why instead of reporting a session that ended.
    async fn sync_screencast(&self) {
        let mut guard = self.screencast.lock().await;
        let (page, blocked) = self.active_view();
        let Some(screencast) = guard.as_mut() else {
            return;
        };
        if let Some(url) = blocked {
            // Neither the picture nor anything else of a refused page may
            // reach the panel: stop showing it and say why.
            screencast.hold(&refusal_reason(&url)).await;
            return;
        }
        // Whatever viewport the panel last asked for, now on another page.
        let (width, height) = screencast.size();
        if screencast.is_casting(&page, width, height) {
            return;
        }
        if let Err(e) = screencast::start(&self.runtime, &page, &mut guard, width, height).await {
            warn!(error = %e, "browser: cannot move the screencast to the active tab");
        }
    }

    /// A page target appeared (AGE-458, AGE-473): resolve it, check it
    /// against the navigation policy, add it to the tab list and — unless
    /// the policy refuses where it landed — make it the tab every consumer
    /// drives. A refused tab is tracked but never shown: it sits in the
    /// strip blocked, and its guard lifts the block if it comes back
    /// somewhere allowed.
    ///
    /// Every failure leaves the session exactly as it was — an untrackable
    /// tab is invisible, which is what it was before this existed.
    pub(super) async fn track_target(self: &Arc<Self>, target_id: TargetId) {
        if self.tabs.lock().find(&target_id).is_some() {
            return;
        }
        let Some(page) = self.resolve_page(&target_id).await else {
            warn!(target = %target_id.inner(), "browser: a new tab never became drivable");
            return;
        };

        // chromiumoxide auto-attaches to targets its pages open with
        // `waitForDebuggerOnStart`, and nothing ever resumes them, so a popup
        // sits frozen before its first script runs unless we say so.
        if let Err(e) = page.execute(RunIfWaitingForDebuggerParams::default()).await {
            debug!(error = ?e, "browser: runIfWaitingForDebugger on the new tab failed");
        }

        let url = settled_url(&page).await;
        self.track(&page, url).await;
    }

    /// Add `page` to the tab list. The check here is only the *first* one: a
    /// tab that passes it and then navigates itself somewhere refused is
    /// blocked by its navigation guard, which runs for the tab's whole
    /// lifetime, active or not (AGE-473).
    async fn track(self: &Arc<Self>, page: &Page, url: String) {
        let target_id = page.target_id();
        if self.tabs.lock().find(target_id).is_some() {
            return;
        }
        let blocked = (!targets::may_drive_url(&self.policy, &url)).then(|| url.clone());

        // The guard has to be watching before the tab is in the strip, or a
        // tab that navigates itself the instant it is tracked slips through.
        let guard = match targets::spawn_navigation_guard(self, page).await {
            Ok(guard) => guard,
            Err(e) => {
                warn!(error = %e, "browser: cannot watch the new tab's navigation; not tracking it");
                return;
            }
        };
        {
            let mut tabs = self.tabs.lock();
            // Two paths can learn about one target — the watcher's
            // `targetCreated` and `reopen_blank_page` — so the insert is what
            // dedupes, under the lock.
            if tabs.find(target_id).is_some() {
                guard.abort();
                return;
            }
            tabs.open.push(Tab {
                page: page.clone(),
                title: String::new(),
                url: if url.is_empty() {
                    "about:blank".to_string()
                } else {
                    url.clone()
                },
                blocked: blocked.clone(),
                listeners: Vec::new(),
            });
        }
        {
            let mut listeners = self.listeners.lock();
            listeners.retain(|handle| !handle.is_finished());
            listeners.push(guard);
        }

        match blocked {
            Some(url) => {
                warn!(
                    url = %url,
                    "browser: a new tab opened outside the navigation policy; blocked, not shown"
                );
                self.publish_tabs();
            }
            None => {
                info!(url = %url, "browser: following a new tab");
                self.activate(page).await;
            }
        }
        // A tab that loaded while it was being resolved has already fired
        // the load event its guard would have read the title on.
        self.page_loaded(page).await;
    }

    /// Make `page` — one of the tracked tabs — the tab every consumer
    /// drives: the screencast moves to it, forwarded input and the tools
    /// follow, element refs are invalidated as a navigation would, the
    /// address bar gets its URL, and its console/network output takes over
    /// the buffers the tools read.
    ///
    /// Nothing happens if `page` is no longer tracked: a tab looked up a
    /// moment ago can have closed since, and `active` must never point at a
    /// page that is not in the list.
    async fn activate(&self, page: &Page) {
        let (url, blocked) = {
            let mut tabs = self.tabs.lock();
            let Some(tab) = tabs.find(page.target_id()) else {
                return;
            };
            let (url, blocked) = (tab.url.clone(), tab.blocked.clone());
            // The tab we are leaving must not go on writing into the buffers.
            if let Some(previous) = tabs.active_tab_mut() {
                for handle in previous.listeners.drain(..) {
                    handle.abort();
                }
            }
            tabs.active = page.clone();
            (url, blocked)
        };
        // Every element ref the agent holds belongs to the page underneath,
        // and whatever the previous tab logged or requested is not this one's.
        self.invalidate_snapshot();
        self.events.clear();
        self.publish_tabs();
        let _ = self.current_url.send(url);
        if blocked.is_none() {
            self.attach_listeners(page).await;
        }
        self.sync_screencast().await;
    }

    /// The human picks a tab from the strip (AGE-473). Takes control first,
    /// like any other user-initiated change to what the session drives: the
    /// agent's tools now address this tab, and every element ref it held is
    /// stale.
    pub async fn select_tab(&self, id: &str) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        let target = TargetId::new(id);
        let page = {
            let tabs = self.tabs.lock();
            if tabs.active.target_id() == &target {
                return Ok(());
            }
            tabs.find(&target).map(|tab| tab.page.clone())
        }
        .ok_or_else(|| BrowserError::Protocol(format!("no open tab with id {id}")))?;
        self.control.take();
        info!(target = %id, "browser: switching to another tab");
        self.activate(&page).await;
        Ok(())
    }

    /// The human closes a tab from the strip (AGE-473): it leaves the list
    /// at once — falling back to a neighbour if it was the active one — and
    /// then the CDP target is closed, not just hidden.
    pub async fn close_tab(self: &Arc<Self>, id: &str) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        let target = TargetId::new(id);
        let page = self
            .tabs
            .lock()
            .find(&target)
            .map(|tab| tab.page.clone())
            .ok_or_else(|| BrowserError::Protocol(format!("no open tab with id {id}")))?;
        self.control.take();
        info!(target = %id, "browser: closing a tab");
        // Bookkeeping first, so nothing is ever pointed at a page being torn
        // down; the `targetDestroyed` that follows finds nothing to do.
        self.remove_tab(&target).await;
        if let Err(e) = page.close().await {
            debug!(error = ?e, "browser: closing the tab failed");
        }
        Ok(())
    }

    /// A target went away, on its own or because we closed it.
    pub(super) async fn target_closed(self: &Arc<Self>, target_id: &TargetId) {
        self.remove_tab(target_id).await;
    }

    /// A document finished loading in a tracked tab: read its title for the
    /// strip. Not for a blocked tab — its title is content from an origin
    /// the policy refuses, and nothing of that reaches anyone.
    pub(super) async fn page_loaded(&self, page: &Page) {
        let readable = self
            .tabs
            .lock()
            .find(page.target_id())
            .is_some_and(|tab| tab.blocked.is_none());
        if !readable {
            return;
        }
        let title = match page.get_title().await {
            Ok(title) => title.unwrap_or_default(),
            Err(e) => {
                debug!(error = ?e, "browser: cannot read the tab title");
                return;
            }
        };
        let changed = {
            let mut tabs = self.tabs.lock();
            match tabs.find_mut(page.target_id()) {
                Some(tab) if tab.blocked.is_none() && tab.title != title => {
                    tab.title = title;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.publish_tabs();
        }
    }

    /// Drop a tab from the list. When it was the active one, hand
    /// screencast, input and the tools to the neighbour on its left (or the
    /// new first tab) — never leave them on a page that no longer exists —
    /// and when it was the last one, open a blank page to stand in.
    async fn remove_tab(self: &Arc<Self>, target_id: &TargetId) {
        let fallback = {
            let mut tabs = self.tabs.lock();
            let Some(index) = tabs
                .open
                .iter()
                .position(|tab| tab.page.target_id() == target_id)
            else {
                return;
            };
            let removed = tabs.open.remove(index);
            for handle in removed.listeners {
                handle.abort();
            }
            if tabs.active.target_id() != target_id {
                None
            } else {
                Some(
                    tabs.open
                        .get(index.saturating_sub(1))
                        .map(|tab| tab.page.clone()),
                )
            }
        };
        self.publish_tabs();
        match fallback {
            None => {}
            Some(Some(page)) => {
                info!("browser: the active tab closed; back to another open tab");
                self.activate(&page).await;
            }
            Some(None) => self.reopen_blank_page().await,
        }
    }

    /// The last tab closed. The session stays usable: open a blank page and
    /// track it like any other, so it is the tab every consumer drives.
    async fn reopen_blank_page(self: &Arc<Self>) {
        info!("browser: the last tab closed; opening a blank page");
        // On the session's runtime: `close_tab` may be called from a thread
        // with no Tokio context, and the deadline here is a Tokio timer.
        let opened = self.runtime.spawn({
            let this = self.clone();
            async move {
                let guard = this.browser.lock().await;
                let browser = guard.as_ref()?;
                match tokio::time::timeout(
                    Duration::from_secs(DEFAULT_TIMEOUT_SECS),
                    browser.new_page("about:blank"),
                )
                .await
                {
                    Ok(Ok(page)) => Some(page),
                    Ok(Err(e)) => {
                        warn!(error = %e, "browser: cannot open a blank page after the last tab closed");
                        None
                    }
                    Err(_) => {
                        warn!("browser: opening a blank page after the last tab closed timed out");
                        None
                    }
                }
            }
        });
        let Ok(Some(page)) = opened.await else {
            return;
        };
        // The watcher sees this target too; `track` dedupes under the lock.
        self.track(&page, "about:blank".to_string()).await;
    }

    /// A page navigated itself. The policy is re-checked here, not only when
    /// the agent asks for a navigation (AGE-458) — see [`Self::ensure_allowed`]
    /// for why. Every tracked tab is guarded, not just the active one
    /// (AGE-473): a background tab that lands somewhere refused is blocked
    /// the moment it does, not once someone switches to it.
    pub(super) async fn page_navigated(&self, target_id: &TargetId, url: String) {
        let refused = !targets::may_drive_url(&self.policy, &url);
        let (is_active, was_blocked) = {
            let mut tabs = self.tabs.lock();
            let is_active = tabs.active.target_id() == target_id;
            let Some(tab) = tabs.find_mut(target_id) else {
                return;
            };
            let was_blocked = tab.blocked.is_some();
            tab.url = url.clone();
            tab.blocked = refused.then(|| url.clone());
            if refused {
                // Nothing a refused page produces may reach the tools —
                // not even later, once the page has come back: stop the
                // pumps here, under the same lock that puts the block up.
                for handle in tab.listeners.drain(..) {
                    handle.abort();
                }
            } else if is_active {
                // Under the lock, so a switch cannot slip a background
                // tab's URL into the address bar.
                let _ = self.current_url.send(url.clone());
            }
            (is_active, was_blocked)
        };
        self.publish_tabs();

        if !is_active {
            if refused {
                warn!(
                    url = %url,
                    "browser: a background tab navigated itself outside the navigation policy; blocked"
                );
            }
            return;
        }

        self.invalidate_snapshot();
        if refused {
            warn!(
                url = %url,
                "browser: the page navigated itself outside the navigation policy; refusing it"
            );
            // Neither the picture nor the console text of a refused page
            // may reach anyone: drop what it produced and suspend the cast.
            self.events.clear();
            self.sync_screencast().await;
        } else if was_blocked {
            info!(url = %url, "browser: the page came back somewhere allowed");
            self.unblocked(&self.active_page()).await;
        } else {
            self.sync_screencast().await;
        }
    }

    /// The active tab came back somewhere allowed: revive the cast, and
    /// start capturing its console/network output again — its pumps were
    /// stopped when the block went up (or never started, for a tab
    /// activated while blocked). Whatever reached the buffers before that
    /// is not this page's and is dropped first.
    async fn unblocked(&self, page: &Page) {
        self.events.clear();
        self.attach_listeners(page).await;
        self.sync_screencast().await;
    }

    /// Turn a target id into a drivable page. The target is discovered before
    /// it is attached, so `get_page` has nothing to hand back for a moment.
    async fn resolve_page(&self, target_id: &TargetId) -> Option<Page> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(NEW_TAB_TIMEOUT_SECS);
        loop {
            {
                let guard = self.browser.lock().await;
                let browser = guard.as_ref()?;
                // A bounded wait: this holds the browser handle, which
                // `shutdown` also needs.
                if let Ok(Ok(page)) = tokio::time::timeout(
                    Duration::from_secs(DEFAULT_TIMEOUT_SECS),
                    browser.get_page(target_id.clone()),
                )
                .await
                {
                    return Some(page);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Capture console and network output from the tab that just became
    /// active (or readable), into the same buffers the tools drain. Best
    /// effort: losing a tab's console is not a reason to refuse to show it.
    ///
    /// The pumps are the tab's own: they are aborted when it stops being
    /// the active one, so nothing it does afterwards reaches the tools.
    async fn attach_listeners(&self, page: &Page) {
        match super::events::spawn_listeners(&self.runtime, page, self.events.clone()).await {
            Ok(handles) => {
                let mut tabs = self.tabs.lock();
                let still_active = tabs.active.target_id() == page.target_id();
                match tabs.find_mut(page.target_id()) {
                    // Still the active tab, still allowed, and not yet
                    // pumped: the pumps are its to keep.
                    Some(tab)
                        if still_active && tab.blocked.is_none() && tab.listeners.is_empty() =>
                    {
                        tab.listeners.extend(handles);
                    }
                    // Closed, superseded, refused, or already pumped while
                    // the domains were being enabled; nothing extra may
                    // write.
                    _ => {
                        for handle in handles {
                            handle.abort();
                        }
                    }
                }
            }
            Err(e) => warn!(error = %e, "browser: no console/network capture for this tab"),
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

    /// Click an element the agent saw in its latest snapshot (AGE-489).
    ///
    /// Refused while the user holds control, like every other mutating
    /// action, and on a blocked tab, like everything else. The checks that
    /// make the click itself safe — same element as the snapshot showed, on
    /// screen, nothing covering it — live in [`super::click`]. A navigation
    /// the click starts is vetted by the per-tab guard exactly like one the
    /// page started on its own (AGE-458); `navigated` tells the caller its
    /// refs are gone.
    pub async fn click(&self, node: &SnapshotNode) -> Result<ClickResult, BrowserError> {
        self.ensure_alive()?;
        self.control.ensure_agent()?;
        let page = self.page()?;
        let generation_before = self.snapshot_generation();
        let point = with_deadline(
            DEFAULT_TIMEOUT_SECS,
            &format!("clicking {}", node.r#ref),
            click::click(&page, node),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(CLICK_SETTLE_MS)).await;
        let snapshot_generation = self.snapshot_generation();
        // Read live rather than from `current_url`: the click may have
        // activated a new tab (AGE-458), and the tool's answer must say
        // where the agent is now.
        let url = self
            .active_page()
            .url()
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        Ok(ClickResult {
            point,
            url,
            navigated: snapshot_generation != generation_before,
            snapshot_generation,
        })
    }

    /// Replace the contents of a text field the agent saw in its latest
    /// snapshot (AGE-492). Same refusals as [`Self::click`]; the checks
    /// specific to typing — credential fields, read-back — live in
    /// [`super::typing`]. Returns the field's value afterwards.
    pub async fn type_text(&self, node: &SnapshotNode, text: &str) -> Result<String, BrowserError> {
        self.ensure_alive()?;
        self.control.ensure_agent()?;
        let page = self.page()?;
        with_deadline(
            DEFAULT_TIMEOUT_SECS,
            &format!("typing into {}", node.r#ref),
            typing::type_text(&page, node, text),
        )
        .await
    }

    /// Forward a mouse event. A no-op — not an error — when the agent
    /// holds control: a stray event racing a handback must not steer a
    /// session nobody handed to the user.
    pub async fn dispatch_mouse(&self, event: MouseInput) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        // A page the policy refuses is not shown, so a forwarded click has
        // nothing to aim at — and driving it would be exactly the bypass the
        // block exists to stop (AGE-458).
        self.ensure_allowed()?;
        if self.control_holder() != ControlHolder::User {
            return Ok(());
        }
        input::dispatch_mouse(&self.active_page(), event).await
    }

    /// Forward a keyboard event. Same no-op-when-agent-owns-it rule as
    /// [`Self::dispatch_mouse`].
    pub async fn dispatch_key(&self, event: KeyInput) -> Result<(), BrowserError> {
        self.ensure_alive()?;
        self.ensure_allowed()?;
        if self.control_holder() != ControlHolder::User {
            return Ok(());
        }
        input::dispatch_key(&self.active_page(), event).await
    }

    /// Stop every task a tab owns. The guards live in `listeners`.
    fn abort_tab_listeners(&self) {
        for tab in self.tabs.lock().open.iter_mut() {
            for handle in tab.listeners.drain(..) {
                handle.abort();
            }
        }
    }

    /// Close the browser and stop every task this session owns.
    pub async fn shutdown(&self) {
        self.stop_screencast().await;
        self.abort_tab_listeners();
        for handle in self.listeners.lock().drain(..) {
            handle.abort();
        }
        let browser = self.browser.lock().await.take();
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
        self.abort_tab_listeners();
        for handle in self.listeners.lock().drain(..) {
            handle.abort();
        }
        if let Some(handle) = self.handler.lock().take() {
            handle.abort();
        }
        // No CDP round trip here — `try_lock` is sync, and stopping the
        // task is enough; the browser process going away ends the cast.
        if let Ok(mut guard) = self.screencast.try_lock()
            && let Some(screencast) = guard.take()
        {
            screencast.abort();
        }
    }
}
