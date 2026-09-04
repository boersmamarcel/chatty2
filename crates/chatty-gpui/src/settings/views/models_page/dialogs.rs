//! The per-model edit form, reached from a roster row's ⋯ menu.
//!
//! Adding is handled by `add_model_sheet` — this is the full parameter form
//! for one model that already exists: name, identifier, provider, and the
//! advanced tab's temperature, token limits, costs and Azure API version.
//! Kept separate so `mod.rs` is dominated by the roster itself, not by form
//! plumbing.

use super::*;

impl ModelsListView {
    pub(super) fn show_edit_model_dialog(
        &mut self,
        model_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace!("Opening Edit Model dialog for ID: {}", model_id);

        // Load existing model data
        let existing_model = match cx.global::<ModelsModel>().get_model(&model_id) {
            Some(model) => model.clone(),
            None => {
                window.push_notification("Model not found", cx);
                return;
            }
        };

        // Track active tab (0 = Basic, 1 = Advanced)
        let active_tab = std::rc::Rc::new(std::cell::Cell::new(0usize));

        // Create input states and pre-populate with existing values
        let name_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("e.g., GPT-4 Turbo");
            state.set_value(existing_model.name.clone(), window, cx);
            state
        });
        let model_id_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("e.g., gpt-4-turbo");
            state.set_value(existing_model.model_identifier.clone(), window, cx);
            state
        });
        let temperature_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("1.0");
            state.set_value(existing_model.temperature.to_string(), window, cx);
            state
        });
        let preamble_input = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder("System instructions for the model");
            state.set_value(existing_model.preamble.clone(), window, cx);
            state
        });
        let max_tokens_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("e.g., 4096");
            if let Some(max_tokens) = existing_model.max_tokens {
                state.set_value(max_tokens.to_string(), window, cx);
            }
            state
        });
        let max_context_window_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("e.g., 200000");
            if let Some(max_context_window) = existing_model.max_context_window {
                state.set_value(max_context_window.to_string(), window, cx);
            }
            state
        });
        let top_p_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("0.0 - 1.0");
            if let Some(top_p) = existing_model.top_p {
                state.set_value(top_p.to_string(), window, cx);
            }
            state
        });
        let cost_input_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("e.g., 2.50");
            if let Some(cost) = existing_model.cost_per_million_input_tokens {
                state.set_value(cost.to_string(), window, cx);
            }
            state
        });
        let cost_output_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("e.g., 10.00");
            if let Some(cost) = existing_model.cost_per_million_output_tokens {
                state.set_value(cost.to_string(), window, cx);
            }
            state
        });
        let api_version_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder(format!("e.g., {}", AZURE_DEFAULT_API_VERSION));
            if let Some(api_version) = existing_model.extra_params.get("api_version") {
                state.set_value(api_version.clone(), window, cx);
            }
            state
        });

        // Get configured providers and find the index of the current provider.
        // Collect once since the result is used twice (provider names + position lookup).
        let configured_providers: Vec<_> = cx
            .global::<ProviderModel>()
            .configured_providers()
            .collect();
        let providers: Vec<String> = configured_providers
            .iter()
            .map(|p| p.provider_type.display_name().to_string())
            .collect();

        let provider_index = configured_providers
            .iter()
            .position(|p| p.provider_type == existing_model.provider_type)
            .map(IndexPath::new);

        let provider_select = cx.new(|cx| SelectState::new(providers, provider_index, window, cx));

        let view = cx.entity().clone();
        let model_id_for_update = model_id.clone();
        let is_azure = matches!(existing_model.provider_type, ProviderType::AzureOpenAI);
        let stored_model = existing_model.clone();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Edit Model")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(600.))
                .child(
                    div()
                        .id("edit-model-form")
                        .overflow_y_scrollbar()
                        .max_h(px(350.))
                        .child(
                            v_flex()
                                .gap_3()
                                .p_4()
                                .child({
                                    let active_tab = active_tab.clone();
                                    TabBar::new("model-tabs")
                                        .selected_index(active_tab.get())
                                        .on_click({
                                            let active_tab = active_tab.clone();
                                            move |index, _, _| {
                                                active_tab.set(*index);
                                            }
                                        })
                                        .child(Tab::new().label("Basic"))
                                        .child(Tab::new().label("Advanced"))
                                })
                                .child({
                                    let current_tab = active_tab.get();
                                    if current_tab == 0 {
                                        // Basic tab
                                        v_flex()
                                            .gap_3()
                                            .p_2()
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_sm().child("Model Name *"))
                                                    .child(Input::new(&name_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_sm().child("Provider *"))
                                                    .child(Select::new(&provider_select)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div().text_sm().child("Model Identifier *"),
                                                    )
                                                    .child(Input::new(&model_id_input)),
                                            )
                                    } else {
                                        // Advanced tab
                                        v_flex()
                                            .gap_3()
                                            .p_2()
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_sm().child("Temperature"))
                                                    .child(Input::new(&temperature_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .child("Preamble / System Prompt"),
                                                    )
                                                    .child(Input::new(&preamble_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .child("Max Tokens (optional)"),
                                                    )
                                                    .child(Input::new(&max_tokens_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .child("Max Context Window (optional)"),
                                                    )
                                                    .child(Input::new(&max_context_window_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div().text_sm().child("Top P (optional)"),
                                                    )
                                                    .child(Input::new(&top_p_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_sm().child(
                                                        "Cost Per Million Input Tokens (USD)",
                                                    ))
                                                    .child(Input::new(&cost_input_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_sm().child(
                                                        "Cost Per Million Output Tokens (USD)",
                                                    ))
                                                    .child(Input::new(&cost_output_input)),
                                            )
                                            .when(is_azure, |this| {
                                                this.child(
                                                    v_flex()
                                                        .gap_1()
                                                        .child(div().text_sm().child(format!(
                                                            "API Version (default: {})",
                                                            AZURE_DEFAULT_API_VERSION
                                                        )))
                                                        .child(Input::new(&api_version_input)),
                                                )
                                            })
                                    }
                                })
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .justify_end()
                                        .pt_4()
                                        .child(Button::new("cancel").label("Cancel").on_click(
                                            move |_, window, cx| {
                                                window.close_dialog(cx);
                                            },
                                        ))
                                        .child(
                                            Button::new("save").primary().label("Save").on_click({
                                                let view = view.clone();
                                                let name_input = name_input.clone();
                                                let model_id_input = model_id_input.clone();
                                                let temperature_input = temperature_input.clone();
                                                let preamble_input = preamble_input.clone();
                                                let max_tokens_input = max_tokens_input.clone();
                                                let max_context_window_input =
                                                    max_context_window_input.clone();
                                                let top_p_input = top_p_input.clone();
                                                let cost_input_input = cost_input_input.clone();
                                                let cost_output_input = cost_output_input.clone();
                                                let api_version_input = api_version_input.clone();
                                                let provider_select = provider_select.clone();
                                                let model_id_for_update =
                                                    model_id_for_update.clone();

                                                move |_, window, cx| {
                                                    // Validate and collect form data
                                                    let name = name_input.read(cx).value();
                                                    let model_identifier =
                                                        model_id_input.read(cx).value();
                                                    let temperature_str =
                                                        temperature_input.read(cx).value();
                                                    let preamble = preamble_input.read(cx).value();
                                                    let max_tokens_str =
                                                        max_tokens_input.read(cx).value();
                                                    let max_context_window_str =
                                                        max_context_window_input.read(cx).value();
                                                    let top_p_str = top_p_input.read(cx).value();
                                                    let provider_index =
                                                        provider_select.read(cx).selected_index(cx);

                                                    // Validation
                                                    if name.trim().is_empty() {
                                                        window.push_notification(
                                                            "Model name is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }
                                                    if model_identifier.trim().is_empty() {
                                                        window.push_notification(
                                                            "Model identifier is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    let temperature = temperature_str
                                                        .parse::<f32>()
                                                        .unwrap_or(1.0)
                                                        .clamp(0.0, 2.0);

                                                    let max_tokens =
                                                        if max_tokens_str.trim().is_empty() {
                                                            None
                                                        } else {
                                                            max_tokens_str
                                                                .parse::<i32>()
                                                                .ok()
                                                                .filter(|&v| v > 0)
                                                        };

                                                    let max_context_window =
                                                        if max_context_window_str.trim().is_empty()
                                                        {
                                                            None
                                                        } else {
                                                            max_context_window_str
                                                                .parse::<i32>()
                                                                .ok()
                                                                .filter(|&v| v > 0)
                                                        };

                                                    let top_p = if top_p_str.trim().is_empty() {
                                                        None
                                                    } else {
                                                        top_p_str
                                                            .parse::<f32>()
                                                            .ok()
                                                            .filter(|&v| (0.0..=1.0).contains(&v))
                                                    };

                                                    let cost_input_str =
                                                        cost_input_input.read(cx).value();
                                                    let cost_output_str =
                                                        cost_output_input.read(cx).value();

                                                    let cost_per_million_input_tokens =
                                                        if cost_input_str.trim().is_empty() {
                                                            None
                                                        } else {
                                                            cost_input_str
                                                                .parse::<f64>()
                                                                .ok()
                                                                .filter(|&v| v >= 0.0)
                                                        };

                                                    let cost_per_million_output_tokens =
                                                        if cost_output_str.trim().is_empty() {
                                                            None
                                                        } else {
                                                            cost_output_str
                                                                .parse::<f64>()
                                                                .ok()
                                                                .filter(|&v| v >= 0.0)
                                                        };

                                                    let all_providers: Vec<&str> = cx
                                                        .global::<ProviderModel>()
                                                        .configured_providers()
                                                        .map(|p| p.provider_type.display_name())
                                                        .collect();
                                                    let provider_str = provider_index
                                                        .and_then(|idx| {
                                                            all_providers.get(idx.row).copied()
                                                        })
                                                        .unwrap_or("OpenRouter");
                                                    let provider_type =
                                                        string_to_provider_type(provider_str);

                                                    let api_version_str =
                                                        api_version_input.read(cx).value();
                                                    let mut extra_params =
                                                        std::collections::HashMap::new();
                                                    if matches!(
                                                        provider_type,
                                                        ProviderType::AzureOpenAI
                                                    ) && !api_version_str.trim().is_empty()
                                                    {
                                                        extra_params.insert(
                                                            "api_version".to_string(),
                                                            api_version_str.trim().to_string(),
                                                        );
                                                    }

                                                    // Start from the stored config so everything
                                                    // this form doesn't edit — capabilities,
                                                    // source, favourite, default — survives.
                                                    let config = ModelConfig {
                                                        id: model_id_for_update.clone(),
                                                        name: name.trim().to_string(),
                                                        provider_type,
                                                        model_identifier: model_identifier
                                                            .trim()
                                                            .to_string(),
                                                        temperature,
                                                        preamble: preamble.to_string(),
                                                        max_tokens,
                                                        max_context_window,
                                                        top_p,
                                                        extra_params,
                                                        cost_per_million_input_tokens,
                                                        cost_per_million_output_tokens,
                                                        ..stored_model.clone()
                                                    };

                                                    // Update the model
                                                    models_controller::update_model(config, cx);

                                                    // Close dialog
                                                    window.close_dialog(cx);

                                                    // Refresh list
                                                    view.update(cx, |view, cx| {
                                                        view.refresh(cx);
                                                    });
                                                }
                                            }),
                                        ),
                                ),
                        ),
                )
        });
    }
}
