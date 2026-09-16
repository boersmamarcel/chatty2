//! CDP session lifecycle: launch, drive, recover, tear down.
//!
//! One session owns one browser process and one *active* page. Three consumers
//! share it — the agent's tools, the artifact viewport (AGE-155), and forwarded
//! user input (AGE-156) — so the session holds the state all three need rather
//! than any one of them owning the browser.
//!
//! The active page is not fixed for the session's lifetime: a page that opens a
//! new tab or window (`target="_blank"`, `window.open`, an OAuth popup) hands
//! the new target to [`BrowserSession::follow_target`], which promotes it so
//! screencast, input and tools all follow what the user is looking at, and
//! closing it falls back to the page the session launched with (AGE-458). See
//! [`super::targets`] for the rule and the watcher that applies it.
//!
//! Two rules shape everything here.
//!
//! **A browser that dies must surface as a typed error, never as a hung tool
//! call.** Every CDP round trip has a deadline, and the event-handler task
//! ending is the crash signal.
//!
//! **Nothing is shown or read that the navigation policy would refuse.** The
//! policy is not a check `browser_navigate` performs once; it is a property of
//! whatever page this session is driving, re-asserted on every navigation that
//! page makes — see [`BrowserSession::ensure_allowed`] and
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

use super::control::{ControlHolder, ControlLock};
use super::error::BrowserError;
use super::events::EventBuffers;
use super::input::{self, KeyInput, MouseInput};
use super::profile::{BrowserProfile, NavigationPolicy};
use super::screencast::{self, Screencast, ScreencastUpdate};
use super::targets;

/// Default deadline for a CDP round trip.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Navigation gets longer — a cold dev server can be slow to first byte.
pub const NAVIGATE_TIMEOUT_SECS: u64 = 60;
/// How long a new tab gets to become drivable before we give up on it.
const NEW_TAB_TIMEOUT_SECS: u64 = 10;
/// How long a new tab gets to land on its real URL before the policy decides
/// whether to follow it. `window.open` reports `about:blank` until then.
const NEW_TAB_SETTLE_SECS: u64 = 2;

/// The URL a newly opened tab settles on.
///
/// A tab is created before it navigates, and reports `about:blank` until then
/// — which is both the wrong thing to show in the address bar and the wrong
/// thing to hand the navigation policy. Wait briefly for the real one; a tab
/// that genuinely stays blank (an opener writing into it directly) costs the
/// wait once and is followed anyway.
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

/// A live browser session: one process, one active page.
pub struct BrowserSession {
    /// `tokio::sync::Mutex` because the target watcher resolves new tabs
    /// through the browser handle, which is a CDP round trip.
    browser: tokio::sync::Mutex<Option<Browser>>,
    /// The page the session launched with. Never replaced: it is what the
    /// session falls back to when a followed tab closes (AGE-458).
    primary: Page,
    /// The page every consumer drives right now — the primary page, or a tab
    /// or window one of them opened (AGE-458).
    active: Mutex<Page>,
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
    /// Pumps CDP events into `events` for the session's own page, plus the
    /// target watcher and the primary page's navigation guard (AGE-458).
    listeners: Mutex<Vec<JoinHandle<()>>>,
    /// The same pumps for the tab currently being followed (AGE-458), kept
    /// apart because they are aborted the moment we stop driving that tab: a
    /// tab we dropped must not go on writing into the buffers the tools read.
    tab_listeners: Mutex<Vec<JoinHandle<()>>>,
    /// Set when the page on screen navigated *itself* somewhere the policy
    /// refuses and there is nothing to fall back to (AGE-458). While set, the
    /// tools refuse, forwarded input is dropped and the cast is suspended.
    blocked: Mutex<Option<String>>,
    /// Live `Page.startScreencast` state, when the artifact viewport
    /// (AGE-155) is watching this session. `tokio::sync::Mutex` because
    /// starting/retargeting holds the guard across CDP round trips — and
    /// because which page is active must be read *under* it (AGE-458), or a
    /// tab promoted between the read and the lock leaves the cast behind.
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

        // Subscribe *after* opening our own page, so its creation cannot be
        // mistaken for a popup (the watcher filters it out by id as well).
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

        let session = Arc::new(Self {
            browser: tokio::sync::Mutex::new(Some(browser)),
            active: Mutex::new(page.clone()),
            primary: page,
            policy,
            profile,
            events,
            snapshot_generation: AtomicU64::new(1),
            dead,
            handler: Mutex::new(Some(handler)),
            listeners: Mutex::new(listeners),
            tab_listeners: Mutex::new(Vec::new()),
            blocked: Mutex::new(None),
            screencast: tokio::sync::Mutex::new(None),
            control: ControlLock::new(),
            current_url: watch::channel(String::from("about:blank")).0,
            _user_data: user_data,
        });

        let watcher = targets::spawn_watcher(&session, created, destroyed);
        session.listeners.lock().push(watcher);

        // The policy is checked again on every navigation this page makes on
        // its own, not only on the ones we asked for (AGE-458).
        match targets::spawn_navigation_guard(&session, &session.primary).await {
            Ok(guard) => session.listeners.lock().push(guard),
            Err(e) => {
                session.shutdown().await;
                return Err(e);
            }
        }

        Ok(session)
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

    /// Err while the page on screen is one the navigation policy refuses
    /// (AGE-458). `browser_navigate` vets the URL it is given and every
    /// redirect hop, but a page can move on its own afterwards — a script, a
    /// meta refresh, an opener setting `popup.location` — and a page the
    /// policy would have refused must not become readable just because the
    /// page, rather than the agent, is what navigated.
    fn ensure_allowed(&self) -> Result<(), BrowserError> {
        match self.blocked.lock().as_deref() {
            Some(url) => Err(BrowserError::NavigationRefused(format!(
                "the page navigated itself to {url}, which this browser profile does not \
                 allow; it is not readable from here — navigate somewhere allowed to continue"
            ))),
            None => Ok(()),
        }
    }

    /// The page this session drives right now. Cloned rather than borrowed
    /// because it can change under the caller: a tab the page opened is
    /// promoted to active while a tool call is in flight (AGE-458).
    pub fn page(&self) -> Result<Page, BrowserError> {
        self.ensure_alive()?;
        self.ensure_allowed()?;
        Ok(self.active_page())
    }

    /// The active page, without the liveness check.
    fn active_page(&self) -> Page {
        self.active.lock().clone()
    }

    /// The target the session launched with — the one a followed tab falls
    /// back to, and the one that is never itself followed (AGE-458).
    pub(super) fn primary_target_id(&self) -> &TargetId {
        self.primary.target_id()
    }

    /// The navigation policy this session's profile carries.
    pub fn policy(&self) -> &NavigationPolicy {
        &self.policy
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
        self.set_blocked(None).await;
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
    /// whatever page is active now (AGE-458) — rather than starting a second
    /// one.
    pub async fn start_screencast(
        &self,
        width: u32,
        height: u32,
    ) -> Result<watch::Receiver<ScreencastUpdate>, BrowserError> {
        self.ensure_alive()?;
        self.ensure_allowed()?;
        let mut guard = self.screencast.lock().await;
        // Read the active page *under* the lock: a tab promoted between the
        // read and the lock would otherwise leave the cast on the old page
        // while input and tools drive the new one (AGE-458).
        let page = self.active_page();
        // The state is handed over by mutable borrow, never taken out: a
        // failed retarget leaves the previous screencast — which Chrome is
        // still encoding — in place, so a later call retargets or stops it
        // instead of asking Chrome to start a second one (AGE-457).
        screencast::start(&page, &mut guard, width, height).await
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

    /// Make the cast show whatever page is active now (AGE-458), keeping the
    /// channel its consumer is already watching. A no-op when nothing is
    /// casting — following a tab must not start a cast nobody asked for.
    ///
    /// Failing here leaves the cast suspended with its channel alive, so the
    /// panel says why instead of reporting a session that ended.
    async fn sync_screencast(&self) {
        let mut guard = self.screencast.lock().await;
        let page = self.active_page();
        let Some(screencast) = guard.as_ref() else {
            return;
        };
        // Whatever viewport the panel last asked for, now on another page.
        let (width, height) = screencast.size();
        if screencast.is_casting(&page, width, height) {
            return;
        }
        if let Err(e) = screencast::start(&page, &mut guard, width, height).await {
            warn!(error = %e, "browser: cannot move the screencast to the active page");
        }
    }

    /// Suspend the cast, telling whoever is watching why. The channel stays
    /// open: a later `sync_screencast` revives it.
    async fn hold_screencast(&self, reason: &str) {
        if let Some(screencast) = self.screencast.lock().await.as_mut() {
            screencast.hold(reason).await;
        }
    }

    /// Follow a tab or window the active page opened (AGE-458): resolve it,
    /// check it against the navigation policy, then make it the page every
    /// consumer drives.
    ///
    /// Every failure leaves the session exactly as it was — an unfollowable
    /// tab is invisible, which is what it was before this existed. The check
    /// here is only the *first* one: a tab that passes it and then navigates
    /// itself somewhere refused is dropped again by the navigation guard.
    pub(super) async fn follow_target(self: &Arc<Self>, target_id: TargetId) {
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
        if !targets::may_drive_url(&self.policy, &url) {
            warn!(
                url = %url,
                "browser: a new tab opened outside the navigation policy; not following it"
            );
            return;
        }

        // The guard has to be watching before the tab is on screen, or a tab
        // that navigates itself the instant it is promoted slips through.
        let guard = match targets::spawn_navigation_guard(self, &page).await {
            Ok(guard) => guard,
            Err(e) => {
                warn!(error = %e, "browser: cannot watch the new tab's navigation; not following it");
                return;
            }
        };

        info!(url = %url, "browser: following a new tab");
        self.retire_tab_listeners();
        // The guard goes with the session's own listeners, *not* the tab's:
        // dropping a tab is one of the things the guard itself decides, and a
        // task cannot abort itself mid-decision. It is self-limiting anyway —
        // its stream ends when the tab does — and only acts while the tab it
        // watches is the active one.
        {
            let mut listeners = self.listeners.lock();
            listeners.retain(|handle| !handle.is_finished());
            listeners.push(guard);
        }
        *self.active.lock() = page.clone();
        // Every element ref the agent holds belongs to the page underneath.
        self.invalidate_snapshot();
        self.set_blocked(None).await;
        let _ = self.current_url.send(if url.is_empty() {
            "about:blank".to_string()
        } else {
            url
        });
        self.attach_listeners(&page).await;
        self.sync_screencast().await;
    }

    /// A target went away. When it is the one we followed, fall back to the
    /// page the session launched with (AGE-458) rather than leaving every
    /// consumer pointed at a page that no longer exists.
    pub(super) async fn target_closed(&self, target_id: &TargetId) {
        if self.active.lock().target_id() != target_id {
            return;
        }
        if self.primary.target_id() == target_id {
            // The session's own page closed; there is nothing to fall back to
            // and `ensure_alive` is what reports the browser dying.
            return;
        }
        self.fall_back_to_primary("the followed tab closed").await;
    }

    /// A page navigated itself. The policy is re-checked here, not only when
    /// the agent asks for a navigation (AGE-458) — see [`Self::ensure_allowed`]
    /// for why.
    pub(super) async fn page_navigated(&self, target_id: &TargetId, url: String) {
        // Only the page on screen matters: a tab nobody is driving shows
        // nothing to anyone and is readable by nothing.
        if self.active.lock().target_id() != target_id {
            return;
        }

        if targets::may_drive_url(&self.policy, &url) {
            // Coming back somewhere allowed lifts a block and revives the cast.
            if self.blocked.lock().is_some() {
                info!(url = %url, "browser: the page came back somewhere allowed");
            }
            self.set_blocked(None).await;
            self.sync_screencast().await;
            self.invalidate_snapshot();
            let _ = self.current_url.send(url);
            return;
        }

        if target_id == self.primary.target_id() {
            warn!(
                url = %url,
                "browser: the page navigated itself outside the navigation policy; refusing it"
            );
            self.set_blocked(Some(url)).await;
            return;
        }

        warn!(
            url = %url,
            "browser: the followed tab navigated itself outside the navigation policy; dropping it"
        );
        let tab = self.active_page();
        self.fall_back_to_primary("the followed tab went somewhere this profile does not allow")
            .await;
        // Nothing wants a refused page left loading in the background.
        if let Err(e) = tab.close().await {
            debug!(error = ?e, "browser: closing the refused tab failed");
        }
    }

    /// Hand screencast, input and the tools back to the page the session
    /// launched with.
    async fn fall_back_to_primary(&self, reason: &str) {
        info!(reason, "browser: back to the session's own page");
        self.retire_tab_listeners();
        *self.active.lock() = self.primary.clone();
        self.invalidate_snapshot();
        // Whatever the tab logged or requested is not this page's.
        self.events.clear();
        let url = self
            .primary
            .url()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "about:blank".to_string());
        // The page we are falling back to may itself be blocked — it is the
        // same page it was, wherever it had got to.
        let blocked = (!targets::may_drive_url(&self.policy, &url)).then(|| url.clone());
        self.set_blocked(blocked).await;
        let _ = self.current_url.send(url);
        self.sync_screencast().await;
    }

    /// Put the block up or take it down, keeping the cast and the event
    /// buffers consistent with it.
    async fn set_blocked(&self, url: Option<String>) {
        let reason = {
            let mut blocked = self.blocked.lock();
            if *blocked == url {
                return;
            }
            *blocked = url.clone();
            url
        };
        match reason {
            Some(url) => {
                // Neither the picture nor the console text of a refused page
                // may reach anyone: stop showing it and drop what it produced.
                self.events.clear();
                self.invalidate_snapshot();
                self.hold_screencast(&format!(
                    "the page navigated to {url}, which this browser profile does not allow"
                ))
                .await;
            }
            None => self.sync_screencast().await,
        }
    }

    /// Stop the console/network pumps belonging to the tab we were following.
    /// A tab we have dropped must not keep writing into the buffers the tools
    /// read. (Its navigation guard lives in `listeners` — see
    /// [`Self::follow_target`] — so this is never the caller's own task.)
    fn retire_tab_listeners(&self) {
        for handle in self.tab_listeners.lock().drain(..) {
            handle.abort();
        }
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

    /// Capture console and network output from a newly followed tab too,
    /// into the same buffers the tools drain. Best effort: losing a tab's
    /// console is not a reason to refuse to show it.
    ///
    /// These go in `tab_listeners`, not `listeners`: they die with the tab we
    /// stop driving, so nothing it does afterwards reaches the tools.
    async fn attach_listeners(&self, page: &Page) {
        match super::events::spawn_listeners(page, self.events.clone()).await {
            Ok(handles) => self.tab_listeners.lock().extend(handles),
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

    /// Close the browser and stop every task this session owns.
    pub async fn shutdown(&self) {
        self.stop_screencast().await;
        self.retire_tab_listeners();
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
        for handle in self
            .listeners
            .lock()
            .drain(..)
            .chain(self.tab_listeners.lock().drain(..))
        {
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
