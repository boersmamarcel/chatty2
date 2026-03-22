use crate::settings::controllers::browser_settings_controller;
use crate::settings::views::browser_credentials_page::{
    CredentialsTableView, GlobalCredentialsTableView,
};
use chatty_browser::settings::BrowserSettingsModel;
use gpui::{App, AppContext, ParentElement, Styled, div};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};

pub fn browser_settings_page() -> SettingPage {
    SettingPage::new("Browser")
        .description("Configure the browser engine, approval rules, and login credentials")
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Browser Engine")
                .description(
                    "Enable the browser engine to allow the AI to navigate websites, \
                     interact with forms, and extract structured data from web pages. \
                     When the full browser engine is not available, the browse tool \
                     falls back to HTTP fetching (read-only, no JavaScript execution).",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Browser",
                        SettingField::switch(
                            |cx: &App| cx.global::<BrowserSettingsModel>().enabled,
                            |_val: bool, cx: &mut App| {
                                browser_settings_controller::toggle_browser(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "When enabled, the AI can browse websites, click elements, fill forms, \
                         and extract content. Tools: browse, browser_action, browser_extract, \
                         browser_auth, browser_tabs.",
                    ),
                    SettingItem::new(
                        "Headless Mode",
                        SettingField::switch(
                            |cx: &App| cx.global::<BrowserSettingsModel>().headless,
                            |_val: bool, cx: &mut App| {
                                browser_settings_controller::toggle_headless(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "Run the browser without a visible window. Session capture always \
                         uses a visible window regardless of this setting.",
                    ),
                    SettingItem::new(
                        "Max Tabs",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 1.0,
                                max: 20.0,
                                ..Default::default()
                            },
                            |cx: &App| cx.global::<BrowserSettingsModel>().max_tabs as f64,
                            |val: f64, cx: &mut App| {
                                browser_settings_controller::set_max_tabs(val as u32, cx);
                            },
                        )
                        .default_value(5.0),
                    )
                    .description("Maximum number of concurrent browser tabs (1-20)."),
                    SettingItem::new(
                        "Page Load Timeout",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 5.0,
                                max: 120.0,
                                ..Default::default()
                            },
                            |cx: &App| cx.global::<BrowserSettingsModel>().timeout_seconds as f64,
                            |val: f64, cx: &mut App| {
                                browser_settings_controller::set_timeout(val as u32, cx);
                            },
                        )
                        .default_value(30.0),
                    )
                    .description("Seconds to wait for a page to load before timing out (5-120)."),
                ]),
            SettingGroup::new()
                .title("Approval Settings")
                .description(
                    "Control which browser actions require user approval before executing.",
                )
                .items(vec![
                    SettingItem::new(
                        "Require Auth Approval",
                        SettingField::switch(
                            |cx: &App| cx.global::<BrowserSettingsModel>().require_auth_approval,
                            |_val: bool, cx: &mut App| {
                                browser_settings_controller::toggle_auth_approval(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Require user approval before the AI authenticates with stored credentials.",
                    ),
                    SettingItem::new(
                        "Require Action Approval",
                        SettingField::switch(
                            |cx: &App| cx.global::<BrowserSettingsModel>().require_action_approval,
                            |_val: bool, cx: &mut App| {
                                browser_settings_controller::toggle_action_approval(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Require user approval before the AI clicks, fills, or selects elements on a page.",
                    ),
                ]),
            SettingGroup::new()
                .title("Login Profiles")
                .description(
                    "Manage login credentials for websites the AI can authenticate to. \
                     Secrets are stored in the OS keyring — never written to disk.\n\n\
                     • Form Login — provide CSS selectors and a username/password. \
                     The AI fills the form automatically.\n\n\
                     • Session Capture — for OAuth/2FA sites, manually log in and \
                     capture session cookies.",
                )
                .items(vec![SettingItem::render(|_options, window, cx| {
                    let view =
                        if let Some(existing) = cx.try_global::<GlobalCredentialsTableView>() {
                            if let Some(view) = existing.view.clone() {
                                view
                            } else {
                                let new_view =
                                    cx.new(|cx| CredentialsTableView::new(window, cx));
                                cx.set_global(GlobalCredentialsTableView {
                                    view: Some(new_view.clone()),
                                });
                                new_view
                            }
                        } else {
                            let new_view = cx.new(|cx| CredentialsTableView::new(window, cx));
                            cx.set_global(GlobalCredentialsTableView {
                                view: Some(new_view.clone()),
                            });
                            new_view
                        };

                    div().w_full().child(view)
                })]),
        ])
}
