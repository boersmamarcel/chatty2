//! Existing paths close to one that does not exist, for a read that
//! missed: the same file name elsewhere in the workspace, or a near-miss
//! name in the nearest directory that does exist. A model that guessed a
//! path wrong gets the right one back in the error instead of spending
//! calls listing directories to find it.
//!
//! The work is bounded: at most [`SCAN_CAP`] directory entries are looked
//! at, breadth first from the workspace root, and nothing outside the
//! root is read.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};

/// Suggestions returned at most.
pub const SUGGESTION_LIMIT: usize = 5;

/// Directory entries looked at, across both searches, at most.
pub const SCAN_CAP: usize = 4_000;

/// Directories the same-name search does not descend into: build output,
/// dependencies and VCS metadata, where a same-named file is never the
/// one meant. Hidden directories are skipped too.
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "venv",
    "build",
    "dist",
];

/// How alike two names must be (0..1, by characters) for a sibling to be
/// suggested.
const NAME_SIMILARITY: f32 = 0.6;

/// Up to [`SUGGESTION_LIMIT`] existing paths under `root` close to
/// `requested` (relative to `root`, or absolute), as paths relative to
/// `root`; directories end in `/`. Empty when `requested` is outside
/// `root` or nothing is close.
pub fn similar_existing_paths(root: &Path, requested: &str) -> Vec<String> {
    let requested = Path::new(requested);
    let absolute = normalize(&if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    });
    let Ok(relative) = absolute.strip_prefix(root) else {
        return Vec::new();
    };
    let wanted: Vec<String> = relative
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let Some(file_name) = wanted.last() else {
        return Vec::new();
    };

    let mut budget = SCAN_CAP;
    // (score, path): same-name files score above 1, near-miss siblings below.
    let mut found: Vec<(f32, PathBuf)> = Vec::new();
    for path in same_name_files(root, file_name, &mut budget) {
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let shared_tail = rel
            .components()
            .rev()
            .zip(wanted.iter().rev())
            .take_while(|(have, want)| have.as_os_str().to_string_lossy() == want.as_str())
            .count();
        found.push((1.0 + shared_tail as f32, path));
    }
    found.extend(near_miss_siblings(root, &wanted, &mut budget));

    found.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.as_os_str().len().cmp(&b.1.as_os_str().len()))
    });
    let mut suggestions: Vec<String> = Vec::new();
    for (_, path) in found {
        let mut shown = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            shown.push('/');
        }
        if !suggestions.contains(&shown) {
            suggestions.push(shown);
        }
        if suggestions.len() == SUGGESTION_LIMIT {
            break;
        }
    }
    suggestions
}

/// Files named `name` (ignoring case) under `root`, breadth first, looking
/// at no more than `budget` entries.
fn same_name_files(root: &Path, name: &str, budget: &mut usize) -> Vec<PathBuf> {
    let name = name.to_lowercase();
    let mut found = Vec::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if *budget == 0 {
                return found;
            }
            *budget -= 1;
            let entry_name = entry.file_name().to_string_lossy().to_lowercase();
            // `file_type` does not follow symlinks, so a link cannot lead
            // the walk out of the root or round in a loop.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !entry_name.starts_with('.') && !SKIPPED_DIRS.contains(&entry_name.as_str()) {
                    queue.push_back(entry.path());
                }
            } else if entry_name == name {
                found.push(entry.path());
            }
        }
    }
    found
}

/// Entries of the deepest existing directory on the way to `wanted` whose
/// names are close to the first component missing below it. For a
/// missing directory, a close one is suggested with the rest of the path
/// when that exists.
fn near_miss_siblings(root: &Path, wanted: &[String], budget: &mut usize) -> Vec<(f32, PathBuf)> {
    let mut dir = root.to_path_buf();
    let mut depth = 0;
    while depth + 1 < wanted.len() && dir.join(&wanted[depth]).is_dir() {
        dir.push(&wanted[depth]);
        depth += 1;
    }
    let missing = wanted[depth].to_lowercase();
    let rest: PathBuf = wanted[depth + 1..].iter().collect();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let score = name_similarity(&name, &missing);
        if score < NAME_SIMILARITY {
            continue;
        }
        let with_rest = entry.path().join(&rest);
        let path = if !rest.as_os_str().is_empty() && with_rest.exists() {
            with_rest
        } else {
            entry.path()
        };
        found.push((score, path));
    }
    found
}

/// How alike two file names are, 0..1: equal stems (`a.py` vs `a.txt`)
/// or one name inside the other count as close; otherwise the share of
/// characters in common.
fn name_similarity(a: &str, b: &str) -> f32 {
    let stem = |s: &str| s.split('.').next().unwrap_or(s).to_string();
    if a == b {
        return 1.0;
    }
    if stem(a) == stem(b) && !stem(a).is_empty() {
        return 0.95;
    }
    if a.len() >= 3 && b.len() >= 3 && (a.contains(b) || b.contains(a)) {
        return 0.9;
    }
    similar::TextDiff::from_chars(a, b).ratio()
}

/// `path` with `.` and `..` resolved lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for file in files {
            touch(tmp.path(), file);
        }
        tmp
    }

    fn root(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().canonicalize().unwrap()
    }

    #[test]
    fn the_same_file_name_elsewhere_is_suggested_closest_path_first() {
        let tmp = tree(&[
            "src/pkg/db/models/query.py",
            "src/pkg/other/query.py",
            "docs/query.py",
        ]);
        let got = similar_existing_paths(&root(&tmp), "pkg/db/models/query.py");
        assert_eq!(got[0], "src/pkg/db/models/query.py", "{got:?}");
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn near_miss_names_in_the_nearest_existing_directory_are_suggested() {
        let tmp = tree(&["src/utils.py", "src/util_test.py", "src/main.py"]);
        let got = similar_existing_paths(&root(&tmp), "src/util.py");
        assert!(got.contains(&"src/utils.py".to_string()), "{got:?}");
        assert!(!got.contains(&"src/main.py".to_string()), "{got:?}");
        // Same stem, other extension.
        let got = similar_existing_paths(&root(&tmp), "src/main.rs");
        assert_eq!(got, vec!["src/main.py".to_string()]);
    }

    #[test]
    fn a_misnamed_directory_is_corrected_with_the_rest_of_the_path() {
        let tmp = tree(&["src/helpers/io.py"]);
        let got = similar_existing_paths(&root(&tmp), "src/helper/io.py");
        assert_eq!(got, vec!["src/helpers/io.py".to_string()]);
    }

    #[test]
    fn absolute_paths_inside_the_root_work_and_outside_ones_get_nothing() {
        let tmp = tree(&["a/b/c.txt"]);
        let root = root(&tmp);
        let inside = format!("{}/c.txt", root.display());
        assert_eq!(similar_existing_paths(&root, &inside), vec!["a/b/c.txt"]);
        assert!(similar_existing_paths(&root, "/definitely/elsewhere/c.txt").is_empty());
        assert!(similar_existing_paths(&root, "../c.txt").is_empty());
    }

    #[test]
    fn nothing_close_suggests_nothing() {
        let tmp = tree(&["src/alpha.py"]);
        assert!(similar_existing_paths(&root(&tmp), "zzz/qqq.rs").is_empty());
    }

    #[test]
    fn at_most_five_suggestions_and_dependency_dirs_are_skipped() {
        let mut files: Vec<String> = (0..8).map(|i| format!("m{i}/conf.ini")).collect();
        files.push("node_modules/x/conf.ini".to_string());
        files.push(".git/conf.ini".to_string());
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let tmp = tree(&refs);
        let got = similar_existing_paths(&root(&tmp), "conf.ini");
        assert_eq!(got.len(), SUGGESTION_LIMIT, "{got:?}");
        assert!(
            got.iter().all(|p| p.starts_with('m')),
            "no dependency or VCS copies: {got:?}"
        );
    }

    #[test]
    fn the_walk_stops_when_the_entry_budget_is_spent() {
        let tmp = tree(&["a/x.txt", "b/x.txt", "c/x.txt"]);
        // The root's three directories use the whole budget: nothing
        // below them is looked at.
        let mut budget = 3;
        assert!(same_name_files(&root(&tmp), "x.txt", &mut budget).is_empty());
        assert_eq!(budget, 0);
        let mut budget = SCAN_CAP;
        assert_eq!(same_name_files(&root(&tmp), "x.txt", &mut budget).len(), 3);
        assert_eq!(budget, SCAN_CAP - 6);
    }
}
