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
}

impl ToolRow {
    pub fn new(tool: ToolCallBlock) -> Self {
        Self { tool }
    }
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

        div()
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
            .when_some(err, |this, err| {
                this.child(Tag::danger().small().child(err))
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
            )
    }
}
