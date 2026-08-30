use std::rc::Rc;

use chatty_core::models::message_types::{ToolCallBlock, ToolCallState};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::collapsible::Collapsible as CollapsibleEl;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::ticker::HeadlineTicker;
use super::tool_row::ToolRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Edit,
    Explore,
    Search,
    External,
    Command,
}

pub fn classify_tool(name: &str) -> ToolKind {
    let n = name.to_ascii_lowercase();
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

/// Counted sentence: edits → explore → searches → external → commands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTally {
    pub edits: usize,
    pub explore: usize,
    pub searches: usize,
    pub external: usize,
    pub commands: usize,
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
            }
            if let Some(output) = tool.output.as_deref() {
                let (a, r) = count_diff_lines(output);
                tally.added += a;
                tally.removed += r;
            }
        }
        tally
    }

    /// Legacy single-string form (tests / fallbacks). Prefer [`Self::phrase_spans`].
    pub fn sentence(&self) -> String {
        self.phrase_spans()
            .into_iter()
            .map(|(verb, rest)| match verb {
                Some(v) => format!("{v}{rest}"),
                None => rest,
            })
            .collect::<Vec<_>>()
            .join(", ")
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
        if parts.is_empty() {
            parts.push((Some("Worked"), String::new()));
        }
        parts
    }

    pub fn all_success(tools: &[ToolCallBlock]) -> bool {
        !tools.is_empty()
            && tools
                .iter()
                .all(|t| matches!(t.state, ToolCallState::Success))
    }

    pub fn has_failure(tools: &[ToolCallBlock]) -> bool {
        tools
            .iter()
            .any(|t| matches!(t.state, ToolCallState::Error(_)))
    }
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
    ticker: Option<Entity<HeadlineTicker>>,
    on_toggle: Option<ActivityToggle>,
}

impl ActivityGroup {
    pub fn new(tools: Vec<ToolCallBlock>) -> Self {
        Self {
            tools,
            open: true,
            ticker: None,
            on_toggle: None,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn ticker(mut self, ticker: Entity<HeadlineTicker>) -> Self {
        self.ticker = Some(ticker);
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
        // Failures and in-flight groups stay expanded; settled success uses `open`.
        let open = if running || RunTally::has_failure(&self.tools) {
            true
        } else {
            self.open
        };

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
            .when(running, |this| {
                if let Some(ticker) = self.ticker.clone() {
                    this.child(ticker)
                } else {
                    this
                }
            })
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
                    .children(self.tools.into_iter().map(ToolRow::new)),
            )
    }
}
