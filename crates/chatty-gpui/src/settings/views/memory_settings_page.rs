use crate::settings::controllers::execution_settings_controller;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use chatty_core::services::MemoryService;
use chatty_core::services::embedding_service::EmbeddingService;
use chatty_core::settings::models::providers_store::ProviderType;
use gpui::{App, IntoElement, SharedString, Styled};
use gpui_component::{
    Disableable,
    button::{Button, ButtonVariants},
    menu::{DropdownMenu, PopupMenuItem},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
};

pub fn memory_settings_page() -> SettingPage {
    SettingPage::new("Memory")
        .description(
            "Persistent agent memory across conversations. \
             The agent can store facts, preferences, and decisions, \
             then recall them in future conversations.",
        )
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Agent Memory")
                .description(
                    "When enabled, the agent can store and recall information across \
                     conversations using remember and search_memory tools.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Agent Memory",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().memory_enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_memory(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Master toggle for the memory system.",
                    ),
                    SettingItem::new(
                        "Purge All Memory",
                        SettingField::render(|_options, _window, cx| {
                            let has_memory = cx.try_global::<MemoryService>().is_some();
                            let enabled = cx.global::<ExecutionSettingsModel>().memory_enabled;

                            Button::new("purge-memory-btn")
                                .label("Purge All Memory")
                                .danger()
                                .disabled(!has_memory || !enabled)
                                .on_click(|_, _, cx| {
                                    execution_settings_controller::purge_memory(cx);
                                })
                                .into_any_element()
                        }),
                    )
                    .description(
                        "Permanently delete all stored memories. This cannot be undone.",
                    ),
                ]),
            SettingGroup::new()
                .title("Semantic Search")
                .description(
                    "Use vector similarity to find memories by meaning, not just keywords. \
                     Requires an embedding provider (any configured provider except Anthropic).",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Semantic Search",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().embedding_enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_embedding(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "When enabled, memory search uses both keyword matching and \
                         vector similarity for more accurate recall.",
                    ),
                    SettingItem::new(
                        "Embedding Provider",
                        SettingField::render(|_options, _window, cx| {
                            let settings = cx.global::<ExecutionSettingsModel>();
                            let enabled = settings.memory_enabled && settings.embedding_enabled;
                            let current_provider = settings.embedding_provider.clone();

                            let current_label = current_provider
                                .as_ref()
                                .map(|p| format!("{:?}", p))
                                .unwrap_or_else(|| "Select provider...".to_string());

                            // Get configured providers that support embeddings
                            let providers: Vec<ProviderType> = cx
                                .try_global::<chatty_core::settings::models::ProviderModel>()
                                .map(|pm| {
                                    pm.providers()
                                        .iter()
                                        .filter(|p| {
                                            EmbeddingService::provider_supports_embeddings(
                                                &p.provider_type,
                                            )
                                        })
                                        .map(|p| p.provider_type.clone())
                                        .collect()
                                })
                                .unwrap_or_default();

                            let cp = current_provider.clone();
                            Button::new("embedding-provider-dropdown")
                                .label(current_label)
                                .dropdown_caret(true)
                                .outline()
                                .w_full()
                                .disabled(!enabled)
                                .dropdown_menu_with_anchor(
                                    gpui::Corner::BottomLeft,
                                    move |mut menu, _, _| {
                                        for provider in &providers {
                                            let is_checked = cp.as_ref() == Some(provider);
                                            let provider_clone = provider.clone();
                                            let label = format!("{:?}", provider);
                                            menu = menu.item(
                                                PopupMenuItem::new(label)
                                                    .checked(is_checked)
                                                    .on_click(move |_, _, cx| {
                                                        execution_settings_controller::set_embedding_provider(
                                                            provider_clone.clone(),
                                                            cx,
                                                        );
                                                    }),
                                            );
                                        }
                                        menu
                                    },
                                )
                                .into_any_element()
                        }),
                    )
                    .description(
                        "Provider for computing embeddings (can differ from your chat model). \
                         Anthropic does not offer an embedding API.",
                    ),
                    SettingItem::new(
                        "Embedding Model",
                        SettingField::input(
                            |cx: &App| {
                                let settings = cx.global::<ExecutionSettingsModel>();
                                let model = settings.embedding_model.clone().unwrap_or_default();
                                let placeholder = settings
                                    .embedding_provider
                                    .as_ref()
                                    .and_then(|p| EmbeddingService::default_model_for_provider(p))
                                    .unwrap_or("text-embedding-3-small");
                                if model.is_empty() {
                                    placeholder.to_string().into()
                                } else {
                                    model.into()
                                }
                            },
                            |val: SharedString, cx: &mut App| {
                                let model = if val.is_empty() {
                                    None
                                } else {
                                    Some(val.to_string())
                                };
                                execution_settings_controller::set_embedding_model(model, cx);
                            },
                        ),
                    )
                    .description(
                        "Model identifier for embeddings. Leave empty to use the provider's default.",
                    ),
                ]),
        ])
}
