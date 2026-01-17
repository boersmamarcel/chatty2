use crate::settings::controllers::models_controller;
use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::ProviderType;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Root,
};

pub struct ModelFormView {
    mode: ModelFormMode,
    // Form fields
    model_id: Option<String>,
    name: String,
    provider_type: ProviderType,
    model_identifier: String,
    temperature: f32,
    preamble: String,
    max_tokens: String,
    top_p: String,
}

#[derive(Clone, Debug)]
pub enum ModelFormMode {
    Create,
    Edit(String), // model_id
}

impl ModelFormView {
    pub fn new_create(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            mode: ModelFormMode::Create,
            model_id: None,
            name: String::new(),
            provider_type: ProviderType::OpenAI,
            model_identifier: String::new(),
            temperature: 1.0,
            preamble: String::new(),
            max_tokens: String::new(),
            top_p: String::new(),
        }
    }

    pub fn new_edit(
        model: &ModelConfig,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            mode: ModelFormMode::Edit(model.id.clone()),
            model_id: Some(model.id.clone()),
            name: model.name.clone(),
            provider_type: model.provider_type.clone(),
            model_identifier: model.model_identifier.clone(),
            temperature: model.temperature,
            preamble: model.preamble.clone(),
            max_tokens: model
                .max_tokens
                .map(|t| t.to_string())
                .unwrap_or_default(),
            top_p: model.top_p.map(|t| t.to_string()).unwrap_or_default(),
        }
    }

    fn save(&self, cx: &mut Context<Self>) {
        // Validate required fields
        if self.name.trim().is_empty() {
            eprintln!("Model name is required");
            return;
        }
        if self.model_identifier.trim().is_empty() {
            eprintln!("Model identifier is required");
            return;
        }

        // Parse optional fields
        let max_tokens = if self.max_tokens.trim().is_empty() {
            None
        } else {
            match self.max_tokens.parse::<i32>() {
                Ok(val) if val > 0 => Some(val),
                _ => {
                    eprintln!("Invalid max tokens value");
                    return;
                }
            }
        };

        let top_p = if self.top_p.trim().is_empty() {
            None
        } else {
            match self.top_p.parse::<f32>() {
                Ok(val) if (0.0..=1.0).contains(&val) => Some(val),
                _ => {
                    eprintln!("Invalid top_p value (must be between 0.0 and 1.0)");
                    return;
                }
            }
        };

        let config = ModelConfig {
            id: self
                .model_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name: self.name.trim().to_string(),
            provider_type: self.provider_type.clone(),
            model_identifier: self.model_identifier.trim().to_string(),
            temperature: self.temperature,
            preamble: self.preamble.clone(),
            max_tokens,
            top_p,
            extra_params: std::collections::HashMap::new(),
        };

        // Save the model to global state
        match &self.mode {
            ModelFormMode::Create => {
                cx.update_global(|models: &mut ModelsModel, _cx| {
                    models.add_model(config.clone());
                });
            }
            ModelFormMode::Edit(_) => {
                cx.update_global(|models: &mut ModelsModel, _cx| {
                    models.update_model(config.clone());
                });
            }
        }

        // Trigger async save
        cx.emit(ModelFormAction::SaveComplete(config));

        // Close the modal window
        cx.defer(|cx| {
            cx.update_global(|window_state: &mut models_controller::GlobalModelFormWindow, _cx| {
                window_state.handle = None;
            });
        });
    }

    fn cancel(&self, cx: &mut Context<Self>) {
        // Close the modal window
        cx.defer(|cx| {
            cx.update_global(|window_state: &mut models_controller::GlobalModelFormWindow, _cx| {
                window_state.handle = None;
            });
        });
    }
}

impl Render for ModelFormView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match &self.mode {
            ModelFormMode::Create => "Add New Model",
            ModelFormMode::Edit(_) => "Edit Model",
        };

        let theme = cx.theme();
        let bg_color = theme.background;
        let border_color = theme.border;

        // Handle action events
        cx.subscribe(
            &cx.view().clone(),
            |view, _, event: &ModelFormAction, cx| match event {
                ModelFormAction::Save => view.save(cx),
                ModelFormAction::Cancel => view.cancel(cx),
                ModelFormAction::SaveComplete(config) => {
                    // Trigger async save to disk
                    let models_to_save = cx.global::<ModelsModel>().models().to_vec();
                    cx.spawn(|_view, _cx| async move {
                        use crate::MODELS_REPOSITORY;
                        let repo = MODELS_REPOSITORY.clone();
                        if let Err(e) = repo.save_all(models_to_save).await {
                            eprintln!("Failed to save models: {}", e);
                            eprintln!("Changes will be lost on restart - please try again");
                        }
                    })
                    .detach();

                    // Refresh settings window to show the new/updated model
                    cx.refresh_windows();
                }
            },
        )
        .detach();

        v_flex()
            .w_full()
            .h_full()
            .bg(bg_color)
            .p_6()
            .gap_4()
            .child(
                // Title
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .child(
                // Scrollable form content
                v_flex()
                    .flex_1()
                    .gap_4()
                    .overflow_y_scroll()
                    .child(self.render_form_field("Model Name", "e.g., GPT-4 Turbo", &self.name))
                    .child(self.render_provider_dropdown())
                    .child(self.render_form_field(
                        "Model Identifier",
                        "e.g., gpt-4-turbo",
                        &self.model_identifier,
                    ))
                    .child(self.render_temperature_field())
                    .child(self.render_preamble_field())
                    .child(self.render_form_field(
                        "Max Tokens (optional)",
                        "e.g., 4096",
                        &self.max_tokens,
                    ))
                    .child(self.render_form_field("Top P (optional)", "0.0 - 1.0", &self.top_p)),
            )
            .child(
                // Action buttons
                h_flex()
                    .gap_2()
                    .justify_end()
                    .border_t_1()
                    .border_color(border_color)
                    .pt_4()
                    .child(
                        Button::new("cancel-btn")
                            .label("Cancel")
                            .outline()
                            .on_click(|_, _, cx| {
                                cx.emit(ModelFormAction::Cancel);
                            }),
                    )
                    .child(
                        Button::new("save-btn")
                            .label("Save")
                            .primary()
                            .on_click(|_, _, cx| {
                                cx.emit(ModelFormAction::Save);
                            }),
                    ),
            )
    }
}

impl ModelFormView {
    fn render_form_field(&self, label: &str, placeholder: &str, value: &str) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(
                div()
                    .w_full()
                    .p_2()
                    .border_1()
                    .border_color(rgb(0x444444))
                    .rounded_md()
                    .child(if value.is_empty() {
                        div().text_color(rgb(0x666666)).child(placeholder)
                    } else {
                        div().child(value)
                    }),
            )
    }

    fn render_provider_dropdown(&self) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Provider"),
            )
            .child(
                div()
                    .w_full()
                    .p_2()
                    .border_1()
                    .border_color(rgb(0x444444))
                    .rounded_md()
                    .child(self.provider_type.display_name()),
            )
    }

    fn render_temperature_field(&self) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Temperature: {:.1}", self.temperature)),
            )
            .child(
                div()
                    .w_full()
                    .p_2()
                    .border_1()
                    .border_color(rgb(0x444444))
                    .rounded_md()
                    .child(format!("{:.1}", self.temperature))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x666666))
                            .child("Range: 0.0 - 2.0"),
                    ),
            )
    }

    fn render_preamble_field(&self) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Preamble / System Prompt"),
            )
            .child(
                div()
                    .w_full()
                    .p_2()
                    .border_1()
                    .border_color(rgb(0x444444))
                    .rounded_md()
                    .min_h(px(100.0))
                    .child(if self.preamble.is_empty() {
                        div()
                            .text_color(rgb(0x666666))
                            .child("System instructions for the model")
                    } else {
                        div().child(&self.preamble)
                    }),
            )
    }
}

// Actions emitted by the form
pub enum ModelFormAction {
    Save,
    Cancel,
    SaveComplete(ModelConfig),
}

impl EventEmitter<ModelFormAction> for ModelFormView {}
