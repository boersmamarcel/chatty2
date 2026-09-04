use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::skeleton::Skeleton;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::verb::tool_row_label;

/// Compact tool-call row. Verb tense encodes state; path and +/- are separate.
#[derive(IntoElement)]
pub struct ToolRow {
    tool: ToolCallBlock,
    /// 1-based position among failures of the same tool in this group.
    ///
    /// Two stacked failure cards used to be indistinguishable — same tool,
    /// same redacted text, no way to tell a retry from a second call
    /// (AGE-187). Anything above 1 is labelled.
    attempt: usize,
}

impl ToolRow {
    pub fn new(tool: ToolCallBlock) -> Self {
        Self { tool, attempt: 1 }
    }

    pub fn attempt(mut self, attempt: usize) -> Self {
        self.attempt = attempt;
        self
    }
}

/// First line of an error, for the inline summary.
///
/// The full text goes in the detail line below; this keeps the row itself one
/// line tall when a tool returns a stack or a multi-line payload.
fn error_headline(error: &str) -> String {
    error.lines().next().unwrap_or(error).trim().to_string()
}

fn source_icon(source: &ToolSource) -> Option<IconName> {
    match source {
        ToolSource::Local => None,
        ToolSource::HiveCloud => Some(IconName::Building2),
        ToolSource::Internet { .. } => Some(IconName::Globe),
        ToolSource::ExternalService { .. } => Some(IconName::SquareTerminal),
    }
}

impl RenderOnce for ToolRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let attempt = self.attempt;
        let tool = self.tool;
        let id = if tool.id.is_empty() {
            tool.tool_name.clone()
        } else {
            tool.id.clone()
        };
        let label = tool_row_label(
            &tool.display_name,
            &tool.tool_name,
            &tool.state,
            &tool.input,
            tool.output.as_deref(),
        );
        let err = match &tool.state {
            ToolCallState::Error(err) => Some(err.clone()),
            _ => None,
        };
        let copy_value = tool.output.clone().unwrap_or_else(|| tool.input.clone());
        let icon = source_icon(&tool.source);
        let verb_color = if matches!(tool.state, ToolCallState::Error(_)) {
            cx.theme().danger
        } else {
            // Success stays on foreground — sage is reserved for +N tags.
            cx.theme().foreground
        };

        let row = div()
            .id(ElementId::Name(format!("tool-row-{id}").into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_lg()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
            .when_some(icon, |this, icon| {
                this.child(
                    Icon::new(icon)
                        .size_3()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .when(matches!(tool.state, ToolCallState::Running), |this| {
                this.child(Skeleton::new().w(px(72.)).h(px(10.)))
            })
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(verb_color)
                    .child(label.verb.clone()),
            )
            .when(!label.subject.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .min_w_0()
                        .truncate()
                        .font_family("monospace")
                        .text_color(cx.theme().muted_foreground)
                        .child(label.subject.clone()),
                )
            })
            .when_some(label.added, |this, n| {
                this.child(Tag::success().small().child(format!("+{n}")))
            })
            .when_some(label.removed.filter(|n| *n > 0), |this, n| {
                this.child(Tag::danger().small().child(format!("−{n}")))
            })
            .when(attempt > 1, |this| {
                this.child(Tag::danger().small().child(format!("attempt {attempt}")))
            })
            .child(div().flex_1())
            .child(
                Clipboard::new(ElementId::Name(format!("tool-copy-{id}").into())).value(copy_value),
            )
            .child(
                Button::new(ElementId::Name(format!("tool-open-{id}").into()))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::ExternalLink))
                    .tooltip("Open"),
            );

        let Some(err) = err else {
            return row.into_any_element();
        };

        // A failure gets its own full-width line rather than a truncating chip
        // in the row: the message is the whole point of the card, and it names
        // the tool so two stacked failures are never ambiguous (AGE-187).
        let headline = error_headline(&err);
        let has_detail = err.trim() != headline;
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(row)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .pb_1()
                    .pl_5()
                    .child(
                        div()
                            .text_xs()
                            .min_w_0()
                            .text_color(cx.theme().danger)
                            .child(format!("{}: {headline}", tool.tool_name)),
                    )
                    .child(div().flex_1())
                    .when(has_detail, |this| {
                        // The full payload is one click away instead of being
                        // dropped on the floor.
                        this.child(
                            Clipboard::new(ElementId::Name(format!("tool-error-copy-{id}").into()))
                                .value(err.clone()),
                        )
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::error_headline;

    /// A tool that returns a stack or a multi-line payload must not stretch
    /// the row; the rest is behind the copy control (AGE-187).
    #[test]
    fn headline_is_the_first_line() {
        let err = "browser_navigate: ERR_NETWORK_CHANGED\n  at Page::goto\n  at ...";
        assert_eq!(error_headline(err), "browser_navigate: ERR_NETWORK_CHANGED");
    }

    #[test]
    fn single_line_errors_are_their_own_headline() {
        let err = "path is outside the workspace";
        assert_eq!(error_headline(err), err);
    }

    #[test]
    fn headline_is_trimmed() {
        assert_eq!(error_headline("  spaced out  \n rest"), "spaced out");
    }

    #[test]
    fn empty_error_stays_empty() {
        assert_eq!(error_headline(""), "");
    }
}
