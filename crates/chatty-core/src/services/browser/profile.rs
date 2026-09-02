//! Browser profiles and the navigation policy each one carries.
//!
//! A session is created against a named profile. The profile decides the
//! user-data directory (cookie jar) and which URLs the agent may navigate to.
//!
//! Note the inversion versus `fetch_tool`: that tool blocks loopback as an SSRF
//! defence, because it fetches URLs the model chose on the open internet. Lane A
//! is the mirror image — loopback and workspace-local files are the *only* things
//! allowed, because the point is reviewing what we just built.

use std::path::{Path, PathBuf};

use super::error::BrowserError;

/// Which profile a session runs under.
///
/// Lane A uses `Ephemeral`; Lane B's `Persistent` variant arrives with AGE-157.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserProfile {
    /// A throwaway user-data directory, wiped on teardown. No credentials.
    Ephemeral,
    /// A named directory under app data that survives restarts.
    #[allow(dead_code)]
    Persistent { name: String },
}

impl BrowserProfile {
    /// Short label used in logs and error messages.
    pub fn label(&self) -> &str {
        match self {
            BrowserProfile::Ephemeral => "ephemeral",
            BrowserProfile::Persistent { name } => name,
        }
    }
}

/// What a profile is allowed to navigate to.
#[derive(Clone, Debug)]
pub enum NavigationPolicy {
    /// Lane A: loopback HTTP(S) plus `file://` under the workspace. Nothing else.
    LocalOnly { workspace: Option<PathBuf> },
    /// Lane B (AGE-158): a per-task origin allowlist. Not constructed yet.
    #[allow(dead_code)]
    Allowlist { origins: Vec<String> },
}

impl NavigationPolicy {
    /// Lane A policy anchored at the configured workspace directory.
    pub fn local_only(workspace: Option<PathBuf>) -> Self {
        NavigationPolicy::LocalOnly { workspace }
    }

    /// Check a single URL against the policy.
    ///
    /// Returns `NavigationRefused` naming the policy, never a bare bool — the
    /// agent needs to know *why* so it does not retry the same thing.
    pub fn check(&self, url: &str) -> Result<(), BrowserError> {
        match self {
            NavigationPolicy::LocalOnly { workspace } => {
                check_local_only(url, workspace.as_deref())
            }
            NavigationPolicy::Allowlist { origins } => check_allowlist(url, origins),
        }
    }

    /// Check every hop of a redirect chain. A page cannot bounce the agent
    /// somewhere the policy would have refused up front.
    pub fn check_chain(&self, urls: &[String]) -> Result<(), BrowserError> {
        for url in urls {
            self.check(url)?;
        }
        Ok(())
    }
}

/// Hostnames that count as loopback for Lane A.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return true;
    }
    // RFC 6761 reserves the whole `.localhost` TLD for loopback.
    if let Some(prefix) = host.strip_suffix(".localhost")
        && !prefix.is_empty()
    {
        return true;
    }
    // The rest of 127.0.0.0/8 is loopback too (dev servers sometimes bind it).
    if let Ok(std::net::IpAddr::V4(ip)) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    false
}

fn check_local_only(url: &str, workspace: Option<&Path>) -> Result<(), BrowserError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| BrowserError::NavigationRefused(format!("invalid URL {url}: {e}")))?;

    match parsed.scheme() {
        "http" | "https" => {
            let host = parsed
                .host_str()
                .ok_or_else(|| BrowserError::NavigationRefused(format!("{url} has no host")))?;
            if is_loopback_host(host) {
                Ok(())
            } else {
                Err(BrowserError::NavigationRefused(format!(
                    "{host} is not loopback; this browser profile only allows \
                     localhost and file:// URLs under the workspace"
                )))
            }
        }
        "file" => {
            let workspace = workspace.ok_or(BrowserError::NoWorkspace)?;
            let path = parsed.to_file_path().map_err(|_| {
                BrowserError::NavigationRefused(format!("{url} is not a usable file path"))
            })?;
            if path_is_within(&path, workspace) {
                Ok(())
            } else {
                Err(BrowserError::NavigationRefused(format!(
                    "{} is outside the workspace directory {}",
                    path.display(),
                    workspace.display()
                )))
            }
        }
        other => Err(BrowserError::NavigationRefused(format!(
            "scheme {other}: is not allowed; this browser profile only allows \
             http(s) to localhost and file:// URLs under the workspace"
        ))),
    }
}

fn check_allowlist(url: &str, origins: &[String]) -> Result<(), BrowserError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| BrowserError::NavigationRefused(format!("invalid URL {url}: {e}")))?;
    let origin = parsed.origin().ascii_serialization();
    if origins.iter().any(|o| o == &origin) {
        Ok(())
    } else {
        Err(BrowserError::NavigationRefused(format!(
            "{origin} is not in this task's allowlist"
        )))
    }
}

/// Containment check that resolves `..` without requiring the path to exist.
///
/// `Path::starts_with` alone is not enough: `/ws/../etc/passwd` starts with
/// nothing useful until the traversal is folded out.
fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = normalize(path);
    let root = normalize(root);
    path.starts_with(&root)
}

fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane_a() -> NavigationPolicy {
        NavigationPolicy::local_only(Some(PathBuf::from("/ws")))
    }

    #[test]
    fn allows_loopback_variants() {
        let policy = lane_a();
        for url in [
            "http://localhost:3000/",
            "http://localhost/index.html",
            "https://localhost:8443/app",
            "http://127.0.0.1:5173/",
            "http://127.0.0.2:5173/",
            "http://[::1]:3000/",
            "http://app.localhost:3000/",
        ] {
            assert!(policy.check(url).is_ok(), "{url} should be allowed");
        }
    }

    #[test]
    fn refuses_public_hosts() {
        let policy = lane_a();
        for url in [
            "http://example.com/",
            "https://example.com/",
            "http://192.168.1.10/",
            "http://169.254.169.254/latest/meta-data/",
            "http://notlocalhost/",
            "http://localhost.example.com/",
        ] {
            assert!(
                matches!(policy.check(url), Err(BrowserError::NavigationRefused(_))),
                "{url} should be refused"
            );
        }
    }

    #[test]
    fn refuses_non_http_schemes() {
        let policy = lane_a();
        for url in [
            "ftp://localhost/x",
            "data:text/html,hi",
            "chrome://settings",
        ] {
            assert!(
                matches!(policy.check(url), Err(BrowserError::NavigationRefused(_))),
                "{url} should be refused"
            );
        }
    }

    #[test]
    fn allows_file_urls_inside_workspace() {
        let policy = lane_a();
        assert!(policy.check("file:///ws/dist/index.html").is_ok());
        assert!(policy.check("file:///ws/index.html").is_ok());
    }

    #[test]
    fn refuses_file_urls_outside_workspace() {
        let policy = lane_a();
        for url in ["file:///etc/passwd", "file:///ws2/index.html"] {
            assert!(
                matches!(policy.check(url), Err(BrowserError::NavigationRefused(_))),
                "{url} should be refused"
            );
        }
    }

    #[test]
    fn refuses_file_url_traversal_out_of_workspace() {
        let policy = lane_a();
        let result = policy.check("file:///ws/../etc/passwd");
        assert!(
            matches!(result, Err(BrowserError::NavigationRefused(_))),
            "traversal out of the workspace must be refused, got {result:?}"
        );
    }

    #[test]
    fn file_urls_need_a_workspace() {
        let policy = NavigationPolicy::local_only(None);
        assert!(matches!(
            policy.check("file:///ws/index.html"),
            Err(BrowserError::NoWorkspace)
        ));
    }

    #[test]
    fn redirect_chain_is_checked_hop_by_hop() {
        let policy = lane_a();
        let ok = vec![
            "http://localhost:3000/".to_string(),
            "http://localhost:3000/login".to_string(),
        ];
        assert!(policy.check_chain(&ok).is_ok());

        let escapes = vec![
            "http://localhost:3000/".to_string(),
            "http://evil.example.com/".to_string(),
        ];
        assert!(matches!(
            policy.check_chain(&escapes),
            Err(BrowserError::NavigationRefused(_))
        ));
    }

    #[test]
    fn allowlist_matches_on_origin() {
        let policy = NavigationPolicy::Allowlist {
            origins: vec!["https://example.com".to_string()],
        };
        assert!(policy.check("https://example.com/page").is_ok());
        assert!(policy.check("https://other.com/page").is_err());
        // Scheme and port are part of the origin.
        assert!(policy.check("http://example.com/page").is_err());
    }

    #[test]
    fn profile_labels() {
        assert_eq!(BrowserProfile::Ephemeral.label(), "ephemeral");
        assert_eq!(
            BrowserProfile::Persistent {
                name: "default".into()
            }
            .label(),
            "default"
        );
    }
}
