//! Researchers' notes kept in the workspace, not in the lead's context
//! (AGE-1295).
//!
//! A deep-research lead used to carry every researcher's note in its own
//! history: the `invoke_agent` result held the whole typed handoff. A long
//! run outgrew the window, compaction took the old notes away, and the lead
//! spent its last round researching them again and handed the writer
//! excerpts. So a lead whose `[swarm.rounds]` names a `notes` directory has
//! the host keep each valid note of the rounds' callee in the workspace,
//! `<notes>/<round>-<n>.json`, at the point the leader receives the
//! validated handoff ([`NoteStore::store`]). Its `invoke_agent` result
//! carries a short [`reference`] instead: the file, the sub-question, how
//! many claims and their URLs, and the open questions the next round is
//! planned from.
//!
//! The notes are read back from the files: every other delegation of the
//! lead (the writer, the checker) is offered the list of files
//! ([`NoteStore::offer`]), and the host's citation check reads them
//! ([`read_notes`]), so neither depends on what the lead's history still
//! holds.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::agent_spec::RoundsSection;
use crate::services::round_limit::RoundLimit;

/// What a delegation prompt gets when note files exist, ahead of the list.
pub const NOTE_FILES_HEADING: &str = "Research notes";

/// Where one leader keeps its rounds' notes. Cloning shares the count.
#[derive(Clone, Debug)]
pub struct NoteStore {
    workspace: PathBuf,
    /// The notes directory, relative to the workspace.
    dir: String,
    /// The callee whose handoffs are notes.
    agent: String,
    /// The leader's round count: a note is filed under the round it came
    /// back in.
    rounds: RoundLimit,
    /// The highest note number used per round.
    numbers: Arc<Mutex<BTreeMap<u32, u32>>>,
}

impl NoteStore {
    /// A store for `rounds` when it names a `notes` directory; `None`
    /// otherwise, which keeps every result as it was.
    ///
    /// One store is one lead build, so one run: notes an earlier run left
    /// directly in the directory are moved into `earlier-<unix seconds>/`
    /// first. Nothing is deleted, and the offer, the files and the
    /// citation check (which read only the directory's own files) see this
    /// run's notes alone, numbered from `1-1`.
    pub fn new(workspace: &Path, rounds: &RoundsSection, count: RoundLimit) -> Option<Self> {
        let dir = rounds.notes.clone()?;
        if let Err(error) = set_aside_earlier_notes(workspace, &dir) {
            tracing::warn!(%error, dir, "Could not set an earlier run's notes aside");
        }
        Some(Self {
            workspace: workspace.to_path_buf(),
            dir,
            agent: rounds.agent.clone(),
            rounds: count,
            numbers: Arc::default(),
        })
    }

    /// The callee whose handoffs are notes.
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Write `note` to the next free `<dir>/<round>-<n>.json`; the path
    /// relative to the workspace. A file already there (an earlier run in
    /// the same workspace) is never overwritten.
    pub fn store(&self, note: &Value) -> std::io::Result<String> {
        let round = self.rounds.rounds().max(1);
        let dir = self.workspace.join(&self.dir);
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string_pretty(note).map_err(std::io::Error::other)?;
        let mut numbers = self.numbers.lock();
        let number = numbers.entry(round).or_default();
        loop {
            *number += 1;
            let name = format!("{round}-{number}.json");
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(&name))
            {
                Ok(mut file) => {
                    file.write_all(text.as_bytes())?;
                    return Ok(format!("{}/{name}", self.dir.trim_end_matches('/')));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// The note files in the directory, relative to the workspace, in
    /// round and number order.
    pub fn files(&self) -> Vec<String> {
        note_files(&self.workspace, &self.dir)
            .into_iter()
            .map(|(_, relative)| relative)
            .collect()
    }

    /// `prompt` for a delegation to `callee`, with every note file listed
    /// after it, when `callee` is not the researcher and there are notes.
    pub fn offer(&self, callee: &str, prompt: String) -> String {
        if callee == self.agent {
            return prompt;
        }
        let files = self.files();
        if files.is_empty() {
            return prompt;
        }
        let list: String = files.iter().map(|file| format!("\n- {file}")).collect();
        format!(
            "{prompt}\n\n{NOTE_FILES_HEADING} ({} files, added by the host): every note the \
             researchers handed back, one JSON file each. Read every one of them with \
             read_file; they are the notes, whatever the brief above quotes from \
             them.{list}",
            files.len()
        )
    }
}

/// Move the note files directly under `workspace/dir` into a fresh
/// `earlier-<unix seconds>` directory beside them. Nothing to do when there
/// are none.
fn set_aside_earlier_notes(workspace: &Path, dir: &str) -> std::io::Result<()> {
    let files = note_files(workspace, dir);
    if files.is_empty() {
        return Ok(());
    }
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let base = workspace.join(dir);
    let mut aside = base.join(format!("earlier-{seconds}"));
    let mut n = 1;
    while aside.exists() {
        n += 1;
        aside = base.join(format!("earlier-{seconds}-{n}"));
    }
    std::fs::create_dir_all(&aside)?;
    for (_, relative) in files {
        let from = workspace.join(&relative);
        if let Some(name) = from.file_name() {
            std::fs::rename(&from, aside.join(name))?;
        }
    }
    Ok(())
}

/// Whether `handoff` is a note: a typed handoff carrying `claims`.
pub fn is_note(handoff: &Value) -> bool {
    handoff["claims"].is_array()
}

/// The short reference the lead gets for a note kept at `file`: the file,
/// the sub-question, the claim count, the cited URLs (each once) and the
/// open questions.
pub fn reference(file: &str, note: &Value) -> Value {
    let claims = note["claims"].as_array().map_or(&[][..], Vec::as_slice);
    let mut seen = BTreeSet::new();
    let urls: Vec<&str> = claims
        .iter()
        .filter_map(|claim| claim["url"].as_str())
        .filter(|url| seen.insert(*url))
        .collect();
    json!({
        "note_file": file,
        "sub_question": note["sub_question"],
        "claims": claims.len(),
        "urls": urls,
        "open_questions": note["open_questions"],
    })
}

/// What the lead's `invoke_agent` result says in place of the note.
pub fn reference_text(file: &str, note: &Value) -> String {
    let claims = note["claims"].as_array().map_or(0, Vec::len);
    format!(
        "The note is saved to {file} ({claims} claims); `note` below is its reference. The \
         host offers every note file to the writer and the checker, so do not copy notes \
         into their briefs."
    )
}

/// `(round, n)` of a note file name `<round>-<n>.json`.
fn note_number(name: &str) -> Option<(u32, u32)> {
    let (round, n) = name.strip_suffix(".json")?.split_once('-')?;
    Some((round.parse().ok()?, n.parse().ok()?))
}

/// The note files under `workspace/dir`, sorted by round and number, each
/// relative to the workspace.
fn note_files(workspace: &Path, dir: &str) -> Vec<((u32, u32), String)> {
    let Ok(entries) = std::fs::read_dir(workspace.join(dir)) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = note_number(&name)?;
            Some((number, format!("{}/{name}", dir.trim_end_matches('/'))))
        })
        .collect();
    files.sort();
    files
}

/// Every note kept under `workspace/dir`, in round and number order. A
/// file that does not parse, or is no note, is skipped.
pub fn read_notes(workspace: &Path, dir: &str) -> Vec<Value> {
    note_files(workspace, dir)
        .into_iter()
        .filter_map(|(_, relative)| {
            let text = std::fs::read_to_string(workspace.join(relative)).ok()?;
            serde_json::from_str::<Value>(&text).ok()
        })
        .filter(is_note)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, urls: &[&str]) -> Value {
        json!({
            "sub_question": format!("{id}: q"),
            "claims": urls.iter().map(|url| json!({
                "claim": "c", "url": url, "quote": "q", "accessed": "2026-11-12"
            })).collect::<Vec<_>>(),
            "open_questions": ["open"],
        })
    }

    fn new_store(workspace: &Path) -> NoteStore {
        let rounds = RoundsSection {
            agent: "r".to_string(),
            max: 4,
            notes: Some("notes".to_string()),
        };
        let count = RoundLimit::new(Some(rounds.clone()));
        NoteStore::new(workspace, &rounds, count).expect("notes named")
    }

    /// Notes are filed per round, never overwrite a file already there,
    /// and read back in round and number order.
    #[test]
    fn notes_are_filed_by_round_and_read_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = new_store(dir.path());
        let first = store.store(&note("SQ1", &["https://a.example/1"])).unwrap();
        let second = store.store(&note("SQ2", &["https://b.example/2"])).unwrap();
        assert_eq!(
            (first.as_str(), second.as_str()),
            ("notes/1-1.json", "notes/1-2.json")
        );

        std::fs::write(dir.path().join("notes/README.txt"), "not a note").unwrap();
        std::fs::write(dir.path().join("notes/1-10.json"), "{\"verdict\":\"PASS\"}").unwrap();
        let third = store.store(&note("SQ3", &["https://c.example/3"])).unwrap();
        assert_eq!(third, "notes/1-3.json");
        assert_eq!(
            store.files(),
            [
                "notes/1-1.json",
                "notes/1-2.json",
                "notes/1-3.json",
                "notes/1-10.json"
            ]
        );
        let read = read_notes(dir.path(), "notes");
        assert_eq!(read.len(), 3, "the verdict is no note");
        assert_eq!(read[0]["sub_question"], "SQ1: q");
        assert_eq!(read[2]["sub_question"], "SQ3: q");
    }

    /// AGE-1295 review: a new store (a new run) over a directory that
    /// already holds notes moves them into `earlier-*/` and starts at
    /// `1-1.json`; nothing of the earlier run is offered or read.
    #[test]
    fn a_new_run_sets_the_earlier_runs_notes_aside() {
        let dir = tempfile::tempdir().unwrap();
        let first = new_store(dir.path());
        first
            .store(&note("OLD1", &["https://old.example/1"]))
            .unwrap();
        first
            .store(&note("OLD2", &["https://old.example/2"]))
            .unwrap();
        std::fs::write(dir.path().join("notes/README.txt"), "kept").unwrap();

        let second = new_store(dir.path());
        assert!(second.files().is_empty(), "{:?}", second.files());
        assert!(read_notes(dir.path(), "notes").is_empty());
        assert_eq!(second.offer("writer", "Write.".to_string()), "Write.");
        assert_eq!(
            second
                .store(&note("NEW1", &["https://new.example/1"]))
                .unwrap(),
            "notes/1-1.json"
        );
        let read = read_notes(dir.path(), "notes");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0]["sub_question"], "NEW1: q");

        // The earlier run's notes are kept, set aside, not deleted.
        let aside: Vec<_> = std::fs::read_dir(dir.path().join("notes"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("earlier-"))
            .collect();
        assert_eq!(aside.len(), 1);
        let earlier = aside[0].path();
        for name in ["1-1.json", "1-2.json"] {
            let text = std::fs::read_to_string(earlier.join(name)).unwrap();
            assert!(text.contains("OLD"), "{name}: {text}");
        }
        assert!(
            dir.path().join("notes/README.txt").is_file(),
            "only notes move"
        );
    }

    /// The reference names the file, the sub-question, the claim count,
    /// each URL once and the open questions; it carries no quote.
    #[test]
    fn the_reference_is_short_and_carries_no_quote() {
        let note = note(
            "SQ1",
            &[
                "https://a.example/1",
                "https://a.example/1",
                "https://b.example",
            ],
        );
        let reference = reference("notes/1-1.json", &note);
        assert_eq!(reference["note_file"], "notes/1-1.json");
        assert_eq!(reference["claims"], 3);
        assert_eq!(
            reference["urls"],
            json!(["https://a.example/1", "https://b.example"])
        );
        assert_eq!(reference["open_questions"], json!(["open"]));
        assert!(!reference.to_string().contains("\"quote\""));
        assert!(reference_text("notes/1-1.json", &note).contains("notes/1-1.json"));
    }

    /// Every other callee is offered the files; the researcher is not, and
    /// nothing is added before the first note.
    #[test]
    fn other_callees_are_offered_every_note_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = new_store(dir.path());
        assert_eq!(store.offer("writer", "Write.".to_string()), "Write.");
        store.store(&note("SQ1", &["https://a.example/1"])).unwrap();
        store.store(&note("SQ2", &["https://b.example/2"])).unwrap();
        let offered = store.offer("writer", "Write.".to_string());
        assert!(
            offered.starts_with("Write.\n\nResearch notes (2 files"),
            "{offered}"
        );
        assert!(
            offered.contains("\n- notes/1-1.json\n- notes/1-2.json"),
            "{offered}"
        );
        assert_eq!(store.offer("r", "Brief.".to_string()), "Brief.");
    }

    #[test]
    fn no_notes_directory_no_store() {
        let rounds = RoundsSection {
            agent: "r".to_string(),
            max: 4,
            notes: None,
        };
        assert!(NoteStore::new(Path::new("/tmp"), &rounds, RoundLimit::default()).is_none());
    }
}
