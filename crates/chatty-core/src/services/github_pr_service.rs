//! Resolve the GitHub pull request that belongs to a workspace's current branch.
//!
//! # What lives here
//!
//! - `parse_github_remote` — `owner/repo` from the https / ssh forms of a
//!   GitHub remote URL. Non-GitHub remotes return `None` so GitLab and
//!   friends can be added later without touching the callers.
//! - `resolve_pull_request` — the whole lookup: is this a git repo, does
//!   `origin` point at GitHub, what branch are we on, and which PR has that
//!   branch as its head.
//! - `PullRequestSummary` / `CheckRun` — the UI-facing shape.
//!
//! # Lookup order
//!
//! The `gh` CLI is preferred: it is already authenticated, so it works for
//! private repositories. When `gh` is missing, logged out, or the branch has
//! no PR, the REST API is tried instead — anonymously, or with `GITHUB_TOKEN`
//! / `GH_TOKEN` when one is exported. No token is ever persisted or logged.
//!
//! Every failure path resolves to `None`: the caller renders no bar, and
//! nothing here is allowed to surface an error into the chat.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::http_client;

/// How long a single `gh` invocation or REST call may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A GitHub repository coordinate parsed out of a remote URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepo {
    pub owner: String,
    pub repo: String,
}

impl GitHubRepo {
    /// `owner/repo`, the form GitHub URLs and the REST API use.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Lifecycle state of the pull request, which drives the bar's styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

/// Rolled-up state of a check run, or of all of them together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckState {
    Pending,
    Passing,
    Failing,
}

/// One CI check attached to the pull request's head commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub state: CheckState,
    /// Details page for this check, when the provider reports one.
    pub url: Option<String>,
}

/// Everything the PR status bar renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub url: String,
    /// `owner/repo`.
    pub repo: String,
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub checks: Vec<CheckRun>,
}

impl PullRequestSummary {
    /// Just the repository name, without the owner — what the bar shows.
    pub fn repo_name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }

    /// Rolled-up CI state, or `None` when the PR has no checks at all.
    pub fn checks_state(&self) -> Option<CheckState> {
        aggregate_check_state(&self.checks)
    }
}

/// Worst-first roll-up: one failure fails the lot, anything unfinished is
/// pending, otherwise it passes.
fn aggregate_check_state(checks: &[CheckRun]) -> Option<CheckState> {
    if checks.is_empty() {
        return None;
    }
    if checks.iter().any(|c| c.state == CheckState::Failing) {
        return Some(CheckState::Failing);
    }
    if checks.iter().any(|c| c.state == CheckState::Pending) {
        return Some(CheckState::Pending);
    }
    Some(CheckState::Passing)
}

/// Parse `owner/repo` out of a GitHub remote URL.
///
/// Handles the https, `git@`-scp, and `ssh://` forms, with or without the
/// trailing `.git`. Anything that is not a github.com host returns `None`.
pub fn parse_github_remote(url: &str) -> Option<GitHubRepo> {
    let url = url.trim();

    // scp-style: git@github.com:owner/repo.git
    let path = if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        if !is_github_host(host) {
            return None;
        }
        path
    } else {
        // URL-style: https://, http://, ssh://, git://
        let rest = url
            .split_once("://")
            .map(|(_scheme, rest)| rest)
            .unwrap_or(url);
        // Strip any userinfo (ssh://git@github.com/...)
        let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        // Drop an explicit port, if any.
        let host = host.split_once(':').map(|(h, _)| h).unwrap_or(host);
        if !is_github_host(host) {
            return None;
        }
        path
    };

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    // Reject nested paths — GitHub repos are exactly two segments.
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }

    Some(GitHubRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

fn is_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com")
}

/// Resolve the pull request whose head is the workspace's current branch.
///
/// Returns `None` — never an error — when the workspace is not a git
/// repository, `origin` is not GitHub, the branch is detached, no PR exists,
/// or the lookup fails for any other reason.
pub async fn resolve_pull_request(workspace_root: &Path) -> Option<PullRequestSummary> {
    if !is_git_repository(workspace_root).await {
        return None;
    }

    let remote = run_git(workspace_root, &["remote", "get-url", "origin"]).await?;
    let repo = parse_github_remote(&remote)?;

    let branch = run_git(workspace_root, &["branch", "--show-current"]).await?;
    if branch.is_empty() {
        debug!("Detached HEAD, no branch to resolve a pull request for");
        return None;
    }

    if let Some(summary) = resolve_via_gh(workspace_root, &repo, &branch).await {
        return Some(summary);
    }

    resolve_via_rest(&repo, &branch).await
}

async fn is_git_repository(workspace_root: &Path) -> bool {
    run_git(workspace_root, &["rev-parse", "--git-dir"])
        .await
        .is_some()
}

/// Run a read-only git command in the workspace, returning trimmed stdout.
async fn run_git(workspace_root: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        debug!(args = ?args, "git command failed while resolving pull request");
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// gh CLI
// ---------------------------------------------------------------------------

/// Fields `gh pr view` is asked for; kept next to the parser that reads them.
const GH_JSON_FIELDS: &str = "number,title,state,isDraft,url,additions,deletions,statusCheckRollup";

async fn resolve_via_gh(
    workspace_root: &Path,
    repo: &GitHubRepo,
    branch: &str,
) -> Option<PullRequestSummary> {
    let command = tokio::process::Command::new("gh")
        .args(["pr", "view", "--json", GH_JSON_FIELDS])
        .current_dir(workspace_root)
        .output();

    let output = tokio::time::timeout(REQUEST_TIMEOUT, command)
        .await
        .ok()?
        .ok()?;

    if !output.status.success() {
        debug!("gh pr view did not resolve a pull request; falling back to REST");
        return None;
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    parse_gh_pr(&value, &repo.slug(), branch)
}

/// Build a summary from `gh pr view --json …` output.
fn parse_gh_pr(value: &serde_json::Value, repo: &str, branch: &str) -> Option<PullRequestSummary> {
    let number = value.get("number")?.as_u64()?;
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ if value.get("isDraft").and_then(|v| v.as_bool()) == Some(true) => PrState::Draft,
        _ => PrState::Open,
    };

    let checks = value
        .get("statusCheckRollup")
        .and_then(|v| v.as_array())
        .map(|entries| entries.iter().filter_map(parse_gh_check).collect())
        .unwrap_or_default();

    Some(PullRequestSummary {
        number,
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        state,
        url: value
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}")),
        repo: repo.to_string(),
        branch: branch.to_string(),
        additions: value.get("additions").and_then(|v| v.as_u64()).unwrap_or(0),
        deletions: value.get("deletions").and_then(|v| v.as_u64()).unwrap_or(0),
        checks,
    })
}

/// A rollup entry is either a CheckRun (Actions) or a StatusContext (older
/// commit statuses); the two spell their fields differently.
fn parse_gh_check(entry: &serde_json::Value) -> Option<CheckRun> {
    let str_field = |key: &str| entry.get(key).and_then(|v| v.as_str());

    if let Some(name) = str_field("name") {
        let state = match str_field("status") {
            Some("COMPLETED") => conclusion_to_state(str_field("conclusion")),
            _ => CheckState::Pending,
        };
        return Some(CheckRun {
            name: name.to_string(),
            state,
            url: str_field("detailsUrl").map(|s| s.to_string()),
        });
    }

    // StatusContext
    let context = str_field("context")?;
    let state = match str_field("state") {
        Some("SUCCESS") => CheckState::Passing,
        Some("PENDING") | Some("EXPECTED") => CheckState::Pending,
        _ => CheckState::Failing,
    };
    Some(CheckRun {
        name: context.to_string(),
        state,
        url: str_field("targetUrl").map(|s| s.to_string()),
    })
}

/// Map a completed check's conclusion. Neutral and skipped are not failures.
fn conclusion_to_state(conclusion: Option<&str>) -> CheckState {
    match conclusion.map(|c| c.to_ascii_uppercase()) {
        Some(c) if c == "SUCCESS" || c == "NEUTRAL" || c == "SKIPPED" => CheckState::Passing,
        None => CheckState::Pending,
        _ => CheckState::Failing,
    }
}

// ---------------------------------------------------------------------------
// REST fallback
// ---------------------------------------------------------------------------

async fn resolve_via_rest(repo: &GitHubRepo, branch: &str) -> Option<PullRequestSummary> {
    let client = http_client::default_client(REQUEST_TIMEOUT.as_secs());
    // Optional, never persisted and never logged.
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|t| !t.is_empty());

    let get = |url: String| {
        let mut request = client
            .get(url)
            .header("Accept", "application/vnd.github+json");
        if let Some(token) = token.as_deref() {
            request = request.bearer_auth(token);
        }
        request
    };

    // The list endpoint is the only one that can find a PR by head branch,
    // but it omits additions/deletions — hence the follow-up detail call.
    let slug = repo.slug();
    let list_url = format!(
        "https://api.github.com/repos/{slug}/pulls?state=all&per_page=1&head={}:{branch}",
        repo.owner
    );
    let listed: Vec<serde_json::Value> = send_json(get(list_url)).await?;
    let number = listed.first()?.get("number")?.as_u64()?;

    let detail_url = format!("https://api.github.com/repos/{slug}/pulls/{number}");
    let detail: serde_json::Value = send_json(get(detail_url)).await?;

    let head_sha = detail
        .get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|v| v.as_str());
    let checks = match head_sha {
        Some(sha) => {
            let checks_url =
                format!("https://api.github.com/repos/{slug}/commits/{sha}/check-runs");
            send_json::<serde_json::Value>(get(checks_url))
                .await
                .map(|v| parse_rest_check_runs(&v))
                .unwrap_or_default()
        }
        None => Vec::new(),
    };

    parse_rest_pr(&detail, &slug, branch, checks)
}

async fn send_json<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Option<T> {
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        debug!(status = %response.status(), "GitHub REST lookup failed");
        return None;
    }
    response.json::<T>().await.ok()
}

/// Build a summary from `GET /repos/{owner}/{repo}/pulls/{number}`.
fn parse_rest_pr(
    detail: &serde_json::Value,
    repo: &str,
    branch: &str,
    checks: Vec<CheckRun>,
) -> Option<PullRequestSummary> {
    let number = detail.get("number")?.as_u64()?;
    let merged = detail
        .get("merged_at")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    let state = if merged {
        PrState::Merged
    } else if detail.get("state").and_then(|v| v.as_str()) == Some("closed") {
        PrState::Closed
    } else if detail.get("draft").and_then(|v| v.as_bool()) == Some(true) {
        PrState::Draft
    } else {
        PrState::Open
    };

    Some(PullRequestSummary {
        number,
        title: detail
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        state,
        url: detail
            .get("html_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}")),
        repo: repo.to_string(),
        branch: branch.to_string(),
        additions: detail
            .get("additions")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        deletions: detail
            .get("deletions")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        checks,
    })
}

/// Build check runs from `GET /repos/{owner}/{repo}/commits/{sha}/check-runs`.
fn parse_rest_check_runs(value: &serde_json::Value) -> Vec<CheckRun> {
    value
        .get("check_runs")
        .and_then(|v| v.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|run| {
                    let name = run.get("name")?.as_str()?.to_string();
                    let state = match run.get("status").and_then(|v| v.as_str()) {
                        Some("completed") => {
                            conclusion_to_state(run.get("conclusion").and_then(|v| v.as_str()))
                        }
                        _ => CheckState::Pending,
                    };
                    Some(CheckRun {
                        name,
                        state,
                        url: run
                            .get("html_url")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> Option<GitHubRepo> {
        Some(GitHubRepo {
            owner: owner.to_string(),
            repo: name.to_string(),
        })
    }

    #[test]
    fn parses_https_and_ssh_remote_forms() {
        assert_eq!(
            parse_github_remote("https://github.com/boersmamarcel/chatty2.git"),
            repo("boersmamarcel", "chatty2")
        );
        assert_eq!(
            parse_github_remote("https://github.com/boersmamarcel/chatty2"),
            repo("boersmamarcel", "chatty2")
        );
        assert_eq!(
            parse_github_remote("git@github.com:boersmamarcel/chatty2.git"),
            repo("boersmamarcel", "chatty2")
        );
        assert_eq!(
            parse_github_remote("ssh://git@github.com/boersmamarcel/chatty2.git"),
            repo("boersmamarcel", "chatty2")
        );
        assert_eq!(
            parse_github_remote("  https://github.com/boersmamarcel/chatty2/  "),
            repo("boersmamarcel", "chatty2")
        );
    }

    #[test]
    fn rejects_non_github_remotes() {
        assert_eq!(parse_github_remote("https://gitlab.com/foo/bar.git"), None);
        assert_eq!(parse_github_remote("git@bitbucket.org:foo/bar.git"), None);
        assert_eq!(parse_github_remote("/srv/git/local.git"), None);
        assert_eq!(parse_github_remote("https://github.com/onlyowner"), None);
        // A lookalike host must not be mistaken for github.com.
        assert_eq!(parse_github_remote("https://github.com.evil.io/a/b"), None);
    }

    #[test]
    fn slug_joins_owner_and_repo() {
        assert_eq!(
            GitHubRepo {
                owner: "a".into(),
                repo: "b".into()
            }
            .slug(),
            "a/b"
        );
    }

    #[test]
    fn parses_merged_pr_from_gh_output() {
        let json = serde_json::json!({
            "number": 590,
            "title": "Add animation recording infrastructure",
            "state": "MERGED",
            "isDraft": false,
            "url": "https://github.com/boersmamarcel/chatty2/pull/590",
            "additions": 1757,
            "deletions": 450,
            "statusCheckRollup": []
        });
        let pr = parse_gh_pr(&json, "boersmamarcel/chatty2", "claude/gif").unwrap();
        assert_eq!(pr.number, 590);
        assert_eq!(pr.state, PrState::Merged);
        assert_eq!((pr.additions, pr.deletions), (1757, 450));
        assert_eq!(pr.repo_name(), "chatty2");
        assert_eq!(pr.checks_state(), None);
    }

    #[test]
    fn draft_is_distinguished_from_open() {
        let json = serde_json::json!({ "number": 1, "state": "OPEN", "isDraft": true });
        assert_eq!(
            parse_gh_pr(&json, "o/r", "b").unwrap().state,
            PrState::Draft
        );
        let json = serde_json::json!({ "number": 1, "state": "OPEN", "isDraft": false });
        assert_eq!(parse_gh_pr(&json, "o/r", "b").unwrap().state, PrState::Open);
    }

    #[test]
    fn falls_back_to_a_constructed_url_when_gh_omits_one() {
        let json = serde_json::json!({ "number": 42, "state": "OPEN" });
        assert_eq!(
            parse_gh_pr(&json, "o/r", "b").unwrap().url,
            "https://github.com/o/r/pull/42"
        );
    }

    #[test]
    fn parses_both_rollup_entry_shapes() {
        let json = serde_json::json!({
            "number": 1,
            "state": "OPEN",
            "statusCheckRollup": [
                {
                    "__typename": "CheckRun",
                    "name": "test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "detailsUrl": "https://ci/1"
                },
                {
                    "__typename": "CheckRun",
                    "name": "clippy",
                    "status": "IN_PROGRESS"
                },
                {
                    "__typename": "StatusContext",
                    "context": "legacy",
                    "state": "FAILURE",
                    "targetUrl": "https://ci/2"
                }
            ]
        });
        let pr = parse_gh_pr(&json, "o/r", "b").unwrap();
        assert_eq!(pr.checks.len(), 3);
        assert_eq!(pr.checks[0].state, CheckState::Passing);
        assert_eq!(pr.checks[0].url.as_deref(), Some("https://ci/1"));
        assert_eq!(pr.checks[1].state, CheckState::Pending);
        assert_eq!(pr.checks[2].state, CheckState::Failing);
        assert_eq!(pr.checks[2].name, "legacy");
    }

    #[test]
    fn skipped_and_neutral_conclusions_are_not_failures() {
        assert_eq!(conclusion_to_state(Some("skipped")), CheckState::Passing);
        assert_eq!(conclusion_to_state(Some("NEUTRAL")), CheckState::Passing);
        assert_eq!(conclusion_to_state(Some("FAILURE")), CheckState::Failing);
        assert_eq!(conclusion_to_state(Some("CANCELLED")), CheckState::Failing);
        assert_eq!(conclusion_to_state(None), CheckState::Pending);
    }

    #[test]
    fn check_rollup_is_worst_first() {
        let run = |state| CheckRun {
            name: "c".into(),
            state,
            url: None,
        };
        assert_eq!(aggregate_check_state(&[]), None);
        assert_eq!(
            aggregate_check_state(&[run(CheckState::Passing)]),
            Some(CheckState::Passing)
        );
        assert_eq!(
            aggregate_check_state(&[run(CheckState::Passing), run(CheckState::Pending)]),
            Some(CheckState::Pending)
        );
        assert_eq!(
            aggregate_check_state(&[run(CheckState::Pending), run(CheckState::Failing)]),
            Some(CheckState::Failing)
        );
    }

    #[test]
    fn parses_rest_pull_request_detail() {
        let detail = serde_json::json!({
            "number": 589,
            "title": "Refactor chat engine",
            "state": "open",
            "draft": false,
            "merged_at": null,
            "html_url": "https://github.com/o/r/pull/589",
            "additions": 10,
            "deletions": 2
        });
        let pr = parse_rest_pr(&detail, "o/r", "feature", Vec::new()).unwrap();
        assert_eq!(pr.number, 589);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.branch, "feature");

        let merged = serde_json::json!({
            "number": 1,
            "state": "closed",
            "merged_at": "2026-09-01T00:00:00Z"
        });
        assert_eq!(
            parse_rest_pr(&merged, "o/r", "b", Vec::new())
                .unwrap()
                .state,
            PrState::Merged
        );

        let closed = serde_json::json!({ "number": 1, "state": "closed", "merged_at": null });
        assert_eq!(
            parse_rest_pr(&closed, "o/r", "b", Vec::new())
                .unwrap()
                .state,
            PrState::Closed
        );
    }

    #[test]
    fn parses_rest_check_runs_payload() {
        let json = serde_json::json!({
            "check_runs": [
                { "name": "test", "status": "completed", "conclusion": "success",
                  "html_url": "https://ci/1" },
                { "name": "fmt", "status": "queued" }
            ]
        });
        let checks = parse_rest_check_runs(&json);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].state, CheckState::Passing);
        assert_eq!(checks[1].state, CheckState::Pending);
        assert_eq!(aggregate_check_state(&checks), Some(CheckState::Pending));
    }

    #[tokio::test]
    async fn non_git_directory_resolves_to_no_pull_request() {
        let dir = std::env::temp_dir().join("chatty-pr-service-not-a-repo");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resolve_pull_request(&dir).await.is_none());
    }
}
