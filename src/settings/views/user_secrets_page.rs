use crate::settings::controllers::user_secrets_controller;
use crate::settings::models::user_secrets_store::UserSecretsModel;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Global, IntoElement, Render, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    table::{Column, Table, TableDelegate, TableState},
    v_flex,
};
use gpui_component::{Icon, IconName};

// ── Global singleton ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GlobalSecretsTableView {
    pub view: Option<Entity<SecretsTableView>>,
}

impl Global for GlobalSecretsTableView {}

// ── Table delegate ──────────────────────────────────────────────────────────

pub struct SecretsTableDelegate {
    columns: Vec<Column>,
}

impl SecretsTableDelegate {
    fn new() -> Self {
        Self {
            columns: vec![
                Column::new("name", "Variable Name")
                    .width(px(200.))
                    .resizable(false)
                    .movable(false),
                Column::new("value", "Value")
                    .width(px(240.))
                    .resizable(false)
                    .movable(false),
                Column::new("actions", "")
                    .width(px(80.))
                    .resizable(false)
                    .movable(false),
            ],
        }
    }
}

impl TableDelegate for SecretsTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, cx: &App) -> usize {
        cx.global::<UserSecretsModel>().secrets.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> &Column {
        &self.columns[col_ix]
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let model = cx.global::<UserSecretsModel>();
        let secret = &model.secrets[row_ix];
        let key = secret.key.clone();
        let table_entity = cx.entity().clone();

        match col_ix {
            // Column 0: Variable Name
            0 => div()
                .size_full()
                .flex()
                .items_center()
                .text_sm()
                .child(key)
                .into_any_element(),
            // Column 1: Value (masked or revealed)
            1 => {
                let is_revealed = model.revealed_keys.contains(&key);
                let text = if is_revealed {
                    secret.value.clone()
                } else {
                    "••••••••".to_string()
                };
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(text)
                    .into_any_element()
            }
            // Column 2: Actions (eye toggle + delete)
            2 => {
                let is_revealed = model.revealed_keys.contains(&key);
                let eye_icon = if is_revealed {
                    IconName::Eye
                } else {
                    IconName::EyeOff
                };
                let key_for_toggle = key.clone();
                let key_for_delete = key;
                let table_for_toggle = table_entity.clone();
                let table_for_delete = table_entity;

                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("eye-{}", row_ix)))
                            .icon(Icon::new(eye_icon))
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                user_secrets_controller::toggle_revealed(&key_for_toggle, cx);
                                table_for_toggle.update(cx, |state, cx| state.refresh(cx));
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("del-{}", row_ix)))
                            .icon(Icon::new(IconName::Close))
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                user_secrets_controller::remove_secret(&key_for_delete, cx);
                                table_for_delete.update(cx, |state, cx| state.refresh(cx));
                            }),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .py_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No secrets configured. Click \"Add Secret\" below to add one.")
    }
}

// ── Table view entity ───────────────────────────────────────────────────────

pub struct SecretsTableView {
    focus_handle: FocusHandle,
    table_state: Entity<TableState<SecretsTableDelegate>>,
}

impl SecretsTableView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let delegate = SecretsTableDelegate::new();
        let table_state = cx.new(|cx| {
            let mut state = TableState::new(delegate, window, cx);
            state.row_selectable = false;
            state.col_selectable = false;
            state.col_resizable = false;
            state.col_movable = false;
            state.sortable = false;
            state
        });

        Self {
            focus_handle,
            table_state,
        }
    }

    fn show_add_secret_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let key_input = cx.new(|cx| InputState::new(window, cx).placeholder("VARIABLE_NAME"));
        let value_input = cx.new(|cx| InputState::new(window, cx).placeholder("secret value"));
        let table_state = self.table_state.clone();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Add Secret")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(450.))
                .child(
                    div().id("add-secret-form").child(
                        v_flex()
                            .gap_3()
                            .p_4()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().child("Variable Name"))
                                    .child(Input::new(&key_input)),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().child("Value"))
                                    .child(Input::new(&value_input)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_end()
                                    .pt_4()
                                    .child(Button::new("cancel-secret").label("Cancel").on_click(
                                        move |_, window, cx| {
                                            window.close_dialog(cx);
                                        },
                                    ))
                                    .child(
                                        Button::new("save-secret")
                                            .primary()
                                            .label("Save")
                                            .on_click({
                                                let key_input = key_input.clone();
                                                let value_input = value_input.clone();
                                                let table_state = table_state.clone();
                                                move |_, window, cx| {
                                                    let key = key_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let value =
                                                        value_input.read(cx).value().to_string();

                                                    if key.is_empty() {
                                                        window.push_notification(
                                                            "Variable name is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    user_secrets_controller::add_secret(
                                                        key, value, cx,
                                                    );
                                                    table_state
                                                        .update(cx, |state, cx| state.refresh(cx));
                                                    window.close_dialog(cx);
                                                }
                                            }),
                                    ),
                            ),
                    ),
                )
        });
    }
}

impl Focusable for SecretsTableView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SecretsTableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();

        v_flex()
            .size_full()
            .gap_3()
            .child(
                div()
                    .min_h(px(200.))
                    .max_h(px(400.))
                    .child(Table::new(&self.table_state).bordered(true).xsmall()),
            )
            .child(
                Button::new("add-secret-btn")
                    .label("+ Add Secret")
                    .primary()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |view, cx| {
                            view.show_add_secret_dialog(window, cx);
                        });
                    }),
            )
    }
}

// ── Setting page entry point ────────────────────────────────────────────────

pub fn user_secrets_page() -> SettingPage {
    SettingPage::new("Secrets")
        .description(
            "Environment variables injected into shell sessions. \
             Scripts can access these via os.environ[\"KEY\"] — \
             values are never shown to the AI.",
        )
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Environment Secrets")
                .description(
                    "These key-value pairs are exported as environment variables in every shell \
                 session. The AI can use the variable names in scripts but never sees the \
                 actual values.",
                )
                .items(vec![SettingItem::new(
                    "",
                    SettingField::render(|_options, window, cx| {
                        let view = if let Some(existing) = cx.try_global::<GlobalSecretsTableView>()
                        {
                            if let Some(view) = existing.view.clone() {
                                view
                            } else {
                                let new_view = cx.new(|cx| SecretsTableView::new(window, cx));
                                cx.set_global(GlobalSecretsTableView {
                                    view: Some(new_view.clone()),
                                });
                                new_view
                            }
                        } else {
                            let new_view = cx.new(|cx| SecretsTableView::new(window, cx));
                            cx.set_global(GlobalSecretsTableView {
                                view: Some(new_view.clone()),
                            });
                            new_view
                        };

                        div()
                            .size_full()
                            .min_h(px(280.))
                            .child(view)
                            .into_any_element()
                    }),
                )]),
        ])
}
