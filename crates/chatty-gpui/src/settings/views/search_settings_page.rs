use crate::settings::controllers::{execution_settings_controller, search_settings_controller};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::search_settings::{SearchProvider, SearchSettingsModel};
use crate::settings::views::providers_view::masked_api_key_field;
use gpui::{App, IntoElement, SharedString, Styled};
use gpui_component::{
    button::Button,
    menu::{DropdownMenu, PopupMenuItem},
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

pub fn search_settings_page() -> SettingPage {
    SettingPage::new("Internet")
        .description("Configure how the AI accesses the internet")
        .resettable(false)
        .groups(vec![
            // ── Master toggle ────────────────────────────────────────────
            SettingGroup::new()
                .title("Internet Access")
                .description(
                    "Master switch for all internet-facing tools. When disabled, the AI \
                     cannot fetch web pages, search the web, use browser automation, \
                     or run code in cloud sandboxes.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Internet Access",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().fetch_enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_fetch(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Enables the built-in web fetch tool and gates all other internet \
                     services below. Disable to completely prevent internet access.",
                    ),
                ]),
            // ── Web Search ───────────────────────────────────────────────
            SettingGroup::new()
                .title("Web Search")
                .description(
                    "Allow the AI to search the web for current information. \
                     If no API key is configured, a basic DuckDuckGo fallback is used.",
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
            // ── Browser Automation ───────────────────────────────────────
            SettingGroup::new()
                .title("Browser Automation")
                .description(
                    "Cloud service that lets the AI control a real web browser \
                     to interact with websites (fill forms, click buttons, extract data). \
                     Powered by browser-use.com.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Browser Automation",
                        SettingField::switch(
                            |cx: &App| cx.global::<SearchSettingsModel>().browser_use_enabled,
                            |_val: bool, cx: &mut App| {
                                search_settings_controller::toggle_browser_use(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description("Activate the browser_use tool. Requires an API key below."),
                    SettingItem::new(
                        "API Key",
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
                    .description("Get your key from browser-use.com/cloud"),
                ]),
            // ── Cloud Sandbox ────────────────────────────────────────────
            SettingGroup::new()
                .title("Cloud Sandbox")
                .description(
                    "Secure, isolated cloud environments for running code. \
                     The AI can spin up an ephemeral sandbox, execute code in any \
                     language, and return the output. Powered by Daytona.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Cloud Sandbox",
                        SettingField::switch(
                            |cx: &App| cx.global::<SearchSettingsModel>().daytona_enabled,
                            |_val: bool, cx: &mut App| {
                                search_settings_controller::toggle_daytona(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description("Activate the daytona_run tool. Requires an API key below."),
                    SettingItem::new(
                        "API Key",
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
                    .description("Get your key from app.daytona.io"),
                ]),
        ])
}
