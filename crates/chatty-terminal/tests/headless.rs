//! Headless tests: a real PTY and a real child process, no view attached.
//!
//! Every wait is a poll against a generous deadline, never a fixed sleep, so
//! the tests hold up on a loaded machine.
#![cfg(unix)]

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chatty_terminal::{Region, TerminalConfig, TerminalEvent, TerminalHandle};

const DEADLINE: Duration = Duration::from_secs(30);

fn sh(script: &str) -> TerminalConfig {
    TerminalConfig {
        shell: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        ..TerminalConfig::default()
    }
}

/// Pump events until `done` holds for the current terminal state. Returns
/// every event seen on the way.
fn wait_until(
    term: &TerminalHandle,
    events: &Receiver<TerminalEvent>,
    what: &str,
    mut done: impl FnMut(&TerminalHandle, &[TerminalEvent]) -> bool,
) -> Vec<TerminalEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if done(term, &seen) {
            return seen;
        }
        if start.elapsed() > DEADLINE {
            panic!(
                "timed out waiting for {what}; screen:\n{}\nevents: {seen:?}",
                term.snapshot(Region::Screen).text
            );
        }
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => seen.push(event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if done(term, &seen) {
                    return seen;
                }
                panic!("event channel closed while waiting for {what}");
            }
        }
    }
}

fn screen_contains(term: &TerminalHandle, needle: &str) -> bool {
    term.snapshot(Region::Screen).text.contains(needle)
}

#[test]
fn printf_shows_up_on_screen_after_a_wakeup() {
    let (term, events) = TerminalHandle::spawn(sh("printf hello-t1; sleep 5")).unwrap();
    let generation_before = term.generation();

    let seen = wait_until(&term, &events, "hello-t1 after a wakeup", |t, seen| {
        seen.iter().any(|e| matches!(e, TerminalEvent::Wakeup)) && screen_contains(t, "hello-t1")
    });

    assert!(seen.iter().any(|e| matches!(e, TerminalEvent::Wakeup)));
    let snap = term.snapshot(Region::Screen);
    assert!(snap.text.contains("hello-t1"), "{snap:?}");
    assert_eq!((snap.cols, snap.rows), (80, 24));
    assert_eq!(snap.cursor_row, 0);
    assert!(term.generation() > generation_before);
}

#[test]
fn resize_reaches_the_child() {
    // ncurses `tput cols` prefers an exported COLUMNS/LINES over the PTY's
    // TIOCGWINSZ, and the child inherits this process's environment (the
    // PTY options can only add variables), so clear them in the script.
    let (term, events) = TerminalHandle::spawn(sh(
        "unset COLUMNS LINES; printf ready; read _x; tput cols; sleep 5",
    ))
    .unwrap();
    wait_until(&term, &events, "ready", |t, _| screen_contains(t, "ready"));

    term.resize(40, 10).unwrap();
    term.write(b"\n").unwrap();

    wait_until(&term, &events, "tput cols = 40", |t, _| {
        t.snapshot(Region::Screen)
            .text
            .lines()
            .any(|l| l.trim() == "40")
    });
    let snap = term.snapshot(Region::Screen);
    assert_eq!((snap.cols, snap.rows), (40, 10));
}

#[test]
fn kill_reports_child_exit_and_leaves_no_process() {
    let (term, events) = TerminalHandle::spawn(sh("printf up; sleep 30")).unwrap();
    let pid = term.pid().expect("unix child has a pid") as i32;
    wait_until(&term, &events, "up", |t, _| screen_contains(t, "up"));

    term.kill();

    wait_until(&term, &events, "ChildExit", |_, seen| {
        seen.iter()
            .any(|e| matches!(e, TerminalEvent::ChildExit(_)))
    });
    assert!(term.has_exited());

    // The shell was reaped, and nothing is left in its process group (the
    // `sleep` it forked included). Orphans are reaped by init, so poll.
    let start = Instant::now();
    loop {
        let shell_gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err();
        let group_gone = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), None).is_err();
        if shell_gone && group_gone {
            break;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "pid {pid} or its group still alive"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn drop_kills_the_child() {
    // The shell ignores SIGHUP, so alacritty's own hang-up on PTY drop cannot
    // end it: only the crate's SIGKILL does. Without it, drop would block in
    // the PTY's `wait()` for the full minute.
    let (term, events) = TerminalHandle::spawn(sh("trap '' HUP; printf up; sleep 60")).unwrap();
    let pid = term.pid().unwrap() as i32;
    wait_until(&term, &events, "up", |t, _| screen_contains(t, "up"));

    let start = Instant::now();
    drop(term);
    let took = start.elapsed();

    assert!(took < Duration::from_secs(10), "drop took {took:?}");
    // Drop joins the PTY thread, which reaps the shell: gone immediately.
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err());
}

#[test]
fn scrollback_returns_the_last_n_lines() {
    let (term, events) = TerminalHandle::spawn(sh("seq 1 1000; sleep 5")).unwrap();
    wait_until(&term, &events, "1000", |t, _| {
        t.snapshot(Region::Screen).text.lines().any(|l| l == "1000")
    });

    let snap = term.snapshot(Region::Scrollback { lines: 50 });
    let expected: Vec<String> = (951..=1000).map(|n| n.to_string()).collect();
    assert_eq!(snap.text.lines().collect::<Vec<_>>(), expected);

    // Asking for more than exists returns everything, from the first line.
    let all = term.snapshot(Region::Scrollback { lines: 5000 });
    assert_eq!(all.text.lines().count(), 1000);
    assert_eq!(all.text.lines().next(), Some("1"));
}

#[test]
fn byte_tap_sees_exactly_what_the_child_wrote() {
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&seen);
    // Escape sequences and multi-byte UTF-8 go through untouched; the only
    // rewrite is the tty line discipline's `\n` -> `\r\n` (onlcr).
    let script = r"printf 'a\033[1mb\033[0m\342\202\254 '; seq 1 20000";
    let (term, events) = TerminalHandle::spawn_with_tap(sh(script), move |bytes| {
        sink.lock().unwrap().extend_from_slice(bytes)
    })
    .unwrap();

    wait_until(&term, &events, "child exit", |_, seen| {
        seen.iter()
            .any(|e| matches!(e, TerminalEvent::ChildExit(_)))
    });
    drop(term);

    let mut expected = b"a\x1b[1mb\x1b[0m\xe2\x82\xac ".to_vec();
    for n in 1..=20000 {
        expected.extend_from_slice(format!("{n}\r\n").as_bytes());
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), expected.len());
    assert!(
        *seen == expected,
        "tapped bytes differ from the child's output"
    );
}

#[test]
fn wide_chars_and_trailing_blanks_in_snapshot() {
    let (term, events) = TerminalHandle::spawn(sh("printf '漢字 ok   \\n'; sleep 5")).unwrap();
    wait_until(&term, &events, "wide text", |t, _| screen_contains(t, "ok"));
    let snap = term.snapshot(Region::Screen);
    assert_eq!(snap.text, "漢字 ok");
}
