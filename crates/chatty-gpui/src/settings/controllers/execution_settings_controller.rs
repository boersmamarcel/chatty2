use crate::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use chatty_core::services::MemoryService;
use gpui::{App, AsyncApp};
use tracing::{debug, error, info, warn};

/// Emit `RebuildRequired` so the active conversation's agent is rebuilt
/// with the current execution tool settings (bash, filesystem, MCP management).
fn notify_tool_set_changed(cx: &mut App) {
    if let Some(weak_notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
    {
        info!("Notifying tool set changed — triggering agent rebuild");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    } else {
        debug!(
            "notify_tool_set_changed: GlobalAgentConfigNotifier not found — agent will not be rebuilt"
        );
    }
}

/// Toggle code execution enabled/disabled and persist to disk
pub fn toggle_execution(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let old_enabled = cx.global::<ExecutionSettingsModel>().enabled;
    let new_enabled = !old_enabled;
    info!(
        old = old_enabled,
        new = new_enabled,
        "Toggling code execution"
    );
    cx.global_mut::<ExecutionSettingsModel>().enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update workspace directory and persist to disk
pub fn set_workspace_dir(dir: Option<String>, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().workspace_dir = dir;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt (workspace dir affects fs tools)
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update approval mode and persist to disk
pub fn set_approval_mode(mode: ApprovalMode, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().approval_mode = mode;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle filesystem read tools enabled/disabled and persist to disk
pub fn toggle_filesystem_read(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .filesystem_read_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .filesystem_read_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle network isolation enabled/disabled and persist to disk
pub fn toggle_network_isolation(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_isolation = !cx.global::<ExecutionSettingsModel>().network_isolation;
    cx.global_mut::<ExecutionSettingsModel>().network_isolation = new_isolation;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new network setting
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle the add_mcp_service tool enabled/disabled and persist to disk.
/// When disabled, the LLM cannot register new MCP servers.
pub fn toggle_mcp_service_tool(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .mcp_service_tool_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .mcp_service_tool_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle the built-in fetch tool enabled/disabled and persist to disk.
pub fn toggle_fetch(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx.global::<ExecutionSettingsModel>().fetch_enabled;
    cx.global_mut::<ExecutionSettingsModel>().fetch_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update the Docker host setting and persist to disk.
pub fn set_docker_host(host: Option<String>, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().docker_host = host;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new docker host
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle Docker code execution enabled/disabled and persist to disk.
pub fn toggle_docker_code_execution(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .docker_code_execution_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .docker_code_execution_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle the built-in browser tool enabled/disabled and persist to disk.
pub fn toggle_browser(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx.global::<ExecutionSettingsModel>().browser_enabled;
    cx.global_mut::<ExecutionSettingsModel>().browser_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle git integration tools enabled/disabled and persist to disk.
pub fn toggle_git(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx.global::<ExecutionSettingsModel>().git_enabled;
    cx.global_mut::<ExecutionSettingsModel>().git_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update the shell timeout in seconds and persist to disk.
/// Clamps the value to 1–600 seconds.
pub fn set_timeout_seconds(seconds: u32, cx: &mut App) {
    let seconds = seconds.clamp(1, 600);

    // 1. Apply update immediately
    info!(seconds, "Setting shell timeout");
    cx.global_mut::<ExecutionSettingsModel>().timeout_seconds = seconds;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update the maximum output size in bytes and persist to disk.
/// Clamps the value to 1 KB – 1024 KB (1024–1048576 bytes).
pub fn set_max_output_bytes(bytes: usize, cx: &mut App) {
    let bytes = bytes.clamp(1024, 1024 * 1024);

    // 1. Apply update immediately
    info!(bytes, "Setting max output bytes");
    cx.global_mut::<ExecutionSettingsModel>().max_output_bytes = bytes;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update the maximum number of agentic turns and persist to disk
pub fn set_max_agent_turns(turns: u32, cx: &mut App) {
    // 1. Apply update immediately
    info!(turns, "Setting max agent turns");
    cx.global_mut::<ExecutionSettingsModel>().max_agent_turns = turns;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle agent memory enabled/disabled and persist to disk.
///
/// When toggled ON, initializes the MemoryService global if not already present.
/// When toggled OFF, removes the MemoryService global so memory tools are no longer injected.
pub fn toggle_memory(cx: &mut App) {
    let new_enabled = !cx.global::<ExecutionSettingsModel>().memory_enabled;
    info!(new = new_enabled, "Toggling agent memory");
    cx.global_mut::<ExecutionSettingsModel>().memory_enabled = new_enabled;

    if new_enabled {
        // Initialize MemoryService if not already present
        if cx.try_global::<MemoryService>().is_none() {
            cx.spawn(async move |cx: &mut AsyncApp| {
                if let Some(data_dir) = chatty_core::services::memory_service::memory_data_dir() {
                    match MemoryService::open_or_create(&data_dir).await {
                        Ok(service) => {
                            cx.update(|cx| {
                                cx.set_global(service);
                                info!("Agent memory service initialized from settings toggle");
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to set MemoryService global"))
                            .ok();
                        }
                        Err(e) => {
                            warn!(error = ?e, "Failed to initialize memory service");
                        }
                    }
                }
            })
            .detach();
        }
    }

    let settings = cx.global::<ExecutionSettingsModel>().clone();
    cx.refresh_windows();
    notify_tool_set_changed(cx);

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Clear all agent memory (purge the memory store).
pub fn purge_memory(cx: &mut App) {
    let memory_service = cx.try_global::<MemoryService>().cloned();

    if let Some(service) = memory_service {
        info!("Purging all agent memory");
        cx.spawn(|_cx: &mut AsyncApp| async move {
            if let Err(e) = service.clear().await {
                error!(error = ?e, "Failed to purge memory");
            } else {
                info!("Agent memory purged successfully");
            }
        })
        .detach();
    } else {
        warn!("Cannot purge memory: MemoryService not initialized");
    }
}

/// Fetch an Entra ID bearer token, initialize the `EmbeddingService`, and wire up
/// the memory vector index. Called from `cx.spawn()` in both `toggle_embedding` and
/// `reinit_embedding_service` to avoid duplicating the async init logic.
///
/// # Token expiry
/// The bearer token returned by `fetch_entra_id_token` is valid for ~1 hour.
/// The `EmbeddingService` holds this token for its lifetime without refreshing it.
/// TODO: Re-fetch the token on each embedding call or add expiry tracking so that
/// long-running sessions (>1h) do not start producing authentication errors.
async fn init_azure_entra_embedding(
    provider_type: chatty_core::settings::models::providers_store::ProviderType,
    model_name: String,
    base_url: Option<String>,
    mem_svc: Option<MemoryService>,
    cx: &mut AsyncApp,
) {
    use chatty_core::services::embedding_service::try_create_embedding_service;

    info!(
        "Fetching Entra ID token for Azure OpenAI embeddings — service will be available shortly"
    );
    let azure_token = match chatty_core::auth::azure_auth::fetch_entra_id_token().await {
        Ok(token) => Some(token),
        Err(e) => {
            warn!(error = ?e, "Failed to fetch Entra ID token for Azure OpenAI embeddings");
            None
        }
    };
    // api_key is None for Entra ID; the bearer token handles auth
    if let Some(embed_svc) = try_create_embedding_service(
        &provider_type,
        &model_name,
        None,
        base_url.as_deref(),
        azure_token,
    ) {
        let model_id = embed_svc.model_identifier();
        cx.update(|cx| {
            let skill_service = chatty_core::services::SkillService::new(Some(embed_svc.clone()));
            cx.set_global(embed_svc);
            cx.set_global(skill_service);
        })
        .map_err(|e| warn!(error = ?e, "Failed to set EmbeddingService and SkillService globals"))
        .ok();

        if let Some(mem_svc) = mem_svc {
            if let Err(e) = mem_svc.enable_vec().await {
                warn!(error = ?e, "Failed to enable vector index");
            } else if let Err(e) = mem_svc.set_vec_model(&model_id).await {
                warn!(error = ?e, "Failed to set vector model");
            } else {
                info!(model = %model_id, "Vector search enabled for memory");
            }
        }
    }
}

/// Toggle semantic (vector) search for memory.
/// Initializes or removes the EmbeddingService global accordingly.
pub fn toggle_embedding(cx: &mut App) {
    use chatty_core::services::embedding_service::{
        EmbeddingService, try_create_embedding_service,
    };

    let new_enabled = !cx.global::<ExecutionSettingsModel>().embedding_enabled;
    info!(new = new_enabled, "Toggling semantic search");
    cx.global_mut::<ExecutionSettingsModel>().embedding_enabled = new_enabled;

    if new_enabled {
        // Try to initialize EmbeddingService if not already set
        if cx.try_global::<EmbeddingService>().is_none() {
            use chatty_core::settings::models::providers_store::{AzureAuthMethod, ProviderType};

            let settings = cx.global::<ExecutionSettingsModel>().clone();
            if let (Some(provider_type), Some(model_name)) = (
                settings.embedding_provider.as_ref(),
                settings.embedding_model.as_ref(),
            ) {
                let provider_config = cx
                    .try_global::<chatty_core::settings::models::ProviderModel>()
                    .and_then(|pm| {
                        pm.providers()
                            .iter()
                            .find(|p| &p.provider_type == provider_type)
                            .cloned()
                    });
                let base_url = provider_config.as_ref().and_then(|p| p.base_url.clone());
                let is_azure_entra = *provider_type == ProviderType::AzureOpenAI
                    && provider_config.as_ref().map(|p| p.azure_auth_method())
                        == Some(AzureAuthMethod::EntraId);

                if is_azure_entra {
                    // Entra ID requires async token fetch — spawn a task
                    let provider_type = provider_type.clone();
                    let model_name = model_name.clone();
                    let mem_svc = cx.try_global::<MemoryService>().cloned();
                    cx.spawn(async move |cx| {
                        init_azure_entra_embedding(
                            provider_type,
                            model_name,
                            base_url,
                            mem_svc,
                            cx,
                        )
                        .await;
                    })
                    .detach();
                } else {
                    // API key auth — initialize synchronously
                    let api_key = provider_config.as_ref().and_then(|p| p.api_key.clone());
                    if let Some(embed_svc) = try_create_embedding_service(
                        provider_type,
                        model_name,
                        api_key.as_deref(),
                        base_url.as_deref(),
                        None,
                    ) {
                        let model_id = embed_svc.model_identifier();
                        let skill_service =
                            chatty_core::services::SkillService::new(Some(embed_svc.clone()));
                        cx.set_global(embed_svc);
                        cx.set_global(skill_service);

                        // Enable vector index on memory service
                        let mem_svc = cx.try_global::<MemoryService>().cloned();
                        if let Some(mem_svc) = mem_svc {
                            cx.spawn(async move |_cx: &mut AsyncApp| {
                                if let Err(e) = mem_svc.enable_vec().await {
                                    warn!(error = ?e, "Failed to enable vector index");
                                } else if let Err(e) = mem_svc.set_vec_model(&model_id).await {
                                    warn!(error = ?e, "Failed to set vector model");
                                } else {
                                    info!(model = %model_id, "Vector search enabled for memory");
                                }
                            })
                            .detach();
                        }
                    }
                }
            }
        }
    }

    let settings = cx.global::<ExecutionSettingsModel>().clone();
    cx.refresh_windows();
    notify_tool_set_changed(cx);

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Set the embedding provider and reinitialize the EmbeddingService.
pub fn set_embedding_provider(
    provider_type: chatty_core::settings::models::providers_store::ProviderType,
    cx: &mut App,
) {
    use chatty_core::services::embedding_service::EmbeddingService;

    info!(provider = ?provider_type, "Setting embedding provider");
    cx.global_mut::<ExecutionSettingsModel>().embedding_provider = Some(provider_type.clone());

    // Always update model to the new provider's default — the old model
    // is almost certainly invalid for the new provider.
    if let Some(default_model) = EmbeddingService::default_model_for_provider(&provider_type) {
        cx.global_mut::<ExecutionSettingsModel>().embedding_model = Some(default_model.to_string());
    }

    // Reinitialize embedding service if enabled
    reinit_embedding_service(cx);

    let settings = cx.global::<ExecutionSettingsModel>().clone();
    cx.refresh_windows();
    notify_tool_set_changed(cx);

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Set the embedding model name and reinitialize the EmbeddingService.
pub fn set_embedding_model(model: Option<String>, cx: &mut App) {
    info!(model = ?model, "Setting embedding model");
    cx.global_mut::<ExecutionSettingsModel>().embedding_model = model;

    // Reinitialize embedding service if enabled
    reinit_embedding_service(cx);

    let settings = cx.global::<ExecutionSettingsModel>().clone();
    cx.refresh_windows();
    notify_tool_set_changed(cx);

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Recreate the EmbeddingService global from current settings.
fn reinit_embedding_service(cx: &mut App) {
    use chatty_core::services::embedding_service::try_create_embedding_service;
    use chatty_core::settings::models::providers_store::{AzureAuthMethod, ProviderType};

    let settings = cx.global::<ExecutionSettingsModel>();
    if !settings.embedding_enabled {
        return;
    }

    let (Some(provider_type), Some(model_name)) = (
        settings.embedding_provider.clone(),
        settings.embedding_model.clone(),
    ) else {
        return;
    };

    let provider_config = cx
        .try_global::<chatty_core::settings::models::ProviderModel>()
        .and_then(|pm| {
            pm.providers()
                .iter()
                .find(|p| p.provider_type == provider_type)
                .cloned()
        });
    let api_key = provider_config.as_ref().and_then(|p| p.api_key.clone());
    let base_url = provider_config.as_ref().and_then(|p| p.base_url.clone());
    let is_azure_entra = provider_type == ProviderType::AzureOpenAI
        && provider_config.as_ref().map(|p| p.azure_auth_method())
            == Some(AzureAuthMethod::EntraId);

    if is_azure_entra {
        // Entra ID requires async token fetch — spawn a task
        let mem_svc = cx.try_global::<MemoryService>().cloned();
        cx.spawn(async move |cx| {
            init_azure_entra_embedding(provider_type, model_name, base_url, mem_svc, cx).await;
        })
        .detach();
    } else if let Some(embed_svc) = try_create_embedding_service(
        &provider_type,
        &model_name,
        api_key.as_deref(), // api_key unused for Entra ID; only used in this API key branch
        base_url.as_deref(),
        None,
    ) {
        let model_id = embed_svc.model_identifier();
        let skill_service = chatty_core::services::SkillService::new(Some(embed_svc.clone()));
        cx.set_global(embed_svc);
        cx.set_global(skill_service);

        let mem_svc = cx.try_global::<MemoryService>().cloned();
        if let Some(mem_svc) = mem_svc {
            cx.spawn(async move |_cx: &mut AsyncApp| {
                if let Err(e) = mem_svc.enable_vec().await {
                    warn!(error = ?e, "Failed to enable vector index");
                } else if let Err(e) = mem_svc.set_vec_model(&model_id).await {
                    warn!(error = ?e, "Failed to set vector model");
                } else {
                    info!(model = %model_id, "Vector search re-initialized with new model");
                }
            })
            .detach();
        }
    }
}

/// Toggle filesystem write tools enabled/disabled and persist to disk
pub fn toggle_filesystem_write(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .filesystem_write_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .filesystem_write_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::execution_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}
