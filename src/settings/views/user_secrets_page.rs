use crate::settings::controllers::user_secrets_controller;
use crate::settings::models::user_secrets_store::UserSecretsModel;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, Render,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex,
};
use gpui_component::{Icon, IconName};

// ── Global singleton ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GlobalSecretsTableView {
    pub view: Option<Entity<SecretsTableView>>,
}

impl Global for GlobalSecretsTableView {}

// ── Table view entity ───────────────────────────────────────────────────────

pub struct SecretsTableView {
    focus_handle: FocusHandle,
}

impl SecretsTableView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        Self { focus_handle }
    }

    fn show_add_secret_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let key_input = cx.new(|cx| InputState::new(window, cx).placeholder("VARIABLE_NAME"));
        let value_input = cx.new(|cx| InputState::new(window, cx).placeholder("secret value"));
        let view_entity = cx.entity().clone();

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
                                                let view_entity = view_entity.clone();
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
                                                    // Notify the view entity to re-render
                                                    view_entity.update(cx, |_, cx| cx.notify());
                                                    window.close_dialog(cx);
                                                }
                                            }),
                                    ),
                            ),
                    ),
                )
        });
    }

    /// Render a header row for the secrets table.
    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("Variable Name"),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("Value"),
            )
            .child(
                div()
                    .w(px(80.))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground),
            )
    }

    /// Render a single secret row.
    fn render_row(
        &self,
        row_ix: usize,
        key: String,
        value: String,
        is_revealed: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let display_value = if is_revealed {
            value
        } else {
            "••••••••".to_string()
        };

        let eye_icon = if is_revealed {
            IconName::Eye
        } else {
            IconName::EyeOff
        };

        let key_for_toggle = key.clone();
        let key_for_delete = key.clone();
        let view_for_toggle = cx.entity().clone();
        let view_for_delete = cx.entity().clone();

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(key),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(display_value),
            )
            .child(
                h_flex()
                    .w(px(80.))
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new(SharedString::from(format!("eye-{}", row_ix)))
                            .icon(Icon::new(eye_icon))
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                user_secrets_controller::toggle_revealed(&key_for_toggle, cx);
                                view_for_toggle.update(cx, |_, cx| cx.notify());
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("del-{}", row_ix)))
                            .icon(Icon::new(IconName::Close))
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                user_secrets_controller::remove_secret(&key_for_delete, cx);
                                view_for_delete.update(cx, |_, cx| cx.notify());
                            }),
                    ),
            )
    }

    /// Render the empty state.
    fn render_empty(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_center()
            .py_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No secrets configured. Click \"Add Secret\" below to add one.")
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
        let model = cx.global::<UserSecretsModel>();
        let secrets = model.secrets.clone();
        let revealed_keys = model.revealed_keys.clone();

        let table = v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(self.render_header(cx))
            .map(|this| {
                if secrets.is_empty() {
                    this.child(self.render_empty(cx))
                } else {
                    this.children(secrets.iter().enumerate().map(|(ix, secret)| {
                        let is_revealed = revealed_keys.contains(&secret.key);
                        self.render_row(
                            ix,
                            secret.key.clone(),
                            secret.value.clone(),
                            is_revealed,
                            cx,
                        )
                        .into_any_element()
                    }))
                }
            });

        v_flex().size_full().gap_3().child(table).child(
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

                        div().size_full().child(view).into_any_element()
                    }),
                )]),
        ])
}
