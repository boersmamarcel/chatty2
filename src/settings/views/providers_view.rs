use crate::settings::models::providers_model::{ProviderConfig, ProviderModel, ProviderType};
use gpui::{App, SharedString};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

pub fn providers_page() -> SettingPage {
    SettingPage::new("Providers").resettable(true).groups(vec![
        create_openai_group(),
        create_anthropic_group(),
        create_gemini_group(),
        create_cohere_group(),
        create_perplexity_group(),
        create_xai_group(),
        create_azure_openai_group(),
        create_huggingface_group(),
        create_ollama_group(),
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

fn create_cohere_group() -> SettingGroup {
    create_provider_group(
        "Cohere",
        "Configure Cohere API access - Command R, Command R+, embeddings",
        ProviderType::Cohere,
        "Enter your Cohere API key for Command models",
    )
}

fn create_perplexity_group() -> SettingGroup {
    create_provider_group(
        "Perplexity",
        "Configure Perplexity API access - online search-powered AI responses",
        ProviderType::Perplexity,
        "Enter your Perplexity API key for search-enhanced responses",
    )
}

fn create_xai_group() -> SettingGroup {
    create_provider_group(
        "xAI",
        "Configure xAI (Grok) API access - Grok models by Elon Musk",
        ProviderType::XAI,
        "Enter your xAI API key for Grok models",
    )
}

fn create_azure_openai_group() -> SettingGroup {
    create_provider_group(
        "Azure OpenAI",
        "Configure Azure OpenAI API access - Enterprise GPT-4, GPT-3.5 on Microsoft Azure",
        ProviderType::AzureOpenAI,
        "Enter your Azure OpenAI API key for enterprise deployments",
    )
}

fn create_huggingface_group() -> SettingGroup {
    create_provider_group(
        "HuggingFace",
        "Configure HuggingFace API access - open-source models, Inference API",
        ProviderType::HuggingFace,
        "Enter your HuggingFace API token for model access",
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
                        update_or_create_ollama(cx, val.to_string());
                    },
                ),
            )
            .description("Ollama server URL (default: http://localhost:11434)"),
        ])
}

// Generic helper to create a provider group with API key and Base URL
fn create_provider_group(
    title: &'static str,
    description: &'static str,
    provider_type: ProviderType,
    api_key_description: &'static str,
) -> SettingGroup {
    let provider_type_for_api = provider_type.clone();
    let provider_type_for_api_set = provider_type.clone();
    let provider_type_for_url = provider_type.clone();
    let provider_type_for_url_set = provider_type;

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
                        update_or_create_provider(
                            cx,
                            provider_type_for_api_set.clone(),
                            val.to_string(),
                        );
                    },
                ),
            )
            .description(api_key_description),
            SettingItem::new(
                "Base URL",
                SettingField::input(
                    move |cx: &App| {
                        cx.global::<ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| p.provider_type == provider_type_for_url)
                            .and_then(|p| p.base_url.clone())
                            .unwrap_or_default()
                            .into()
                    },
                    move |val: SharedString, cx: &mut App| {
                        update_provider_base_url(
                            cx,
                            provider_type_for_url_set.clone(),
                            val.to_string(),
                        );
                    },
                ),
            )
            .description("Optional: Custom API endpoint"),
        ])
}

// Helper function to update or create a provider with an API key
fn update_or_create_provider(cx: &mut App, provider_type: ProviderType, api_key: String) {
    let model = cx.global_mut::<ProviderModel>();

    // Find existing provider
    if let Some(provider) = model
        .providers_mut()
        .iter_mut()
        .find(|p| p.provider_type == provider_type)
    {
        // Update existing provider
        if api_key.is_empty() {
            provider.api_key = None;
        } else {
            provider.api_key = Some(api_key);
        }
    } else if !api_key.is_empty() {
        // Create new provider only if API key is not empty
        let config = ProviderConfig::new(provider_type.display_name().to_string(), provider_type)
            .with_api_key(api_key);
        model.add_provider(config);
    }

    cx.refresh_windows();
}

// Helper function to update provider base URL
fn update_provider_base_url(cx: &mut App, provider_type: ProviderType, base_url: String) {
    let model = cx.global_mut::<ProviderModel>();

    if let Some(provider) = model
        .providers_mut()
        .iter_mut()
        .find(|p| p.provider_type == provider_type)
    {
        if base_url.is_empty() {
            provider.base_url = None;
        } else {
            provider.base_url = Some(base_url);
        }
        cx.refresh_windows();
    }
}

// Special helper for Ollama (doesn't require API key)
fn update_or_create_ollama(cx: &mut App, base_url: String) {
    let model = cx.global_mut::<ProviderModel>();

    // Find existing Ollama provider
    if let Some(provider) = model
        .providers_mut()
        .iter_mut()
        .find(|p| matches!(p.provider_type, ProviderType::Ollama))
    {
        // Update existing provider
        if base_url.is_empty() || base_url == "http://localhost:11434" {
            provider.base_url = None;
        } else {
            provider.base_url = Some(base_url);
        }
    } else if !base_url.is_empty() {
        // Create new Ollama provider
        let config =
            ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama).with_base_url(base_url);
        model.add_provider(config);
    }

    cx.refresh_windows();
}
