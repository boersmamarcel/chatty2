use crate::settings::controllers::models_controller;
use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::{ProviderModel, ProviderType};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Global, IntoElement, Render, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IndexPath, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    list::{List, ListDelegate, ListItem, ListState},
    scroll::ScrollableElement,
    select::{Select, SelectState},
    tab::{Tab, TabBar},
    v_flex,
};
use tracing::trace;

// Global state to store the models list view
#[derive(Default)]
pub struct GlobalModelsListView {
    pub view: Option<Entity<ModelsListView>>,
}

impl Global for GlobalModelsListView {}

// Helper function to convert provider display name to ProviderType
fn string_to_provider_type(s: &str) -> ProviderType {
    match s {
        "OpenAI" => ProviderType::OpenAI,
        "Anthropic" => ProviderType::Anthropic,
        "Google Gemini" => ProviderType::Gemini,
        "Mistral" => ProviderType::Mistral,
        "Ollama" => ProviderType::Ollama,
        "Azure OpenAI" => ProviderType::AzureOpenAI,
        _ => ProviderType::OpenAI, // Default fallback
    }
}

pub struct ModelsListView {
    focus_handle: FocusHandle,
    list_state: Entity<ListState<ModelsListDelegate>>,
}

impl ModelsListView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let delegate = ModelsListDelegate::new(cx);
        let list_state = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));

        Self {
            focus_handle,
            list_state,
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.list_state.update(cx, |state, cx| {
            state.delegate_mut().reload(cx);
            cx.notify();
        });
    }

    fn show_add_model_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let top_p_input = cx.new(|cx| InputState::new(window, cx).placeholder("0.0 - 1.0"));
        let cost_input_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 2.50"));
        let cost_output_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 10.00"));
        let api_version_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g., 2024-10-21"));

        // Get configured providers from the global store
        let providers: Vec<String> = cx
            .global::<ProviderModel>()
            .configured_providers()
            .iter()
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
            .first()
            .map(|p| p.provider_type == ProviderType::AzureOpenAI)
            .unwrap_or(false);
        let is_azure_cell = std::rc::Rc::new(std::cell::Cell::new(initial_is_azure));

        let view = cx.entity().clone();

        window.open_dialog(cx, move |dialog, _, _| {
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
                                                        .child(div().text_sm().child(
                                                            "API Version (default: 2024-10-21)",
                                                        ))
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
                                                        .iter()
                                                        .map(|p| p.provider_type.display_name())
                                                        .collect();
                                                    let provider_str = provider_index
                                                        .and_then(|idx| {
                                                            all_providers.get(idx.row).copied()
                                                        })
                                                        .unwrap_or("OpenAI");
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

    fn show_edit_model_dialog(
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

        // Get configured providers and find the index of the current provider
        let configured_providers = cx.global::<ProviderModel>().configured_providers();
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
                                                let top_p_input = top_p_input.clone();
                                                let cost_input_input = cost_input_input.clone();
                                                let cost_output_input = cost_output_input.clone();
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
                                                        .iter()
                                                        .map(|p| p.provider_type.display_name())
                                                        .collect();
                                                    let provider_str = provider_index
                                                        .and_then(|idx| {
                                                            all_providers.get(idx.row).copied()
                                                        })
                                                        .unwrap_or("OpenAI");
                                                    let provider_type =
                                                        string_to_provider_type(provider_str);

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
                                                        top_p,
                                                        extra_params:
                                                            std::collections::HashMap::new(),
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

impl Focusable for ModelsListView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelsListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .size_full()
            .gap_4()
            .track_focus(&self.focus_handle)
            .child(
                // Header with title and add button
                h_flex()
                    .justify_between()
                    .items_center()
                    .pb_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex().gap_1().child(
                            div()
                                .text_2xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("AI Models"),
                        ),
                    )
                    .child(
                        Button::new("add-model-btn")
                            .label("+ Add Model")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                trace!("Add Model button clicked");
                                this.show_add_model_dialog(window, cx);
                            })),
                    ),
            )
            .child(
                // List container
                div()
                    .flex_1()
                    .w_full()
                    .child(List::new(&self.list_state).max_h(px(600.)).min_w(px(500.))),
            )
    }
}

struct ModelsListDelegate {
    sections: Vec<(ProviderType, Vec<ModelConfig>)>,
    selected_index: Option<IndexPath>,
    all_models: Vec<ModelConfig>,
    search_query: String,
}

impl ModelsListDelegate {
    fn new(cx: &mut App) -> Self {
        let mut delegate = Self {
            sections: Vec::new(),
            selected_index: None,
            all_models: Vec::new(),
            search_query: String::new(),
        };
        delegate.reload(cx);
        delegate
    }

    fn reload(&mut self, cx: &mut App) {
        let models = cx.global::<ModelsModel>().models().to_vec();
        self.all_models = models;
        self.rebuild_sections();
    }

    fn rebuild_sections(&mut self) {
        self.sections.clear();

        let provider_types = vec![
            ProviderType::OpenAI,
            ProviderType::Anthropic,
            ProviderType::Gemini,
            ProviderType::Mistral,
            ProviderType::Ollama,
            ProviderType::AzureOpenAI,
        ];

        for provider_type in provider_types {
            let mut provider_models: Vec<ModelConfig> = self
                .all_models
                .iter()
                .filter(|m| m.provider_type == provider_type)
                .cloned()
                .collect();

            // Apply search filter
            if !self.search_query.is_empty() {
                let query = self.search_query.to_lowercase();
                provider_models.retain(|m| {
                    m.name.to_lowercase().contains(&query)
                        || m.model_identifier.to_lowercase().contains(&query)
                });
            }

            // Only add section if it has models
            if !provider_models.is_empty() {
                self.sections.push((provider_type, provider_models));
            }
        }
    }

    fn get_model(&self, ix: IndexPath) -> Option<&ModelConfig> {
        self.sections
            .get(ix.section)
            .and_then(|(_, models)| models.get(ix.row))
    }
}

impl ListDelegate for ModelsListDelegate {
    type Item = ListItem;

    fn sections_count(&self, _cx: &App) -> usize {
        self.sections.len()
    }

    fn items_count(&self, section: usize, _cx: &App) -> usize {
        self.sections
            .get(section)
            .map(|(_, models)| models.len())
            .unwrap_or(0)
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let (provider_type, models) = self.sections.get(section)?;
        let provider_name = provider_type.display_name().to_string();
        let count = models.len();
        let theme = cx.theme();

        Some(
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .bg(theme.background)
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(provider_name),
                )
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(theme.muted)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}", count)),
                ),
        )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let model = self.get_model(ix)?.clone();
        let model_id = model.id.clone();
        let model_id_for_delete = model.id.clone();
        let is_selected = Some(ix) == self.selected_index;
        let theme = cx.theme();

        // Create unique IDs for buttons using computed row index
        let row_index = ix.section * 1000 + ix.row;

        Some(
            ListItem::new(ix)
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(model.name.clone()),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new(("edit-model", row_index))
                                                .label("Edit")
                                                .small()
                                                .outline()
                                                .on_click(move |_, window, cx| {
                                                    // Access global view and show edit dialog
                                                    let view_entity = cx
                                                        .try_global::<GlobalModelsListView>()
                                                        .and_then(|g| g.view.clone());

                                                    if let Some(view) = view_entity {
                                                        view.update(cx, |view, inner_cx| {
                                                            view.show_edit_model_dialog(
                                                                model_id.clone(),
                                                                window,
                                                                inner_cx,
                                                            );
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new(("delete-model", row_index))
                                                .label("Delete")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    // Delete from store
                                                    models_controller::delete_model(
                                                        model_id_for_delete.clone(),
                                                        cx,
                                                    );

                                                    // Refresh the list view
                                                    let view_entity = cx
                                                        .try_global::<GlobalModelsListView>()
                                                        .and_then(|g| g.view.clone());

                                                    if let Some(view) = view_entity {
                                                        view.update(cx, |view, cx| {
                                                            view.refresh(cx);
                                                        });
                                                    }
                                                }),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("Model: {}", model.model_identifier)),
                        )
                        .child(
                            h_flex()
                                .gap_4()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("Temperature: {:.1}", model.temperature))
                                .when_some(model.max_tokens, |this, max_tokens| {
                                    this.child(format!("Max tokens: {}", max_tokens))
                                })
                                .when_some(model.top_p, |this, top_p| {
                                    this.child(format!("Top P: {:.2}", top_p))
                                }),
                        ),
                )
                .selected(is_selected)
                .px_3()
                .py_3(),
        )
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let message = if self.search_query.is_empty() {
            "No models configured yet"
        } else {
            "No models match your search"
        };

        v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .gap_2()
            .py_8()
            .child(
                div()
                    .text_lg()
                    .text_color(theme.muted_foreground)
                    .child(message),
            )
            .when(!self.search_query.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground.opacity(0.7))
                        .child("Try adjusting your search terms"),
                )
            })
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> gpui::Task<()> {
        self.search_query = query.to_string();
        self.rebuild_sections();
        cx.notify();
        gpui::Task::ready(())
    }
}
