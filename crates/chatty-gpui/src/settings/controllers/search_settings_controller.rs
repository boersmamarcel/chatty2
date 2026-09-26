use crate::settings::models::search_settings::{SearchProvider, SearchSettingsModel};
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use gpui::{App, AsyncApp, Global};
use tracing::{debug, error, info};

/// Emit `RebuildRequired` so the active conversation's agent is rebuilt
/// with the current search tool settings.
fn notify_tool_set_changed(cx: &mut App) {
    if let Some(notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.try_upgrade())
    {
        info!("Notifying search settings changed — triggering agent rebuild");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    } else {
        debug!(
            "notify_tool_set_changed: GlobalAgentConfigNotifier not found — agent will not be rebuilt"
        );
    }
}

/// Save search settings asynchronously
fn save_async(cx: &mut App) {
    let settings = cx.global::<SearchSettingsModel>().clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::search_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save search settings");
        }
    })
    .detach();
}

/// Toggle web search enabled/disabled and persist to disk
pub fn toggle_search(cx: &mut App) {
    let new_enabled = !cx.global::<SearchSettingsModel>().enabled;
    info!(new = new_enabled, "Toggling web search");
    cx.global_mut::<SearchSettingsModel>().enabled = new_enabled;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the active search provider and persist to disk
pub fn set_active_provider(provider: SearchProvider, cx: &mut App) {
    info!(provider = %provider, "Setting active search provider");
    cx.global_mut::<SearchSettingsModel>().active_provider = provider;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the Tavily API key and persist to disk
pub fn set_tavily_api_key(key: String, cx: &mut App) {
    let api_key = if key.is_empty() { None } else { Some(key) };
    cx.global_mut::<SearchSettingsModel>().tavily_api_key = api_key;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the Brave Search API key and persist to disk
pub fn set_brave_api_key(key: String, cx: &mut App) {
    let api_key = if key.is_empty() { None } else { Some(key) };
    cx.global_mut::<SearchSettingsModel>().brave_api_key = api_key;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the maximum number of search results and persist to disk
pub fn set_max_results(count: usize, cx: &mut App) {
    let count = count.clamp(1, 20);
    info!(count, "Setting max search results");
    cx.global_mut::<SearchSettingsModel>().max_results = count;

    cx.refresh_windows();
    save_async(cx);
}

/// Toggle page extracts in search results and persist to disk
pub fn toggle_page_extracts(cx: &mut App) {
    let new_enabled = !cx.global::<SearchSettingsModel>().page_extracts;
    info!(new = new_enabled, "Toggling search page extracts");
    cx.global_mut::<SearchSettingsModel>().page_extracts = new_enabled;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Toggle browser-use enabled/disabled and persist to disk
pub fn toggle_browser_use(cx: &mut App) {
    let new_enabled = !cx.global::<SearchSettingsModel>().browser_use_enabled;
    info!(new = new_enabled, "Toggling browser-use");
    cx.global_mut::<SearchSettingsModel>().browser_use_enabled = new_enabled;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the browser-use API key and persist to disk
pub fn set_browser_use_api_key(key: String, cx: &mut App) {
    let api_key = if key.is_empty() { None } else { Some(key) };
    cx.global_mut::<SearchSettingsModel>().browser_use_api_key = api_key;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Toggle Daytona enabled/disabled and persist to disk
pub fn toggle_daytona(cx: &mut App) {
    let new_enabled = !cx.global::<SearchSettingsModel>().daytona_enabled;
    info!(new = new_enabled, "Toggling Daytona");
    cx.global_mut::<SearchSettingsModel>().daytona_enabled = new_enabled;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the Daytona API key and persist to disk
pub fn set_daytona_api_key(key: String, cx: &mut App) {
    let api_key = if key.is_empty() { None } else { Some(key) };
    cx.global_mut::<SearchSettingsModel>().daytona_api_key = api_key;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Set the keyless-search reranker endpoint and persist to disk
pub fn set_rerank_url(url: String, cx: &mut App) {
    cx.global_mut::<SearchSettingsModel>().rerank_url = non_empty(url);

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Set the model name sent to the reranker endpoint and persist to disk
pub fn set_rerank_model(model: String, cx: &mut App) {
    cx.global_mut::<SearchSettingsModel>().rerank_model = non_empty(model);

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// The outcome of the reranker Test button.
#[derive(Default)]
pub enum RerankTestStatus {
    #[default]
    Idle,
    Testing,
    Done(Result<String, String>),
}

impl Global for RerankTestStatus {}

/// Send one request to the configured reranker and record the outcome.
pub fn test_reranker(cx: &mut App) {
    let settings = cx.global::<SearchSettingsModel>();
    let Some((url, model)) = settings
        .reranker()
        .map(|(url, model)| (url.to_string(), model.to_string()))
    else {
        cx.set_global(RerankTestStatus::Done(Err(
            "Set both the endpoint and the model first".to_string(),
        )));
        cx.refresh_windows();
        return;
    };
    cx.set_global(RerankTestStatus::Testing);
    cx.refresh_windows();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let result = chatty_core::tools::SearchWebTool::probe_reranker(&url, &model)
            .await
            .map_err(|e| e.to_string());
        cx.update(|cx| {
            cx.set_global(RerankTestStatus::Done(result));
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
}
