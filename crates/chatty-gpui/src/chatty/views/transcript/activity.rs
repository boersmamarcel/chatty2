use std::rc::Rc;

use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::collapsible::Collapsible as CollapsibleEl;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::tool_row::ToolRow;
use super::verb::tool_row_label;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Edit,
    Explore,
    Search,
    External,
    Command,
    /// A browser control handoff between the user and the agent (AGE-156):
    /// not something the agent did, so it must not be counted as one of
    /// its edits, searches or explorations.
    Handoff,
}

pub fn classify_tool(name: &str) -> ToolKind {
    let n = name.to_ascii_lowercase();
    if n == "browser_take_control" || n == "browser_release_control" {
        return ToolKind::Handoff;
    }
    if n.contains("todo") || n == "verify_completion" {
        // Agent plan tools are Plan blocks, not edits.
        return ToolKind::Explore;
    }
    if n.contains("diff") || n.contains("edit") || n.contains("write") || n.contains("apply") {
        ToolKind::Edit
    } else if n.contains("search") || n.contains("grep") || n.contains("glob") {
        ToolKind::Search
    } else if n.contains("web") || n.contains("fetch") || n.contains("http") || n.contains("mcp") {
        ToolKind::External
    } else if n.contains("bash")
        || n.contains("shell")
        || n.contains("exec")
        || n.contains("command")
    {
        ToolKind::Command
    } else {
        ToolKind::Explore
    }
}

/// Counted sentence: edits → explore → searches → external → commands, then
/// how many of those calls failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTally {
    pub edits: usize,
    pub explore: usize,
    pub searches: usize,
    pub external: usize,
    pub commands: usize,
    pub handoffs: usize,
    pub failed: usize,
    pub added: usize,
    pub removed: usize,
}

impl RunTally {
    pub fn from_tools(tools: &[ToolCallBlock]) -> Self {
        let mut tally = Self::default();
        for tool in tools {
            match classify_tool(&tool.tool_name) {
                ToolKind::Edit => tally.edits += 1,
                ToolKind::Explore => tally.explore += 1,
                ToolKind::Search => tally.searches += 1,
                ToolKind::External => tally.external += 1,
                ToolKind::Command => tally.commands += 1,
                ToolKind::Handoff => tally.handoffs += 1,
            }
            if matches!(tool.state, ToolCallState::Error(_)) {
                tally.failed += 1;
            }
            if let Some(output) = tool.output.as_deref() {
                let (a, r) = count_diff_lines(output);
                tally.added += a;
                tally.removed += r;
            }
        }
        tally
    }

    /// Two-tone spans: optional bold verb + muted remainder. Omit zero categories.
    pub fn phrase_spans(&self) -> Vec<(Option<&'static str>, String)> {
        let mut parts = Vec::new();
        match self.edits {
            0 => {}
            1 => parts.push((Some("Edited"), " 1 file".into())),
            n => parts.push((Some("Edited"), format!(" {n} files"))),
        }
        match self.explore {
            0 => {}
            1 => parts.push((Some("explored"), " 1 file".into())),
            n => parts.push((Some("explored"), format!(" {n} files"))),
        }
        match self.searches {
            0 => {}
            1 => parts.push((None, "1 search".into())),
            n => parts.push((None, format!("{n} searches"))),
        }
        match self.external {
            0 => {}
            1 => parts.push((None, "1 tool".into())),
            n => parts.push((None, format!("{n} tools"))),
        }
        match self.commands {
            0 => {}
            1 => parts.push((Some("ran"), " 1 command".into())),
            n => parts.push((Some("ran"), format!(" {n} commands"))),
        }
        match self.handoffs {
            0 => {}
            1 => parts.push((None, "1 browser handoff".into())),
            n => parts.push((None, format!("{n} browser handoffs"))),
        }
        if parts.is_empty() {
            parts.push((Some("Worked"), String::new()));
        }
        // A quiet count, not a verdict: agents probe paths that turn out not
        // to exist all the time. The failures themselves are in the rows.
        if self.failed > 0 {
            parts.push((None, format!("{} failed", self.failed)));
        }
        parts
    }

    pub fn all_success(tools: &[ToolCallBlock]) -> bool {
        !tools.is_empty()
            && tools
                .iter()
                .all(|t| matches!(t.state, ToolCallState::Success))
    }
}

/// How long a new live action takes to fade in over the previous one.
pub const LIVE_FADE_MS: u64 = 300;

/// "Reading src/main.rs": the present-tense label, even once the call has
/// settled, because the header narrates what the agent is doing, not how
/// each call ended.
pub fn live_headline(tool: &ToolCallBlock) -> String {
    tool_row_label(
        &tool.display_name,
        &tool.tool_name,
        &ToolCallState::Running,
        &tool.input,
        None,
    )
    .headline()
}

fn count_diff_lines(output: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in output.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

type ActivityToggle = Rc<dyn Fn(&mut App)>;

#[derive(IntoElement)]
pub struct ActivityGroup {
    tools: Vec<ToolCallBlock>,
    open: bool,
    on_toggle: Option<ActivityToggle>,
}

impl ActivityGroup {
    pub fn new(tools: Vec<ToolCallBlock>) -> Self {
        Self {
            tools,
            open: true,
            on_toggle: None,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn on_toggle(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for ActivityGroup {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tally = RunTally::from_tools(&self.tools);
        let running = self
            .tools
            .iter()
            .any(|t| matches!(t.state, ToolCallState::Running));
        let open = self.open;

        let on_toggle = self.on_toggle.clone();
        let chevron = if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };

        let mut sentence = div()
            .id("activity-sentence")
            .flex()
            .flex_row()
            .flex_wrap()
            .items_baseline()
            .gap(px(0.))
            .text_xs()
            .min_w_0()
            .flex_1();
        let phrases = tally.phrase_spans();
        for (ix, (verb, rest)) in phrases.into_iter().enumerate() {
            if ix > 0 {
                sentence =
                    sentence.child(div().text_color(cx.theme().muted_foreground).child(", "));
            }
            if let Some(v) = verb {
                let label = if ix == 0 {
                    // Capitalise first verb only.
                    let mut chars = v.chars();
                    match chars.next() {
                        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                        None => v.to_string(),
                    }
                } else {
                    v.to_string()
                };
                sentence = sentence.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(label),
                );
            }
            sentence = sentence.child(div().text_color(cx.theme().muted_foreground).child(rest));
        }

        let header = div()
            .id("activity-header")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .w_full()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                if let Some(cb) = &on_toggle {
                    cb(cx);
                }
            })
            .when(!running && RunTally::all_success(&self.tools), |this| {
                this.child(
                    Icon::new(IconName::Check)
                        .size_3()
                        .text_color(cx.theme().success),
                )
            })
            .child(sentence)
            .when(tally.added > 0, |this| {
                this.child(Tag::success().small().child(format!("+{}", tally.added)))
            })
            .when(tally.removed > 0, |this| {
                this.child(Tag::danger().small().child(format!("−{}", tally.removed)))
            })
            .child(
                Icon::new(chevron)
                    .size_3()
                    .text_color(cx.theme().muted_foreground),
            );

        CollapsibleEl::new()
            .open(open)
            .w_full()
            .bg(cx.theme().group_box)
            .rounded_2xl()
            .overflow_hidden()
            .child(header)
            .content(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .ml_3()
                    .children({
                        // Number repeated failures of the same tool so a retry
                        // is visibly a retry (AGE-187).
                        let mut failures: std::collections::HashMap<String, usize> =
                            std::collections::HashMap::new();
                        self.tools
                            .into_iter()
                            .map(|tool| {
                                let attempt = if matches!(tool.state, ToolCallState::Error(_)) {
                                    let n = failures.entry(tool.tool_name.clone()).or_insert(0);
                                    *n += 1;
                                    *n
                                } else {
                                    1
                                };
                                ToolRow::new(tool).attempt(attempt)
                            })
                            .collect::<Vec<_>>()
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would drag in `gpui::test`, which shadows the
    // built-in `#[test]` attribute.
    use super::{RunTally, ToolKind, classify_tool, live_headline};
    use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};

    #[test]
    fn live_headline_narrates_a_failed_call_like_a_running_one() {
        let tool = ToolCallBlock {
            id: "r".into(),
            tool_name: "read_file".into(),
            display_name: "read_file".into(),
            input: r#"{"path":".opencode/skills/storytelling/SKILL.md"}"#.into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Error("No such file or directory".into()),
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        assert_eq!(
            live_headline(&tool),
            "Reading .opencode/skills/storytelling/SKILL.md"
        );
    }

    #[test]
    fn failed_calls_are_counted_at_the_end_of_the_sentence() {
        let tool = |name: &str, state: ToolCallState| ToolCallBlock {
            id: name.into(),
            tool_name: name.into(),
            display_name: name.into(),
            input: "{}".into(),
            output: None,
            output_preview: None,
            state,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        let missing = || ToolCallState::Error("No such file or directory".into());
        let tools = vec![
            tool("read_file", ToolCallState::Success),
            tool("read_file", missing()),
            tool("read_skill", missing()),
        ];
        let tally = RunTally::from_tools(&tools);
        assert_eq!(tally.failed, 2);
        assert_eq!(
            tally.phrase_spans(),
            vec![
                (Some("explored"), " 3 files".to_string()),
                (None, "2 failed".to_string()),
            ]
        );
        let clean = RunTally::from_tools(&tools[..1]);
        assert_eq!(
            clean.phrase_spans(),
            vec![(Some("explored"), " 1 file".to_string())]
        );
    }

    #[test]
    fn browser_handoffs_are_their_own_kind() {
        assert_eq!(classify_tool("browser_take_control"), ToolKind::Handoff);
        assert_eq!(classify_tool("browser_release_control"), ToolKind::Handoff);
        // The real browser tools are unaffected.
        assert_eq!(classify_tool("browser_navigate"), ToolKind::Explore);
    }

    #[test]
    fn handoffs_are_counted_instead_of_being_filed_as_explored_files() {
        let tools = vec![
            ToolCallBlock::browser_control_handoff(true, "https://example.com"),
            ToolCallBlock::browser_control_handoff(false, "https://example.com"),
        ];
        let tally = RunTally::from_tools(&tools);
        assert_eq!(tally.handoffs, 2);
        assert_eq!(tally.explore, 0);
        assert_eq!(
            tally.phrase_spans(),
            vec![(None, "2 browser handoffs".to_string())]
        );

        let one = RunTally::from_tools(&tools[..1]);
        assert_eq!(
            one.phrase_spans(),
            vec![(None, "1 browser handoff".to_string())]
        );
    }
}
