//! The append-only record of who talked to whom.
//!
//! One JSONL row per task, message and refusal, at
//! `<data_dir>/chatty/fabric/edges-<pid>.jsonl`. When the next row would take
//! the file past [`MAX_EDGE_LOG_BYTES`], the file is renamed to
//! `edges-<pid>.<k>.jsonl` (the first free `k` from 1) and a new one started;
//! rotated files are never rewritten or deleted here.
//!
//! Every field of [`EdgeRow`] is always written, `null` when absent, so the
//! schema is the same on every row (`goldens/edge_log_schema.jsonl`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::directory::ConversationScope;
use crate::task_table::RunId;

/// A log file is rotated before it would grow past this.
pub const MAX_EDGE_LOG_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Task,
    Message,
    Refusal,
}

/// One edge: a task, a message or a refusal between two parties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeRow {
    /// Unix time in milliseconds.
    pub ts: u64,
    pub kind: EdgeKind,
    /// The sender's broker-assigned name, or a description of the peer for a
    /// refusal of something that never became a node.
    pub from: String,
    pub to: String,
    pub scope: Option<ConversationScope>,
    pub run: Option<RunId>,
    pub chain: Vec<String>,
    /// Payload size: the prompt or message body, in bytes.
    pub bytes: u64,
    pub outcome: String,
}

impl EdgeRow {
    /// The current time in the row's `ts` unit.
    pub fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// An open edge log for this process.
#[derive(Debug)]
pub struct EdgeLog {
    dir: PathBuf,
    pid: u32,
    file: File,
    size: u64,
}

impl EdgeLog {
    /// Open (or continue) this process's log under `data_dir`, normally
    /// `dirs::data_dir()`.
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        Self::open_for_pid(data_dir, std::process::id())
    }

    fn open_for_pid(data_dir: &Path, pid: u32) -> io::Result<Self> {
        let dir = data_dir.join("chatty").join("fabric");
        fs::create_dir_all(&dir)?;
        let (file, size) = open_append(&current_path(&dir, pid))?;
        Ok(Self {
            dir,
            pid,
            file,
            size,
        })
    }

    /// The file rows are written to now.
    pub fn path(&self) -> PathBuf {
        current_path(&self.dir, self.pid)
    }

    /// Append one row, rotating first if it would not fit.
    pub fn append(&mut self, row: &EdgeRow) -> io::Result<()> {
        let mut line = serde_json::to_vec(row)?;
        line.push(b'\n');
        let len = line.len() as u64;
        if self.size > 0 && self.size + len > MAX_EDGE_LOG_BYTES {
            self.rotate()?;
        }
        self.file.write_all(&line)?;
        self.size += len;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let rotated = (1u32..)
            .map(|k| self.dir.join(format!("edges-{}.{k}.jsonl", self.pid)))
            .find(|p| !p.exists())
            .expect("an unbounded range always has a free name");
        fs::rename(self.path(), rotated)?;
        let (file, size) = open_append(&self.path())?;
        self.file = file;
        self.size = size;
        Ok(())
    }
}

fn current_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("edges-{pid}.jsonl"))
}

fn open_append(path: &Path) -> io::Result<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::Directory;
    use crate::task_table::TaskTable;

    const GOLDEN_PID: u32 = 4242;

    #[test]
    fn edge_log_schema_golden() {
        let data = tempfile::tempdir().unwrap();
        let mut dir = Directory::new();
        let scope = ConversationScope::new("conv-1");
        let root = dir.admit("leader", None, scope.clone()).unwrap();
        let coder = dir
            .admit("local-coder", Some(root.id()), scope.clone())
            .unwrap();
        let mut tasks = TaskTable::new();
        let run = tasks
            .open(root.id(), coder.id(), None, vec!["leader-0".into()])
            .unwrap();

        let mut log = EdgeLog::open_for_pid(data.path(), GOLDEN_PID).unwrap();
        assert_eq!(
            log.path(),
            data.path()
                .join("chatty/fabric")
                .join(format!("edges-{GOLDEN_PID}.jsonl"))
        );
        let rows = [
            EdgeRow {
                ts: 1_790_000_000_000,
                kind: EdgeKind::Task,
                from: root.name().to_string(),
                to: coder.name().to_string(),
                scope: Some(scope.clone()),
                run: Some(run),
                chain: vec!["leader-0".into()],
                bytes: 42,
                outcome: "completed".into(),
            },
            EdgeRow {
                ts: 1_790_000_000_500,
                kind: EdgeKind::Message,
                from: coder.name().to_string(),
                to: root.name().to_string(),
                scope: Some(scope.clone()),
                run: Some(run),
                chain: vec!["leader-0".into(), "local-coder-0".into()],
                bytes: 17,
                outcome: "pending".into(),
            },
            EdgeRow {
                ts: 1_790_000_001_000,
                kind: EdgeKind::Refusal,
                from: "unregistered peer".into(),
                to: "local-coder-0".into(),
                scope: None,
                run: None,
                chain: vec![],
                bytes: 0,
                outcome: "refused: not_on_tree".into(),
            },
        ];
        for row in &rows {
            log.append(row).unwrap();
        }

        let written = fs::read_to_string(log.path()).unwrap();
        for (line, row) in written.lines().zip(&rows) {
            assert_eq!(&serde_json::from_str::<EdgeRow>(line).unwrap(), row);
        }

        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens/edge_log_schema.jsonl");
        if std::env::var("UPDATE_GOLDENS").is_ok() {
            fs::write(&golden, &written).unwrap();
            return;
        }
        let expected = fs::read_to_string(&golden).unwrap_or_else(|_| {
            panic!(
                "missing golden {}; re-run with UPDATE_GOLDENS=1",
                golden.display()
            )
        });
        assert_eq!(
            expected, written,
            "the edge-log schema changed. Later epics (PL-S4, PL-S7) read these rows; \
             if the change is deliberate, re-run with UPDATE_GOLDENS=1 and say why in review"
        );
    }

    #[test]
    fn edge_log_rotates_at_10mb() {
        let data = tempfile::tempdir().unwrap();
        let mut log = EdgeLog::open_for_pid(data.path(), 7).unwrap();
        let row = EdgeRow {
            ts: 1,
            kind: EdgeKind::Task,
            from: "a-0".into(),
            to: "b-0".into(),
            scope: Some(ConversationScope::new("c")),
            run: None,
            chain: vec![],
            bytes: 0,
            outcome: "x".repeat(100 * 1024),
        };
        let row_len = serde_json::to_vec(&row).unwrap().len() as u64 + 1;
        let per_file = MAX_EDGE_LOG_BYTES / row_len;
        let total = per_file * 2 + 3;
        for _ in 0..total {
            log.append(&row).unwrap();
        }

        let fabric = data.path().join("chatty/fabric");
        let first = fabric.join("edges-7.1.jsonl");
        let second = fabric.join("edges-7.2.jsonl");
        let current = fabric.join("edges-7.jsonl");
        for full in [&first, &second] {
            let size = fs::metadata(full).unwrap().len();
            assert!(
                size <= MAX_EDGE_LOG_BYTES,
                "{} is {size} bytes",
                full.display()
            );
            assert!(
                size > MAX_EDGE_LOG_BYTES - row_len,
                "{} rotated early at {size} bytes",
                full.display()
            );
        }
        assert!(!fabric.join("edges-7.3.jsonl").exists());
        assert_eq!(fs::metadata(&current).unwrap().len(), 3 * row_len);

        // Nothing is lost or torn across the rotations.
        let rows: usize = [&first, &second, &current]
            .iter()
            .map(|p| {
                fs::read_to_string(p)
                    .unwrap()
                    .lines()
                    .inspect(|l| {
                        serde_json::from_str::<EdgeRow>(l).unwrap();
                    })
                    .count()
            })
            .sum();
        assert_eq!(rows as u64, total);

        // Reopening continues the current file rather than truncating it.
        drop(log);
        let mut reopened = EdgeLog::open_for_pid(data.path(), 7).unwrap();
        reopened.append(&row).unwrap();
        assert_eq!(fs::metadata(&current).unwrap().len(), 4 * row_len);
    }
}
