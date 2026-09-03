//! Shared slash-command catalog for the chat input, used by both the GPUI
//! desktop app and the terminal UI.
//!
//! [`SLASH_COMMANDS`] is the single source of truth for every built-in
//! command; each entry's `gpui`/`tui` flags say which frontend(s) render it,
//! so the two catalogs cannot silently drift apart the way separate
//! hand-maintained lists did before (AGE-172). Adding a command here is the
//! core change; each UI only needs thin dispatch code for what happens when
//! it's selected — see `slash_commands.rs` in chatty-gpui / `ui/input.rs` in
//! chatty-tui.
//!
//! `/model`, `/tools`, `/modules`, `/update` and `/quit` are TUI-only by
//! design, not an oversight: GPUI already exposes model selection, execution
//! settings and module settings through dedicated UI (picker, Settings
//! pages), has its own auto-update notification flow, and is closed like any
//! other desktop window — a text command would just duplicate existing UI.

/// A single built-in slash command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommandSpec {
    /// The command as typed, e.g. `"/compact"`.
    pub command: &'static str,
    /// One-line description shown in the picker.
    pub description: &'static str,
    /// Text inserted into the input when the command is selected.
    pub insert_text: &'static str,
    /// When true the command is sent immediately on selection; when false
    /// `insert_text` is placed into the input so the user can add arguments.
    pub execute_immediately: bool,
    /// Available in the GPUI desktop app.
    pub gpui: bool,
    /// Available in the terminal UI.
    pub tui: bool,
}

/// All built-in slash commands. See the module docs for the `gpui`/`tui`
/// availability policy.
pub const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        command: "/model",
        description: "Switch model or open model picker",
        insert_text: "/model",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/tools",
        description: "Toggle tools or open tool picker",
        insert_text: "/tools",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/modules",
        description: "Show or update module runtime settings",
        insert_text: "/modules",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/add-dir",
        description: "Add a directory to allowed workspace access",
        insert_text: "/add-dir ",
        execute_immediately: false,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/agent",
        description: "Launch a sub-agent with a prompt",
        insert_text: "/agent ",
        execute_immediately: false,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/clear",
        description: "Clear conversation history",
        insert_text: "/clear",
        execute_immediately: true,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/new",
        description: "Start a new conversation",
        insert_text: "/new",
        execute_immediately: true,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/compact",
        description: "Summarize conversation history to reduce context",
        insert_text: "/compact",
        execute_immediately: true,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/context",
        description: "Show context window usage",
        insert_text: "/context",
        execute_immediately: true,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/copy",
        description: "Copy latest response to clipboard",
        insert_text: "/copy",
        execute_immediately: true,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/update",
        description: "Trigger CLI auto-update (if installed)",
        insert_text: "/update",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/cwd",
        description: "Show current working directory",
        insert_text: "/cwd",
        execute_immediately: true,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/cd",
        description: "Change working directory",
        insert_text: "/cd ",
        execute_immediately: false,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/quit",
        description: "Quit Chatty",
        insert_text: "/quit",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
];

/// The subset of [`SLASH_COMMANDS`] available in the GPUI desktop app.
pub fn gpui_commands() -> impl Iterator<Item = &'static SlashCommandSpec> {
    SLASH_COMMANDS.iter().filter(|c| c.gpui)
}

/// The subset of [`SLASH_COMMANDS`] available in the terminal UI.
pub fn tui_commands() -> impl Iterator<Item = &'static SlashCommandSpec> {
    SLASH_COMMANDS.iter().filter(|c| c.tui)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for cmd in SLASH_COMMANDS {
            assert!(
                seen.insert(cmd.command),
                "duplicate command: {}",
                cmd.command
            );
        }
    }

    #[test]
    fn every_command_is_available_on_at_least_one_platform() {
        for cmd in SLASH_COMMANDS {
            assert!(
                cmd.gpui || cmd.tui,
                "{} is not available on any platform",
                cmd.command
            );
        }
    }

    #[test]
    fn gpui_and_tui_subsets_only_contain_flagged_commands() {
        assert!(gpui_commands().all(|c| c.gpui));
        assert!(tui_commands().all(|c| c.tui));
    }
}
