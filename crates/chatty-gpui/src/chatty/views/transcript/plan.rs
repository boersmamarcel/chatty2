use std::rc::Rc;
use std::time::Duration;

use crate::assets::CustomIcon;
use chatty_core::services::{AgentTaskSnapshot, AgentTodo, AgentTodoStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::GLYPH_OPACITY_MS;

/// Compact sticky strip height (AGE-129).
pub const PLAN_STRIP_HEIGHT: f32 = 34.0;
/// Top padding reserved for the strip so showing it does not reflow the list.
pub const PLAN_LIST_TOP_PADDING: f32 = 36.0;

type PlanJump = Rc<dyn Fn(&mut App)>;

#[derive(IntoElement)]
pub struct PlanBlock {
    snapshot: AgentTaskSnapshot,
}

impl PlanBlock {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self { snapshot }
    }
}

impl RenderOnce for PlanBlock {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (done, total, _, _) = plan_counts(&self.snapshot);
        let last = self.snapshot.todos.len().saturating_sub(1);

        div()
            .id("plan-block")
            .w_full()
            .rounded_2xl()
            .bg(cx.theme().green_light)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child("To-dos"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{done}/{total}")),
                    ),
            )
            .children(
                self.snapshot
                    .todos
                    .into_iter()
                    .enumerate()
                    .map(|(ix, todo)| plan_step_row(todo, ix == last, cx)),
            )
    }
}

#[derive(IntoElement)]
pub struct PlanStrip {
    snapshot: AgentTaskSnapshot,
    open: bool,
    on_jump: Option<PlanJump>,
    on_open_change: Option<Rc<dyn Fn(bool, &mut App)>>,
}

impl PlanStrip {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self {
            snapshot,
            open: false,
            on_jump: None,
            on_open_change: None,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn on_jump(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_jump = Some(Rc::new(f));
        self
    }

    pub fn on_open_change(mut self, f: impl Fn(bool, &mut App) + 'static) -> Self {
        self.on_open_change = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for PlanStrip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (done, total, failed, current) = plan_counts(&self.snapshot);
        let count_label = if failed > 0 {
            format!("Plan {done} of {total} · {failed} failed")
        } else {
            format!("Plan {done} of {total}")
        };
        let overlay = PlanOverlay {
            snapshot: self.snapshot.clone(),
            on_jump: self.on_jump.clone(),
            max_height: window.bounds().size.height * 0.6,
        };
        let on_open_change = self.on_open_change.clone();
        let dots = self.snapshot.todos.clone();

        let popover = Popover::new("plan-overlay")
            .open(self.open)
            .appearance(false)
            .overlay_closable(true)
            .on_open_change(move |open, _, cx| {
                if let Some(cb) = &on_open_change {
                    cb(*open, cx);
                }
            })
            .trigger(
                Button::new("plan-strip")
                    .ghost()
                    .compact()
                    .w_full()
                    .rounded(px(PLAN_STRIP_HEIGHT / 2.0))
                    .bg(cx.theme().green_light)
                    .child(
                        div()
                            .id("plan-strip-inner")
                            .h(px(PLAN_STRIP_HEIGHT))
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(3.))
                                    .children(dots.into_iter().map(|todo| step_dot(&todo, cx))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .flex_shrink_0()
                                    .child(count_label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(current),
                            )
                            .child(Icon::new(IconName::ChevronRight).size_3()),
                    ),
            )
            .content(move |_, _, _| overlay.clone());

        div()
            .id("plan-strip-wrap")
            .w_full()
            .child(popover)
            .with_animation(
                "plan-strip-fade",
                Animation::new(Duration::from_millis(120)),
                |this, delta| this.opacity(delta),
            )
    }
}

#[derive(Clone, IntoElement)]
pub struct PlanOverlay {
    snapshot: AgentTaskSnapshot,
    on_jump: Option<PlanJump>,
    max_height: Pixels,
}

impl PlanOverlay {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self {
            snapshot,
            on_jump: None,
            max_height: px(420.),
        }
    }
}

impl RenderOnce for PlanOverlay {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let blocked = self
            .snapshot
            .todos
            .iter()
            .find(|todo| matches!(todo.status, AgentTodoStatus::Blocked))
            .and_then(|todo| {
                todo.blocked_reason
                    .as_ref()
                    .map(|reason| format!("{} — {reason}", todo.title))
            });
        let on_jump = self.on_jump.clone();
        let snapshot = self.snapshot.clone();

        div()
            .id("plan-overlay-body")
            .w(px(440.))
            .max_w(px(520.))
            .max_h(self.max_height)
            .flex()
            .flex_col()
            .gap_2()
            .bg(cx.theme().background)
            .rounded_2xl()
            .shadow_md()
            .p_3()
            .child(
                div()
                    .id("plan-overlay-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(PlanBlock::new(snapshot)),
            )
            .when_some(blocked, |this, message| {
                this.child(Alert::warning("plan-blocked", message))
            })
            .child(
                Button::new("jump-to-plan")
                    .ghost()
                    .small()
                    .label("Jump to the plan message")
                    .on_click(move |_, _, cx| {
                        if let Some(cb) = &on_jump {
                            cb(cx);
                        }
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("The run keeps going."),
            )
    }
}

fn plan_counts(snapshot: &AgentTaskSnapshot) -> (usize, usize, usize, String) {
    let done = snapshot
        .todos
        .iter()
        .filter(|todo| matches!(todo.status, AgentTodoStatus::Done))
        .count();
    let failed = snapshot
        .todos
        .iter()
        .filter(|todo| matches!(todo.status, AgentTodoStatus::Blocked))
        .count();
    let total = snapshot.todos.len().max(1);
    let current = snapshot
        .todos
        .iter()
        .find(|todo| matches!(todo.status, AgentTodoStatus::InProgress))
        .map(|todo| todo.title.clone())
        .or_else(|| {
            snapshot
                .todos
                .iter()
                .find(|todo| matches!(todo.status, AgentTodoStatus::Pending))
                .map(|todo| todo.title.clone())
        })
        .unwrap_or_else(|| format!("{done}/{total} complete"));
    (done, total, failed, current)
}

fn plan_step_row(todo: AgentTodo, last: bool, cx: &App) -> AnyElement {
    let done = matches!(todo.status, AgentTodoStatus::Done);
    let running = matches!(todo.status, AgentTodoStatus::InProgress);
    let blocked = matches!(todo.status, AgentTodoStatus::Blocked);
    let muted = cx.theme().muted_foreground;
    let title_color = if done || matches!(todo.status, AgentTodoStatus::Pending) {
        muted
    } else if blocked {
        cx.theme().danger
    } else {
        cx.theme().foreground
    };

    div()
        .id(ElementId::Name(format!("plan-step-{}", todo.id).into()))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .text_sm()
        .text_color(title_color)
        .when(!last, |this| {
            this.border_b_1()
                .border_color(cx.theme().border.opacity(0.45))
        })
        .when(done, |this| this.line_through())
        .when(running, |this| this.font_weight(FontWeight::SEMIBOLD))
        .child(step_marker(&todo, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(todo.title.clone())
                .when_some(todo.blocked_reason.clone(), |this, reason| {
                    this.child(div().text_xs().text_color(cx.theme().danger).child(reason))
                }),
        )
        .into_any_element()
}

fn step_marker(todo: &AgentTodo, cx: &App) -> AnyElement {
    match todo.status {
        AgentTodoStatus::Done => Icon::new(CustomIcon::CheckCircle)
            .size_4()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        AgentTodoStatus::InProgress => div()
            .size(px(16.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().primary)
            .child(
                Icon::new(IconName::ArrowRight)
                    .size_3()
                    .text_color(cx.theme().background),
            )
            .with_animation(
                ElementId::Name(format!("plan-running-{}", todo.id).into()),
                Animation::new(Duration::from_millis(GLYPH_OPACITY_MS)).repeat(),
                |this, delta| {
                    let wave = (delta * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                    this.opacity(0.55 + 0.45 * wave)
                },
            )
            .into_any_element(),
        AgentTodoStatus::Blocked => Icon::new(CustomIcon::Lock)
            .size_4()
            .text_color(cx.theme().danger)
            .into_any_element(),
        AgentTodoStatus::Pending => Icon::new(CustomIcon::CircleDashed)
            .size_4()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
    }
}

fn step_dot(todo: &AgentTodo, cx: &App) -> AnyElement {
    match todo.status {
        AgentTodoStatus::Done => Icon::new(CustomIcon::CircleDot)
            .size_3()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        AgentTodoStatus::InProgress => div()
            .size(px(7.))
            .rounded_full()
            .bg(cx.theme().primary)
            .with_animation(
                ElementId::Name(format!("plan-dot-{}", todo.id).into()),
                Animation::new(Duration::from_millis(GLYPH_OPACITY_MS)).repeat(),
                |this, delta| {
                    let wave = (delta * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                    this.opacity(0.55 + 0.45 * wave)
                },
            )
            .into_any_element(),
        AgentTodoStatus::Blocked => div()
            .size(px(7.))
            .rounded_full()
            .bg(cx.theme().danger)
            .into_any_element(),
        AgentTodoStatus::Pending => div()
            .size(px(7.))
            .rounded_full()
            .border_1()
            .border_color(cx.theme().muted_foreground.opacity(0.45))
            .bg(gpui::transparent_white())
            .into_any_element(),
    }
}
