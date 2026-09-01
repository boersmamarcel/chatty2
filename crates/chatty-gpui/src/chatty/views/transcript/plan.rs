use std::rc::Rc;
use std::time::Duration;

use crate::assets::CustomIcon;
use chatty_core::services::{AgentTaskSnapshot, AgentTodo, AgentTodoStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::kbd::Kbd;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::GLYPH_OPACITY_MS;

/// Compact sticky strip height (AGE-129).
pub const PLAN_STRIP_HEIGHT: f32 = 34.0;
/// Top padding reserved for the strip so showing it does not reflow the list.
pub const PLAN_LIST_TOP_PADDING: f32 = 36.0;

type PlanJump = Rc<dyn Fn(&mut App)>;
type PlanOpenChange = Rc<dyn Fn(bool, &mut App)>;
type PlanDecide = Rc<dyn Fn(bool, &mut App)>;

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
            .bg(cx.theme().secondary)
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
                    .map(|(ix, todo)| plan_step_row(todo, ix == last, false, None, None, cx)),
            )
    }
}

#[derive(IntoElement)]
pub struct PlanStrip {
    snapshot: AgentTaskSnapshot,
    open: bool,
    pending_command: Option<String>,
    on_jump: Option<PlanJump>,
    on_open_change: Option<PlanOpenChange>,
    on_decide: Option<PlanDecide>,
}

impl PlanStrip {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self {
            snapshot,
            open: false,
            pending_command: None,
            on_jump: None,
            on_open_change: None,
            on_decide: None,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn pending_command(mut self, command: Option<String>) -> Self {
        self.pending_command = command;
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

    pub fn on_decide(mut self, f: impl Fn(bool, &mut App) + 'static) -> Self {
        self.on_decide = Some(Rc::new(f));
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
            pending_command: self.pending_command.clone(),
            on_jump: self.on_jump.clone(),
            on_decide: self.on_decide.clone(),
            on_close: self.on_open_change.clone(),
            max_height: window.bounds().size.height * 0.6,
        };
        let on_open_change = self.on_open_change.clone();
        let dots = self.snapshot.todos.clone();
        let chevron = if self.open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };

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
                    .bg(cx.theme().secondary)
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
                                    .text_color(cx.theme().foreground)
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
                            .child(
                                Icon::new(chevron)
                                    .size_3()
                                    .text_color(cx.theme().muted_foreground),
                            ),
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
    pending_command: Option<String>,
    on_jump: Option<PlanJump>,
    on_decide: Option<PlanDecide>,
    on_close: Option<PlanOpenChange>,
    max_height: Pixels,
}

impl PlanOverlay {
    pub fn new(snapshot: AgentTaskSnapshot) -> Self {
        Self {
            snapshot,
            pending_command: None,
            on_jump: None,
            on_decide: None,
            on_close: None,
            max_height: px(420.),
        }
    }
}

impl RenderOnce for PlanOverlay {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (done, total, _, _) = plan_counts(&self.snapshot);
        let meta = format!("{done} of {total} done");
        let on_jump = self.on_jump.clone();
        let on_close = self.on_close.clone();
        let on_decide = self.on_decide.clone();
        let pending = self.pending_command.clone();
        let last = self.snapshot.todos.len().saturating_sub(1);

        div()
            .id("plan-overlay-body")
            .w(px(480.))
            .max_w(px(560.))
            .max_h(self.max_height)
            .flex()
            .flex_col()
            .gap_2()
            .bg(cx.theme().background)
            .rounded_2xl()
            .shadow_md()
            .p_4()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .text_color(cx.theme().foreground)
                            .child("Plan"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta),
                    )
                    .child(
                        Button::new("plan-overlay-close")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Close))
                            .on_click(move |_, _, cx| {
                                if let Some(cb) = &on_close {
                                    cb(false, cx);
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .id("plan-overlay-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(
                        self.snapshot
                            .todos
                            .into_iter()
                            .enumerate()
                            .map(|(ix, todo)| {
                                let nest = matches!(todo.status, AgentTodoStatus::Blocked)
                                    || (matches!(todo.status, AgentTodoStatus::InProgress)
                                        && pending.is_some());
                                let cmd = if nest {
                                    pending.clone().or_else(|| todo.blocked_reason.clone())
                                } else {
                                    None
                                };
                                plan_step_row(todo, ix == last, true, cmd, on_decide.clone(), cx)
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("jump-to-plan")
                            .ghost()
                            .small()
                            .label("Jump to the plan message")
                            .text_color(cx.theme().primary)
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
                            .child("Esc or click away — the run keeps going"),
                    ),
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

fn plan_step_row(
    todo: AgentTodo,
    last: bool,
    overlay: bool,
    approval_command: Option<String>,
    on_decide: Option<PlanDecide>,
    cx: &App,
) -> AnyElement {
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

    let trailing: AnyElement = if overlay && running {
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().primary)
            .child("running")
            .into_any_element()
    } else if overlay && blocked {
        div()
            .text_xs()
            .text_color(cx.theme().danger)
            .child("blocked")
            .into_any_element()
    } else {
        div().into_any_element()
    };

    let mut row = div()
        .id(ElementId::Name(format!("plan-step-{}", todo.id).into()))
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .text_sm()
        .text_color(title_color)
        .when(!last && !overlay, |this| {
            this.border_b_1()
                .border_color(cx.theme().border.opacity(0.45))
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .when(done, |this| this.line_through())
                .when(running, |this| this.font_weight(FontWeight::SEMIBOLD))
                .child(step_marker(&todo, cx))
                .child(div().flex_1().min_w_0().child(todo.title.clone()))
                .child(trailing),
        );

    if let Some(command) = approval_command {
        #[cfg(target_os = "macos")]
        let (approve_ks, deny_ks) = ("cmd-y", "cmd-shift-n");
        #[cfg(target_os = "linux")]
        let (approve_ks, deny_ks) = ("alt-y", "alt-shift-n");
        #[cfg(target_os = "windows")]
        let (approve_ks, deny_ks) = ("ctrl-y", "ctrl-shift-n");
        let on_decide_yes = on_decide.clone();
        let on_decide_no = on_decide;
        row = row.child(
            div()
                .ml_6()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Waiting on approval — `{command}`")),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            Button::new(ElementId::Name(
                                format!("plan-approve-{}", todo.id).into(),
                            ))
                            .primary()
                            .small()
                            .child(div().flex().gap_1().child("Approve").child(Kbd::new(
                                Keystroke::parse(approve_ks).expect("approve shortcut"),
                            )))
                            .on_click(move |_, _, cx| {
                                if let Some(cb) = &on_decide_yes {
                                    cb(true, cx);
                                }
                            }),
                        )
                        .child(
                            Button::new(ElementId::Name(format!("plan-deny-{}", todo.id).into()))
                                .ghost()
                                .small()
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(div().flex().gap_1().child("Deny").child(Kbd::new(
                                    Keystroke::parse(deny_ks).expect("deny shortcut"),
                                )))
                                .on_click(move |_, _, cx| {
                                    if let Some(cb) = &on_decide_no {
                                        cb(false, cx);
                                    }
                                }),
                        ),
                ),
        );
    }

    row.into_any_element()
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
            .border_2()
            .border_color(cx.theme().primary)
            .flex()
            .items_center()
            .justify_center()
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
            .text_color(cx.theme().success)
            .into_any_element(),
        AgentTodoStatus::InProgress => div()
            .size(px(7.))
            .rounded_full()
            .border_1()
            .border_color(cx.theme().primary)
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
            .border_color(cx.theme().muted_foreground)
            .into_any_element(),
    }
}
