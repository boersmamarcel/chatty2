use crate::settings::controllers::search_settings_controller;
use crate::settings::models::search_settings::{SearchProvider, SearchSettingsModel};
use crate::settings::views::providers_view::masked_api_key_field;
use gpui::{App, IntoElement, SharedString, Styled};
use gpui_component::{
    button::Button,
    menu::{DropdownMenu, PopupMenuItem},
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

pub fn search_settings_page() -> SettingPage {
    SettingPage::new("External Services")
        .description("Configure web search and external service integrations for the AI assistant")
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Web Search")
                .description(
                    "Enable web search so the AI can look up current information. \
                     Requires an API key for the selected search provider.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Web Search",
                        SettingField::switch(
                            |cx: &App| cx.global::<SearchSettingsModel>().enabled,
                            |_val: bool, cx: &mut App| {
                                search_settings_controller::toggle_search(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "When enabled, the AI can search the web to find up-to-date information.",
                    ),
                    SettingItem::new(
                        "Search Provider",
                        SettingField::render(|_options, _window, cx| {
                            let current_provider =
                                cx.global::<SearchSettingsModel>().active_provider.clone();
                            let current_label = match current_provider {
                                SearchProvider::Tavily => "Tavily",
                                SearchProvider::Brave => "Brave",
                            };

                            Button::new("search-provider-dropdown")
                                .label(current_label)
                                .dropdown_caret(true)
                                .outline()
                                .w_full()
                                .dropdown_menu_with_anchor(
                                    gpui::Corner::BottomLeft,
                                    move |menu, _, _| {
                                        menu.item(
                                            PopupMenuItem::new("Tavily")
                                                .checked(matches!(
                                                    current_provider,
                                                    SearchProvider::Tavily
                                                ))
                                                .on_click(|_, _, cx| {
                                                    search_settings_controller::set_active_provider(
                                                        SearchProvider::Tavily,
                                                        cx,
                                                    );
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("Brave")
                                                .checked(matches!(
                                                    current_provider,
                                                    SearchProvider::Brave
                                                ))
                                                .on_click(|_, _, cx| {
                                                    search_settings_controller::set_active_provider(
                                                        SearchProvider::Brave,
                                                        cx,
                                                    );
                                                }),
                                        )
                                    },
                                )
                                .into_any_element()
                        }),
                    )
                    .description("Select which search engine to use for web searches."),
                    SettingItem::new(
                        "Max Results",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 1.0,
                                max: 20.0,
                                ..Default::default()
                            },
                            |cx: &App| cx.global::<SearchSettingsModel>().max_results as f64,
                            |val: f64, cx: &mut App| {
                                search_settings_controller::set_max_results(val as usize, cx);
                            },
                        )
                        .default_value(5.0),
                    )
                    .description("Maximum number of search results to return per query (1-20)."),
                ]),
            SettingGroup::new()
                .title("Search API Keys")
                .description(
                    "Enter your API keys for each search provider. \
                     You only need a key for the provider you want to use.",
                )
                .items(vec![
                    SettingItem::new(
                        "Tavily API Key",
                        masked_api_key_field(
                            |cx: &App| {
                                cx.global::<SearchSettingsModel>()
                                    .tavily_api_key
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                search_settings_controller::set_tavily_api_key(val.to_string(), cx);
                            },
                        ),
                    )
                    .description("Get your API key from tavily.com"),
                    SettingItem::new(
                        "Brave API Key",
                        masked_api_key_field(
                            |cx: &App| {
                                cx.global::<SearchSettingsModel>()
                                    .brave_api_key
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                search_settings_controller::set_brave_api_key(val.to_string(), cx);
                            },
                        ),
                    )
                    .description("Get your API key from brave.com/search/api"),
                ]),
            SettingGroup::new()
                .title("Browser Use")
                .description(
                    "browser-use is a cloud service that lets the AI control a real web browser \
                     to complete tasks described in natural language. \
                     Get your API key from browser-use.com.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Browser Use",
                        SettingField::switch(
                            |cx: &App| cx.global::<SearchSettingsModel>().browser_use_enabled,
                            |_val: bool, cx: &mut App| {
                                search_settings_controller::toggle_browser_use(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "When enabled and an API key is set, the AI can use browser-use to \
                         automate browser tasks (e.g., 'find the contact email on example.com').",
                    ),
                    SettingItem::new(
                        "Browser Use API Key",
                        masked_api_key_field(
                            |cx: &App| {
                                cx.global::<SearchSettingsModel>()
                                    .browser_use_api_key
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                search_settings_controller::set_browser_use_api_key(
                                    val.to_string(),
                                    cx,
                                );
                            },
                        ),
                    )
                    .description("Get your API key from browser-use.com/cloud"),
                ]),
            SettingGroup::new()
                .title("Daytona")
                .description(
                    "Daytona provides secure, isolated cloud sandbox environments for \
                     running code. The AI can create an ephemeral sandbox, execute code, \
                     return the output, and clean up automatically. \
                     Get your API key from app.daytona.io.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Daytona",
                        SettingField::switch(
                            |cx: &App| cx.global::<SearchSettingsModel>().daytona_enabled,
                            |_val: bool, cx: &mut App| {
                                search_settings_controller::toggle_daytona(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "When enabled and an API key is set, the AI can run code in secure \
                         Daytona cloud sandboxes with internet access.",
                    ),
                    SettingItem::new(
                        "Daytona API Key",
                        masked_api_key_field(
                            |cx: &App| {
                                cx.global::<SearchSettingsModel>()
                                    .daytona_api_key
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                search_settings_controller::set_daytona_api_key(
                                    val.to_string(),
                                    cx,
                                );
                            },
                        ),
                    )
                    .description("Get your API key from app.daytona.io"),
                ]),
        ])
}
