//! The site gate (AGE-1915): request interception for `look` and unattended
//! `ask_risky` origins, and the saved session (ADR-0106) loaded into the
//! ephemeral profile.
//!
//! The decisions are in [`super::sites`]; this file only wires them to CDP.
//! A session is a secret: nothing here logs it, and nothing returns it.

use std::sync::Arc;

use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, FulfillRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    CookieParam, ErrorReason, ResourceType, SetCookiesParams,
};
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::page::Page;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tracing::warn;

use super::error::BrowserError;
use super::sites::{self, RequestVerdict};
use crate::settings::models::execution_settings::{ActOnSite, SavedSession, SiteMode};

/// What the interceptor refused since the last [`SiteGate::take_outcome`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    /// Non-GET requests refused on look-only origins.
    pub look_blocked: usize,
    /// The first risky request refused on an ask-risky origin: (method, url
    /// without query).
    pub risky_blocked: Option<(String, String)>,
}

/// Holds the approval of one risky action; dropping it ends the approval.
#[must_use = "dropping the guard ends the approval"]
pub struct RiskyApproval {
    gate: Arc<SiteGate>,
}

impl Drop for RiskyApproval {
    fn drop(&mut self) {
        self.gate.reset_risky();
    }
}

pub struct SiteGate {
    act_on: Vec<ActOnSite>,
    /// Only sessions whose origin is granted.
    sessions: Vec<SavedSession>,
    unattended: bool,
    /// The origin of the approved action that is running: its risky requests
    /// (and only those) go through until the action ends (AGE-2043, AGE-2154).
    approved_origin: Mutex<Option<String>>,
    outcome: Mutex<GateOutcome>,
}

impl SiteGate {
    pub fn new(options: &super::LaunchOptions) -> Arc<Self> {
        let sessions = sites::granted_sessions(&options.act_on, &options.sessions)
            .into_iter()
            .cloned()
            .collect();
        Arc::new(Self {
            act_on: options.act_on.clone(),
            sessions,
            unattended: options.unattended,
            approved_origin: Mutex::new(None),
            outcome: Mutex::new(GateOutcome::default()),
        })
    }

    pub fn act_on(&self) -> &[ActOnSite] {
        &self.act_on
    }

    /// Whether any request needs judging at all. Without this, Fetch stays
    /// off and no request is ever paused.
    fn intercepts(&self) -> bool {
        self.act_on.iter().any(|s| match s.mode {
            SiteMode::Look => true,
            SiteMode::AskRisky => self.unattended,
            SiteMode::Any => false,
        })
    }

    /// Let risky requests to `origin` through while the returned guard lives
    /// (an approved action). Dropping the guard refuses them again, on every
    /// path out of the action.
    pub fn approve_risky_for(self: &Arc<Self>, origin: &str) -> RiskyApproval {
        *self.approved_origin.lock() = Some(origin.to_string());
        RiskyApproval { gate: self.clone() }
    }

    /// Whether an approved action is running right now.
    pub fn allows_risky(&self) -> bool {
        self.approved_origin.lock().is_some()
    }

    /// Whether the approved action lets a request to `url` through: only its
    /// own origin.
    fn risky_allowed_for(&self, url: &str) -> bool {
        match (
            self.approved_origin.lock().as_deref(),
            sites::origin_of(url),
        ) {
            (Some(approved), Some(origin)) => approved == origin,
            _ => false,
        }
    }

    /// Refuse risky requests again (a navigate, the end of an action).
    pub fn reset_risky(&self) {
        *self.approved_origin.lock() = None;
    }

    /// Take (and reset) what was refused.
    pub fn take_outcome(&self) -> GateOutcome {
        std::mem::take(&mut *self.outcome.lock())
    }

    /// Load the cookies of every granted origin's saved session
    /// (`Network.setCookies`). A cookie whose domain does not cover the
    /// session's own origin is dropped: a session cannot plant cookies on
    /// another site.
    pub async fn load_cookies(&self, page: &Page) {
        let mut params = Vec::new();
        for session in &self.sessions {
            let Some(host) = reqwest::Url::parse(&session.origin)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
            else {
                continue;
            };
            let cookies = session.storage_state["cookies"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for mut cookie in cookies {
                let Some(obj) = cookie.as_object_mut() else {
                    continue;
                };
                if let Some(domain) = obj.get("domain").and_then(|d| d.as_str()) {
                    let d = domain.trim_start_matches('.');
                    if !(host == d || host.ends_with(&format!(".{d}"))) {
                        continue;
                    }
                } else {
                    obj.insert("url".into(), session.origin.clone().into());
                }
                // Playwright writes -1 for a session cookie; CDP wants it absent.
                if obj
                    .get("expires")
                    .and_then(|e| e.as_f64())
                    .is_some_and(|e| e <= 0.0)
                {
                    obj.remove("expires");
                }
                obj.retain(|k, _| {
                    matches!(
                        k.as_str(),
                        "name"
                            | "value"
                            | "url"
                            | "domain"
                            | "path"
                            | "secure"
                            | "httpOnly"
                            | "sameSite"
                            | "expires"
                    )
                });
                if let Ok(p) = serde_json::from_value::<CookieParam>(cookie) {
                    params.push(p);
                }
            }
        }
        if params.is_empty() {
            return;
        }
        let count = params.len();
        if let Err(e) = page.execute(SetCookiesParams::new(params)).await {
            warn!(error = %e, "browser: cannot load the saved session's cookies");
        } else {
            tracing::debug!(count, "browser: saved session cookies loaded");
        }
    }

    /// Put the saved localStorage into `page` on its first navigation to
    /// each granted origin: a script that runs before the page's own, sets
    /// only keys that are absent, and marks itself done for the tab.
    pub async fn load_local_storage(&self, page: &Page) {
        for session in &self.sessions {
            let Some(origins) = session.storage_state["origins"].as_array() else {
                continue;
            };
            for entry in origins {
                // The entry's own origin must be the session's origin.
                let Some(origin) = entry["origin"].as_str() else {
                    continue;
                };
                if sites::origin_of(origin) != sites::origin_of(&session.origin) {
                    continue;
                }
                let Some(items) = entry["localStorage"].as_array() else {
                    continue;
                };
                let pairs: Vec<[String; 2]> = items
                    .iter()
                    .filter_map(|i| {
                        Some([
                            i["name"].as_str()?.to_string(),
                            i["value"].as_str()?.to_string(),
                        ])
                    })
                    .collect();
                if pairs.is_empty() {
                    continue;
                }
                let (Ok(o), Ok(p)) = (
                    serde_json::to_string(&sites::origin_of(&session.origin)),
                    serde_json::to_string(&pairs),
                ) else {
                    continue;
                };
                let source = format!(
                    "(()=>{{try{{if(location.origin!=={o})return;\
                     if(sessionStorage.getItem('__chatty_ls'))return;\
                     for(const [k,v] of {p})if(localStorage.getItem(k)===null)localStorage.setItem(k,v);\
                     sessionStorage.setItem('__chatty_ls','1');}}catch(e){{}}}})();"
                );
                if let Err(e) = page
                    .execute(AddScriptToEvaluateOnNewDocumentParams::new(source))
                    .await
                {
                    warn!(error = %e, "browser: cannot load the saved session's storage");
                }
            }
        }
    }

    /// Start judging this page's requests. The task lives as long as the
    /// page: a paused request is never left without an answer because the
    /// task is subscribed *before* Fetch is enabled.
    pub(super) async fn spawn_interceptor(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        page: &Page,
    ) -> Result<Option<JoinHandle<()>>, BrowserError> {
        if !self.intercepts() {
            return Ok(None);
        }
        let mut paused = page
            .event_listener::<EventRequestPaused>()
            .await
            .map_err(|e| BrowserError::Protocol(format!("cannot watch requests: {e}")))?;
        page.execute(FetchEnableParams::default())
            .await
            .map_err(|e| BrowserError::Protocol(format!("cannot enable Fetch domain: {e}")))?;
        let gate = self.clone();
        let page = page.clone();
        Ok(Some(runtime.spawn(async move {
            while let Some(event) = paused.next().await {
                let id = event.request_id.clone();
                let method = event.request.method.clone();
                let url = event.request.url.clone();
                let allow_risky = gate.risky_allowed_for(&url);
                let verdict = sites::judge_request(
                    &gate.act_on,
                    gate.unattended && !allow_risky,
                    &method,
                    &url,
                );
                let result = match verdict {
                    RequestVerdict::Allow => page
                        .execute(ContinueRequestParams::new(id))
                        .await
                        .map(|_| ()),
                    blocked => {
                        {
                            let mut out = gate.outcome.lock();
                            if blocked == RequestVerdict::BlockLook {
                                out.look_blocked += 1;
                            } else if out.risky_blocked.is_none() {
                                let mut bare = url.clone();
                                if let Ok(mut u) = reqwest::Url::parse(&url) {
                                    u.set_query(None);
                                    u.set_fragment(None);
                                    bare = u.to_string();
                                }
                                out.risky_blocked = Some((method.clone(), bare));
                            }
                        }
                        // A refused page navigation (a form submit) would
                        // leave an error page; an empty 204 keeps the page
                        // exactly as it was. Everything else fails.
                        if event.resource_type == ResourceType::Document {
                            page.execute(FulfillRequestParams::new(id, 204))
                                .await
                                .map(|_| ())
                        } else {
                            page.execute(FailRequestParams::new(id, ErrorReason::BlockedByClient))
                                .await
                                .map(|_| ())
                        }
                    }
                };
                if let Err(e) = result {
                    warn!(error = %e, "browser: cannot answer a paused request");
                }
            }
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::browser::LaunchOptions;

    fn gate() -> Arc<SiteGate> {
        let options = LaunchOptions {
            act_on: vec![
                ActOnSite {
                    origin: "https://a.test".into(),
                    mode: SiteMode::AskRisky,
                },
                ActOnSite {
                    origin: "https://b.test".into(),
                    mode: SiteMode::AskRisky,
                },
            ],
            unattended: true,
            ..Default::default()
        };
        SiteGate::new(&options)
    }

    /// The interceptor's verdict for a POST to `url` right now.
    fn post_verdict(gate: &SiteGate, url: &str) -> RequestVerdict {
        sites::judge_request(
            &gate.act_on,
            gate.unattended && !gate.risky_allowed_for(url),
            "POST",
            url,
        )
    }

    #[test]
    fn approval_lets_only_the_approved_origin_through() {
        let gate = gate();
        let _approval = gate.approve_risky_for("https://a.test");
        assert_eq!(
            post_verdict(&gate, "https://a.test/delete"),
            RequestVerdict::Allow
        );
        assert_eq!(
            post_verdict(&gate, "https://b.test/delete"),
            RequestVerdict::BlockRisky
        );
    }

    #[test]
    fn dropping_the_approval_refuses_risky_requests_again() {
        let gate = gate();
        drop(gate.approve_risky_for("https://a.test"));
        assert!(!gate.allows_risky());
        assert_eq!(
            post_verdict(&gate, "https://a.test/delete"),
            RequestVerdict::BlockRisky
        );
    }

    #[test]
    fn navigate_reset_refuses_page_posts_after_an_approved_click() {
        let gate = gate();
        let approval = gate.approve_risky_for("https://a.test");
        // after_action ends the approval (it takes the guard); a later
        // navigate resets again and the page's own POST is refused.
        drop(approval);
        gate.reset_risky();
        assert_eq!(
            post_verdict(&gate, "https://a.test/delete"),
            RequestVerdict::BlockRisky
        );
    }
}
