//! Site grants, modes and the risky-action rules (AGE-1915, ADR-0105/0106/0108).
//!
//! Everything here is pure: the rules sit in one place so a classifier can
//! replace them later, and so they are tested without a browser. When a rule
//! cannot tell, the answer is "risky".

use crate::settings::models::execution_settings::{ActOnSite, SavedSession, SiteMode};

/// Word stems that make a control or a request path risky. A token is risky
/// when it *starts with* one of these (`deleted`, `payment`, `users`).
const RISKY_PREFIXES: &[&str] = &[
    "delet",
    "remov",
    "cancel",
    "refund",
    "void",
    "pay",
    "send",
    "notif",
    "email",
    "messag",
    "repl",
    "password",
    "passwd",
    "user",
    "role",
    "permission",
    "destroy",
    "revoke",
    "erase",
    "purchase",
];

/// Short words that only count as whole tokens (`mail`, not `mailbox`-like
/// accidents such as `sentence`).
const RISKY_EXACT: &[&str] = &["sent", "mail", "msg", "pwd", "rm", "del"];

/// Lowercase word tokens of `text`, split on anything that is not a letter
/// or digit and on camelCase boundaries (`DeleteUser` -> `delete`, `user`).
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn text_is_risky(text: &str) -> bool {
    tokens(text).iter().any(|t| {
        RISKY_EXACT.contains(&t.as_str()) || RISKY_PREFIXES.iter().any(|p| t.starts_with(p))
    })
}

/// Whether clicking a control with this accessible role and name is risky.
/// An unnamed control is unknown, so it is risky.
pub fn control_is_risky(role: &str, name: &str) -> bool {
    if name.trim().is_empty() {
        return true;
    }
    text_is_risky(name) || text_is_risky(role)
}

/// Whether a request is risky by method and path: any `DELETE`, or a
/// non-GET whose path has a risky word. GET, HEAD and OPTIONS are not.
pub fn request_is_risky(method: &str, url: &str) -> bool {
    let method = method.to_ascii_uppercase();
    match method.as_str() {
        "DELETE" => true,
        "GET" | "HEAD" | "OPTIONS" => false,
        _ => match reqwest::Url::parse(url) {
            Ok(u) => text_is_risky(u.path()),
            // Unparseable: unsure.
            Err(_) => true,
        },
    }
}

/// `scheme://host[:port]` of an http(s) URL, lowercase, default port
/// dropped. `None` for anything else (file, about, data...).
pub fn origin_of(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?;
    Some(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

/// The mode granted for the page at `url`, if its origin is in the list.
pub fn mode_for(act_on: &[ActOnSite], url: &str) -> Option<SiteMode> {
    let origin = origin_of(url)?;
    act_on
        .iter()
        .find(|s| origin_of(&s.origin).as_deref() == Some(origin.as_str()))
        .map(|s| s.mode)
}

/// The granted origins, normalised, for messages.
pub fn granted_origins(act_on: &[ActOnSite]) -> Vec<String> {
    act_on
        .iter()
        .map(|s| origin_of(&s.origin).unwrap_or_else(|| s.origin.clone()))
        .collect()
}

/// The saved sessions whose origin is granted. Any other is dropped.
pub fn granted_sessions<'a>(
    act_on: &[ActOnSite],
    sessions: &'a [SavedSession],
) -> Vec<&'a SavedSession> {
    sessions
        .iter()
        .filter(|s| mode_for(act_on, &s.origin).is_some())
        .collect()
}

/// What an acting tool does on the current page.
#[derive(Debug, PartialEq, Eq)]
pub enum ActionGate {
    /// Run. `look` says the site is look-only (nothing may be sent).
    Proceed { look: bool },
    /// Put it to the person as an approval card.
    Ask,
    /// Risky, and nobody is attached: return `needs_approval`.
    NeedsApproval,
    /// Not a granted site and nobody is attached.
    Refuse,
}

/// Decide for one action. `is_click` is false for typing, which sends
/// nothing by itself. `off_lane_a` is whether the page is outside localhost
/// and the workspace (the rule before grants existed).
pub fn decide(
    act_on: &[ActOnSite],
    url: &str,
    off_lane_a: bool,
    is_click: bool,
    role: &str,
    name: &str,
    attended: bool,
) -> ActionGate {
    match mode_for(act_on, url) {
        Some(SiteMode::Any) => ActionGate::Proceed { look: false },
        Some(SiteMode::Look) => ActionGate::Proceed { look: true },
        Some(SiteMode::AskRisky) => {
            if is_click && control_is_risky(role, name) {
                if attended {
                    ActionGate::Ask
                } else {
                    ActionGate::NeedsApproval
                }
            } else {
                ActionGate::Proceed { look: false }
            }
        }
        None if off_lane_a => {
            if attended {
                ActionGate::Ask
            } else {
                ActionGate::Refuse
            }
        }
        None => ActionGate::Proceed { look: false },
    }
}

/// What a request interceptor does with one request.
#[derive(Debug, PartialEq, Eq)]
pub enum RequestVerdict {
    Allow,
    /// Look mode: a non-GET request.
    BlockLook,
    /// Ask-risky mode, unattended: a risky request.
    BlockRisky,
}

/// Judge a request to `url` made with `method`.
pub fn judge_request(
    act_on: &[ActOnSite],
    unattended: bool,
    method: &str,
    url: &str,
) -> RequestVerdict {
    match mode_for(act_on, url) {
        Some(SiteMode::Look) if !method.eq_ignore_ascii_case("GET") => RequestVerdict::BlockLook,
        Some(SiteMode::AskRisky) if unattended && request_is_risky(method, url) => {
            RequestVerdict::BlockRisky
        }
        _ => RequestVerdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(origin: &str, mode: SiteMode) -> ActOnSite {
        ActOnSite {
            origin: origin.into(),
            mode,
        }
    }

    #[test]
    fn risky_controls_are_found_by_name() {
        for name in [
            "Delete",
            "Remove item",
            "Cancel order",
            "Refund",
            "Void invoice",
            "Pay now",
            "Send",
            "Notify customer",
            "Email receipt",
            "Message",
            "Reply",
            "Change password",
            "Add user",
            "Roles",
            "Permissions",
            "DeleteAccount",
            "Deleting…",
            "e-mail",
        ] {
            assert!(control_is_risky("button", name), "{name}");
        }
    }

    #[test]
    fn plain_controls_are_not_risky() {
        for name in ["Save", "Next", "Open", "Search", "Sentence case", "Close"] {
            assert!(!control_is_risky("button", name), "{name}");
        }
    }

    #[test]
    fn an_unnamed_control_is_risky() {
        assert!(control_is_risky("button", ""));
        assert!(control_is_risky("button", "  "));
    }

    #[test]
    fn requests_are_risky_by_method_and_path() {
        assert!(request_is_risky("DELETE", "https://a.test/items/1"));
        assert!(request_is_risky("post", "https://a.test/orders/1/refund"));
        assert!(request_is_risky("PUT", "https://a.test/api/users/4"));
        assert!(!request_is_risky("POST", "https://a.test/save"));
        assert!(!request_is_risky("GET", "https://a.test/delete-me"));
        assert!(request_is_risky("POST", "not a url"));
    }

    #[test]
    fn origins_are_exact() {
        assert_eq!(
            origin_of("https://A.test:443/x?y#z").as_deref(),
            Some("https://a.test")
        );
        assert_eq!(
            origin_of("http://a.test:8080/").as_deref(),
            Some("http://a.test:8080")
        );
        assert_eq!(origin_of("file:///x"), None);
        let list = [site("https://a.test", SiteMode::Any)];
        assert_eq!(mode_for(&list, "https://a.test/p"), Some(SiteMode::Any));
        assert_eq!(mode_for(&list, "http://a.test/p"), None);
        assert_eq!(mode_for(&list, "https://a.test:8443/p"), None);
        assert_eq!(mode_for(&list, "https://sub.a.test/p"), None);
    }

    #[test]
    fn a_session_for_an_ungranted_origin_is_not_loaded() {
        let act_on = [site("https://a.test", SiteMode::AskRisky)];
        let sessions = [
            SavedSession {
                origin: "https://a.test".into(),
                storage_state: serde_json::json!({}),
            },
            SavedSession {
                origin: "https://b.test".into(),
                storage_state: serde_json::json!({}),
            },
        ];
        let kept = granted_sessions(&act_on, &sessions);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].origin, "https://a.test");
    }

    #[test]
    fn grants_decide_each_action() {
        let url = "https://a.test/page";
        let g = |mode, name, attended| {
            decide(
                &[site("https://a.test", mode)],
                url,
                true,
                true,
                "button",
                name,
                attended,
            )
        };
        assert_eq!(
            g(SiteMode::Any, "Delete", false),
            ActionGate::Proceed { look: false }
        );
        assert_eq!(
            g(SiteMode::Look, "Delete", false),
            ActionGate::Proceed { look: true }
        );
        assert_eq!(
            g(SiteMode::AskRisky, "Save", false),
            ActionGate::Proceed { look: false }
        );
        assert_eq!(
            g(SiteMode::AskRisky, "Delete", false),
            ActionGate::NeedsApproval
        );
        assert_eq!(g(SiteMode::AskRisky, "Delete", true), ActionGate::Ask);
        // Elsewhere: ask as before, refuse with nobody there.
        assert_eq!(
            decide(&[], url, true, true, "button", "Save", true),
            ActionGate::Ask
        );
        assert_eq!(
            decide(&[], url, true, true, "button", "Save", false),
            ActionGate::Refuse
        );
        // Lane A is unchanged.
        assert_eq!(
            decide(
                &[],
                "http://127.0.0.1/",
                false,
                true,
                "button",
                "Delete",
                false
            ),
            ActionGate::Proceed { look: false }
        );
        // Typing sends nothing and is not judged by name.
        assert_eq!(
            decide(
                &[site("https://a.test", SiteMode::AskRisky)],
                url,
                true,
                false,
                "textbox",
                "Message",
                false
            ),
            ActionGate::Proceed { look: false }
        );
    }

    #[test]
    fn requests_are_judged_by_mode() {
        let look = [site("https://a.test", SiteMode::Look)];
        assert_eq!(
            judge_request(&look, false, "POST", "https://a.test/save"),
            RequestVerdict::BlockLook
        );
        assert_eq!(
            judge_request(&look, false, "GET", "https://a.test/"),
            RequestVerdict::Allow
        );
        assert_eq!(
            judge_request(&look, false, "POST", "https://other.test/"),
            RequestVerdict::Allow
        );
        let ask = [site("https://a.test", SiteMode::AskRisky)];
        assert_eq!(
            judge_request(&ask, true, "DELETE", "https://a.test/x"),
            RequestVerdict::BlockRisky
        );
        assert_eq!(
            judge_request(&ask, true, "POST", "https://a.test/save"),
            RequestVerdict::Allow
        );
        assert_eq!(
            judge_request(&ask, false, "DELETE", "https://a.test/x"),
            RequestVerdict::Allow
        );
        let any = [site("https://a.test", SiteMode::Any)];
        assert_eq!(
            judge_request(&any, true, "DELETE", "https://a.test/x"),
            RequestVerdict::Allow
        );
    }
}
