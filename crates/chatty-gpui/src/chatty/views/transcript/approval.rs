use std::collections::BTreeSet;
use std::sync::Arc;

use chatty_core::models::message_types::{ApprovalBlock, ApprovalState};
use gpui::*;
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::collapsible::Collapsible as CollapsibleEl;
use gpui_component::kbd::Kbd;
use gpui_component::{ActiveTheme, Sizable};

pub type ApprovalCallback = Arc<dyn Fn(bool, &mut App) + Send + Sync>;

#[derive(IntoElement)]
pub struct ApprovalCard {
    approval: ApprovalBlock,
    on_decide: Option<ApprovalCallback>,
}

impl ApprovalCard {
    pub fn new(approval: ApprovalBlock) -> Self {
        Self {
            approval,
            on_decide: None,
        }
    }

    pub fn on_decide<F>(mut self, f: F) -> Self
    where
        F: Fn(bool, &mut App) + Send + Sync + 'static,
    {
        self.on_decide = Some(Arc::new(f));
        self
    }
}

impl RenderOnce for ApprovalCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.approval.id.clone();
        match self.approval.state {
            ApprovalState::Pending => {
                let command = self.approval.command.clone();
                let on_decide = self.on_decide;
                #[cfg(target_os = "macos")]
                let (approve_ks, deny_ks) = ("cmd-y", "cmd-shift-n");
                #[cfg(target_os = "linux")]
                let (approve_ks, deny_ks) = ("alt-y", "alt-shift-n");
                #[cfg(target_os = "windows")]
                let (approve_ks, deny_ks) = ("ctrl-y", "ctrl-shift-n");

                div()
                    .id(ElementId::Name(format!("approval-{id}").into()))
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Alert::warning(
                        ElementId::Name(format!("approval-alert-{id}").into()),
                        format!("Run `{command}`?"),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Button::new(ElementId::Name(format!("approval-yes-{id}").into()))
                                    .primary()
                                    .small()
                                    .child(div().flex().gap_1().child("Approve").child(Kbd::new(
                                        Keystroke::parse(approve_ks).expect("approve shortcut"),
                                    )))
                                    .on_click({
                                        let on_decide = on_decide.clone();
                                        move |_, _, cx| {
                                            if let Some(cb) = &on_decide {
                                                cb(true, cx);
                                            }
                                        }
                                    }),
                            )
                            .child(
                                Button::new(ElementId::Name(format!("approval-no-{id}").into()))
                                    .ghost()
                                    .small()
                                    .child(div().flex().gap_1().child("Deny").child(Kbd::new(
                                        Keystroke::parse(deny_ks).expect("deny shortcut"),
                                    )))
                                    .on_click({
                                        let on_decide = on_decide;
                                        move |_, _, cx| {
                                            if let Some(cb) = &on_decide {
                                                cb(false, cx);
                                            }
                                        }
                                    }),
                            ),
                    )
            }
            ApprovalState::Approved | ApprovalState::Denied => {
                let label = match self.approval.state {
                    ApprovalState::Approved => "Approved",
                    _ => "Denied",
                };
                div()
                    .id(ElementId::Name(format!("approval-resolved-{id}").into()))
                    .h(px(28.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{label} · {}", self.approval.command))
            }
        }
    }
}

#[derive(IntoElement)]
pub struct ErrorBlock {
    id: String,
    message: String,
    detail: Option<String>,
}

impl ErrorBlock {
    pub fn new(id: impl Into<String>, message: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            id: id.into(),
            message: message.into(),
            detail,
        }
    }
}

impl RenderOnce for ErrorBlock {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let alert = Alert::error(
            ElementId::Name(format!("error-{}", self.id).into()),
            self.message.clone(),
        );
        if let Some(detail) = self.detail {
            CollapsibleEl::new()
                .open(false)
                .child(alert)
                .content(
                    div()
                        .text_xs()
                        .font_family("monospace")
                        .text_color(cx.theme().muted_foreground)
                        .p_2()
                        .child(detail),
                )
                .into_any_element()
        } else {
            alert.into_any_element()
        }
    }
}

#[derive(Clone, Debug)]
pub struct PathChange {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

#[derive(IntoElement)]
pub struct ChangeTray {
    changes: Vec<PathChange>,
}

impl ChangeTray {
    pub fn new(changes: Vec<PathChange>) -> Self {
        Self { changes }
    }

    pub fn from_paths(paths: impl IntoIterator<Item = String>) -> Self {
        let unique: BTreeSet<String> = paths.into_iter().collect();
        Self {
            changes: unique
                .into_iter()
                .map(|path| PathChange {
                    path,
                    added: 0,
                    removed: 0,
                })
                .collect(),
        }
    }
}

impl RenderOnce for ChangeTray {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.changes.is_empty() {
            return div().into_any_element();
        }
        let count = self.changes.len();
        div()
            .id("change-tray")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pt_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{count} file{}", if count == 1 { "" } else { "s" }))
            .children(self.changes.into_iter().take(4).map(|change| {
                div()
                    .id(ElementId::Name(format!("change-{}", change.path).into()))
                    .child(change.path)
            }))
            .into_any_element()
    }
}
