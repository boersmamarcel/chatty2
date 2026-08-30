use chatty_core::models::message_types::{ToolCallBlock, ToolCallState};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::collapsible::Collapsible as CollapsibleEl;

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
        }
        tally
    }

    pub fn sentence(&self) -> String {
        let mut parts = Vec::new();
        match self.edits {
            0 => {}
            1 => parts.push("Edited 1 file".into()),
            n => parts.push(format!("Edited {n} files")),
        }
        match self.explore {
            0 => {}
            1 => parts.push("explored 1 file".into()),
            n => parts.push(format!("explored {n} files")),
        }
        match self.searches {
            0 => {}
            1 => parts.push("1 search".into()),
            n => parts.push(format!("{n} searches")),
        }
        match self.external {
            0 => {}
            1 => parts.push("1 tool".into()),
            n => parts.push(format!("{n} tools")),
        }
        match self.commands {
            0 => {}
            1 => parts.push("ran 1 command".into()),
            n => parts.push(format!("ran {n} commands")),
        }
        if parts.is_empty() {
            "Worked".to_string()
        } else {
            parts.join(", ")
        }
    }

    pub fn all_success(tools: &[ToolCallBlock]) -> bool {
        !tools.is_empty()
            && tools
                .iter()
                .all(|t| matches!(t.state, ToolCallState::Success))
    }
}

#[derive(IntoElement)]
pub struct ActivityGroup {
    tools: Vec<ToolCallBlock>,
    open: bool,
    ticker: Option<Entity<HeadlineTicker>>,
}

impl ActivityGroup {
    pub fn new(tools: Vec<ToolCallBlock>) -> Self {
        Self {
            tools,
            open: true,
            ticker: None,
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
}

impl RenderOnce for ActivityGroup {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tally = RunTally::from_tools(&self.tools);
        let auto_open = if RunTally::all_success(&self.tools) {
            false
        } else {
            self.open
        };
        let running = self
            .tools
            .iter()
            .any(|t| matches!(t.state, ToolCallState::Running));

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(cx.theme().green_light.opacity(0.55))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(tally.sentence()),
            )
            .when(running, |this| {
                if let Some(ticker) = self.ticker.clone() {
                    this.child(ticker)
                } else {
                    this
                }
            });

        CollapsibleEl::new()
            .open(auto_open)
            .bg(cx.theme().group_box)
            .border_l_2()
            .border_color(cx.theme().border)
            .rounded_xl()
            .child(header)
            .content(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .children(self.tools.into_iter().map(ToolRow::new)),
            )
    }
}
