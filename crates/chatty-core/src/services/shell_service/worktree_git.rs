//! Git inside a sandboxed shell whose workspace is a linked worktree
//! (AGE-757).
//!
//! A worker runs in `<repo>/.chatty/worktrees/<name>` (see
//! `services::worker_tree`). That directory's `.git` is a file naming the
//! real git directory, `<repo>/.git/worktrees/<name>`, and through its
//! `commondir` the repository's shared `.git`: objects, refs, config, hooks.
//! Both live outside the worktree, so a sandbox that binds only the
//! workspace leaves shell `git` with "not a git repository", and a model
//! that sees that runs `git init` over its worktree and loses the branch
//! the leader will be shown.
//!
//! # What the sandbox gets
//!
//! Enough for `git status`, `diff`, `log`, `add` and `commit` on the
//! worker's own branch, and nothing that lets it change how git behaves for
//! the host, which runs git in the same tree after the turn (the runner's
//! commit and evidence, `worker_tree::collect`):
//!
//! - the common git directory **read-only**: `config` and `hooks` are code
//!   the host's git runs, and other branches are not the worker's;
//! - read-write over that: `objects` (every commit writes blobs), the
//!   worktree's own git directory (its `HEAD`, `index`, reflog), and the
//!   directory holding its branch ref and that ref's reflog (for a worker,
//!   `refs/heads/sub-agent/`, shared with its sibling workers' branches);
//! - read-only again over those: the pointers host git follows out of the
//!   worktree, which a rewrite would aim at a repository with a hostile
//!   config: the worktree's `.git` file, and the git directory's `commondir`
//!   and `gitdir`. A bind mount cannot be removed or renamed over, so these
//!   are also what stops `rm .git && git init`.
//!
//! The alternative the issue offered, a shell `git` that fails with a
//! pointer to the `git_add`/`git_commit` tools, was not taken: models use
//! shell git for `status` and `diff` whatever the preamble says, and a
//! failure is exactly what started the `git init` in the first place.
//!
//! [`GIT_INIT_GUARD`] refuses `git init` in the worktree with a message
//! that says why. The read-only binds are the real guard; the message
//! stops the model from spending turns working around them.

use std::path::{Path, PathBuf};

/// The git paths outside a linked worktree its sandbox binds, in bind
/// order (each later group shadows the earlier ones).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct WorktreeGit {
    /// Bound read-only: the repository's common git directory.
    pub read_only: Vec<PathBuf>,
    /// Bound read-write over `read_only`.
    pub read_write: Vec<PathBuf>,
    /// Bound read-only over `read_write` and the workspace: the pointers
    /// out of the worktree.
    pub pinned: Vec<PathBuf>,
}

/// The binds `workspace`'s git needs when it is a linked worktree whose
/// git directory lies outside it; `None` for anything else (a plain
/// repository, a submodule, not a repository at all), which keeps the
/// sandbox as it was.
///
/// Every path exists (bubblewrap refuses to bind a missing one, and a
/// sandbox that fails to start falls back to no sandbox at all). The git
/// paths are canonical, as git writes them into the `.git` file.
pub(super) fn worktree_git(workspace: &Path) -> Option<WorktreeGit> {
    // The `.git` file is pinned where the sandbox binds the workspace: at
    // the path it was given.
    let dot_git = workspace.join(".git");
    let workspace = workspace.canonicalize().ok()?;
    let git_dir = git_dir_of(&dot_git)?;
    // Only a linked worktree's git directory has a `commondir`; a
    // submodule's is the whole repository, which is not the worker's to
    // write.
    let commondir = git_dir.join("commondir");
    let common = std::fs::read_to_string(&commondir).ok()?;
    let common = git_dir.join(common.trim()).canonicalize().ok()?;
    if common.starts_with(&workspace) {
        return None;
    }

    let mut read_write = vec![common.join("objects"), git_dir.clone()];
    if let Some(branch) = branch_ref(&git_dir)
        && let Some(parent) = Path::new(&branch).parent()
    {
        read_write.push(common.join(parent));
        read_write.push(common.join("logs").join(parent));
    }
    read_write.retain(|p| p.is_dir());

    let pinned = [dot_git, commondir, git_dir.join("gitdir")]
        .into_iter()
        .filter(|p| p.is_file())
        .collect();

    Some(WorktreeGit {
        read_only: vec![common],
        read_write,
        pinned,
    })
}

/// The git directory a `.git` file names, canonical. `None` when `.git` is
/// a directory, missing, or names a directory that does not exist.
fn git_dir_of(dot_git: &Path) -> Option<PathBuf> {
    if !dot_git.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(dot_git).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    // Relative to the worktree (`worktree.useRelativePaths`).
    dot_git.parent()?.join(target).canonicalize().ok()
}

/// The branch the worktree has checked out (`refs/heads/…`), or `None`
/// for a detached `HEAD`.
fn branch_ref(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let branch = head.trim().strip_prefix("ref:")?.trim();
    // A ref name git accepted never climbs out of `refs/`, but the file is
    // the worker's to write between sandboxes.
    (branch.starts_with("refs/") && !branch.split('/').any(|c| c == ".." || c.is_empty()))
        .then(|| branch.to_string())
}

/// The commit identity the host's git uses in `workspace`, as the
/// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` pairs a sandboxed shell needs: the
/// sandbox binds neither `HOME` nor `/etc`, so without them shell `git
/// commit` stops at "Author identity unknown", and the fix a model reaches
/// for, `git config user.email`, writes the read-only shared config.
/// Empty where the host has no identity either.
pub(super) fn identity(workspace: &Path) -> Vec<(String, String)> {
    let get = |key: &str| {
        let out = std::process::Command::new("git")
            .args(["config", "--get", key])
            .current_dir(workspace)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        let value = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && !value.is_empty()).then_some(value)
    };
    let mut vars = Vec::new();
    for (key, who) in [("user.name", "NAME"), ("user.email", "EMAIL")] {
        if let Some(value) = get(key) {
            vars.push((format!("GIT_AUTHOR_{who}"), value.clone()));
            vars.push((format!("GIT_COMMITTER_{who}"), value));
        }
    }
    vars
}

/// Shell init for a linked-worktree workspace: a `git` function that
/// refuses `git init` anywhere under `$__chatty_ws` and runs every other
/// git command unchanged. Plain bash 3.2, like the rest of the init.
///
/// It only looks at the directory the command runs in (`$PWD`, or `-C`),
/// so `cd /tmp && git init scratch` still works.
pub(super) const GIT_INIT_GUARD: &str = r#"git() {
local __a __d=$PWD __skip=
for __a in "$@"; do
if [ -n "$__skip" ]; then [ "$__skip" = C ] && case $__a in /*) __d=$__a ;; *) __d=$__d/$__a ;; esac; __skip=; continue; fi
case $__a in
-C) __skip=C ;;
-c|--git-dir|--work-tree|--namespace|--exec-path|--config-env) __skip=1 ;;
-*) ;;
init)
case $__d/ in "$__chatty_ws"/*)
printf '%s\n' "chatty: refused \`git init\`: $__chatty_ws is already a git worktree with its own branch, and a new repository here would hide that branch's history from whoever reviews it. Record changes with \`git add\` and \`git commit\` (or the git_add and git_commit tools)." >&2
return 1 ;;
esac
break ;;
*) break ;;
esac
done
command git "$@"
}
"#;

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with a worker's worktree on `sub-agent/w`, as
    /// `worker_tree::create` lays it out.
    pub(crate) fn repo_with_worktree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f.txt"), "hi\n").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "sub-agent/w",
                ".chatty/worktrees/w",
            ],
        );
        let tree = repo.join(".chatty/worktrees/w");
        (dir, tree)
    }

    #[test]
    fn a_worktree_binds_its_git_dir_objects_and_branch_but_pins_the_pointers() {
        let (dir, tree) = repo_with_worktree();
        let common = dir.path().canonicalize().unwrap().join(".git");
        let binds = worktree_git(&tree).expect("a linked worktree");

        assert_eq!(binds.read_only, vec![common.clone()]);
        assert_eq!(
            binds.read_write,
            vec![
                common.join("objects"),
                common.join("worktrees/w"),
                common.join("refs/heads/sub-agent"),
                common.join("logs/refs/heads/sub-agent"),
            ]
        );
        assert_eq!(
            binds.pinned,
            vec![
                tree.join(".git"),
                common.join("worktrees/w/commondir"),
                common.join("worktrees/w/gitdir"),
            ]
        );
    }

    #[test]
    fn a_plain_repository_or_a_plain_directory_binds_nothing_extra() {
        let (dir, _) = repo_with_worktree();
        assert_eq!(worktree_git(dir.path()), None);
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(worktree_git(plain.path()), None);
    }

    #[test]
    fn a_head_that_climbs_out_of_refs_is_not_bound() {
        let (dir, tree) = repo_with_worktree();
        let git_dir = dir.path().canonicalize().unwrap().join(".git/worktrees/w");
        std::fs::write(git_dir.join("HEAD"), "ref: refs/../../etc\n").unwrap();
        let binds = worktree_git(&tree).unwrap();
        assert_eq!(binds.read_write.len(), 2, "{:?}", binds.read_write);
    }
}
