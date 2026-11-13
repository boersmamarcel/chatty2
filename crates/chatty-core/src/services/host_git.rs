//! Git that chatty runs on the host in a workspace the agent's sandbox can
//! write (AGE-1288, AGE-1362).
//!
//! The sandboxed shell can write most of `.git`. Making `config` and
//! `hooks` read-only was not enough: a sandboxed command could write
//! `.git/commondir` and point git at a directory it controls, or (on macOS)
//! move `.git` away and make a new one. Host git would then read that
//! config and run its filter or diff driver outside the sandbox.
//!
//! So every host git call goes through [`host_git`]:
//!
//! - in a plain repository, `GIT_DIR`, `GIT_COMMON_DIR` and `GIT_WORK_TREE`
//!   are pinned to the workspace's own `.git`, so a planted `commondir` is
//!   never followed; a `.git` that is a symlink, or a plain `.git` with a
//!   `commondir` file, is refused;
//! - the repository's own config is read first (reading runs nothing),
//!   and git is refused when it defines a command git could run: filter,
//!   diff or merge drivers, includes, `core.sshCommand`, `core.askPass`,
//!   `core.gitProxy`, credential helpers;
//! - [`HOST_GIT_HARDENING`] turns off hooks, fsmonitor and the pager
//!   whatever the config says.

use std::path::{Path, PathBuf};

use crate::services::git_service::HOST_GIT_HARDENING;

/// Repository config keys that name a command git may run on the host.
/// Hooks, fsmonitor and the pager are overridden by [`HOST_GIT_HARDENING`]
/// instead, so a repository that sets them (husky's `core.hooksPath`) still
/// works.
const RUNNABLE_CONFIG: &str = r"^(filter|diff|merge)\..*\.(clean|smudge|process|textconv|command|driver)$|^include\.|^includeif\.|^core\.(sshcommand|askpass|gitproxy|alternaterefscommand)$|^credential\..*helper$|^uploadpack\.packobjectshook$";

/// The environment pins for `root`: `Some` for a plain repository, `None`
/// for a linked worktree (its `.git` is a file; the sandbox binds its git
/// directory read-only, AGE-757), `Err` for a layout host git must not use.
fn pins(root: &Path) -> Result<Option<PathBuf>, String> {
    let dot = root.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dot) else {
        return Ok(None);
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; host git will not follow it",
            dot.display()
        ));
    }
    if meta.is_dir() {
        if std::fs::symlink_metadata(dot.join("commondir")).is_ok() {
            return Err(format!(
                "{} has a commondir file; host git will not follow it",
                dot.display()
            ));
        }
        return Ok(Some(dot));
    }
    Ok(None)
}

/// A `git` command for the workspace at `root`, pinned and hardened, after
/// the repository's config passed [`RUNNABLE_CONFIG`]. Add the subcommand
/// and its arguments.
pub(crate) async fn host_git(root: &Path) -> Result<tokio::process::Command, String> {
    let pinned = pins(root)?;
    let base = || {
        let mut command = tokio::process::Command::new("git");
        command
            .args(HOST_GIT_HARDENING)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .current_dir(root)
            .kill_on_drop(true);
        if let Some(dot) = &pinned {
            command
                .env("GIT_DIR", dot)
                .env("GIT_COMMON_DIR", dot)
                .env("GIT_WORK_TREE", root);
        }
        command
    };

    // `git config --get-regexp` exits 1 when nothing matches: that is the
    // pass. Any match is a refusal, naming the keys.
    let output = base()
        .args([
            "config",
            "--local",
            "--name-only",
            "--get-regexp",
            RUNNABLE_CONFIG,
        ])
        .output()
        .await
        .map_err(|e| format!("failed to run git: {e}"))?;
    let names = String::from_utf8_lossy(&output.stdout);
    let names: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
    if output.status.success() && !names.is_empty() {
        return Err(format!(
            "the repository's git config defines commands host git would run ({}); \
             refusing to run git on the host in this workspace",
            names.join(", ")
        ));
    }
    Ok(base())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "t@example.test"]);
        git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "a"]);
        dir
    }

    /// AGE-1362: a planted `.git/commondir` pointing at a directory with a
    /// filter driver is refused; the filter never runs.
    #[tokio::test]
    async fn a_planted_commondir_is_refused() {
        let dir = repo();
        let root = dir.path();
        let marker = root.join("PWNED");
        let evil = root.join("evil");
        std::fs::create_dir_all(&evil).unwrap();
        for sub in ["objects", "refs"] {
            let status = std::process::Command::new("cp")
                .args(["-r"])
                .arg(root.join(".git").join(sub))
                .arg(evil.join(sub))
                .status()
                .unwrap();
            assert!(status.success());
        }
        std::fs::copy(root.join(".git/HEAD"), evil.join("HEAD")).unwrap();
        std::fs::write(
            evil.join("config"),
            format!(
                "[core]\n\trepositoryformatversion = 0\n[filter \"x\"]\n\tclean = \"touch '{}' && cat\"\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::write(root.join(".git/commondir"), "../evil\n").unwrap();
        std::fs::write(root.join(".gitattributes"), "* filter=x\n").unwrap();
        std::fs::write(root.join("a.txt"), "changed").unwrap();

        assert!(host_git(root).await.is_err());
        assert!(!marker.exists());
    }

    /// AGE-1362: a filter driver in the repository's own config is refused;
    /// a plain repository (or one with husky's hooksPath) is not.
    #[tokio::test]
    async fn runnable_config_is_refused_and_plain_config_passes() {
        let dir = repo();
        let root = dir.path();
        git(root, &["config", "core.hooksPath", ".husky"]);
        let mut status = host_git(root).await.expect("a plain repository passes");
        let out = status
            .args(["status", "--porcelain=v1"])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        git(root, &["config", "filter.x.clean", "touch nope"]);
        let refused = host_git(root)
            .await
            .err()
            .expect("a filter driver is refused");
        assert!(refused.contains("filter.x.clean"), "{refused}");
    }

    /// A `.git` that is a symlink is refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_git_dir_is_refused() {
        let real = repo();
        let ws = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(real.path().join(".git"), ws.path().join(".git")).unwrap();
        assert!(host_git(ws.path()).await.is_err());
    }
}
