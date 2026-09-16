//! Following a page that opens a new tab or window (AGE-458).
//!
//! A link with `target="_blank"`, a `window.open` call, an OAuth sign-in
//! popup — each creates a second CDP target. The session used to drive
//! exactly one page, so those targets were never screencast, never received
//! forwarded input, and never reached the agent's tools: from the artifact
//! panel nothing happened at all, and a flow that depends on the popup (most
//! third-party OAuth) could not be completed from either side.
//!
//! The rule here is deliberately smaller than a tab switcher: **the newest
//! page target the navigation policy allows becomes the active one**, and
//! when it closes the session falls back to the page it was launched with.
//! Exactly one page is active at a time, and screencast, forwarded input and
//! the agent's tools all follow it — there is no second viewport to present,
//! so the artifact panel needs no new UI.
//!
//! The policy is checked twice, because once is not enough: when a tab is
//! promoted, and again on **every navigation the page makes on its own**
//! ([`spawn_navigation_guard`]). Without the second check an opener could
//! open a blank window — nothing to refuse — wait for it to be promoted, and
//! then set its `location` to anything it liked, which would put that page's
//! pixels in the artifact panel and its DOM in `browser_snapshot`. The guard
//! watches the page the session is driving, whichever page that is, so it
//! also covers the session's own page navigating itself.
//!
//! Chrome does the attaching for us: chromiumoxide's handler turns on
//! `Target.setDiscoverTargets` at connect and attaches to every target it
//! discovers, so this module only has to decide *which* target to follow and
//! tell the session about it.

use std::sync::{Arc, Weak};

use chromiumoxide::cdp::browser_protocol::page::EventFrameNavigated;
use chromiumoxide::cdp::browser_protocol::target::{
    EventTargetCreated, EventTargetDestroyed, TargetId, TargetInfo,
};
use chromiumoxide::listeners::EventStream;
use chromiumoxide::page::Page;
use futures::StreamExt;
use tokio::task::JoinHandle;

use super::error::BrowserError;
use super::profile::NavigationPolicy;
use super::session::BrowserSession;

/// A URL that carries no content from any origin, so the policy has nothing
/// to refuse: a target that has not navigated yet (`window.open()` reports
/// `about:blank` until its load starts, and a window its opener writes into
/// directly never reports anything else), and Chrome's own error page, which
/// is what a *failed* navigation to an allowed URL leaves behind.
pub(super) fn is_exempt_url(url: &str) -> bool {
    url.is_empty() || url.starts_with("about:blank") || url.starts_with("chrome-error://")
}

/// Whether a freshly created target is one the session should follow.
///
/// Only real pages: an iframe, a worker or Chrome's own background page is
/// not something a user can be handed control of. The session's own page is
/// excluded too — it is the fallback, not a popup.
pub(super) fn is_followable_target(info: &TargetInfo, primary: &TargetId) -> bool {
    info.r#type == "page" && &info.target_id != primary
}

/// Whether the session may show and drive a page sitting at `url`.
///
/// Showing a page widens what the agent can see, so it is gated on the same
/// navigation policy `browser_navigate` is. Asked both when a tab is promoted
/// and on every navigation afterwards, so the answer cannot go stale.
pub(super) fn may_drive_url(policy: &NavigationPolicy, url: &str) -> bool {
    is_exempt_url(url) || policy.check(url).is_ok()
}

/// Watch the browser's targets for the session's lifetime.
///
/// Holds a `Weak` reference so a dropped session ends the task rather than
/// keeping a browser alive nobody is watching; the handle is owned by the
/// session and aborted on shutdown.
pub(super) fn spawn_watcher(
    session: &Arc<BrowserSession>,
    created: EventStream<EventTargetCreated>,
    destroyed: EventStream<EventTargetDestroyed>,
) -> JoinHandle<()> {
    let session = Arc::downgrade(session);
    tokio::spawn(watch(session, created, destroyed))
}

/// Re-check the policy on every navigation `page` makes, for as long as the
/// session is driving it.
///
/// This is the check `browser_navigate` cannot make: it vets the URL it is
/// given and the redirect chain it lands through, but a page that moves
/// afterwards — a script, a meta refresh, an opener assigning
/// `popup.location` — was never re-examined. Whatever the page does, the
/// session decides what happens next ([`BrowserSession::page_navigated`]).
pub(super) async fn spawn_navigation_guard(
    session: &Arc<BrowserSession>,
    page: &Page,
) -> Result<JoinHandle<()>, BrowserError> {
    let navigated = page
        .event_listener::<EventFrameNavigated>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot watch page navigation: {e}")))?;
    let session = Arc::downgrade(session);
    let target_id = page.target_id().clone();
    Ok(tokio::spawn(guard(session, target_id, navigated)))
}

async fn guard(
    session: Weak<BrowserSession>,
    target_id: TargetId,
    mut navigated: EventStream<EventFrameNavigated>,
) {
    while let Some(event) = navigated.next().await {
        // Only the main frame moves the page; an iframe navigating is the
        // page's own business and shows nothing on its own.
        if event.frame.parent_id.is_some() {
            continue;
        }
        let Some(session) = session.upgrade() else {
            return;
        };
        session
            .page_navigated(&target_id, event.frame.url.clone())
            .await;
    }
}

async fn watch(
    session: Weak<BrowserSession>,
    mut created: EventStream<EventTargetCreated>,
    mut destroyed: EventStream<EventTargetDestroyed>,
) {
    loop {
        tokio::select! {
            event = created.next() => {
                let (Some(event), Some(session)) = (event, session.upgrade()) else {
                    return;
                };
                if is_followable_target(&event.target_info, session.primary_target_id()) {
                    session.follow_target(event.target_info.target_id.clone()).await;
                }
            }
            event = destroyed.next() => {
                let (Some(event), Some(session)) = (event, session.upgrade()) else {
                    return;
                };
                session.target_closed(&event.target_id).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str, kind: &str) -> TargetInfo {
        TargetInfo::builder()
            .target_id(TargetId::new(id))
            .r#type(kind)
            .title("t")
            .url("about:blank")
            .attached(true)
            .can_access_opener(false)
            .build()
            .expect("target info")
    }

    #[test]
    fn a_new_page_target_is_followed() {
        let primary = TargetId::new("primary");
        assert!(is_followable_target(&target("popup", "page"), &primary));
    }

    #[test]
    fn the_sessions_own_page_is_not_a_popup() {
        let primary = TargetId::new("primary");
        assert!(!is_followable_target(&target("primary", "page"), &primary));
    }

    #[test]
    fn only_page_targets_are_followed() {
        let primary = TargetId::new("primary");
        for kind in ["iframe", "worker", "service_worker", "background_page"] {
            assert!(
                !is_followable_target(&target("other", kind), &primary),
                "{kind} is not a page the user can drive"
            );
        }
    }

    #[test]
    fn a_page_with_no_content_of_its_own_is_exempt() {
        let policy = NavigationPolicy::local_only(Some("/ws".into()));
        assert!(may_drive_url(&policy, "about:blank"));
        assert!(may_drive_url(&policy, "about:blank#blocked"));
        assert!(may_drive_url(&policy, ""));
        assert!(
            may_drive_url(&policy, "chrome-error://chromewebdata/"),
            "a failed load must not look like a policy violation"
        );
    }

    #[test]
    fn a_page_the_policy_refuses_may_not_be_driven() {
        let policy = NavigationPolicy::local_only(Some("/ws".into()));
        assert!(may_drive_url(&policy, "http://127.0.0.1:3000/callback"));
        assert!(
            !may_drive_url(&policy, "https://accounts.example.com/oauth"),
            "Lane A must not screencast an off-machine page"
        );
    }

    #[test]
    fn the_open_web_policy_still_refuses_internal_targets() {
        let policy = NavigationPolicy::open(Some("/ws".into()));
        assert!(may_drive_url(&policy, "http://127.0.0.1:3000/callback"));
        assert!(
            !may_drive_url(&policy, "http://169.254.169.254/latest/meta-data/"),
            "a page on cloud metadata must not be driven"
        );
    }
}
