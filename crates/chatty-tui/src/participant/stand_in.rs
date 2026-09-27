//! A stand-in worker process for tests that pin what the broker does around
//! a worker rather than what a real worker's model does.
//!
//! Since ADR-0020 a worker cannot be registered in-process under a name the
//! runner is about to hand out: the runner admits the node, makes the
//! connection and gives the child its end at descriptor 3, and only what
//! speaks on that end can serve the task. So the stand-in is a real child —
//! a short `sh` script — that records its argv, says `hello` on descriptor
//! 3, reads its `welcome` and its `task`, and answers with the frames a
//! worker replaying `events` through the real [`TaskMapper`] would send,
//! computed here in advance with the task id filled in by `sed`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chatty_core::session::SessionEvent;
use chatty_protocol_gateway::participant::{PARTICIPANT_FD, ParticipantFrame, encode_frame};
use chatty_protocol_gateway::worker::TaskMapper;

/// What the task id is written as in the canned frames.
const TASK_ID: &str = "@TASK_ID@";

/// Every frame a worker replaying `events` sends for one task, as lines.
fn canned_frames(events: &[SessionEvent]) -> String {
    let mut mapper = TaskMapper::new(TASK_ID);
    let mut frames: Vec<ParticipantFrame> = events.iter().filter_map(|e| mapper.map(e)).collect();
    frames.push(mapper.terminal());
    frames
        .iter()
        .map(|frame| encode_frame(frame).expect("a frame encodes") + "\n")
        .collect()
}

/// A "chatty-tui" in `dir` that appends its argv to `dir/argv.log`, then
/// serves its one task on the broker's connection by replaying `events`,
/// then waits to be reaped as a real worker would.
pub(crate) fn scripted_worker_binary(dir: &Path, events: &[SessionEvent]) -> PathBuf {
    std::fs::write(dir.join("frames.jsonl"), canned_frames(events)).expect("frames written");
    let fd = PARTICIPANT_FD;
    let script = format!(
        r#"#!/bin/sh
here="$(dirname "$0")"
printf '%s\n' "$*" >> "$here/argv.log"
printf '{{"v":2,"type":"hello","card":{{"name":"stand-in"}}}}\n' >&{fd}
read -r welcome <&{fd}
read -r task <&{fd}
id=$(printf '%s' "$task" | sed 's/.*"taskId":"\([^"]*\)".*/\1/')
sed "s/{TASK_ID}/$id/g" "$here/frames.jsonl" >&{fd}
exec sleep 30
"#
    );
    let path = dir.join("chatty-tui");
    std::fs::write(&path, script).expect("the stand-in binary is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the stand-in binary is executable");
    path
}

/// The argv lines the stand-in children in `dir` recorded so far, once
/// there are at least `at_least` of them.
pub(crate) async fn recorded_argv(dir: &Path, at_least: usize) -> Vec<String> {
    let log = dir.join("argv.log");
    for _ in 0..200 {
        let lines: Vec<String> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        if lines.len() >= at_least {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the stand-in child never recorded its argv in {}",
        log.display()
    );
}
