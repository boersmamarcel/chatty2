//! OS notifications for cloud conversations (HX-12).
//!
//! No notification crate: the desktop already shells out to the platform for
//! its one other OS integration (the CLI installer's `osascript`), so this
//! does the same rather than add a dependency.
//!
//! * **Linux** — `notify-send`. With `--action`/`--wait` (libnotify ≥ 0.8) it
//!   prints the action's name when the notification is clicked, which is how
//!   a click opens the conversation. An older `notify-send` rejects those
//!   flags and is run again without them: the notification shows, a click
//!   does nothing.
//! * **macOS** — `osascript`'s `display notification`. It has no click
//!   callback, so the notification is informational there; clicking it brings
//!   up whatever the system opens for a script notification.
//! * **Windows** — not implemented; the call logs and does nothing.
//!
//! A machine with no notification service (no `notify-send`, no session bus)
//! gets a debug log line and nothing else: a missing notification is never
//! worth an error dialog.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Stdio;

use tracing::debug;

/// The action name `notify-send` prints when the notification is clicked.
const OPEN_ACTION: &str = "open";

/// `notify-send` arguments for a notification that reports a click.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn notify_send_args(title: &str, body: &str, with_action: bool) -> Vec<String> {
    let mut args = vec!["--app-name=Chatty".to_string()];
    if with_action {
        args.push(format!("--action={OPEN_ACTION}=Open"));
        args.push("--wait".to_string());
    }
    // `--` so a title that starts with a dash is not read as an option.
    args.extend(["--".to_string(), title.to_string(), markup_escaped(body)]);
    args
}

/// `text` as notification body markup: the freedesktop spec lets a server
/// read the body as a small HTML subset, so a conversation title like
/// `Q&A <draft>` is shown as written instead of being parsed (or dropped as
/// broken markup), and a title cannot add a link.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn markup_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Did the user click the notification, going by `notify-send --wait`'s
/// output?
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn was_clicked(stdout: &str) -> bool {
    stdout.trim() == OPEN_ACTION
}

/// An AppleScript string literal for `text`: backslashes and quotes escaped.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn applescript_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // A line break inside the literal would end the statement.
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Show a notification. Resolves to `true` when the user clicked it, on the
/// platforms that can tell.
pub async fn show(title: &str, body: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        for with_action in [true, false] {
            let output = tokio::process::Command::new("notify-send")
                .args(notify_send_args(title, body, with_action))
                .stdin(Stdio::null())
                .output()
                .await;
            match output {
                Ok(output) if output.status.success() => {
                    return with_action && was_clicked(&String::from_utf8_lossy(&output.stdout));
                }
                // An old libnotify: try again without the click action.
                Ok(_) => continue,
                Err(error) => {
                    debug!(?error, "No notification service (notify-send)");
                    return false;
                }
            }
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification {} with title {}",
            applescript_literal(body),
            applescript_literal(title)
        );
        if let Err(error) = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .output()
            .await
        {
            debug!(?error, "Could not show a notification (osascript)");
        }
        false
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (title, body);
        debug!("OS notifications are not implemented on this platform");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_is_the_open_action_on_stdout() {
        assert!(was_clicked("open\n"));
        assert!(!was_clicked(""));
        assert!(!was_clicked("other\n"));
    }

    #[test]
    fn the_title_cannot_be_read_as_an_option() {
        let args = notify_send_args("--urgency=critical", "body", true);
        let dashdash = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dashdash + 1], "--urgency=critical");
        assert!(args.contains(&"--wait".to_string()));
        assert!(!notify_send_args("t", "b", false).contains(&"--wait".to_string()));
    }

    #[test]
    fn the_body_is_escaped_as_markup() {
        let args = notify_send_args("Chatty", "Finished: Q&A <a href=\"x\">draft</a>", false);
        assert_eq!(
            args.last().unwrap(),
            "Finished: Q&amp;A &lt;a href=\"x\"&gt;draft&lt;/a&gt;"
        );
    }

    #[test]
    fn applescript_text_is_escaped() {
        assert_eq!(
            applescript_literal("say \"hi\" \\ now\nplease"),
            "\"say \\\"hi\\\" \\\\ now please\""
        );
    }
}
