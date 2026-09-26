//! Command records from OSC 133 shell integration, against real shells on a
//! PTY. Each bash runs with its own `HOME` holding a small `.bashrc`, so the
//! tests don't depend on the machine's dotfiles.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chatty_terminal::{CommandRecord, LastCommand, Region, TerminalConfig, TerminalHandle};

const DEADLINE: Duration = Duration::from_secs(30);

/// A fresh `HOME` whose `.bashrc` is `bashrc`.
fn home(name: &str, bashrc: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chatty-terminal-commands-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".bashrc"), bashrc).unwrap();
    dir
}

fn bash(home: &Path) -> TerminalConfig {
    TerminalConfig {
        shell: Some("/bin/bash".into()),
        env: HashMap::from([
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            ("HISTFILE".to_string(), "/dev/null".to_string()),
        ]),
        size: (80, 24),
        ..TerminalConfig::default()
    }
}

fn wait_for(term: &TerminalHandle, what: &str, mut done: impl FnMut(&TerminalHandle) -> bool) {
    let start = Instant::now();
    while !done(term) {
        if start.elapsed() > DEADLINE {
            panic!(
                "timed out waiting for {what}; screen:\n{}\nrecords: {:#?}",
                term.snapshot(Region::Screen).text,
                term.commands()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait for the prompt (`$ ` on the cursor's row), type `line`, and wait
/// for it to finish as record number `n` (1-based).
fn run(term: &TerminalHandle, line: &[u8], n: usize) -> CommandRecord {
    wait_for(term, "the prompt", |t| {
        let snap = t.snapshot(Region::Screen);
        snap.text.lines().nth(snap.cursor_row as usize) == Some("$") && t.commands().len() == n - 1
    });
    term.write(line).unwrap();
    term.write(b"\r").unwrap();
    wait_for(term, "the command to finish", |t| {
        t.commands().get(n - 1).is_some_and(|r| !r.is_running())
    });
    term.commands().remove(n - 1)
}

fn last_command(term: &TerminalHandle) -> (LastCommand, String) {
    let snap = term.snapshot(Region::LastCommand);
    (snap.last_command.expect("set for LastCommand"), snap.text)
}

#[test]
fn bash_records_text_exit_codes_and_output() {
    let home = home("basic", "PS1='$ '\n");
    let (term, _events) = TerminalHandle::spawn(bash(&home)).unwrap();

    let t = run(&term, b"true", 1);
    let f = run(&term, b"false", 2);
    let ab = run(&term, b"echo a; echo b", 3);
    println!("records: {:#?}", term.commands());

    assert_eq!(t.command_text, "true");
    assert_eq!(t.exit_code, Some(0));
    assert_eq!(f.command_text, "false");
    assert_eq!(f.exit_code, Some(1));
    assert_eq!(ab.command_text, "echo a; echo b");
    assert_eq!(ab.exit_code, Some(0));
    // Each command's output starts on the line below its command line and
    // ends where the next prompt starts.
    assert_eq!(f.output_start_line, t.end_line.unwrap() + 1);
    assert_eq!(ab.end_line, Some(ab.output_start_line + 2));

    let (last, output) = last_command(&term);
    println!("last command: {last:#?}\noutput: {output:?}");
    assert_eq!(output, "a\nb");
    let LastCommand::Ran {
        record,
        output_capped,
    } = last
    else {
        panic!("expected a command, got {last:?}");
    };
    assert_eq!(record.command_text, "echo a; echo b");
    assert_eq!(record.exit_code, Some(0));
    assert!(!record.truncated && !output_capped);

    // Output without a trailing newline ends where `D` is, mid-line; the
    // prompt that follows on the same line is not part of it.
    run(&term, b"printf partial", 4);
    assert_eq!(last_command(&term).1, "partial");
}

#[test]
fn the_command_text_is_the_line_after_editing() {
    let home = home("edit", "PS1='$ '\n");
    let (term, _events) = TerminalHandle::spawn(bash(&home)).unwrap();
    // Type `echo abx`, rub out the `x` (DEL), type `c`; then move to the
    // start of the line and back (Ctrl-A, Ctrl-E) for a redraw.
    let r = run(&term, b"echo abx\x7fc\x01\x05", 1);
    assert_eq!(r.command_text, "echo abc");
    assert_eq!(last_command(&term).1, "abc");
}

#[test]
fn a_shell_without_integration_has_no_records() {
    let (term, _events) = TerminalHandle::spawn(TerminalConfig {
        shell: Some("/bin/sh".into()),
        env: HashMap::from([("PS1".to_string(), "$ ".to_string())]),
        ..TerminalConfig::default()
    })
    .unwrap();
    term.write(b"true; echo done-sh\r").unwrap();
    wait_for(&term, "sh to run the command", |t| {
        t.snapshot(Region::Screen)
            .text
            .lines()
            .any(|l| l == "done-sh")
    });
    assert!(term.commands().is_empty());
    let (last, text) = last_command(&term);
    println!("sh last command: {last:?}, text: {text:?}");
    assert_eq!(last, LastCommand::NotActive);
    assert!(text.contains("shell integration not active"), "{text}");
}

#[test]
fn integration_can_be_turned_off() {
    let home = home("off", "PS1='$ '\n");
    let (term, _events) = TerminalHandle::spawn(TerminalConfig {
        shell_integration: false,
        ..bash(&home)
    })
    .unwrap();
    term.write(b"echo done-off\r").unwrap();
    wait_for(&term, "bash to run the command", |t| {
        t.snapshot(Region::Screen)
            .text
            .lines()
            .any(|l| l == "done-off")
    });
    assert!(term.commands().is_empty());
    assert_eq!(last_command(&term).0, LastCommand::NotActive);
}

#[test]
fn the_rc_still_loads_and_its_prompt_is_unchanged() {
    let rc = "PS1='rc> '\nalias hi='echo hello-alias'\ngreet() { echo \"greet $1\"; }\n\
              PROMPT_COMMAND='__user_pc=$?'\n";
    let home = home("rc", rc);
    let (term, _events) = TerminalHandle::spawn(bash(&home)).unwrap();
    let at_prompt = |t: &TerminalHandle| {
        let snap = t.snapshot(Region::Screen);
        snap.text.lines().nth(snap.cursor_row as usize) == Some("rc>")
    };
    wait_for(&term, "the user's prompt", at_prompt);
    term.write(b"hi; greet x; false\r").unwrap();
    wait_for(&term, "the command", |t| {
        t.commands().first().is_some_and(|r| !r.is_running()) && at_prompt(t)
    });
    // The user's PROMPT_COMMAND still sees the command's exit code.
    term.write(b"echo \"pc=$__user_pc\"\r").unwrap();
    wait_for(&term, "the second command", |t| {
        t.commands().get(1).is_some_and(|r| !r.is_running())
    });
    let records = term.commands();
    assert_eq!(records[0].command_text, "hi; greet x; false");
    assert_eq!(records[0].exit_code, Some(1));
    assert_eq!(last_command(&term).1, "pc=1");
    let screen = term.snapshot(Region::Screen).text;
    println!("screen:\n{screen}");
    assert!(
        screen.contains("rc> hi; greet x; false\nhello-alias\ngreet x\nrc> echo"),
        "{screen}"
    );
}

#[test]
fn records_whose_output_left_the_scrollback_are_truncated() {
    let home = home("evict", "PS1='$ '\n");
    let (term, _events) = TerminalHandle::spawn(bash(&home)).unwrap();
    let first = run(&term, b"echo first", 1);
    assert!(!first.truncated);
    // More lines than the 10,000-line scrollback.
    let seq = run(&term, b"seq 1 12000", 2);
    let records = term.commands();
    assert!(records[0].truncated, "{records:#?}");
    assert!(records[1].truncated, "seq's first lines are gone too");
    assert_eq!(seq.exit_code, Some(0));

    let (last, output) = last_command(&term);
    let LastCommand::Ran {
        record,
        output_capped,
    } = last
    else {
        panic!("expected a command, got {last:?}");
    };
    assert!(record.truncated && output_capped);
    assert!(
        output.ends_with("\n11999\n12000"),
        "{}",
        &output[output.len() - 30..]
    );
    assert!(output.len() <= chatty_terminal::LAST_COMMAND_OUTPUT_BYTES);
}

#[test]
fn clearing_the_scrollback_truncates_what_was_in_it() {
    let home = home("clear", "PS1='$ '\n");
    let (term, _events) = TerminalHandle::spawn(bash(&home)).unwrap();
    run(&term, b"echo keep", 1);
    run(&term, b"seq 1 50", 2);
    assert!(
        term.commands().iter().all(|r| !r.truncated),
        "in scrollback"
    );
    run(&term, b"printf '\\e[3J'", 3);
    let records = term.commands();
    assert!(records[0].truncated && records[1].truncated, "{records:#?}");
    assert!(!records[2].truncated, "on screen");
}

/// zsh from `CHATTY_TEST_ZSH`, else `zsh` on `PATH`; the test is skipped
/// without one. `CHATTY_TEST_ZSH_MODULES` sets `module_path` for a zsh
/// unpacked outside its install prefix.
fn zsh() -> Option<String> {
    if let Ok(zsh) = std::env::var("CHATTY_TEST_ZSH") {
        return Some(zsh);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("zsh"))
        .find(|zsh| zsh.is_file())
        .map(|zsh| zsh.to_string_lossy().into_owned())
}

#[test]
fn zsh_records_through_zdotdir_and_restores_it() {
    let Some(zsh) = zsh() else {
        println!("skipped: no zsh");
        return;
    };
    let home = home("zsh", "");
    std::fs::write(
        home.join(".zshenv"),
        "[[ -n $CHATTY_TEST_ZSH_MODULES ]] && module_path=($CHATTY_TEST_ZSH_MODULES)\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".zshrc"),
        "PS1='$ '\nalias hi='echo hello-zsh'\nunsetopt PROMPT_SP\n",
    )
    .unwrap();
    let (term, _events) = TerminalHandle::spawn(TerminalConfig {
        shell: Some(zsh),
        ..bash(&home)
    })
    .unwrap();
    let f = run(&term, b"false", 1);
    let hi = run(&term, b"hi; echo zd=${ZDOTDIR-unset}", 2);
    assert_eq!(f.exit_code, Some(1));
    assert_eq!(hi.command_text, "hi; echo zd=${ZDOTDIR-unset}");
    assert_eq!(last_command(&term).1, "hello-zsh\nzd=unset");
}

#[test]
fn records_stay_on_their_lines_across_a_resize() {
    let home = home("resize", "PS1='$ '\n");
    let (term, _events) = TerminalHandle::spawn(bash(&home)).unwrap();
    run(&term, b"seq 1 30", 1);
    term.resize(40, 10).unwrap();
    let ab = run(&term, b"echo a; echo b", 2);
    assert_eq!(last_command(&term).1, "a\nb");
    term.resize(100, 30).unwrap();
    assert_eq!(last_command(&term).1, "a\nb", "{ab:?}");
    assert!(term.commands().iter().all(|r| !r.truncated));
}
