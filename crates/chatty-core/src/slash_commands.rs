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
//! `/model`, `/tools`, `/modules`, `/agents`, `/update` and `/quit` are
//! TUI-only by design, not an oversight: GPUI already exposes model
//! selection, execution settings, module settings and the agent roster
//! through dedicated UI (picker, Settings pages), has its own auto-update
//! notification flow, and is closed like any other desktop window — a text
//! command would just duplicate existing UI.

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
        description: "Run an agent: /agent <name> <prompt>, or a sub-agent with a prompt",
        insert_text: "/agent ",
        execute_immediately: false,
        gpui: true,
        tui: true,
    },
    SlashCommandSpec {
        command: "/agents",
        description: "List the agents /agent and invoke_agent can reach",
        insert_text: "/agents",
        execute_immediately: true,
        // The desktop lists them on Settings → Agents.
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/swarm",
        description: "Print this conversation's swarm tree",
        insert_text: "/swarm",
        execute_immediately: true,
        // The desktop renders the tree in its own panel (TB-4).
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/msg",
        description: "Message a running agent: /msg <agent> <text>, read at its next tool call",
        insert_text: "/msg ",
        execute_immediately: false,
        // The desktop's transcript sheet has a message box (TM-5).
        gpui: false,
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
        command: "/paste",
        description: "Show the full text of an elided paste, e.g. /paste 1",
        insert_text: "/paste ",
        execute_immediately: false,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/verbose",
        description: "Show or fold full tool-call payloads",
        insert_text: "/verbose",
        execute_immediately: true,
        gpui: false,
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
    // The desktop hands a conversation over from the sidebar menu (HX-16).
    SlashCommandSpec {
        command: "/send-to-cloud",
        description: "Show what sending this conversation to your Hive account takes along; /send-to-cloud confirm sends it",
        insert_text: "/send-to-cloud",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/continue-here",
        description: "Continue a cloud or other-device conversation on this computer",
        insert_text: "/continue-here",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/login",
        description: "Sign in to Hive as this terminal app (its own sign-in, separate from the desktop's)",
        insert_text: "/login",
        execute_immediately: true,
        gpui: false,
        tui: true,
    },
    SlashCommandSpec {
        command: "/logout",
        description: "Sign this terminal app out of Hive",
        insert_text: "/logout",
        execute_immediately: true,
        gpui: false,
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
#[cfg(test)]
pub fn tui_commands() -> impl Iterator<Item = &'static SlashCommandSpec> {
    SLASH_COMMANDS.iter().filter(|c| c.tui)
}

// ── /compact ────────────────────────────────────────────────────────────────

use anyhow::Result;
use rig_core::completion::Message;

use crate::factories::AgentClient;
use crate::models::Conversation;
use crate::token_budget::summarize_oldest_half;

/// Fewest messages a conversation needs before `/compact` will summarize it.
pub const COMPACT_MIN_MESSAGES: usize = 4;

/// What `/compact` did to a history, ready to be applied with
/// [`Conversation::apply_compaction`].
#[derive(Debug, Clone)]
pub struct Compaction {
    /// The replacement history: a summary message plus the kept tail.
    pub new_history: Vec<Message>,
    /// Index in the *original* history where the kept tail starts
    /// (`len / 2`).
    pub midpoint: usize,
    /// Number of original messages collapsed into the summary.
    pub messages_summarized: usize,
    /// Rough estimate of the tokens freed.
    pub estimated_tokens_freed: usize,
}

/// The result of `/compact`, shared by the desktop app and the TUI so both
/// apply the same rule and show the same text (AGE-1323).
#[derive(Debug, Clone)]
pub enum CompactOutcome {
    /// Fewer than [`COMPACT_MIN_MESSAGES`] messages; nothing was summarized.
    TooShort,
    /// The oldest half was summarized.
    Compacted(Compaction),
}

impl CompactOutcome {
    /// The line both frontends show the user for this outcome.
    pub fn message(&self) -> String {
        match self {
            CompactOutcome::TooShort => format!(
                "Conversation is too short to compact (need at least {COMPACT_MIN_MESSAGES} messages)."
            ),
            CompactOutcome::Compacted(c) => format!(
                "Compacted conversation: summarized {} messages (~{} tokens freed).",
                c.messages_summarized, c.estimated_tokens_freed
            ),
        }
    }
}

/// Where `/compact` splits a history of `len` messages (the first `midpoint`
/// are summarized, the rest kept), or `None` when it is too short.
pub fn compact_midpoint(len: usize) -> Option<usize> {
    (len >= COMPACT_MIN_MESSAGES).then_some(len / 2)
}

/// `/compact`: summarize the oldest half of `history` with `agent`.
///
/// Does not touch any conversation; on [`CompactOutcome::Compacted`] the
/// caller applies the result with [`Conversation::apply_compaction`].
pub async fn compact_history(agent: &AgentClient, history: &[Message]) -> Result<CompactOutcome> {
    let Some(midpoint) = compact_midpoint(history.len()) else {
        return Ok(CompactOutcome::TooShort);
    };
    let result = summarize_oldest_half(agent, history).await?;
    Ok(CompactOutcome::Compacted(Compaction {
        new_history: result.new_history,
        midpoint,
        messages_summarized: result.messages_summarized,
        estimated_tokens_freed: result.estimated_tokens_freed,
    }))
}

impl Conversation {
    /// Replace this conversation's history with a [`Compaction`].
    pub fn apply_compaction(&mut self, compaction: Compaction) {
        self.replace_history(compaction.new_history, compaction.midpoint);
    }
}

// ── /agent <remote A2A agent> ───────────────────────────────────────────────

/// What a remote A2A agent answered to a delegated prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2aRun {
    /// False when the agent reported `failed`, the stream broke or the call
    /// could not start.
    pub success: bool,
    /// The agent's concatenated artifact text; on failure the error line
    /// (prefixed with a warning sign). May be empty on success.
    pub response: String,
}

/// Send `prompt` to the remote A2A agent `config` over SSE and collect its
/// answer, calling `on_progress` with each `working` status message. Uses the
/// delegation client: a remote agent's answer takes as long as it takes, and
/// only silence is a failure (AGE-319). Shared by the desktop app and the TUI
/// (AGE-1323); each decides how to show progress and the result.
pub async fn run_a2a_delegation(
    config: &crate::settings::models::a2a_store::A2aAgentConfig,
    prompt: &str,
    mut on_progress: impl FnMut(&str),
) -> A2aRun {
    use crate::services::a2a_client::A2aStreamEvent;
    use futures::StreamExt;

    let client = crate::services::A2aClient::for_delegation();
    let mut stream = match client.send_message_stream(config, prompt).await {
        Ok(stream) => stream,
        Err(e) => {
            return A2aRun {
                success: false,
                response: format!("\u{26a0}\u{fe0f} A2A error: {e:#}"),
            };
        }
    };

    let mut response = String::new();
    let mut success = true;
    while let Some(event) = stream.next().await {
        match event {
            Ok(A2aStreamEvent::StatusUpdate { state, message, .. }) => {
                if state == "failed" {
                    success = false;
                    if let Some(msg) = message {
                        response = format!("\u{26a0}\u{fe0f} {msg}");
                    }
                } else if state == "working"
                    && let Some(msg) = message
                {
                    on_progress(&msg);
                }
            }
            Ok(A2aStreamEvent::ArtifactUpdate { text, .. }) => response.push_str(&text),
            Err(e) => {
                success = false;
                response = format!("\u{26a0}\u{fe0f} A2A error: {e:#}");
                break;
            }
        }
    }
    A2aRun { success, response }
}

// ── Slash menu model ────────────────────────────────────────────────────────

/// A skill loaded from the filesystem for the slash menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillEntry {
    /// Short directory name of the skill (e.g. `"fix-ci"`).
    pub name: String,
    /// Description from the skill's frontmatter.
    pub description: String,
}

/// An agent the `/agent ` picker offers (AGE-761).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPickerEntry {
    /// The name `/agent <name> <prompt>` dispatches to.
    pub name: String,
    /// The spec's description, or what a remote agent's config says.
    pub description: String,
}

/// A row in the slash menu: a built-in command, a filesystem skill, or, after
/// `/agent `, a reachable agent. Only the desktop app produces the agent rows.
#[derive(Clone, Debug, PartialEq)]
pub enum SlashMenuItem {
    Command(&'static SlashCommandSpec),
    Skill(SkillEntry),
    Agent(AgentPickerEntry),
    /// The `/agent ` picker's only row when there is no agent to offer.
    NoAgents,
}

impl SlashMenuItem {
    /// The slash-prefixed display string (e.g. `/compact` or `/fix-ci`).
    pub fn display_command(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.command.to_string(),
            SlashMenuItem::Skill(skill) => format!("/{}", skill.name),
            SlashMenuItem::Agent(agent) => agent.name.clone(),
            SlashMenuItem::NoAgents => "No agents available".to_string(),
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &str {
        match self {
            SlashMenuItem::Command(cmd) => cmd.description,
            SlashMenuItem::Skill(skill) => &skill.description,
            SlashMenuItem::Agent(agent) => &agent.description,
            SlashMenuItem::NoAgents => "",
        }
    }

    /// Whether the item should be applied immediately (no arg input needed).
    pub fn execute_immediately(&self) -> bool {
        match self {
            SlashMenuItem::Command(cmd) => cmd.execute_immediately,
            // Skills insert a prompt the user can review and extend first.
            SlashMenuItem::Skill(_) | SlashMenuItem::Agent(_) | SlashMenuItem::NoAgents => false,
        }
    }

    /// Text to insert into the input when this item is selected.
    pub fn insert_text(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.insert_text.to_string(),
            SlashMenuItem::Skill(skill) => format!("Use the '{}' skill: ", skill.name),
            SlashMenuItem::Agent(agent) => format!("/agent {} ", agent.name),
            SlashMenuItem::NoAgents => String::new(),
        }
    }

    /// True for a filesystem skill.
    pub fn is_skill(&self) -> bool {
        matches!(self, SlashMenuItem::Skill(_))
    }

    /// Whether Enter, Tab or a click can apply it: every row but the empty
    /// agent picker's placeholder.
    pub fn is_selectable(&self) -> bool {
        !matches!(self, SlashMenuItem::NoAgents)
    }
}

/// Which frontend's command subset a menu lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuFrontend {
    Gpui,
    Tui,
}

/// Commands (for `frontend`) then skills whose names start with `query`
/// (case-insensitive; an empty query matches everything).
pub fn command_and_skill_items(
    query: &str,
    frontend: MenuFrontend,
    skills: &[SkillEntry],
) -> Vec<SlashMenuItem> {
    let query = query.to_ascii_lowercase();
    let mut items: Vec<SlashMenuItem> = SLASH_COMMANDS
        .iter()
        .filter(|cmd| match frontend {
            MenuFrontend::Gpui => cmd.gpui,
            MenuFrontend::Tui => cmd.tui,
        })
        .filter(|cmd| {
            query.is_empty()
                || cmd
                    .command
                    .trim_start_matches('/')
                    .to_ascii_lowercase()
                    .starts_with(&query)
        })
        .map(SlashMenuItem::Command)
        .collect();
    items.extend(
        skills
            .iter()
            .filter(|skill| query.is_empty() || skill.name.to_ascii_lowercase().starts_with(&query))
            .map(|skill| SlashMenuItem::Skill(skill.clone())),
    );
    items
}

/// How Up/Down behave at the ends of the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuWrap {
    /// Stop at the first/last row (the terminal UI).
    Clamp,
    /// Jump to the other end (the desktop app).
    Wrap,
}

/// Selection, scroll offset and the last filter of a slash menu. Frontends
/// own the rows (they depend on live input and settings) and only render
/// them; the state moves here so both behave the same. The desktop app
/// mirrors [`SlashMenu::selected`] into its own scroll handle; the terminal
/// stores the list widget's offset in [`SlashMenu::set_offset`].
#[derive(Clone, Debug)]
pub struct SlashMenu {
    wrap: MenuWrap,
    selected: usize,
    offset: usize,
    filter: Option<String>,
}

impl SlashMenu {
    pub fn new(wrap: MenuWrap) -> Self {
        Self {
            wrap,
            selected: 0,
            offset: 0,
            filter: None,
        }
    }

    /// Raw highlighted index; may exceed the row count until normalized.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The highlighted index clamped into `len` rows (0 when empty).
    pub fn selected_in(&self, len: usize) -> usize {
        self.selected.min(len.saturating_sub(1))
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    /// Highlight `index` (hover, click).
    pub fn select(&mut self, index: usize) {
        self.selected = index;
    }

    /// Back to the first row, scrolled to the top. Keeps the filter.
    pub fn reset(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    /// Forget the filter too, so the next query starts fresh.
    pub fn clear(&mut self) {
        self.reset();
        self.filter = None;
    }

    /// Pull the selection back inside `len` rows; with none, reset.
    pub fn normalize(&mut self, len: usize) {
        if len == 0 {
            self.reset();
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    /// Up one row; at the top it wraps or stays, per [`MenuWrap`].
    pub fn move_up(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        match self.wrap {
            MenuWrap::Wrap => {
                self.selected = if self.selected == 0 {
                    len - 1
                } else {
                    self.selected - 1
                };
            }
            MenuWrap::Clamp => {
                self.normalize(len);
                self.selected = self.selected.saturating_sub(1);
            }
        }
    }

    /// Down one row; at the bottom it wraps or stays, per [`MenuWrap`].
    pub fn move_down(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        match self.wrap {
            MenuWrap::Wrap => self.selected = (self.selected + 1) % len,
            MenuWrap::Clamp => {
                self.normalize(len);
                if self.selected + 1 < len {
                    self.selected += 1;
                }
            }
        }
    }

    /// Record the menu's filter; when it differs (ignoring ASCII case) from
    /// the previous one the selection resets. Returns whether it did.
    /// Events that leave the filter alone, like the newline appended before
    /// Enter, keep the arrow-key selection.
    pub fn set_filter(&mut self, filter: &str) -> bool {
        let changed = self
            .filter
            .as_deref()
            .map(|prev| !prev.eq_ignore_ascii_case(filter))
            .unwrap_or(true);
        if changed {
            self.reset();
            self.filter = Some(filter.to_ascii_lowercase());
        }
        changed
    }
}

/// The `/agent ` picker's query: the one word after `/agent `, `None` when
/// the picker is not in play, before the space or once a second space
/// follows the name (AGE-761).
pub fn agent_query(input_text: &str) -> Option<&str> {
    // gpui-component appends Enter's newline to the buffer before it fires
    // `PressEnter`; that is not a space the user typed.
    let text = input_text.trim_start().trim_end_matches(['\n', '\r']);
    let query = text.strip_prefix("/agent ")?;
    (!query.contains(char::is_whitespace)).then_some(query)
}

/// The `/agent ` picker's rows for `input_text`, `None` when the picker is
/// not in play. Names starting with the query come first, then names
/// containing it. With no agent at all, a bare `/agent ` shows the one
/// "No agents available" row; a typed word then closes the picker.
pub fn agent_menu_items(
    input_text: &str,
    agents: &[AgentPickerEntry],
) -> Option<Vec<SlashMenuItem>> {
    let query = agent_query(input_text)?.to_lowercase();
    if agents.is_empty() {
        return Some(if query.is_empty() {
            vec![SlashMenuItem::NoAgents]
        } else {
            Vec::new()
        });
    }
    let (mut items, substring): (Vec<_>, Vec<_>) = agents
        .iter()
        .filter(|agent| agent.name.to_lowercase().contains(&query))
        .partition(|agent| agent.name.to_lowercase().starts_with(&query));
    items.extend(substring);
    Some(
        items
            .into_iter()
            .map(|agent| SlashMenuItem::Agent(agent.clone()))
            .collect(),
    )
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

    #[test]
    fn compact_midpoint_needs_four_messages_and_splits_at_half() {
        assert_eq!(compact_midpoint(0), None);
        assert_eq!(compact_midpoint(3), None);
        assert_eq!(compact_midpoint(4), Some(2));
        assert_eq!(compact_midpoint(5), Some(2));
        assert_eq!(compact_midpoint(11), Some(5));
    }

    #[test]
    fn compact_outcome_texts() {
        assert_eq!(
            CompactOutcome::TooShort.message(),
            "Conversation is too short to compact (need at least 4 messages)."
        );
        let done = CompactOutcome::Compacted(Compaction {
            new_history: Vec::new(),
            midpoint: 2,
            messages_summarized: 2,
            estimated_tokens_freed: 37,
        });
        assert_eq!(
            done.message(),
            "Compacted conversation: summarized 2 messages (~37 tokens freed)."
        );
    }

    fn skill(name: &str) -> SkillEntry {
        SkillEntry {
            name: name.into(),
            description: format!("{name} skill"),
        }
    }

    #[test]
    fn items_filter_commands_then_skills_per_frontend() {
        let skills = [skill("fix-ci"), skill("compose")];
        let tui = command_and_skill_items("com", MenuFrontend::Tui, &skills);
        let names: Vec<String> = tui.iter().map(SlashMenuItem::display_command).collect();
        assert_eq!(names, ["/compact", "/compose"]);
        // /model is TUI-only.
        assert!(
            command_and_skill_items("mod", MenuFrontend::Gpui, &[])
                .iter()
                .all(|i| i.display_command() != "/model")
        );
        assert!(
            command_and_skill_items("MOD", MenuFrontend::Tui, &[])
                .iter()
                .any(|i| i.display_command() == "/model")
        );
        assert!(command_and_skill_items("zzz", MenuFrontend::Tui, &skills).is_empty());
        let all = command_and_skill_items("", MenuFrontend::Gpui, &skills);
        assert_eq!(all.len(), gpui_commands().count() + 2);
    }

    #[test]
    fn menu_clamps_at_both_ends() {
        let mut menu = SlashMenu::new(MenuWrap::Clamp);
        menu.move_up(3);
        assert_eq!(menu.selected(), 0);
        menu.move_down(3);
        menu.move_down(3);
        menu.move_down(3);
        assert_eq!(menu.selected(), 2);
        menu.move_up(3);
        assert_eq!(menu.selected(), 1);
    }

    #[test]
    fn menu_wraps_at_both_ends() {
        let mut menu = SlashMenu::new(MenuWrap::Wrap);
        menu.move_up(3);
        assert_eq!(menu.selected(), 2);
        menu.move_down(3);
        assert_eq!(menu.selected(), 0);
        menu.move_down(3);
        assert_eq!(menu.selected(), 1);
    }

    #[test]
    fn menu_moves_ignore_an_empty_list() {
        for wrap in [MenuWrap::Clamp, MenuWrap::Wrap] {
            let mut menu = SlashMenu::new(wrap);
            menu.move_up(0);
            menu.move_down(0);
            assert_eq!(menu.selected(), 0);
            assert_eq!(menu.selected_in(0), 0);
        }
    }

    #[test]
    fn clamp_menu_pulls_a_stale_selection_back_before_moving() {
        let mut menu = SlashMenu::new(MenuWrap::Clamp);
        menu.select(9);
        menu.move_up(3);
        assert_eq!(menu.selected(), 1);
        menu.select(9);
        menu.move_down(3);
        assert_eq!(menu.selected(), 2);
        assert_eq!(menu.selected_in(2), 1);
        menu.set_offset(4);
        menu.normalize(0);
        assert_eq!((menu.selected(), menu.offset()), (0, 0));
    }

    #[test]
    fn filter_change_resets_selection_and_offset_only_when_it_differs() {
        let mut menu = SlashMenu::new(MenuWrap::Wrap);
        assert!(menu.set_filter("co"));
        menu.select(2);
        menu.set_offset(1);
        assert!(!menu.set_filter("CO"));
        assert_eq!((menu.selected(), menu.offset()), (2, 1));
        assert!(menu.set_filter("com"));
        assert_eq!((menu.selected(), menu.offset()), (0, 0));
        menu.select(1);
        menu.clear();
        assert!(menu.set_filter("com"));
    }

    #[test]
    fn agent_rows_filter_prefix_before_substring() {
        let agents = [
            AgentPickerEntry {
                name: "data-panel".into(),
                description: String::new(),
            },
            AgentPickerEntry {
                name: "panel-lead".into(),
                description: String::new(),
            },
        ];
        let rows = agent_menu_items("/agent pan", &agents).unwrap();
        let names: Vec<String> = rows.iter().map(SlashMenuItem::display_command).collect();
        assert_eq!(names, ["panel-lead", "data-panel"]);
        assert_eq!(
            agent_menu_items("/agent ", &[]).unwrap(),
            vec![SlashMenuItem::NoAgents]
        );
        assert!(agent_menu_items("/agent x", &[]).unwrap().is_empty());
        assert!(agent_menu_items("/agent a b", &agents).is_none());
        assert!(agent_menu_items("/agents", &agents).is_none());
    }
}
