use std::sync::Arc;

use crate::assets::CustomIcon;
use chatty_core::services::{AgentTaskSnapshot, AgentTodo, AgentTodoStatus};
use gpui::prelude::*;
use gpui::*;
use gpui_component::{ActiveTheme, Icon};

pub type ToggleCallback = Arc<dyn Fn(&mut App) + Send + Sync>;

#[derive(IntoElement)]
pub struct AgentTodoPanel {
    snapshot: AgentTaskSnapshot,
    collapsed: bool,
    on_toggle: Option<ToggleCallback>,
}

impl AgentTodoPanel {
    pub fn new(snapshot: AgentTaskSnapshot, collapsed: bool) -> Self {
        Self {
            snapshot,
            collapsed,
            on_toggle: None,
        }
    }

    pub fn on_toggle<F>(mut self, callback: F) -> Self
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_toggle = Some(Arc::new(callback));
        self
    }
}

impl RenderOnce for AgentTodoPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let progress = todo_progress(&self.snapshot);
        let chevron = if self.collapsed { ">" } else { "v" };
        let title = if self.snapshot.verified {
            "Agent plan verified"
        } else {
            "Agent plan"
        };
        let summary = format!("{}/{} complete", progress.done, progress.total);
        let goal = self
            .snapshot
            .goal
            .clone()
            .unwrap_or_else(|| "Working through planned steps".to_string());
        let on_toggle = self.on_toggle.clone();

        div().w_full().px_4().pb_2().child(
            div()
                .w_full()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary.opacity(0.65))
                .shadow_sm()
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().muted.opacity(0.7)))
                        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                            if let Some(callback) = &on_toggle {
                                callback(cx);
                            }
                        })
                        .child(
                            div()
                                .font_family("monospace")
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .w_4()
                                .child(chevron),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().foreground)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(goal),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .px_2()
                                .py(px(1.0))
                                .rounded_full()
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_color(cx.theme().muted_foreground)
                                .child(summary),
                        ),
                )
                .when(!self.collapsed, |panel| {
                    panel.child(
                        div()
                            .px_3()
                            .pb_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(
                                self.snapshot
                                    .todos
                                    .iter()
                                    .map(|todo| render_todo_row(todo, cx)),
                            )
                            .when_some(
                                self.snapshot.verification_reason.clone(),
                                |this, reason| {
                                    this.child(
                                        div()
                                            .mt_2()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(reason),
                                    )
                                },
                            ),
                    )
                }),
        )
    }
}

fn render_todo_row(todo: &AgentTodo, cx: &App) -> Div {
    let (marker, color) = status_marker(todo.status);

    div()
        .flex()
        .items_start()
        .gap_2()
        .rounded_md()
        .px_2()
        .py_1()
        .bg(row_background(todo.status, cx))
        .child(
            div()
                .mt(px(1.0))
                .text_color(color)
                .w_5()
                .h_5()
                .flex()
                .items_center()
                .justify_center()
                .child(render_status_marker(marker, color)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(todo.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(todo_detail(todo)),
                ),
        )
}

fn todo_detail(todo: &AgentTodo) -> String {
    match todo.status {
        AgentTodoStatus::Blocked => todo
            .blocked_reason
            .clone()
            .unwrap_or_else(|| todo.description.clone()),
        _ => todo.description.clone(),
    }
}

#[derive(Clone, Copy)]
enum StatusMarker {
    Icon(CustomIcon),
    Text(&'static str),
}

fn render_status_marker(marker: StatusMarker, color: Rgba) -> AnyElement {
    match marker {
        StatusMarker::Icon(icon) => Icon::new(icon)
            .size(px(14.0))
            .text_color(color)
            .into_any_element(),
        StatusMarker::Text(text) => div()
            .font_family("monospace")
            .text_xs()
            .child(text)
            .into_any_element(),
    }
}

fn status_marker(status: AgentTodoStatus) -> (StatusMarker, Rgba) {
    match status {
        AgentTodoStatus::Pending => (StatusMarker::Icon(CustomIcon::CircleDashed), rgb(0x6b7280)),
        AgentTodoStatus::InProgress => (StatusMarker::Icon(CustomIcon::Loader), rgb(0x2563eb)),
        AgentTodoStatus::Done => (StatusMarker::Icon(CustomIcon::CircleDot), rgb(0x16a34a)),
        AgentTodoStatus::Blocked => (StatusMarker::Text("[!]"), rgb(0xdc2626)),
    }
}

fn row_background(status: AgentTodoStatus, cx: &App) -> Hsla {
    match status {
        AgentTodoStatus::InProgress => cx.theme().accent.opacity(0.16),
        AgentTodoStatus::Blocked => cx.theme().ring.opacity(0.12),
        _ => cx.theme().background.opacity(0.35),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TodoProgress {
    pub done: usize,
    pub total: usize,
}

pub fn todo_progress(snapshot: &AgentTaskSnapshot) -> TodoProgress {
    TodoProgress {
        done: snapshot
            .todos
            .iter()
            .filter(|todo| todo.status == AgentTodoStatus::Done)
            .count(),
        total: snapshot.todos.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(status: AgentTodoStatus) -> AgentTodo {
        AgentTodo {
            id: "t1".to_string(),
            title: "Title".to_string(),
            description: "Description".to_string(),
            status,
            blocked_reason: None,
            reflection: None,
        }
    }

    #[::core::prelude::v1::test]
    fn progress_counts_done_items() {
        let snapshot = AgentTaskSnapshot {
            goal: Some("Goal".to_string()),
            todos: vec![
                todo(AgentTodoStatus::Done),
                todo(AgentTodoStatus::InProgress),
                todo(AgentTodoStatus::Pending),
            ],
            write_todos_called: true,
            verified: false,
            verification_reason: None,
            evidence: Vec::new(),
        };

        assert_eq!(todo_progress(&snapshot), TodoProgress { done: 1, total: 3 });
    }
}
