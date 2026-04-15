use crate::settings::controllers::execution_settings_controller;
use crate::settings::controllers::memory_browser_controller;
use crate::settings::models::MemoryBrowserState;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use chatty_core::services::MemoryService;
use chatty_core::services::embedding_service::EmbeddingService;
use chatty_core::settings::models::providers_store::ProviderType;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex,
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
            memory_browser_group(),
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

fn memory_browser_group() -> SettingGroup {
    SettingGroup::new()
        .title("Memory Browser")
        .description(
            "Browse and inspect all memories stored by the agent. \
             Search for specific memories or expand an entry to see full details.",
        )
        .items(vec![SettingItem::render(|_options, window, cx| {
            let has_memory = cx.try_global::<MemoryService>().is_some();
            let state = cx.global::<MemoryBrowserState>().clone();

            // Persist the search input across frames
            let search_input = window.use_keyed_state("memory-browser-search", cx, |window, cx| {
                InputState::new(window, cx).placeholder("Search memories...")
            });

            v_flex()
                .w_full()
                .gap_3()
                // Stats bar
                .when_some(state.stats.as_ref(), |this, stats| {
                    this.child(render_stats_bar(stats, cx))
                })
                // Search + Refresh controls
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&search_input)))
                        .child(
                            Button::new("memory-browser-search-btn")
                                .small()
                                .icon(Icon::new(IconName::Search))
                                .label("Search")
                                .loading(state.loading)
                                .disabled(!has_memory)
                                .on_click({
                                    let search_input = search_input.clone();
                                    move |_, _window, cx| {
                                        let query =
                                            search_input.read(cx).value().trim().to_string();
                                        memory_browser_controller::load_memories(query, cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("memory-browser-refresh-btn")
                                .small()
                                .ghost()
                                .label("Refresh")
                                .disabled(!has_memory)
                                .on_click(|_, _window, cx| {
                                    memory_browser_controller::load_stats(cx);
                                    memory_browser_controller::load_memories(String::new(), cx);
                                }),
                        ),
                )
                // Load button when memory service exists but entries not yet loaded
                .when(
                    has_memory
                        && state.entries.is_empty()
                        && !state.loading
                        && state.error.is_none(),
                    |this| {
                        this.child(
                            h_flex().w_full().justify_center().py_4().child(
                                Button::new("memory-browser-load-btn")
                                    .label("Load Memories")
                                    .on_click(|_, _window, cx| {
                                        memory_browser_controller::load_stats(cx);
                                        memory_browser_controller::load_memories(String::new(), cx);
                                    }),
                            ),
                        )
                    },
                )
                // No memory service message
                .when(!has_memory, |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Enable Agent Memory above to use the Memory Browser."),
                    )
                })
                // Error message
                .when_some(state.error.as_ref(), |this, error| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().ring)
                            .child(format!("Error: {error}")),
                    )
                })
                // Loading indicator
                .when(state.loading, |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading memories…"),
                    )
                })
                // Memory list
                .when(!state.entries.is_empty(), |this| {
                    this.children(
                        state
                            .entries
                            .iter()
                            .enumerate()
                            .map(|(idx, entry)| {
                                let is_expanded = state.expanded_index == Some(idx);
                                render_memory_entry(idx, entry, is_expanded, cx)
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .into_any_element()
        })])
}

/// Render a compact stats bar with entry count and file size.
fn render_stats_bar(
    stats: &chatty_core::services::memory_service::MemoryStats,
    cx: &gpui::App,
) -> gpui::AnyElement {
    h_flex()
        .w_full()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(cx.theme().muted)
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("🧠"),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(format!("{} memories", stats.entry_count)),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("·"),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format_file_size(stats.file_size_bytes)),
        )
        .into_any_element()
}

/// Render a single memory entry card (collapsed or expanded).
fn render_memory_entry(
    idx: usize,
    entry: &chatty_core::services::memory_service::MemoryHit,
    is_expanded: bool,
    cx: &gpui::App,
) -> gpui::AnyElement {
    let title = entry
        .title
        .clone()
        .unwrap_or_else(|| truncate_str(&entry.text, 80).to_string());
    let full_text = entry.text.clone();
    let title_display = title.clone();
    let frame_id = entry.frame_id;

    let foreground = cx.theme().foreground;
    let muted_fg = cx.theme().muted_foreground;
    let muted_bg = cx.theme().muted;
    let border = cx.theme().border;
    let background = cx.theme().background;

    v_flex()
        .w_full()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(background)
        .overflow_hidden()
        // Header row (always visible, clickable to toggle)
        .child(
            h_flex()
                .id(("memory-entry-header", idx))
                .w_full()
                .px_3()
                .py_2()
                .gap_2()
                .cursor_pointer()
                .when(is_expanded, |this| this.bg(muted_bg))
                .on_click(move |_, _window, cx| {
                    memory_browser_controller::toggle_entry(idx, cx);
                })
                .child(div().text_xs().text_color(muted_fg).child(if is_expanded {
                    "▾"
                } else {
                    "▸"
                }))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(foreground)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(SharedString::from(title_display)),
                ),
        )
        // Expanded: full content + delete button
        .when(is_expanded, |this| {
            this.child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_color(border)
                    .border_t_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(foreground)
                            .child(SharedString::from(full_text)),
                    )
                    .child(
                        h_flex().w_full().justify_end().child(
                            Button::new(("delete-memory", idx))
                                .small()
                                .danger()
                                .icon(Icon::new(IconName::Delete))
                                .label("Delete")
                                .when(frame_id.is_none(), |btn| btn.disabled(true))
                                .on_click(move |_, _window, cx| {
                                    if let Some(fid) = frame_id {
                                        memory_browser_controller::delete_entry(fid, cx);
                                    }
                                }),
                        ),
                    ),
            )
        })
        .into_any_element()
}

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
fn truncate_str(s: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max_chars {
        std::borrow::Cow::Borrowed(s)
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        std::borrow::Cow::Owned(format!("{truncated}…"))
    }
}

/// Format a byte count as a human-readable string (e.g. "1.4 MB").
fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
