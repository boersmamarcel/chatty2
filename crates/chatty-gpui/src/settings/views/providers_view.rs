use crate::settings::controllers::providers_controller;
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderModel, ProviderType};
use gpui::{
    App, AppContext as _, Axis, Entity, SharedString, Styled, Window, prelude::FluentBuilder as _,
};
use gpui_component::{
    AxisExt as _, Sizable,
    input::{Input, InputEvent, InputState},
    setting::{RenderOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};
use std::rc::Rc;

pub fn providers_page() -> SettingPage {
    SettingPage::new("Providers").resettable(true).groups(vec![
        create_openrouter_group(),
        create_ollama_group(),
        create_azure_openai_group(),
    ])
}

fn create_openrouter_group() -> SettingGroup {
    create_provider_group(
        "OpenRouter",
        "Configure OpenRouter API access - gateway to 200+ models (Claude, Gemini, GPT, Mistral, Llama, and more)",
        ProviderType::OpenRouter,
        "Enter your OpenRouter API key (starts with sk-or-) to access all supported models",
    )
}

fn create_ollama_group() -> SettingGroup {
    SettingGroup::new()
        .title("Ollama")
        .description("Configure local Ollama instance - run LLaMA, Mistral, Mixtral locally")
        .items(vec![
            SettingItem::new(
                "Base URL",
                SettingField::input(
                    |cx: &App| {
                        cx.global::<ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| matches!(p.provider_type, ProviderType::Ollama))
                            .and_then(|p| p.base_url.clone())
                            .unwrap_or_else(|| "http://localhost:11434".to_string())
                            .into()
                    },
                    |val: SharedString, cx: &mut App| {
                        providers_controller::update_or_create_ollama(cx, val.to_string());
                    },
                ),
            )
            .description("Ollama server URL (default: http://localhost:11434)")
            .layout(Axis::Vertical),
        ])
}

fn create_azure_openai_group() -> SettingGroup {
    let provider_type_for_api = ProviderType::AzureOpenAI;

    SettingGroup::new()
        .title("Azure OpenAI")
        .description(
            "Configure Azure OpenAI - use Azure-hosted GPT-4o, GPT-4, and other OpenAI models",
        )
        .items(vec![
            SettingItem::new(
                "Use Entra ID",
                SettingField::switch(
                    |cx: &App| {
                        cx.global::<ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| p.provider_type == ProviderType::AzureOpenAI)
                            .map(|p| p.azure_auth_method() == AzureAuthMethod::EntraId)
                            .unwrap_or(false)
                    },
                    |use_entra_id: bool, cx: &mut App| {
                        providers_controller::update_azure_auth_method(cx, use_entra_id);
                    },
                ),
            )
            .description("Authenticate using Entra ID (Azure AD) instead of API key"),
            SettingItem::new(
                "API Key",
                masked_api_key_field(
                    move |cx: &App| {
                        cx.global::<ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| p.provider_type == provider_type_for_api)
                            .and_then(|p| p.api_key.clone())
                            .unwrap_or_default()
                            .into()
                    },
                    move |val: SharedString, cx: &mut App| {
                        let endpoint = azure_endpoint(cx);
                        providers_controller::update_or_create_azure(cx, val.to_string(), endpoint);
                    },
                ),
            )
            .description("Azure API key (not needed if using Entra ID)")
            .layout(Axis::Vertical),
            SettingItem::new(
                "Endpoint URL",
                SettingField::input(
                    |cx: &App| azure_endpoint(cx).into(),
                    |val: SharedString, cx: &mut App| {
                        let api_key = azure_api_key(cx);
                        providers_controller::update_or_create_azure(cx, api_key, val.to_string());
                    },
                ),
            )
            .description("Azure resource URL (e.g., https://my-resource.openai.azure.com)")
            .layout(Axis::Vertical),
        ])
}

fn azure_api_key(cx: &App) -> String {
    cx.global::<ProviderModel>()
        .providers()
        .iter()
        .find(|p| p.provider_type == ProviderType::AzureOpenAI)
        .and_then(|p| p.api_key.clone())
        .unwrap_or_default()
}

fn azure_endpoint(cx: &App) -> String {
    cx.global::<ProviderModel>()
        .providers()
        .iter()
        .find(|p| p.provider_type == ProviderType::AzureOpenAI)
        .and_then(|p| p.base_url.clone())
        .unwrap_or_default()
}

/// Create a masked API key input field with an eye toggle for visibility.
pub fn masked_api_key_field<V, S>(value: V, set_value: S) -> SettingField<SharedString>
where
    V: Fn(&App) -> SharedString + 'static,
    S: Fn(SharedString, &mut App) + 'static,
{
    type SetValueFn = dyn Fn(SharedString, &mut App);
    let set_value: Rc<SetValueFn> = Rc::new(set_value);

    SettingField::render(
        move |options: &RenderOptions, window: &mut Window, cx: &mut App| {
            let current_value = (value)(cx);
            let set_value = set_value.clone();

            struct MaskedInputState {
                input: Entity<InputState>,
                _subscription: gpui::Subscription,
            }

            let state = window
                .use_keyed_state(
                    SharedString::from(format!(
                        "masked-api-key-{}-{}-{}",
                        options.page_ix, options.group_ix, options.item_ix
                    )),
                    cx,
                    |window, cx| {
                        let input = cx.new(|cx| {
                            InputState::new(window, cx)
                                .default_value(current_value)
                                .masked(true)
                        });
                        let set_value = set_value.clone();
                        let _subscription = cx.subscribe(&input, {
                            move |_, input: Entity<InputState>, event: &InputEvent, cx| {
                                if let InputEvent::Change = event {
                                    let val = input.read(cx).value();
                                    (set_value)(val, cx);
                                }
                            }
                        });
                        MaskedInputState {
                            input,
                            _subscription,
                        }
                    },
                )
                .read(cx);

            Input::new(&state.input)
                .mask_toggle()
                .with_size(options.size)
                .map(|this| {
                    if options.layout.is_horizontal() {
                        this.w_64()
                    } else {
                        this.w_full()
                    }
                })
        },
    )
}

/// Generic helper to create a provider group with a masked API key field.
fn create_provider_group(
    title: &'static str,
    description: &'static str,
    provider_type: ProviderType,
    api_key_description: &'static str,
) -> SettingGroup {
    let provider_type_for_api = provider_type.clone();
    let provider_type_for_api_set = provider_type;

    SettingGroup::new()
        .title(title)
        .description(description)
        .items(vec![
            SettingItem::new(
                "API Key",
                masked_api_key_field(
                    move |cx: &App| {
                        cx.global::<ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| p.provider_type == provider_type_for_api)
                            .and_then(|p| p.api_key.clone())
                            .unwrap_or_default()
                            .into()
                    },
                    move |val: SharedString, cx: &mut App| {
                        providers_controller::update_or_create_provider(
                            cx,
                            provider_type_for_api_set.clone(),
                            val.to_string(),
                        );
                    },
                ),
            )
            .description(api_key_description)
            .layout(Axis::Vertical),
        ])
}
