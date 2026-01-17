use crate::settings::controllers::models_controller;
use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::ProviderType;
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
    select::{Select, SelectState},
    v_flex,
};

// Global state to store the models list view
pub struct GlobalModelsListView {
    pub view: Option<Entity<ModelsListView>>,
}

impl Default for GlobalModelsListView {
    fn default() -> Self {
        Self { view: None }
    }
}

impl Global for GlobalModelsListView {}

// Helper function to convert provider display name to ProviderType
fn string_to_provider_type(s: &str) -> ProviderType {
    match s {
        "OpenAI" => ProviderType::OpenAI,
        "Anthropic" => ProviderType::Anthropic,
        "Gemini" => ProviderType::Gemini,
        "Cohere" => ProviderType::Cohere,
        "Perplexity" => ProviderType::Perplexity,
        "XAI" => ProviderType::XAI,
        "Azure OpenAI" => ProviderType::AzureOpenAI,
        "Hugging Face" => ProviderType::HuggingFace,
        "Ollama" => ProviderType::Ollama,
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
        println!("Opening Add Model dialog...");
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

        let providers = vec![
            "OpenAI".to_string(),
            "Anthropic".to_string(),
            "Gemini".to_string(),
            "Cohere".to_string(),
            "Perplexity".to_string(),
            "XAI".to_string(),
            "Azure OpenAI".to_string(),
            "Hugging Face".to_string(),
            "Ollama".to_string(),
        ];
        let provider_select =
            cx.new(|cx| SelectState::new(providers, Some(IndexPath::new(0)), window, cx));

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
                    v_flex()
                        .gap_3()
                        .p_4()
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
                                .child(div().text_sm().child("Model Identifier *"))
                                .child(Input::new(&model_id_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Temperature"))
                                .child(Input::new(&temperature_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Preamble / System Prompt"))
                                .child(Input::new(&preamble_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Max Tokens (optional)"))
                                .child(Input::new(&max_tokens_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Top P (optional)"))
                                .child(Input::new(&top_p_input)),
                        ),
                )
                .footer({
                    let view = view.clone();
                    let name_input = name_input.clone();
                    let model_id_input = model_id_input.clone();
                    let temperature_input = temperature_input.clone();
                    let preamble_input = preamble_input.clone();
                    let max_tokens_input = max_tokens_input.clone();
                    let top_p_input = top_p_input.clone();
                    let provider_select = provider_select.clone();

                    move |_, _, _, _cx| {
                        vec![
                            Button::new("save").primary().label("Save").on_click({
                                let view = view.clone();
                                let name_input = name_input.clone();
                                let model_id_input = model_id_input.clone();
                                let temperature_input = temperature_input.clone();
                                let preamble_input = preamble_input.clone();
                                let max_tokens_input = max_tokens_input.clone();
                                let top_p_input = top_p_input.clone();
                                let provider_select = provider_select.clone();

                                move |_, window, cx| {
                                    // Validate and collect form data
                                    let name = name_input.read(cx).value();
                                    let model_identifier = model_id_input.read(cx).value();
                                    let temperature_str = temperature_input.read(cx).value();
                                    let preamble = preamble_input.read(cx).value();
                                    let max_tokens_str = max_tokens_input.read(cx).value();
                                    let top_p_str = top_p_input.read(cx).value();
                                    let provider_index =
                                        provider_select.read(cx).selected_index(cx);

                                    // Validation
                                    if name.trim().is_empty() {
                                        window.push_notification("Model name is required", cx);
                                        return;
                                    }
                                    if model_identifier.trim().is_empty() {
                                        window
                                            .push_notification("Model identifier is required", cx);
                                        return;
                                    }

                                    let temperature = temperature_str
                                        .parse::<f32>()
                                        .unwrap_or(1.0)
                                        .clamp(0.0, 2.0);

                                    let max_tokens = if max_tokens_str.trim().is_empty() {
                                        None
                                    } else {
                                        max_tokens_str.parse::<i32>().ok().filter(|&v| v > 0)
                                    };

                                    let top_p = if top_p_str.trim().is_empty() {
                                        None
                                    } else {
                                        top_p_str
                                            .parse::<f32>()
                                            .ok()
                                            .filter(|&v| (0.0..=1.0).contains(&v))
                                    };

                                    let providers = vec![
                                        "OpenAI",
                                        "Anthropic",
                                        "Gemini",
                                        "Cohere",
                                        "Perplexity",
                                        "XAI",
                                        "Azure OpenAI",
                                        "Hugging Face",
                                        "Ollama",
                                    ];
                                    let provider_str = provider_index
                                        .and_then(|idx| providers.get(idx.row))
                                        .unwrap_or(&"OpenAI");
                                    let provider_type = string_to_provider_type(provider_str);

                                    let config = ModelConfig {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        name: name.trim().to_string(),
                                        provider_type,
                                        model_identifier: model_identifier.trim().to_string(),
                                        temperature,
                                        preamble: preamble.to_string(),
                                        max_tokens,
                                        top_p,
                                        extra_params: std::collections::HashMap::new(),
                                    };

                                    // Save the model
                                    models_controller::create_model(config, cx);

                                    // Close dialog
                                    window.close_dialog(cx);

                                    // Refresh list
                                    view.update(cx, |view, cx| {
                                        view.refresh(cx);
                                    });
                                }
                            }),
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        ]
                    }
                })
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
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("AI Models"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        "Configure AI models with their parameters (temperature, preamble, etc.)",
                                    ),
                            ),
                    )
                    .child(
                        Button::new("add-model-btn")
                            .label("+ Add Model")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                println!("Add Model button clicked!");
                                this.show_add_model_dialog(window, cx);
                            })),
                    ),
            )
            .child(
                // List container
                div()
                    .flex_1()
                    .child(List::new(&self.list_state).max_h(px(600.))),
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
            ProviderType::Cohere,
            ProviderType::Perplexity,
            ProviderType::XAI,
            ProviderType::AzureOpenAI,
            ProviderType::HuggingFace,
            ProviderType::Ollama,
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
                                                .on_click(move |_, _, cx| {
                                                    // We need to find the ModelsListView and call show_edit_model_dialog
                                                    // This will be handled via a workaround since we can't access the view directly
                                                    models_controller::open_edit_model_modal(
                                                        model_id.clone(),
                                                        cx,
                                                    );
                                                }),
                                        )
                                        .child(
                                            Button::new(("delete-model", row_index))
                                                .label("Delete")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    models_controller::delete_model(
                                                        model_id_for_delete.clone(),
                                                        cx,
                                                    );
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
