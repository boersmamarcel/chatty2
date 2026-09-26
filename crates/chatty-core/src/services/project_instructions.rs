//! Project instructions: the `AGENTS.md` / `CLAUDE.md` files people keep in a
//! repository, and in their home directory, to tell any coding agent how to
//! work there. Read each time an agent is built, so an edit applies to the
//! next conversation or working-directory change.
//!
//! ## Files read, in prompt order
//! 1. Global: `~/.agents/AGENTS.md`, else `~/.claude/CLAUDE.md`
//! 2. Project: in every directory from the git root down to the workspace,
//!    `AGENTS.md`, else `CLAUDE.md`. Outside a git repository only the
//!    workspace itself is read.
//!
//! The nearest file comes last, so it has the final word when two disagree.

use std::path::{Path, PathBuf};

/// File names tried in each project directory; the first that exists is used.
const PROJECT_FILE_NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// Bytes kept from any one file. A repository's CLAUDE.md can run past
/// 100 KB; the prompt is not the place for all of it.
const MAX_FILE_BYTES: usize = 32 * 1024;

/// Bytes of instructions across all files; later files are left out once
/// this is spent.
const MAX_TOTAL_BYTES: usize = 48 * 1024;

/// Global instruction file candidates under `home`, in precedence order.
fn global_candidates(home: &Path) -> [PathBuf; 2] {
    [
        home.join(".agents").join("AGENTS.md"),
        home.join(".claude").join("CLAUDE.md"),
    ]
}

/// Directories from the enclosing git root down to `workspace`, root first.
/// Outside a git repository: the workspace only.
fn project_dirs(workspace: &Path) -> Vec<&Path> {
    let in_repo = workspace.ancestors().any(|d| d.join(".git").exists());
    let mut dirs = Vec::new();
    for dir in workspace.ancestors() {
        dirs.push(dir);
        if !in_repo || dir.join(".git").exists() {
            break;
        }
    }
    dirs.reverse();
    dirs
}

/// The instruction files that apply to `workspace`, in prompt order.
pub fn instruction_files(workspace: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = home
        .and_then(|home| global_candidates(home).into_iter().find(|p| p.is_file()))
        .into_iter()
        .collect();
    for dir in workspace.map(project_dirs).unwrap_or_default() {
        let found = PROJECT_FILE_NAMES
            .iter()
            .map(|name| dir.join(name))
            .find(|p| p.is_file());
        if let Some(file) = found
            && !files.contains(&file)
        {
            files.push(file);
        }
    }
    files
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a char
/// boundary.
fn truncate_at_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The system-prompt section carrying every instruction file for
/// `workspace`, or `None` when there are none.
pub fn instructions_section(workspace: Option<&Path>, home: Option<&Path>) -> Option<String> {
    let mut sections = Vec::new();
    let mut budget = MAX_TOTAL_BYTES;
    let mut left_out = Vec::new();

    for file in instruction_files(workspace, home) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        if budget == 0 {
            left_out.push(file.display().to_string());
            continue;
        }
        let limit = MAX_FILE_BYTES.min(budget);
        let kept = truncate_at_char_boundary(content, limit);
        budget -= kept.len();
        let note = if kept.len() < content.len() {
            format!(
                "\n\n[Truncated: showing the first {} of {} bytes. Read the file for the rest.]",
                kept.len(),
                content.len()
            )
        } else {
            String::new()
        };
        sections.push(format!("### {}\n\n{kept}{note}", file.display()));
    }

    if sections.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Project instructions\n\n\
         The user keeps these instruction files for this workspace (AGENTS.md / CLAUDE.md). \
         Follow them. They are listed from the most general to the most specific; when two \
         disagree, the later one wins. The user's direct requests in this conversation \
         take precedence over them.\n\n",
    );
    out.push_str(&sections.join("\n\n"));
    if !left_out.is_empty() {
        out.push_str(&format!(
            "\n\n[Left out for length: {}. Read them if they matter to the task.]",
            left_out.join(", ")
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn none_when_no_files() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert!(instructions_section(Some(ws.path()), Some(home.path())).is_none());
    }

    #[test]
    fn agents_md_beats_claude_md_in_the_same_directory() {
        let ws = tempfile::tempdir().unwrap();
        write(&ws.path().join("AGENTS.md"), "from agents");
        write(&ws.path().join("CLAUDE.md"), "from claude");

        let section = instructions_section(Some(ws.path()), None).unwrap();

        assert!(section.contains("from agents"));
        assert!(!section.contains("from claude"));
    }

    #[test]
    fn claude_md_is_read_when_there_is_no_agents_md() {
        let ws = tempfile::tempdir().unwrap();
        write(&ws.path().join("CLAUDE.md"), "from claude");

        let section = instructions_section(Some(ws.path()), None).unwrap();

        assert!(section.contains("from claude"));
    }

    /// Global first, then the repository root, then the subfolder the
    /// conversation works in; nothing above the git root.
    #[test]
    fn order_is_global_then_root_to_nearest_and_stops_at_the_git_root() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join(".claude/CLAUDE.md"), "global rule");
        let outer = tempfile::tempdir().unwrap();
        write(&outer.path().join("AGENTS.md"), "above the repo");
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write(&repo.join("AGENTS.md"), "repo rule");
        let sub = repo.join("crates/app");
        write(&sub.join("CLAUDE.md"), "crate rule");

        let section = instructions_section(Some(&sub), Some(home.path())).unwrap();

        let global = section.find("global rule").unwrap();
        let root = section.find("repo rule").unwrap();
        let nearest = section.find("crate rule").unwrap();
        assert!(global < root && root < nearest);
        assert!(!section.contains("above the repo"));
    }

    #[test]
    fn global_agents_md_beats_global_claude_md() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join(".agents/AGENTS.md"), "agents global");
        write(&home.path().join(".claude/CLAUDE.md"), "claude global");

        let section = instructions_section(None, Some(home.path())).unwrap();

        assert!(section.contains("agents global"));
        assert!(!section.contains("claude global"));
    }

    #[test]
    fn a_large_file_is_truncated_with_a_note() {
        let ws = tempfile::tempdir().unwrap();
        write(&ws.path().join("AGENTS.md"), &"é".repeat(MAX_FILE_BYTES));

        let section = instructions_section(Some(ws.path()), None).unwrap();

        assert!(section.contains("[Truncated: showing the first"));
        assert!(section.len() < MAX_FILE_BYTES + 1024);
    }

    #[test]
    fn files_past_the_total_budget_are_named_not_inlined() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".agents/AGENTS.md"),
            &"a".repeat(MAX_FILE_BYTES),
        );
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        write(&repo.path().join("AGENTS.md"), &"b".repeat(MAX_FILE_BYTES));
        let sub = repo.path().join("sub");
        write(&sub.join("AGENTS.md"), "never inlined");

        let section = instructions_section(Some(&sub), Some(home.path())).unwrap();

        assert!(!section.contains("never inlined"));
        assert!(section.contains("[Left out for length:"));
    }
}
