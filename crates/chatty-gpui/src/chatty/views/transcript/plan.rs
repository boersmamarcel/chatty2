use crate::assets::CustomIcon;
use chatty_core::services::{AgentTaskSnapshot, AgentTodoStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, Sizable};

#[derive(IntoElement)]
pub struct PlanBlock {
    snapshot: AgentTaskSnapshot,
    bare: bool,
}

impl PlanBlock {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self {
            snapshot,
            bare: false,
        }
    }

    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }
}

impl RenderOnce for PlanBlock {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let done = self
            .snapshot
            .todos
            .iter()
            .filter(|t| matches!(t.status, AgentTodoStatus::Done))
            .count();
        let total = self.snapshot.todos.len();
        let title = self
            .snapshot
            .goal
            .clone()
            .unwrap_or_else(|| "Plan".to_string());

        div()
            .id("plan-block")
            .w_full()
            .when(!self.bare, |this| {
                this.rounded_2xl()
                    .border_1()
                    .border_color(cx.theme().border)
            })
            .bg(cx.theme().green_light)
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{title} · {done}/{total}")),
            )
            .children(self.snapshot.todos.into_iter().map(|todo| {
                let running = matches!(todo.status, AgentTodoStatus::InProgress);
                let icon = match todo.status {
                    AgentTodoStatus::Done => CustomIcon::CircleDot,
                    AgentTodoStatus::InProgress => CustomIcon::CircleDashed,
                    AgentTodoStatus::Blocked => CustomIcon::Lock,
                    AgentTodoStatus::Pending => CustomIcon::CircleDashed,
                };
                div()
                    .id(ElementId::Name(format!("plan-step-{}", todo.id).into()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .when(running, |this| this.text_color(cx.theme().primary))
                    .child(Icon::new(icon).size_3())
                    .child(todo.title)
            }))
    }
}

#[derive(IntoElement)]
pub struct PlanStrip {
    snapshot: AgentTaskSnapshot,
}

impl PlanStrip {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self { snapshot }
    }
}

impl RenderOnce for PlanStrip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let done = self
            .snapshot
            .todos
            .iter()
            .filter(|t| matches!(t.status, AgentTodoStatus::Done))
            .count();
        let total = self.snapshot.todos.len().max(1);
        let current = self
            .snapshot
            .todos
            .iter()
            .find(|t| matches!(t.status, AgentTodoStatus::InProgress))
            .map(|t| t.title.clone())
            .unwrap_or_else(|| format!("{done}/{total} complete"));

        Button::new("plan-strip")
            .small()
            .bg(cx.theme().green_light)
            .label(current)
    }
}

#[derive(IntoElement)]
pub struct PlanOverlay {
    snapshot: AgentTaskSnapshot,
}

impl PlanOverlay {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self { snapshot }
    }
}

impl RenderOnce for PlanOverlay {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let snapshot = self.snapshot.clone();
        Popover::new("plan-overlay")
            .trigger(Button::new("plan-overlay-trigger").small().label("Plan"))
            .appearance(false)
            .content(move |_, _, cx| {
                div()
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_xl()
                    .p_2()
                    .max_w(px(420.))
                    .child(PlanBlock::new(snapshot.clone()))
            })
    }
}
