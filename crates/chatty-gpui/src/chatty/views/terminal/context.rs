//! The terminal snapshot a message carries (AGE-587): read from the tab the
//! dock shows, and the chip that shows it in the composer and the
//! transcript. The block's format lives in chatty-core
//! ([`chatty_core::services::terminal::context`]); this is the only place
//! one is read from a terminal, so nothing but the desktop with its dock
//! open ever attaches one.

use std::rc::Rc;

use chatty_core::services::shell_service::runner_command_id;
use chatty_core::services::terminal::context::{
    LastCommandSummary, TerminalContext, TerminalContextInput,
};
use chatty_core::token_budget::counter::TokenCounter;
use chatty_terminal::{LastCommand, Region};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable, h_flex};

use super::dock::ContextSource;

/// Snapshot `source` for a message: the last command and its output when
/// the shell marks its commands, else the visible screen. A full-screen
/// program still running has no output of its own yet; its screen is what
/// there is to see.
pub fn read_context(source: &ContextSource) -> TerminalContext {
    let handle = &source.handle;
    let counter = TokenCounter::for_model("");
    let cwd = source.cwd.as_ref().map(|cwd| cwd.display().to_string());
    let last = handle.snapshot(Region::LastCommand);
    let command = match last.last_command {
        Some(LastCommand::Ran { record, .. })
            if !(record.is_running() && last.text.trim().is_empty()) =>
        {
            // The agent's commands are recorded as the line that runs them.
            let command = runner_command_id(&record.command_text)
                .and_then(|id| {
                    source
                        .agent_commands
                        .iter()
                        .find(|(known, _)| known == id)
                        .map(|(_, command)| command.clone())
                })
                .unwrap_or_else(|| record.command_text.clone());
            Some(LastCommandSummary {
                command,
                exit_code: record.exit_code,
                running: record.is_running(),
            })
        }
        _ => None,
    };
    let screen;
    let text = match command {
        Some(_) => last.text.as_str(),
        None => {
            screen = handle.snapshot(Region::Screen);
            screen.text.as_str()
        }
    };
    TerminalContext::build(
        TerminalContextInput {
            tab: &source.tab,
            cwd: cwd.as_deref(),
            last_command: command.as_ref(),
            text,
        },
        &counter,
    )
}

/// What × on the composer's chip does.
pub type RemoveChip = Rc<dyn Fn(&mut App)>;

/// The chip: the label, a click previews the exact block, and × (when
/// `on_remove` is given) leaves it out of the message.
pub fn render_context_chip(
    id: impl Into<String>,
    context: &TerminalContext,
    on_remove: Option<RemoveChip>,
    cx: &App,
) -> AnyElement {
    let id: String = id.into();
    let block = context.block.clone();
    let preview = Popover::new(ElementId::Name(format!("{id}-preview").into()))
        .trigger(
            Button::new(ElementId::Name(format!("{id}-chip").into()))
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::SquareTerminal))
                .label(context.label())
                .tooltip("Terminal snapshot sent with this message. Click to see it."),
        )
        .content(move |_, _, cx| {
            div()
                .id("terminal-context-preview")
                .p_3()
                .max_w(px(720.))
                .max_h(px(420.))
                .overflow_y_scroll()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .shadow_md()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .text_color(cx.theme().foreground)
                .child(block.clone())
        });
    h_flex()
        .id(ElementId::Name(id.clone().into()))
        .flex_none()
        .items_center()
        .gap_0p5()
        .pr_0p5()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .text_color(cx.theme().muted_foreground)
        .child(preview)
        .when_some(on_remove, |this, on_remove| {
            this.child(
                Button::new(ElementId::Name(format!("{id}-remove").into()))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip("Don't send the terminal snapshot with this message")
                    .on_click(move |_, _, cx| on_remove(cx)),
            )
        })
        .into_any_element()
}
