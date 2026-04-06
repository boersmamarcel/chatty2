use crate::settings::controllers::extensions_controller;
use crate::settings::models::extensions_store::{ExtensionKind, ExtensionsModel};
use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::models::marketplace_state::MarketplaceState;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::input::{Input, InputState};
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt as _, h_flex, v_flex};

pub fn extensions_page() -> SettingPage {
    SettingPage::new("Extensions")
        .description(
            "Browse the Hive marketplace to discover and install extensions, \
             or add your own MCP servers and A2A agents.",
        )
        .resettable(false)
        .groups(vec![
            hive_account_group(),
            installed_extensions_group(),
            marketplace_group(),
            add_custom_group(),
        ])
}

// ── Hive Account ───────────────────────────────────────────────────────────

fn hive_account_group() -> SettingGroup {
    SettingGroup::new()
        .title("Hive Account")
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let settings = cx.global::<HiveSettingsModel>();
            let is_logged_in = settings.token.is_some();
            let username = settings.username.clone().unwrap_or_default();
            let registry_url = settings.registry_url.clone();

            if is_logged_in {
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format!("Signed in as {username}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("({registry_url})")),
                    )
                    .child(
                        Button::new("hive-logout")
                            .small()
                            .label("Sign Out")
                            .on_click(|_, _window, cx| {
                                extensions_controller::logout(cx);
                            }),
                    )
                    .into_any_element()
            } else {
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Not signed in"),
                    )
                    .child(
                        Button::new("hive-login")
                            .small()
                            .label("Sign In")
                            .on_click(|_, window, cx| {
                                show_login_dialog(window, cx);
                            }),
                    )
                    .child(
                        Button::new("hive-register")
                            .small()
                            .ghost()
                            .label("Register")
                            .on_click(|_, window, cx| {
                                show_register_dialog(window, cx);
                            }),
                    )
                    .into_any_element()
            }
        })])
}

// ── Installed Extensions ───────────────────────────────────────────────────

fn installed_extensions_group() -> SettingGroup {
    SettingGroup::new()
        .title("Installed")
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let model = cx.global::<ExtensionsModel>();
            let extensions = model.extensions.clone();

            v_flex()
                .w_full()
                .gap_2()
                .when(extensions.is_empty(), |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No extensions installed yet. Browse the marketplace below."),
                    )
                })
                .when(!extensions.is_empty(), |this| {
                    this.children(extensions.iter().map(|ext| {
                        let id = ext.id.clone();
                        let toggle_id = ext.id.clone();
                        let kind_label = match &ext.kind {
                            ExtensionKind::McpServer(_) => "MCP",
                            ExtensionKind::WasmModule => "WASM",
                            ExtensionKind::A2aAgent(_) => "A2A",
                        };
                        let status_icon = if ext.enabled { "🟢" } else { "⏸" };

                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .py_1()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(div().text_sm().child(status_icon.to_string()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(cx.theme().foreground)
                                            .child(ext.display_name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .px_1()
                                            .rounded_sm()
                                            .bg(cx.theme().muted)
                                            .text_color(cx.theme().muted_foreground)
                                            .child(kind_label.to_string()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "toggle-{id}"
                                        )))
                                        .small()
                                        .ghost()
                                        .label(if ext.enabled {
                                            "Disable"
                                        } else {
                                            "Enable"
                                        })
                                        .on_click({
                                            let toggle_id = toggle_id.clone();
                                            move |_, _window, cx| {
                                                extensions_controller::toggle_extension(
                                                    toggle_id.clone(),
                                                    cx,
                                                );
                                            }
                                        }),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "remove-{id}"
                                        )))
                                        .small()
                                        .ghost()
                                        .label("✕")
                                        .on_click({
                                            let id = id.clone();
                                            move |_, _window, cx| {
                                                extensions_controller::uninstall_extension(
                                                    id.clone(),
                                                    cx,
                                                );
                                            }
                                        }),
                                    ),
                            )
                    }))
                })
                .into_any_element()
        })])
}

// ── Marketplace ────────────────────────────────────────────────────────────

fn marketplace_group() -> SettingGroup {
    SettingGroup::new()
        .title("Browse Marketplace")
        .items(vec![SettingItem::render(|_options, window, cx| {
            let state = cx.global::<MarketplaceState>();
            let loading = state.loading;
            let error = state.error.clone();
            let results = state.search_results.clone();
            let featured = state.featured.clone();
            let installed = cx.global::<ExtensionsModel>().clone();

            let search_input = cx.new(|cx| {
                let input = InputState::new(window, cx)
                    .placeholder("Search extensions...");
                input
            });

            v_flex()
                .w_full()
                .gap_3()
                // Search bar
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&search_input)))
                        .child(
                            Button::new("search-marketplace")
                                .small()
                                .icon(Icon::new(IconName::Search))
                                .label("Search")
                                .loading(loading)
                                .on_click({
                                    let search_input = search_input.clone();
                                    move |_, _window, cx| {
                                        let query =
                                            search_input.read(cx).value().trim().to_string();
                                        extensions_controller::search_marketplace(query, cx);
                                    }
                                }),
                        ),
                )
                // Error message
                .when(error.is_some(), |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger_foreground)
                            .child(error.unwrap_or_default()),
                    )
                })
                // Results or featured
                .children({
                    let display_items = if !results.is_empty() {
                        &results
                    } else {
                        &featured
                    };

                    display_items.iter().map(|module| {
                        let name = module.name.clone();
                        let version = module
                            .latest_version
                            .clone()
                            .unwrap_or_else(|| "0.0.0".into());
                        let display = module.display_name.clone();
                        let desc = module.description.clone();
                        let is_installed = installed.is_installed(&name);

                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .py_1p5()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_0p5()
                                    .child(
                                        h_flex().gap_2().items_center().child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(cx.theme().foreground)
                                                .child(display.clone()),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(desc.clone()),
                                    )
                                    .child(
                                        h_flex().gap_2().child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "v{} · ⬇ {}",
                                                    version, module.downloads
                                                )),
                                        ),
                                    ),
                            )
                            .child(if is_installed {
                                Button::new(SharedString::from(format!("installed-{name}")))
                                    .small()
                                    .ghost()
                                    .disabled(true)
                                    .label("Installed ✓")
                                    .into_any_element()
                            } else {
                                Button::new(SharedString::from(format!("install-{name}")))
                                    .small()
                                    .label("Install")
                                    .on_click({
                                        let name = name.clone();
                                        let version = version.clone();
                                        let display = display.clone();
                                        let desc = desc.clone();
                                        move |_, _window, cx| {
                                            extensions_controller::install_extension(
                                                name.clone(),
                                                version.clone(),
                                                display.clone(),
                                                desc.clone(),
                                                cx,
                                            );
                                        }
                                    })
                                    .into_any_element()
                            })
                    })
                })
                .into_any_element()
        })])
}

// ── Add Custom Extension ───────────────────────────────────────────────────

fn add_custom_group() -> SettingGroup {
    SettingGroup::new()
        .title("Add Custom Extension")
        .description("Manually configure an MCP server or A2A agent endpoint.")
        .items(vec![SettingItem::render(|_options, _window, _cx| {
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("add-custom-mcp")
                        .small()
                        .icon(Icon::new(IconName::Plus))
                        .label("Add MCP Server")
                        .on_click(|_, window, cx| {
                            show_add_mcp_dialog(window, cx);
                        }),
                )
                .into_any_element()
        })])
}

// ── Dialogs ────────────────────────────────────────────────────────────────

fn show_login_dialog(window: &mut Window, cx: &mut App) {
    let email_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Email")
    });
    let password_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Password")
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Sign In to Hive")
            .w(px(400.))
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&email_input))
                    .child(Input::new(&password_input).mask_toggle()),
            )
            .child(
                Button::new("do-login")
                    .primary()
                    .label("Sign In")
                    .on_click({
                        let email_input = email_input.clone();
                        let password_input = password_input.clone();
                        move |_, window, cx| {
                            let email = email_input.read(cx).value().trim().to_string();
                            let password = password_input.read(cx).value().to_string();
                            if !email.is_empty() && !password.is_empty() {
                                extensions_controller::login(email, password, cx);
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}

fn show_register_dialog(window: &mut Window, cx: &mut App) {
    let username_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Username (3-39 chars, lowercase)")
    });
    let email_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Email")
    });
    let password_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Password (12+ characters)")
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Register on Hive")
            .w(px(450.))
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&username_input))
                    .child(Input::new(&email_input))
                    .child(Input::new(&password_input).mask_toggle()),
            )
            .child(
                Button::new("do-register")
                    .primary()
                    .label("Register")
                    .on_click({
                        let username_input = username_input.clone();
                        let email_input = email_input.clone();
                        let password_input = password_input.clone();
                        move |_, window, cx| {
                            let username =
                                username_input.read(cx).value().trim().to_string();
                            let email = email_input.read(cx).value().trim().to_string();
                            let password = password_input.read(cx).value().to_string();
                            if !username.is_empty()
                                && !email.is_empty()
                                && password.len() >= 12
                            {
                                extensions_controller::register(
                                    username, email, password, cx,
                                );
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}

fn show_add_mcp_dialog(window: &mut Window, cx: &mut App) {
    let name_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("e.g. github-mcp")
    });
    let url_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("http://localhost:3000/mcp")
    });
    let key_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Optional API key")
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Add MCP Server")
            .w(px(500.))
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&name_input))
                    .child(Input::new(&url_input))
                    .child(Input::new(&key_input)),
            )
            .child(
                Button::new("save-add-mcp")
                    .primary()
                    .label("Add")
                    .on_click({
                        let name_input = name_input.clone();
                        let url_input = url_input.clone();
                        let key_input = key_input.clone();
                        move |_, window, cx| {
                            let name = name_input.read(cx).value().trim().to_string();
                            let url = url_input.read(cx).value().trim().to_string();
                            let api_key = {
                                let v = key_input.read(cx).value().trim().to_string();
                                if v.is_empty() { None } else { Some(v) }
                            };
                            if !name.is_empty() && !url.is_empty() {
                                extensions_controller::add_custom_mcp(
                                    name, url, api_key, cx,
                                );
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}
