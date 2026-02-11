use crate::settings::controllers::providers_controller;
use crate::settings::models::providers_store::{ProviderModel, ProviderType};
use gpui::{App, SharedString};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

pub fn providers_page() -> SettingPage {
    SettingPage::new("Providers").resettable(true).groups(vec![
        create_openai_group(),
        create_anthropic_group(),
        create_gemini_group(),
        create_mistral_group(),
        create_ollama_group(),
        create_azure_openai_group(),
    ])
}

fn create_openai_group() -> SettingGroup {
    create_provider_group(
        "OpenAI",
        "Configure OpenAI API access - GPT-4, GPT-3.5, ChatGPT, DALL-E",
        ProviderType::OpenAI,
        "Enter your OpenAI API key (starts with sk-) for GPT models",
    )
}

fn create_anthropic_group() -> SettingGroup {
    create_provider_group(
        "Anthropic (Claude)",
        "Configure Anthropic API access - Claude 3.5 Sonnet, Claude 3 Opus, Claude 3 Haiku",
        ProviderType::Anthropic,
        "Enter your Anthropic API key (starts with sk-ant-) for Claude models",
    )
}

fn create_gemini_group() -> SettingGroup {
    create_provider_group(
        "Google Gemini",
        "Configure Google Gemini API access - Gemini Pro, Gemini Ultra, Google AI",
        ProviderType::Gemini,
        "Enter your Google AI API key for Gemini models",
    )
}

fn create_mistral_group() -> SettingGroup {
    create_provider_group(
        "Mistral",
        "Configure Mistral API access - Mistral Large, Mistral Medium, Mistral Small",
        ProviderType::Mistral,
        "Enter your Mistral API key for Mistral models",
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
            .description("Ollama server URL (default: http://localhost:11434)"),
        ])
}

fn create_azure_openai_group() -> SettingGroup {
    SettingGroup::new()
        .title("Azure OpenAI")
        .description(
            "Configure Azure OpenAI - use Azure-hosted GPT-4o, GPT-4, and other OpenAI models",
        )
        .items(vec![
            SettingItem::new(
                "API Key",
                SettingField::input(
                    |cx: &App| azure_api_key(cx).into(),
                    |val: SharedString, cx: &mut App| {
                        let endpoint = azure_endpoint(cx);
                        providers_controller::update_or_create_azure(cx, val.to_string(), endpoint);
                    },
                ),
            )
            .description("Your Azure OpenAI API key"),
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
            .description("Azure resource base URL (e.g., https://my-resource.openai.azure.com) - do not include /openai/deployments path"),
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

// Generic helper to create a provider group with API key (only shows API key, not base URL)
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
                SettingField::input(
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
            .description(api_key_description),
        ])
}
