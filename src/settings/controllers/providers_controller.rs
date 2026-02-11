use crate::PROVIDER_REPOSITORY;
use crate::settings::models::providers_store::{ProviderConfig, ProviderModel, ProviderType};
use gpui::{App, AsyncApp};
use tracing::error;

/// Update or create a provider with an API key
pub fn update_or_create_provider(cx: &mut App, provider_type: ProviderType, api_key: String) {
    // 1. Apply update immediately (optimistic update)
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

    // 2. Get updated state for async save
    let providers_to_save = cx.global::<ProviderModel>().providers().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling using GPUI's async runtime
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = PROVIDER_REPOSITORY.clone();
        if let Err(e) = repo.save_all(providers_to_save).await {
            error!(error = ?e, "Failed to save providers, changes will be lost on restart");
        }
    })
    .detach();
}

/// Update or create Azure OpenAI provider (requires API key and endpoint URL)
pub fn update_or_create_azure(cx: &mut App, api_key: String, endpoint_url: String) {
    // Check whether a complete record already exists on disk before mutating.
    // If so, any update (including clearing a field) must be persisted.
    let was_complete = cx
        .global::<ProviderModel>()
        .providers()
        .iter()
        .find(|p| p.provider_type == ProviderType::AzureOpenAI)
        .map(|p| {
            p.api_key.as_ref().is_some_and(|k| !k.is_empty())
                && p.base_url.as_ref().is_some_and(|u| !u.is_empty())
        })
        .unwrap_or(false);

    // 1. Always update in-memory state so each field round-trips correctly via
    //    the view's read callbacks (azure_api_key / azure_endpoint). Without
    //    this the sibling field always reads back as empty, breaking creation.
    let model = cx.global_mut::<ProviderModel>();

    if let Some(provider) = model
        .providers_mut()
        .iter_mut()
        .find(|p| p.provider_type == ProviderType::AzureOpenAI)
    {
        provider.api_key = if api_key.is_empty() {
            None
        } else {
            Some(api_key.clone())
        };
        provider.base_url = if endpoint_url.is_empty() {
            None
        } else {
            Some(endpoint_url.clone())
        };
    } else if !api_key.is_empty() || !endpoint_url.is_empty() {
        let mut config = ProviderConfig::new(
            ProviderType::AzureOpenAI.display_name().to_string(),
            ProviderType::AzureOpenAI,
        );
        if !api_key.is_empty() {
            config.api_key = Some(api_key.clone());
        }
        if !endpoint_url.is_empty() {
            config.base_url = Some(endpoint_url.clone());
        }
        model.add_provider(config);
    }

    // 2. Only persist when both fields are now present, or when updating a
    //    previously-complete record (e.g. user is clearing/changing a field).
    let both_set = !api_key.is_empty() && !endpoint_url.is_empty();
    let should_save = both_set || was_complete;

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async only when appropriate — no partial records written to disk
    if should_save {
        let providers_to_save = cx.global::<ProviderModel>().providers().to_vec();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            let repo = PROVIDER_REPOSITORY.clone();
            if let Err(e) = repo.save_all(providers_to_save).await {
                error!(error = ?e, "Failed to save providers, changes will be lost on restart");
            }
        })
        .detach();
    }
}

/// Update or create Ollama provider (doesn't require API key)
pub fn update_or_create_ollama(cx: &mut App, base_url: String) {
    // 1. Apply update immediately (optimistic update)
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

    // 2. Get updated state for async save
    let providers_to_save = cx.global::<ProviderModel>().providers().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling using GPUI's async runtime
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = PROVIDER_REPOSITORY.clone();
        if let Err(e) = repo.save_all(providers_to_save).await {
            error!(error = ?e, "Failed to save providers, changes will be lost on restart");
        }
    })
    .detach();
}
