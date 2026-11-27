use crate::settings::controllers::search_settings_controller::RerankTestStatus;
use crate::settings::controllers::{execution_settings_controller, search_settings_controller};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::search_settings::{SearchProvider, SearchSettingsModel};
use crate::settings::views::providers_view::masked_api_key_field;
use gpui::{App, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::{
    ActiveTheme, Sizable,
    button::Button,
    h_flex,
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
                    "One switch for everything below. Off, the AI cannot open web pages, \
                     search the web, use a cloud browser or run code in the cloud.",
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
                        "Lets the AI open web pages, and turns on the services below that you \
                     set up. Turn it off to keep the AI off the internet entirely.",
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
                    .description("Select which search engine to use for web searches. ↗ Sends queries to an external search provider."),
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
            // ── Keyless Search ───────────────────────────────────────────
            SettingGroup::new()
                .title("Better results without a search key (optional)")
                .description(
                    "If you run a ranking model on this computer, Chatty can use it to put \
                     the best results first when no search key is set, for about a second \
                     more per search. Most people leave this empty.",
                )
                .items(vec![
                    SettingItem::new(
                        "Ranking Server Address",
                        SettingField::input(
                            |cx: &App| {
                                cx.global::<SearchSettingsModel>()
                                    .rerank_url
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                search_settings_controller::set_rerank_url(val.to_string(), cx);
                            },
                        ),
                    )
                    .description("Where the ranking model answers, for example http://127.0.0.1:8001/rerank."),
                    SettingItem::new(
                        "Ranking Model",
                        SettingField::input(
                            |cx: &App| {
                                cx.global::<SearchSettingsModel>()
                                    .rerank_model
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                search_settings_controller::set_rerank_model(val.to_string(), cx);
                            },
                        ),
                    )
                    .description("The model's name as that server knows it, for example BAAI/bge-reranker-v2-m3."),
                    SettingItem::new(
                        "Test Ranking Server",
                        SettingField::render(|_options, _window, cx| {
                            let (color, message) = match cx.try_global::<RerankTestStatus>() {
                                Some(RerankTestStatus::Testing) => {
                                    (cx.theme().muted_foreground, "Testing…".to_string())
                                }
                                Some(RerankTestStatus::Done(Ok(msg))) => {
                                    (cx.theme().success, msg.clone())
                                }
                                Some(RerankTestStatus::Done(Err(msg))) => {
                                    (cx.theme().danger, msg.clone())
                                }
                                Some(RerankTestStatus::Idle) | None => {
                                    (cx.theme().muted_foreground, String::new())
                                }
                            };
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().text_xs().text_color(color).child(message))
                                .child(
                                    Button::new("test-reranker")
                                        .label("Test")
                                        .small()
                                        .outline()
                                        .on_click(|_, _, cx| {
                                            search_settings_controller::test_reranker(cx);
                                        }),
                                )
                                .into_any_element()
                        }),
                    )
                    .description("Asks the server above to rank one test search."),
                ]),
            // ── Browser Automation ───────────────────────────────────────
            SettingGroup::new()
                .title("Cloud Browser")
                .description(
                    "A browser in the cloud (browser-use.com) the AI can use on websites: \
                     fill in forms, click through pages, read what they show.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Cloud Browser",
                        SettingField::switch(
                            |cx: &App| cx.global::<SearchSettingsModel>().browser_use_enabled,
                            |_val: bool, cx: &mut App| {
                                search_settings_controller::toggle_browser_use(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description("Let the AI use the cloud browser. ↗ The pages it visits go through browser-use.com."),
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
                    .description("Let the AI run code on a computer in the cloud. ↗ The code and its files go to daytona.io."),
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
