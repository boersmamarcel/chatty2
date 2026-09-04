//! "Provider keys" — one sheet for every provider's credentials.
//!
//! Replaces the old Providers page: each provider is a row with its status,
//! its masked credential and a Test button. Azure's extra fields expand in
//! the row being edited rather than living on a page of their own.

use super::*;
use crate::settings::controllers::providers_controller;
use crate::settings::models::providers_store::AzureAuthMethod;
use chatty_core::settings::providers::ollama::discovery::discover_ollama_models;
use chatty_core::settings::providers::openrouter::discovery::verify_openrouter_key;
use gpui::AsyncApp;
use std::cell::RefCell;
use std::rc::Rc;

/// The outcome of pressing Test on a provider row.
#[derive(Clone, PartialEq, Eq)]
enum TestState {
    Testing,
    Ok(String),
    Failed(String),
}

/// Record the latest Test result for a provider, replacing any earlier one.
fn set_test_state(
    states: &Rc<RefCell<Vec<(ProviderType, TestState)>>>,
    provider_type: ProviderType,
    state: TestState,
) {
    let mut states = states.borrow_mut();
    states.retain(|(p, _)| p != &provider_type);
    states.push((provider_type, state));
}

fn azure_field(cx: &App, read: impl Fn(&crate::settings::models::providers_store::ProviderConfig) -> Option<String>) -> String {
    cx.global::<ProviderModel>()
        .providers()
        .iter()
        .find(|p| p.provider_type == ProviderType::AzureOpenAI)
        .and_then(read)
        .unwrap_or_default()
}

impl ModelsListView {
    pub(super) fn show_provider_keys_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        trace!("Opening Provider keys sheet");

        // Which row is expanded for editing, and the last Test result per provider.
        let expanded: Rc<RefCell<Option<ProviderType>>> = Rc::new(RefCell::new(None));
        let test_state: Rc<RefCell<Vec<(ProviderType, TestState)>>> =
            Rc::new(RefCell::new(Vec::new()));

        let openrouter_key = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("sk-or-…")
                .masked(true);
            let current = cx_provider_key(cx, &ProviderType::OpenRouter);
            state.set_value(current, window, cx);
            state
        });
        let ollama_url = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("http://localhost:11434");
            let current = cx
                .global::<ProviderModel>()
                .providers()
                .iter()
                .find(|p| p.provider_type == ProviderType::Ollama)
                .and_then(|p| p.base_url.clone())
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            state.set_value(current, window, cx);
            state
        });
        let azure_key = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Paste key").masked(true);
            let current = azure_field(cx, |p| p.api_key.clone());
            state.set_value(current, window, cx);
            state
        });
        let azure_endpoint = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder("https://my-resource.openai.azure.com");
            let current = azure_field(cx, |p| p.base_url.clone());
            state.set_value(current, window, cx);
            state
        });
        let azure_deployment = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("gpt-5-chat");
            let current = azure_field(cx, |p| p.extra_config.get("deployment").cloned());
            state.set_value(current, window, cx);
            state
        });

        window.open_dialog(cx, move |dialog, _window, cx| {
            let fg = cx.theme().foreground;
            let muted_fg = cx.theme().muted_foreground;
            let border = cx.theme().border;
            let accent = cx.theme().primary;
            let accent_bg = cx.theme().accent;
            let success = cx.theme().success;
            let warning = cx.theme().warning;
            let danger = cx.theme().danger;

            let configured: Vec<ProviderType> = cx
                .global::<ProviderModel>()
                .configured_providers()
                .map(|p| p.provider_type.clone())
                .collect();
            let models = cx.global::<ModelsModel>().models();
            let azure_expanded = expanded.borrow().as_ref() == Some(&ProviderType::AzureOpenAI);
            let uses_entra_id = cx
                .global::<ProviderModel>()
                .providers()
                .iter()
                .find(|p| p.provider_type == ProviderType::AzureOpenAI)
                .map(|p| p.azure_auth_method() == AzureAuthMethod::EntraId)
                .unwrap_or(false);

            let status_line = |provider_type: &ProviderType| -> (gpui::Hsla, String) {
                let count = models
                    .iter()
                    .filter(|m| &m.provider_type == provider_type)
                    .count();
                if configured.contains(provider_type) {
                    (success, format!("Connected · {count} models"))
                } else {
                    (
                        warning,
                        "Key missing — models hidden until connected".to_string(),
                    )
                }
            };

            let test_label = |provider_type: &ProviderType| -> Option<(gpui::Hsla, String)> {
                test_state
                    .borrow()
                    .iter()
                    .find(|(p, _)| p == provider_type)
                    .map(|(_, state)| match state {
                        TestState::Testing => (muted_fg, "Testing…".to_string()),
                        TestState::Ok(msg) => (success, msg.clone()),
                        TestState::Failed(msg) => (danger, msg.clone()),
                    })
            };

            dialog
                .title("Provider keys")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(660.))
                .child(
                    v_flex()
                        .child(
                            div()
                                .px_4()
                                .pb_3()
                                .text_xs()
                                .text_color(muted_fg)
                                .child("Stored with your app settings, not in your project files"),
                        )
                        // ── OpenRouter ────────────────────────────────────
                        .child({
                            let (dot, status) = status_line(&ProviderType::OpenRouter);
                            v_flex()
                                .px_4()
                                .py_3()
                                .gap_2()
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(div().size(px(7.)).rounded_full().bg(dot))
                                        .child(div().flex_1().text_sm().text_color(fg).child("OpenRouter"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted_fg)
                                                .child(status),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .flex_1()
                                                .child(Input::new(&openrouter_key).small().mask_toggle()),
                                        )
                                        .child({
                                            let openrouter_key = openrouter_key.clone();
                                            Button::new("save-openrouter")
                                                .label("Save")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    let key = openrouter_key.read(cx).value().to_string();
                                                    providers_controller::update_or_create_provider(
                                                        cx,
                                                        ProviderType::OpenRouter,
                                                        key,
                                                    );
                                                })
                                        })
                                        .child({
                                            let openrouter_key = openrouter_key.clone();
                                            let test_state = test_state.clone();
                                            Button::new("test-openrouter")
                                                .label("Test")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    let key =
                                                        openrouter_key.read(cx).value().to_string();
                                                    set_test_state(
                                                        &test_state,
                                                        ProviderType::OpenRouter,
                                                        TestState::Testing,
                                                    );
                                                    cx.refresh_windows();

                                                    let test_state = test_state.clone();
                                                    cx.spawn(async move |cx: &mut AsyncApp| {
                                                        let result = verify_openrouter_key(&key).await;
                                                        set_test_state(
                                                            &test_state,
                                                            ProviderType::OpenRouter,
                                                            match result {
                                                                Ok(()) => TestState::Ok(
                                                                    "Key verified".to_string(),
                                                                ),
                                                                Err(e) => TestState::Failed(
                                                                    e.to_string(),
                                                                ),
                                                            },
                                                        );
                                                        cx.update(|cx| cx.refresh_windows()).ok();
                                                    })
                                                    .detach();
                                                })
                                        }),
                                )
                                .when_some(test_label(&ProviderType::OpenRouter), |this, (color, msg)| {
                                    this.child(div().text_xs().text_color(color).child(msg))
                                })
                        })
                        // ── Ollama ────────────────────────────────────────
                        .child({
                            let (dot, status) = status_line(&ProviderType::Ollama);
                            v_flex()
                                .px_4()
                                .py_3()
                                .gap_2()
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(div().size(px(7.)).rounded_full().bg(dot))
                                        .child(div().flex_1().text_sm().text_color(fg).child("Ollama"))
                                        .child(div().text_xs().text_color(muted_fg).child(status)),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(div().flex_1().child(Input::new(&ollama_url).small()))
                                        .child({
                                            let ollama_url = ollama_url.clone();
                                            Button::new("save-ollama")
                                                .label("Save")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    let url = ollama_url.read(cx).value().to_string();
                                                    providers_controller::update_or_create_ollama(cx, url);
                                                })
                                        })
                                        .child({
                                            let ollama_url = ollama_url.clone();
                                            let test_state = test_state.clone();
                                            Button::new("test-ollama")
                                                .label("Test")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    let url = ollama_url.read(cx).value().to_string();
                                                    set_test_state(
                                                        &test_state,
                                                        ProviderType::Ollama,
                                                        TestState::Testing,
                                                    );
                                                    cx.refresh_windows();

                                                    let test_state = test_state.clone();
                                                    cx.spawn(async move |cx: &mut AsyncApp| {
                                                        let result =
                                                            discover_ollama_models(&url).await;
                                                        set_test_state(
                                                            &test_state,
                                                            ProviderType::Ollama,
                                                            match result {
                                                                Ok(models) => TestState::Ok(format!(
                                                                    "Running · {} models",
                                                                    models.len()
                                                                )),
                                                                Err(e) => TestState::Failed(
                                                                    e.to_string(),
                                                                ),
                                                            },
                                                        );
                                                        cx.update(|cx| cx.refresh_windows()).ok();
                                                    })
                                                    .detach();
                                                })
                                        }),
                                )
                                .when_some(test_label(&ProviderType::Ollama), |this, (color, msg)| {
                                    this.child(div().text_xs().text_color(color).child(msg))
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .child("No key needed — a local instance is detected automatically."),
                                )
                        })
                        // ── Azure OpenAI ──────────────────────────────────
                        .child({
                            let (dot, status) = status_line(&ProviderType::AzureOpenAI);
                            let expanded_for_click = expanded.clone();

                            v_flex()
                                .px_4()
                                .py_3()
                                .gap_2()
                                .when(azure_expanded, |this| this.bg(accent_bg))
                                .child(
                                    h_flex()
                                        .id("azure-row-header")
                                        .gap_2()
                                        .items_center()
                                        .cursor_pointer()
                                        .child(div().size(px(7.)).rounded_full().bg(dot))
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_sm()
                                                .text_color(fg)
                                                .child("Azure OpenAI"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(if configured
                                                    .contains(&ProviderType::AzureOpenAI)
                                                {
                                                    muted_fg
                                                } else {
                                                    warning
                                                })
                                                .child(status),
                                        )
                                        .child(
                                            Icon::new(if azure_expanded {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .size_3()
                                            .text_color(muted_fg),
                                        )
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                            let mut slot = expanded_for_click.borrow_mut();
                                            *slot = if slot.as_ref() == Some(&ProviderType::AzureOpenAI) {
                                                None
                                            } else {
                                                Some(ProviderType::AzureOpenAI)
                                            };
                                            drop(slot);
                                            cx.refresh_windows();
                                        }),
                                )
                                .when(azure_expanded, |this| {
                                    this.child(
                                        v_flex()
                                            .gap_3()
                                            .pt_2()
                                            .child(
                                                h_flex()
                                                    .justify_between()
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        v_flex()
                                                            .child(
                                                                div()
                                                                    .text_sm()
                                                                    .child("Use Entra ID instead of a key"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(muted_fg)
                                                                    .child(
                                                                        "Authenticate with your Azure AD account",
                                                                    ),
                                                            ),
                                                    )
                                                    .child(
                                                        Button::new("azure-entra-toggle")
                                                            .label(if uses_entra_id { "On" } else { "Off" })
                                                            .small()
                                                            .when(uses_entra_id, |b| b.primary())
                                                            .when(!uses_entra_id, |b| b.outline())
                                                            .on_click(move |_, _, cx| {
                                                                providers_controller::update_azure_auth_method(
                                                                    cx,
                                                                    !uses_entra_id,
                                                                );
                                                            }),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_3()
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(muted_fg)
                                                                    .child("API key"),
                                                            )
                                                            .child(
                                                                Input::new(&azure_key)
                                                                    .small()
                                                                    .mask_toggle(),
                                                            ),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(muted_fg)
                                                                    .child("Endpoint URL"),
                                                            )
                                                            .child(Input::new(&azure_endpoint).small()),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_3()
                                                    .items_end()
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(muted_fg)
                                                                    .child("Deployment name"),
                                                            )
                                                            .child(Input::new(&azure_deployment).small()),
                                                    )
                                                    .child({
                                                        let azure_key = azure_key.clone();
                                                        let azure_endpoint = azure_endpoint.clone();
                                                        Button::new("azure-connect")
                                                            .label("Connect & fetch models")
                                                            .small()
                                                            .primary()
                                                            .on_click(move |_, _, cx| {
                                                                let key =
                                                                    azure_key.read(cx).value().to_string();
                                                                let endpoint = azure_endpoint
                                                                    .read(cx)
                                                                    .value()
                                                                    .to_string();
                                                                providers_controller::update_or_create_azure(
                                                                    cx, key, endpoint,
                                                                );
                                                            })
                                                    }),
                                            ),
                                    )
                                })
                        })
                        // ── Footer ────────────────────────────────────────
                        .child(
                            h_flex()
                                .px_4()
                                .py_3()
                                .gap_3()
                                .items_center()
                                .border_t_1()
                                .border_color(border)
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .child("Removing a key hides its models but keeps their settings."),
                                )
                                .child(
                                    Button::new("provider-keys-done")
                                        .label("Done")
                                        .small()
                                        .primary()
                                        .on_click(|_, window, cx| {
                                            window.close_dialog(cx);
                                        }),
                                ),
                        ),
                )
        });
    }
}

fn cx_provider_key(cx: &App, provider_type: &ProviderType) -> String {
    cx.global::<ProviderModel>()
        .providers()
        .iter()
        .find(|p| &p.provider_type == provider_type)
        .and_then(|p| p.api_key.clone())
        .unwrap_or_default()
}
