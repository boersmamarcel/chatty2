use crate::settings::controllers::models_controller;
use crate::settings::models::models_store::ModelsModel;
use crate::settings::models::providers_store::ProviderType;
use gpui::{div, prelude::*, rgb, IntoElement, Styled};
use gpui_component::{
    button::{Button, ButtonVariants},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    Sizable,
};

pub fn models_page() -> SettingPage {
    SettingPage::new("Models")
        .description("Configure AI models and their parameters")
        .resettable(true)
        .groups(create_model_groups())
}

fn create_model_groups() -> Vec<SettingGroup> {
    // Define all provider types
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

    let mut groups = Vec::new();

    // Add "Add Model" button as first group
    groups.push(create_add_model_group());

    // Create a group for each provider that has models
    for provider_type in provider_types {
        groups.push(create_provider_models_group(provider_type));
    }

    groups
}

fn create_add_model_group() -> SettingGroup {
    SettingGroup::new()
        .title("Models")
        .description("Configure AI models with their parameters (temperature, preamble, etc.)")
        .items(vec![SettingItem::new(
            "Add New Model",
            SettingField::render(|_options, _window, cx| {
                Button::new("add-model-btn")
                    .label("+ Add Model")
                    .primary()
                    .on_click(|_, _, cx| {
                        models_controller::open_create_model_modal(cx);
                    })
                    .into_any_element()
            }),
        )
        .description("Add a new AI model configuration")])
}

fn create_provider_models_group(provider_type: ProviderType) -> SettingGroup {
    let provider_name = provider_type.display_name().to_string();
    let provider_type_clone = provider_type.clone();

    SettingGroup::new()
        .title(provider_name.clone())
        .description(format!("Models configured for {}", provider_name))
        .items(vec![SettingItem::new(
            "Configured Models",
            SettingField::render(move |_options, _window, cx| {
                let models = cx.global::<ModelsModel>().models_by_provider(&provider_type_clone);

                if models.is_empty() {
                    return div()
                        .text_color(rgb(0x888888))
                        .child("No models configured for this provider")
                        .into_any_element();
                }

                let mut container = div().flex().flex_col().gap_2();

                for (idx, model) in models.iter().enumerate() {
                    let model_id = model.id.clone();
                    let model_id_for_delete = model.id.clone();

                    let model_card = div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .border_1()
                        .border_color(rgb(0x333333))
                        .rounded_md()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child(model.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new(("edit-model", idx))
                                                .label("Edit")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    models_controller::open_edit_model_modal(
                                                        model_id.clone(),
                                                        cx,
                                                    );
                                                }),
                                        )
                                        .child(
                                            Button::new(("delete-model", idx))
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
                                .text_color(rgb(0xAAAAAA))
                                .child(format!("Model: {}", model.model_identifier)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_4()
                                .text_sm()
                                .text_color(rgb(0xAAAAAA))
                                .child(format!("Temperature: {:.1}", model.temperature))
                                .when_some(model.max_tokens, |this, max_tokens| {
                                    this.child(format!("Max tokens: {}", max_tokens))
                                }),
                        );

                    container = container.child(model_card);
                }

                container.into_any_element()
            }),
        )])
}
