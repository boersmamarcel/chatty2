use crate::PROVIDER_REPOSITORY;
use crate::settings::models::providers_store::{ProviderConfig, ProviderModel, ProviderType};
use gpui::App;

/// Update or create a provider with an API key
pub fn update_or_create_provider(cx: &mut App, provider_type: ProviderType, api_key: String) {
    // 1. Take snapshot BEFORE any changes (for potential rollback)
    let _snapshot = cx.global::<ProviderModel>().snapshot();

    // 2. Apply update immediately (optimistic update)
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

    // 3. Get updated state for async save
    let providers_to_save = cx.global::<ProviderModel>().providers().to_vec();

    // 4. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 5. Save async with error handling
    let repo = PROVIDER_REPOSITORY.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()
            .expect("Failed to create tokio runtime for saving providers");
        rt.block_on(async move {
            if let Err(e) = repo.save_all(providers_to_save).await {
                eprintln!("Failed to save providers: {}", e);
                // TODO: Send rollback message to UI thread
                // For now, just log the error - user will see old state on restart
            }
        });
    });
}

/// Update or create Ollama provider (doesn't require API key)
pub fn update_or_create_ollama(cx: &mut App, base_url: String) {
    // 1. Take snapshot BEFORE any changes (for potential rollback)
    let _snapshot = cx.global::<ProviderModel>().snapshot();

    // 2. Apply update immediately (optimistic update)
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

    // 3. Get updated state for async save
    let providers_to_save = cx.global::<ProviderModel>().providers().to_vec();

    // 4. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 5. Save async with error handling
    let repo = PROVIDER_REPOSITORY.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()
            .expect("Failed to create tokio runtime for saving Ollama provider");
        rt.block_on(async move {
            if let Err(e) = repo.save_all(providers_to_save).await {
                eprintln!("Failed to save providers: {}", e);
                // TODO: Send rollback message to UI thread
                // For now, just log the error - user will see old state on restart
            }
        });
    });
}
