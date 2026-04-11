//! Standalone utility functions for the TUI engine.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;

use crate::events::AppEvent;

pub(super) fn common_ancestor(left: &Path, right: &Path) -> Option<PathBuf> {
    let mut ancestor = PathBuf::new();
    for (l, r) in left.components().zip(right.components()) {
        if l == r {
            match l {
                Component::RootDir => ancestor.push(Path::new("/")),
                _ => ancestor.push(l.as_os_str()),
            }
        } else {
            break;
        }
    }
    if ancestor.as_os_str().is_empty() {
        None
    } else {
        Some(ancestor)
    }
}

pub(super) fn copy_text_to_clipboard(text: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        copy_via_command("pbcopy", &[], text)
    }
    #[cfg(target_os = "windows")]
    {
        return copy_via_command("clip", &[], text);
    }
    #[cfg(target_os = "linux")]
    {
        if copy_via_command("wl-copy", &[], text).is_ok() {
            return Ok(());
        }
        if copy_via_command("xclip", &["-selection", "clipboard"], text).is_ok() {
            return Ok(());
        }
        bail!("No clipboard utility found. Install wl-clipboard or xclip.")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = text;
        bail!("Clipboard copy is not supported on this platform")
    }
}

fn copy_via_command(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = ProcessCommand::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to launch '{}'", program))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .context("Failed to write clipboard contents")?;
    }

    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        bail!("'{}' returned non-zero exit status", program)
    }
}

pub(super) fn run_sub_agent_process(
    executable: PathBuf,
    model_id: String,
    prompt: String,
    auto_approve: bool,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<String> {
    use std::io::BufRead as _;

    let mut command = ProcessCommand::new(executable);
    command
        .arg("--headless")
        .arg("--model")
        .arg(model_id)
        .arg("--message")
        .arg(prompt)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if auto_approve {
        command.arg("--auto-approve");
    }

    let mut child = command
        .spawn()
        .context("Failed to launch sub-agent process")?;

    // Drain stderr in a background thread, forwarding each line as a progress event.
    let stderr = child.stderr.take();
    let stderr_thread = std::thread::spawn(move || {
        if let Some(stderr) = stderr {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let _ = event_tx.send(AppEvent::SubAgentProgress(line));
            }
        }
    });

    // Wait for the process and collect stdout (stderr was already taken above).
    let output = child
        .wait_with_output()
        .context("Failed to wait for sub-agent process")?;

    // Ensure the stderr thread has finished before we return.
    let _ = stderr_thread.join();

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        bail!(
            "exit code {:?}: sub-agent process failed",
            output.status.code()
        )
    }
}

pub(crate) fn sanitize_progress_line(line: &str) -> String {
    let mut cleaned = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('\u{7}') => break,
                            Some('\u{1b}') => {
                                if chars.next_if_eq(&'\\').is_some() {
                                    break;
                                }
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        if ch.is_control() && ch != '\t' {
            continue;
        }

        cleaned.push(ch);
    }

    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{common_ancestor, sanitize_progress_line};
    use crate::engine::commands::Command;
    use std::path::Path;

    #[test]
    fn parses_new_slash_commands() {
        use crate::engine::ChatEngine;
        assert_eq!(
            ChatEngine::parse_command("/add-dir ./src"),
            Some(Command::AddDir(Some("./src".to_string())))
        );
        assert_eq!(
            ChatEngine::parse_command("/agent summarize this"),
            Some(Command::Agent(Some("summarize this".to_string())))
        );
        assert_eq!(
            ChatEngine::parse_command("/clear"),
            Some(Command::Clear)
        );
        assert_eq!(
            ChatEngine::parse_command("/new"),
            Some(Command::Clear)
        );
        assert_eq!(
            ChatEngine::parse_command("/compact"),
            Some(Command::Compact)
        );
        assert_eq!(
            ChatEngine::parse_command("/context"),
            Some(Command::Context)
        );
        assert_eq!(
            ChatEngine::parse_command("/copy"),
            Some(Command::Copy)
        );
        assert_eq!(
            ChatEngine::parse_command("/cwd"),
            Some(Command::Cwd(None))
        );
        assert_eq!(
            ChatEngine::parse_command("/cd ../workspace"),
            Some(Command::Cwd(Some("../workspace".to_string())))
        );
    }

    #[test]
    fn computes_common_ancestor_for_paths() {
        let left = Path::new("/home/user/project/src");
        let right = Path::new("/home/user/project/docs");
        let ancestor = common_ancestor(left, right).unwrap();
        assert_eq!(ancestor, Path::new("/home/user/project"));
    }

    #[test]
    fn strips_ansi_and_control_sequences_from_progress_lines() {
        let line = "\u{1b}[2K\r\u{1b}[0;32mResolving dependencies...\u{1b}[0m";
        assert_eq!(sanitize_progress_line(line), "Resolving dependencies...");
    }

    #[test]
    fn keeps_tabs_in_progress_lines() {
        assert_eq!(
            sanitize_progress_line("Step\t1:\tPreparing"),
            "Step\t1:\tPreparing"
        );
    }

    #[test]
    fn strips_osc_sequences_from_progress_lines() {
        let line = "\u{1b}]0;chatty\u{7}Installing tools";
        assert_eq!(sanitize_progress_line(line), "Installing tools");
    }

    #[test]
    fn strips_standalone_escape_characters() {
        let line = "\u{1b}Resolving dependencies";
        assert_eq!(sanitize_progress_line(line), "Resolving dependencies");
    }
}
