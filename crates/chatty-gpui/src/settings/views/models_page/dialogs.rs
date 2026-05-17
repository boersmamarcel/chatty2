//! Add / edit model dialog flows for `ModelsListView`.
//!
//! These two methods are the bulk of the original `models_page.rs` and
//! mix form construction, validation, and persistence for every
//! supported provider type (Anthropic, OpenAI, Azure, Ollama, Mistral,
//! Gemini, OpenRouter). Kept separate so `mod.rs` is dominated by the
//! view lifecycle and Render impl, not by dialog plumbing.

use super::*;

impl ModelsListView {
    pub(super) fn show_add_model_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        trace!("Opening Add Model dialog");

        // Track active tab (0 = Basic, 1 = Advanced)
        let active_tab = std::rc::Rc::new(std::cell::Cell::new(0usize));

        // Create fresh input states for the dialog
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., GPT-4 Turbo"));
        let model_id_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g., gpt-4-turbo"));
        let temperature_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("1.0");
            state.set_value("1.0".to_string(), window, cx);
            state
        });
        let preamble_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("System instructions for the model"));
        let max_tokens_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 4096"));
        let max_context_window_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 200000"));
        let top_p_input = cx.new(|cx| InputState::new(window, cx).placeholder("0.0 - 1.0"));
        let cost_input_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 2.50"));
        let cost_output_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 10.00"));
        let api_version_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 2024-10-21"));

        // Get configured providers from the global store
        let providers: Vec<String> = cx
            .global::<ProviderModel>()
            .configured_providers()
            .map(|p| p.provider_type.display_name().to_string())
            .collect();

        // Handle empty provider list
        if providers.is_empty() {
            window.push_notification(
                "Please configure at least one provider in Settings > Providers before adding models",
                cx,
            );
            return;
        }

        let provider_select =
            cx.new(|cx| SelectState::new(providers, Some(IndexPath::new(0)), window, cx));

        // Track whether the currently selected provider is Azure (Rc<Cell> to avoid capturing cx in dialog closure)
        let initial_is_azure = cx
            .global::<ProviderModel>()
            .configured_providers()
            .next()
            .map(|p| p.provider_type == ProviderType::AzureOpenAI)
            .unwrap_or(false);
        let is_azure_cell = std::rc::Rc::new(std::cell::Cell::new(initial_is_azure));

        // Track whether the currently selected provider is OpenRouter
        let initial_is_openrouter = cx
            .global::<ProviderModel>()
            .configured_providers()
            .next()
            .map(|p| p.provider_type == ProviderType::OpenRouter)
            .unwrap_or(false);
        let is_openrouter_cell = std::rc::Rc::new(std::cell::Cell::new(initial_is_openrouter));

        // Keep is_azure_cell / is_openrouter_cell in sync when the user changes the provider selection.
        cx.subscribe(&provider_select, {
            let is_azure_cell = is_azure_cell.clone();
            let is_openrouter_cell = is_openrouter_cell.clone();
            let provider_select = provider_select.clone();
            move |_this, _entity, event: &SelectEvent<Vec<String>>, cx| {
                if matches!(event, SelectEvent::Confirm(_)) {
                    let providers: Vec<_> = cx
                        .global::<ProviderModel>()
                        .configured_providers()
                        .collect();
                    let selected = provider_select.read(cx).selected_index(cx);
                    let provider_type = selected
                        .and_then(|idx| providers.get(idx.row))
                        .map(|p| p.provider_type.clone())
                        .unwrap_or(ProviderType::OpenRouter);
                    is_azure_cell.set(provider_type == ProviderType::AzureOpenAI);
                    is_openrouter_cell.set(provider_type == ProviderType::OpenRouter);
                }
            }
        })
        .detach();

        let view = cx.entity().clone();

        let catalog_models: Vec<
            chatty_core::settings::providers::openrouter::discovery::OpenRouterModel,
        > = cx
            .try_global::<OpenRouterCatalog>()
            .map(|c| c.models.clone())
            .unwrap_or_default();
        let theme_secondary = cx.theme().secondary;
        let theme_border = cx.theme().border;
        let muted_foreground = cx.theme().muted_foreground;

        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search OpenRouter models..."));

        window.open_dialog(cx, move |dialog, _window, cx| {
            dialog
                .title("Add New Model")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(600.))
                .child(
                    div()
                        .id("add-model-form")
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
                                    let is_azure = is_azure_cell.get();
                                    let is_openrouter = is_openrouter_cell.get();
                                    if current_tab == 0 {
                                        // Basic tab
                                        let mut root = v_flex()
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
                                            );

                                            if is_openrouter && !catalog_models.is_empty() {
                                                let search_input = search_input.clone();
                                                let name_input = name_input.clone();
                                                let model_id_input = model_id_input.clone();
                                                let max_context_window_input = max_context_window_input.clone();
                                                let cost_input_input = cost_input_input.clone();
                                                let cost_output_input = cost_output_input.clone();
                                                root = root.child(
                                                    v_flex()
                                                        .gap_1()
                                                        .child(div().text_sm().child("OpenRouter Catalog"))
                                                        .child(Input::new(&search_input))
                                                        .child({
                                                            let query = search_input.read(cx).value().to_lowercase();
                                                            let filtered: Vec<_> = catalog_models
                                                                    .iter()
                                                                    .filter(|m| {
                                                                        query.is_empty()
                                                                            || m.name.to_lowercase().contains(&query)
                                                                            || m.id.to_lowercase().contains(&query)
                                                                    })
                                                                    .take(40)
                                                                    .cloned()
                                                                    .collect();
                                                            div()
                                                                .max_h(px(180.0))
                                                                .overflow_y_scrollbar()
                                                                .border_1()
                                                                .border_color(theme_border)
                                                                .rounded_sm()
                                                                .flex()
                                                                .flex_col()
                                                                .children(if filtered.is_empty() {
                                                                    vec![div()
                                                                        .px_3()
                                                                        .py_2()
                                                                        .text_sm()
                                                                        .text_color(muted_foreground)
                                                                        .child("No matches")
                                                                        .into_any_element()]
                                                                } else {
                                                                    filtered.into_iter().map(|m| {
                                                                        let model_id_val = m.id.clone();
                                                                        let display_name_val = m.name.clone();
                                                                        let ctx_len_val = m.context_length;
                                                                        let prompt_cost_val = m.pricing.prompt.parse::<f64>().ok();
                                                                        let completion_cost_val = m.pricing.completion.parse::<f64>().ok();
                                                                        let ni = name_input.clone();
                                                                        let mi = model_id_input.clone();
                                                                        let mci = max_context_window_input.clone();
                                                                        let cii = cost_input_input.clone();
                                                                        let coi = cost_output_input.clone();
                                                                        div()
                                                                            .px_3()
                                                                            .py_2()
                                                                            .cursor_pointer()
                                                                            .hover(|style| style.bg(theme_secondary))
                                                                            .text_sm()
                                                                            .child(display_name_val.clone())
                                                                            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                                                                                ni.update(cx, |state, _cx| { state.set_value(display_name_val.clone(), _window, _cx); });
                                                                                mi.update(cx, |state, _cx| { state.set_value(model_id_val.clone(), _window, _cx); });
                                                                                mci.update(cx, |state, _cx| { state.set_value(ctx_len_val.to_string(), _window, _cx); });
                                                                                if let Some(cost) = prompt_cost_val {
                                                                                    let cost_per_million = cost * 1_000_000.0;
                                                                                    cii.update(cx, |state, _cx| { state.set_value(format!("{:.4}", cost_per_million), _window, _cx); });
                                                                                }
                                                                                if let Some(cost) = completion_cost_val {
                                                                                    let cost_per_million = cost * 1_000_000.0;
                                                                                    coi.update(cx, |state, _cx| { state.set_value(format!("{:.4}", cost_per_million), _window, _cx); });
                                                                                }
                                                                            })
                                                                            .into_any_element()
                                                                    }).collect::<Vec<_>>()
                                                                })
                                                        })
                                                );
                                            }

                                        root = root.child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    div().text_sm().child("Model Identifier *"),
                                                )
                                                .child(Input::new(&model_id_input)),
                                        );
                                        root
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

                                                    let config = ModelConfig {
                                                        id: uuid::Uuid::new_v4().to_string(),
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
                                                        supports_images: false,
                                                        supports_pdf: false,
                                                        supports_temperature: true,
                                                    };

                                                    // Save the model (capabilities auto-set by create_model)
                                                    models_controller::create_model(config, cx);

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
                                                        supports_images: false,
                                                        supports_pdf: false,
                                                        supports_temperature: true,
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

