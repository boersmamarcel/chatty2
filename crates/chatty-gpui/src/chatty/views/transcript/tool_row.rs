use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::skeleton::Skeleton;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

/// Compact tool-call row. Verb tense encodes state.
#[derive(IntoElement)]
pub struct ToolRow {
    tool: ToolCallBlock,
}

impl ToolRow {
    pub fn new(tool: ToolCallBlock) -> Self {
        Self { tool }
    }
}

fn verb(name: &str, state: &ToolCallState) -> String {
    let base = name
        .rsplit(['.', '_', ':'])
        .next()
        .unwrap_or(name)
        .replace('_', " ");
    match state {
        ToolCallState::Running => format!("{base}…"),
        ToolCallState::Success => past_tense(&base),
        ToolCallState::Error(_) => format!("Failed {base}"),
    }
}

fn past_tense(verb: &str) -> String {
    if verb.ends_with('e') {
        format!("{verb}d")
    } else if verb.ends_with('y') && verb.len() > 1 {
        format!("{}ied", &verb[..verb.len() - 1])
    } else {
        format!("{verb}ed")
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
        let label = verb(&tool.display_name, &tool.state);
        let (tag, tag_success) = match &tool.state {
            ToolCallState::Success => (None, true),
            ToolCallState::Error(err) => (Some(err.clone()), false),
            ToolCallState::Running => (None, true),
        };
        let copy_value = tool.output.clone().unwrap_or_else(|| tool.input.clone());
        let icon = source_icon(&tool.source);

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
                    .text_color(if matches!(tool.state, ToolCallState::Error(_)) {
                        cx.theme().danger
                    } else if matches!(tool.state, ToolCallState::Success) {
                        cx.theme().success
                    } else {
                        cx.theme().foreground
                    })
                    .child(label),
            )
            .when_some(tag, |this, err| {
                this.child(if tag_success {
                    Tag::success().small().child(err)
                } else {
                    Tag::danger().small().child(err)
                })
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
