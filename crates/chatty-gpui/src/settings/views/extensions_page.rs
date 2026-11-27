use crate::chatty::views::footer::progress_circle::ProgressCircle;
use crate::settings::controllers::{extensions_controller, teams_controller};
use crate::settings::models::extensions_store::{ExtensionKind, ExtensionsModel};
use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::models::marketplace_state::{MarketplaceState, MarketplaceTab};
use crate::settings::models::mcp_store::McpAuthStatus;
use crate::settings::models::{DiscoveredModulesModel, ModuleLoadStatus};
use crate::settings::views::plugins_page::add_to_agent_button;
use chatty_core::hive::models::AgentSpecListing;
use chatty_core::team_install::TeamRecord;
use chatty_module_registry::TrustLevel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::input::{Input, InputState};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt as _, alert::Alert, h_flex,
    switch::Switch, v_flex,
};

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
    // A titled item, not a bare element, so the settings search finds it
    // ("hive", "account", "sign in", "sync") (AGE-2129).
    SettingGroup::new().items(vec![SettingItem::new(
        "Hive account",
        SettingField::render(|_options, _window, cx| {
            let settings = cx.global::<HiveSettingsModel>();
            let is_logged_in = settings.token.is_some();
            let username = settings.username.clone().unwrap_or_default();
            let hive_url = settings.hive_url.clone();
            let state = cx.global::<MarketplaceState>();
            let signed_out_of_hive = state.signed_out_of_hive;
            let account_error = state.account_error.clone();
            let server_version = state.hive_server.clone();
            let client_too_old = state.client_too_old.clone();
            let sync_new = settings.sync_new_conversations;
            let existing_offered = settings.sync_existing_offered;
            let muted = cx.theme().muted_foreground;

            let status = if is_logged_in {
                format!("Signed in as {username}")
            } else if signed_out_of_hive {
                "Your Hive session ended. Sign in again to keep using your account.".to_string()
            } else {
                "Not signed in. Sign in to sync your conversations and install from the marketplace."
                    .to_string()
            };

            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(if is_logged_in {
                            cx.theme().foreground
                        } else {
                            muted
                        })
                        .child(status),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Hive address: {hive_url}")),
                )
                .when_some(server_version.filter(|_| is_logged_in), |this, version| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Server version: {version}")),
                    )
                })
                .when_some(client_too_old, |this, message| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(message),
                    )
                })
                .when_some(account_error, |this, message| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(message),
                    )
                })
                .when(is_logged_in, |this| {
                    this.child(
                        Switch::new("hive-sync-new")
                            .checked(sync_new)
                            .label("Sync new conversations")
                            .on_click(|checked: &bool, _window, cx| {
                                extensions_controller::set_sync_new_conversations(*checked, cx);
                            }),
                    )
                    .child(div().text_xs().text_color(muted).child(if sync_new {
                        "New conversations are saved to your account after their first reply, so you can open them on the web and your other devices. Turn this off to keep new conversations on this device."
                    } else {
                        "New conversations stay on this device. Turn this on to save them to your account."
                    }))
                    .child(if existing_offered {
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Your earlier conversations were offered to your account already.")
                            .into_any_element()
                    } else {
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("hive-sync-existing")
                                    .small()
                                    .label("Sync my existing conversations")
                                    .on_click(|_, _window, cx| {
                                        crate::chatty::controllers::app_controller::request_sync_existing_conversations(cx);
                                        cx.refresh_windows();
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Conversations from before you signed in stay on this device until you choose this."),
                            )
                            .into_any_element()
                    })
                })
                .child(if is_logged_in {
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("hive-logout")
                                .small()
                                .label("Sign out")
                                .on_click(|_, _window, cx| {
                                    extensions_controller::logout(cx);
                                }),
                        )
                        .into_any_element()
                } else {
                    h_flex()
                        .gap_2()
                        .child(Button::new("hive-login").small().label("Sign in").on_click(
                            |_, window, cx| {
                                show_login_dialog(window, cx);
                            },
                        ))
                        .child(
                            Button::new("hive-register")
                                .small()
                                .ghost()
                                .label("Create account")
                                .on_click(|_, window, cx| {
                                    show_register_dialog(window, cx);
                                }),
                        )
                        .into_any_element()
                })
                .into_any_element()
        }),
    )
    .description("Sign in, set your Hive address, and sync your conversations.")
    .layout(Axis::Vertical)])
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
                        let is_wasm_module = matches!(&ext.kind, ExtensionKind::WasmModule);
                        let kind_label = match &ext.kind {
                            ExtensionKind::McpServer(_) => "MCP",
                            ExtensionKind::WasmModule => "Plugin",
                            ExtensionKind::A2aAgent(_) => "A2A",
                        };
                        // An enabled MCP server whose sign-in the start skipped
                        // (AGE-2129): say so, and offer the sign-in.
                        let needs_sign_in = ext.enabled
                            && match &ext.kind {
                                ExtensionKind::McpServer(cfg) => matches!(
                                    model.mcp_auth_status(&cfg.name),
                                    Some(McpAuthStatus::NeedsAuth)
                                ),
                                _ => false,
                            };
                        // A dot, not an emoji: 🟢 and ⏸ drew as a grey dot
                        // and a box on Linux (AGE-2129).
                        let status_color = if ext.enabled {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground.opacity(0.5)
                        };
                        let status_label = if ext.enabled { "On" } else { "Off" };

                        // For WASM modules: look up discovered module metadata for
                        // execution_mode, wasm_file presence, and the registry's
                        // load failure, if any.
                        let (discovered_exec_mode, has_wasm_file, load_error, trust_level) =
                            if is_wasm_module {
                                cx.try_global::<DiscoveredModulesModel>()
                                    .and_then(|dm| {
                                        dm.modules
                                            .iter()
                                            .find(|m| {
                                                m.name == ext.id || m.directory_name == ext.id
                                            })
                                            .map(|m| {
                                                let has_wasm = m.wasm_file != "remote"
                                                    && !m.wasm_file.is_empty();
                                                let load_error = match &m.status {
                                                    ModuleLoadStatus::Error(reason) => {
                                                        Some(reason.clone())
                                                    }
                                                    _ => None,
                                                };
                                                (
                                                    m.execution_mode.clone(),
                                                    has_wasm,
                                                    load_error,
                                                    m.trust_level.clone(),
                                                )
                                            })
                                    })
                                    .unwrap_or_default()
                            } else {
                                (String::new(), false, None, None)
                            };
                        let effective_exec_mode = if is_wasm_module
                            && (discovered_exec_mode.is_empty() || discovered_exec_mode == "local")
                        {
                            "local"
                        } else {
                            discovered_exec_mode.as_str()
                        };

                        // Show current execution mode for WASM modules and egress badges for
                        // external services.
                        let egress_badge: Option<(&'static str, u32)> = match &ext.kind {
                            ExtensionKind::WasmModule => match effective_exec_mode {
                                "remote_only" => Some(("☁ Cloud Only", 0x3B82F6)),
                                "remote" => Some(("☁ Cloud", 0x3B82F6)),
                                _ => Some(("• Local", 0x6B7280)),
                            },
                            ExtensionKind::McpServer(cfg) => {
                                let is_local = crate::chatty::services::url_is_local(&cfg.url);
                                if !is_local {
                                    Some(("↗ External", 0xA855F7))
                                } else {
                                    None
                                }
                            }
                            ExtensionKind::A2aAgent(cfg) => {
                                let is_local = crate::chatty::services::url_is_local(&cfg.url);
                                if !is_local {
                                    Some(("↗ External", 0xA855F7))
                                } else {
                                    None
                                }
                            }
                        };

                        let row = h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .py_1()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("status-{}", ext.id)))
                                            .size(px(8.))
                                            .rounded_full()
                                            .bg(status_color)
                                            .tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(status_label)
                                                    .build(window, cx)
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(cx.theme().foreground)
                                            .child(ext.display_name.clone()),
                                    )
                                    .when(
                                        ext.pricing_model.as_deref() != Some("free")
                                            && ext.pricing_model.is_some(),
                                        |el| {
                                            el.child(
                                                div()
                                                    .text_xs()
                                                    .px_1()
                                                    .rounded_sm()
                                                    .bg(gpui::rgb(0xFEF3C7))
                                                    .text_color(gpui::rgb(0x92400E))
                                                    .child("Paid"),
                                            )
                                        },
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .px_1()
                                            .rounded_sm()
                                            .bg(cx.theme().muted)
                                            .text_color(cx.theme().muted_foreground)
                                            .child(kind_label.to_string()),
                                    )
                                    .when(egress_badge.is_some(), |el| {
                                        let (label, color) = egress_badge.unwrap();
                                        el.child(
                                            div()
                                                .text_xs()
                                                .px_1()
                                                .rounded_sm()
                                                .border_1()
                                                .border_color(gpui::rgb(color))
                                                .text_color(gpui::rgb(color))
                                                .child(label),
                                        )
                                    })
                                    .when_some(trust_level, |el, trust| {
                                        el.child(trust_badge(&trust, cx))
                                    })
                                    .when(needs_sign_in, |el| {
                                        el.child(
                                            div()
                                                .text_xs()
                                                .px_1()
                                                .rounded_sm()
                                                .bg(cx.theme().warning)
                                                .text_color(cx.theme().warning_foreground)
                                                .child("Needs sign-in"),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    // AGE-806: per-agent opt-in to reach this A2A agent on
                                    // a private network (LAN, Tailscale). Off by default;
                                    // cloud metadata and IPv6 link-local stay refused
                                    // either way.
                                    .when_some(
                                        match &ext.kind {
                                            ExtensionKind::A2aAgent(cfg) => {
                                                Some(cfg.allow_private_network)
                                            }
                                            _ => None,
                                        },
                                        |el, allow_private_network| {
                                            let toggle_id = id.clone();
                                            el.child(
                                                Button::new(SharedString::from(format!(
                                                    "a2a-private-network-{toggle_id}"
                                                )))
                                                .small()
                                                .ghost()
                                                .label(if allow_private_network {
                                                    "Private network: on"
                                                } else {
                                                    "Private network: off"
                                                })
                                                .on_click(move |_, _window, cx| {
                                                    extensions_controller::toggle_a2a_private_network(
                                                        toggle_id.clone(),
                                                        cx,
                                                    );
                                                }),
                                            )
                                        },
                                    )
                                    // Execution mode toggle: only for WASM modules that are
                                    // not remote_only.
                                    .when(
                                        is_wasm_module && effective_exec_mode != "remote_only",
                                        |el| {
                                            let mode_id = id.clone();
                                            let is_currently_remote =
                                                effective_exec_mode == "remote";
                                            let (btn_label, target_mode) = if is_currently_remote {
                                                ("Switch to Local", "local")
                                            } else {
                                                ("Switch to Cloud", "remote")
                                            };
                                            // Can switch to local only if wasm file exists.
                                            let can_click = !is_currently_remote || has_wasm_file;
                                            el.child(
                                                Button::new(SharedString::from(format!(
                                                    "exec-mode-{mode_id}"
                                                )))
                                                .small()
                                                .ghost()
                                                .label(btn_label)
                                                .when(!can_click, |btn| btn.disabled(true))
                                                .on_click({
                                                    let mode_id = mode_id.clone();
                                                    let target = target_mode.to_string();
                                                    move |_, _window, cx| {
                                                        extensions_controller::set_execution_mode(
                                                            mode_id.clone(),
                                                            target.clone(),
                                                            cx,
                                                        );
                                                    }
                                                }),
                                            )
                                        },
                                    )
                                    .when(needs_sign_in, |el| {
                                        let sign_in_id = id.clone();
                                        el.child(
                                            Button::new(SharedString::from(format!(
                                                "mcp-sign-in-{sign_in_id}"
                                            )))
                                            .small()
                                            .primary()
                                            .label("Sign in")
                                            .on_click(move |_, _window, cx| {
                                                extensions_controller::sign_in_mcp(
                                                    sign_in_id.clone(),
                                                    cx,
                                                );
                                            }),
                                        )
                                    })
                                    .child(
                                        Button::new(SharedString::from(format!("toggle-{id}")))
                                            .small()
                                            .ghost()
                                            .label(if ext.enabled { "Disable" } else { "Enable" })
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
                                        Button::new(SharedString::from(format!("remove-{id}")))
                                            .small()
                                            .ghost()
                                            .icon(Icon::new(IconName::Delete).size(px(14.)))
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
                            );

                        // The module registry's scan failure for this module
                        // (invalid module.toml, broken .wasm, duplicate name).
                        v_flex()
                            .w_full()
                            .child(row)
                            .when_some(load_error, |el, reason| {
                                el.child(
                                    div()
                                        .pl_6()
                                        .pb_1()
                                        .text_xs()
                                        .text_color(cx.theme().danger)
                                        .child(format!("Failed to load: {reason}")),
                                )
                            })
                    }))
                })
                .into_any_element()
        })])
}

/// The trust a loaded module has (PL-H5a): `Signed`/`Verified` from its
/// install record, `Local` for one without a record.
pub(crate) fn trust_badge(trust: &TrustLevel, cx: &App) -> Div {
    let (label, color) = match trust {
        TrustLevel::Local => ("Trust: local", cx.theme().muted_foreground),
        TrustLevel::Signed => ("Trust: signed", gpui::rgb(0x16A34A).into()),
        TrustLevel::Verified => ("Trust: verified", gpui::rgb(0x16A34A).into()),
    };
    div()
        .text_xs()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_color(color)
        .child(label)
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
            let tab = state.tab;
            let team_notice = state.team_notice.clone();
            // The Teams tab lists teams instead (team_rows).
            let (results, featured) = if tab == MarketplaceTab::Teams {
                (Vec::new(), Vec::new())
            } else {
                (results, featured)
            };
            let installed = cx.global::<ExtensionsModel>().clone();

            // use_keyed_state persists the InputState entity across re-renders
            // so the input keeps focus and typed text between frames.
            let search_input =
                window.use_keyed_state("marketplace-search-input", cx, |window, cx| {
                    InputState::new(window, cx).placeholder("Search the marketplace...")
                });

            let tab_button = |id: &'static str, label: &'static str, this: MarketplaceTab| {
                Button::new(id)
                    .small()
                    .label(label)
                    .when(tab == this, |b| b.primary())
                    .when(tab != this, |b| b.ghost())
                    .on_click({
                        let search_input = search_input.clone();
                        move |_, _window, cx| {
                            cx.global_mut::<MarketplaceState>().tab = this;
                            if this == MarketplaceTab::Teams {
                                let query = search_input.read(cx).value().trim().to_string();
                                teams_controller::search_teams(query, cx);
                            }
                            cx.refresh_windows();
                        }
                    })
            };

            v_flex()
                .w_full()
                .gap_3()
                // Plugins | Teams (MK-T2)
                .child(
                    h_flex()
                        .gap_1()
                        .child(tab_button(
                            "marketplace-tab-plugins",
                            "Plugins",
                            MarketplaceTab::Plugins,
                        ))
                        .child(tab_button(
                            "marketplace-tab-teams",
                            "Teams",
                            MarketplaceTab::Teams,
                        )),
                )
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
                                        if cx.global::<MarketplaceState>().tab
                                            == MarketplaceTab::Teams
                                        {
                                            teams_controller::search_teams(query, cx);
                                        } else {
                                            extensions_controller::search_marketplace(query, cx);
                                        }
                                    }
                                }),
                        ),
                )
                // Error message
                .when_some(error, |this, error| {
                    this.child(Alert::error("marketplace-error", error).small().on_close(
                        |_event, _window, cx| {
                            let state = cx.global_mut::<MarketplaceState>();
                            state.error = None;
                        },
                    ))
                })
                .when_some(
                    team_notice.filter(|_| tab == MarketplaceTab::Teams),
                    |this, notice| {
                        // In a sized box: unwrapped, a long notice widens
                        // the column and squeezes the search field.
                        this.child(div().w_full().overflow_hidden().child(
                            Alert::success("team-notice", notice).small().on_close(
                                |_event, _window, cx| {
                                    cx.global_mut::<MarketplaceState>().team_notice = None;
                                },
                            ),
                        ))
                    },
                )
                .when(tab == MarketplaceTab::Teams, |this| {
                    this.children(team_rows(cx))
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
                        let download_pct = cx
                            .try_global::<MarketplaceState>()
                            .and_then(|s| s.download_progress(&name))
                            .map(|p| p * 100.0);

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
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "v{} · ⬇ {}",
                                                        version, module.downloads
                                                    )),
                                            )
                                            .when(module.pricing_model != "free", |el| {
                                                el.child(
                                                    div()
                                                        .text_xs()
                                                        .px_1()
                                                        .rounded_sm()
                                                        .bg(gpui::rgb(0xFEF3C7))
                                                        .text_color(gpui::rgb(0x92400E))
                                                        .child("Paid"),
                                                )
                                            }),
                                    ),
                            )
                            .child(if is_installed {
                                // An installed plugin is tools for an agent:
                                // offer to add it to one (PL-U5b, AGE-718).
                                h_flex()
                                    .gap_1()
                                    .child(add_to_agent_button(&name))
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "uninstall-{name}"
                                        )))
                                        .small()
                                        .ghost()
                                        .label("Uninstall")
                                        .on_click({
                                            let name = name.clone();
                                            move |_, _window, cx| {
                                                extensions_controller::uninstall_extension(
                                                    name.clone(),
                                                    cx,
                                                );
                                            }
                                        }),
                                    )
                                    .into_any_element()
                            } else if let Some(pct) = download_pct {
                                // Download in progress — show animated progress circle + percentage.
                                h_flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(
                                        ProgressCircle::new(SharedString::from(format!(
                                            "dl-progress-{name}"
                                        )))
                                        .value(pct)
                                        .small(),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{pct:.0}%")),
                                    )
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
                                        let pricing_model = module.pricing_model.clone();
                                        let execution_mode = module.execution_mode.clone();
                                        move |_, _window, cx| {
                                            extensions_controller::install_extension(
                                                name.clone(),
                                                version.clone(),
                                                display.clone(),
                                                desc.clone(),
                                                pricing_model.clone(),
                                                execution_mode.clone(),
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

// ── Teams (MK-T2, AGE-841) ─────────────────────────────────────────────────

/// One row per published team: who made it, its version and installs, the
/// plugins it locks, its members and example prompt, and Install team /
/// Update / Uninstall.
fn team_rows(cx: &App) -> Vec<AnyElement> {
    let state = cx.global::<MarketplaceState>();
    if state.team_results.is_empty() && !state.loading {
        return vec![
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No published teams match. Search by name, publisher or task.")
                .into_any_element(),
        ];
    }
    state
        .team_results
        .iter()
        .map(|listing| {
            let installed = state
                .installed_teams
                .iter()
                .find(|team| team.leader == listing.name);
            // A member of an installed team came with it and goes with it.
            let member_of = state
                .installed_teams
                .iter()
                .find(|team| team.leader != listing.name && team.specs.contains(&listing.name))
                .map(|team| team.leader.clone());
            let installing = state.installing_team.as_deref() == Some(listing.name.as_str());
            team_row(listing, installed, member_of, installing, cx)
        })
        .collect()
}

fn team_row(
    listing: &AgentSpecListing,
    installed: Option<&TeamRecord>,
    member_of: Option<String>,
    installing: bool,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let line = |text: String| div().text_xs().text_color(muted).child(text);
    let badge = |text: &'static str| {
        div()
            .text_xs()
            .px_1()
            .rounded_sm()
            .bg(gpui::rgb(0xFEF3C7))
            .text_color(gpui::rgb(0x92400E))
            .child(text)
    };
    let name = listing.name.clone();
    let members: Vec<String> = listing
        .members
        .iter()
        .map(|m| format!("{} {}", m.name, m.version))
        .collect();

    let action = if let Some(leader) = member_of.filter(|_| installed.is_none()) {
        line(format!("installed with {leader}")).into_any_element()
    } else if installing {
        Button::new(SharedString::from(format!("team-installing-{name}")))
            .small()
            .label("Verifying…")
            .loading(true)
            .disabled(true)
            .into_any_element()
    } else {
        let install_label = match installed {
            Some(team) if team.version == listing.latest_version => None,
            Some(_) => Some("Update team"),
            None => Some("Install team"),
        };
        h_flex()
            .gap_1()
            .when_some(install_label, |row, label| {
                row.child(
                    Button::new(SharedString::from(format!("install-team-{name}")))
                        .small()
                        .label(label)
                        .when(listing.pricing_model != "free", |b| b.disabled(true))
                        .on_click({
                            let listing = listing.clone();
                            move |_, _window, cx| {
                                teams_controller::install_team(listing.clone(), cx);
                            }
                        }),
                )
            })
            .when(installed.is_some(), |row| {
                row.child(
                    Button::new(SharedString::from(format!("uninstall-team-{name}")))
                        .small()
                        .ghost()
                        .label("Uninstall")
                        .on_click({
                            let name = name.clone();
                            move |_, _window, cx| {
                                teams_controller::uninstall_team(name.clone(), cx);
                            }
                        }),
                )
            })
            .into_any_element()
    };

    h_flex()
        .w_full()
        .items_start()
        .justify_between()
        .gap_3()
        .py_1p5()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            v_flex()
                .flex_1()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child(name.clone()),
                        )
                        .when(listing.pricing_model != "free", |el| {
                            el.child(badge("Paid team · not available yet"))
                        })
                        .when_some(installed, |el, team| {
                            el.child(line(format!("installed {}", team.version)))
                        }),
                )
                .when_some(listing.description.clone(), |el, desc| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child(desc),
                    )
                })
                .child(line(format!(
                    "by {} · v{} · {} install{}",
                    listing.author_username,
                    listing.latest_version,
                    listing.install_count,
                    if listing.install_count == 1 { "" } else { "s" }
                )))
                .when(!members.is_empty(), |el| {
                    el.child(line(format!("Team: {}", members.join(", "))))
                })
                .when(!listing.lockfile.is_empty(), |el| {
                    el.child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .child(line("Plugins (locked):".to_string()))
                            .children(listing.lockfile.iter().map(|plugin| {
                                h_flex()
                                    .gap_1()
                                    .child(line(format!(
                                        "{} {}",
                                        plugin.locked.module, plugin.locked.version
                                    )))
                                    .when(plugin.pricing_model != "free", |el| {
                                        el.child(badge("Paid"))
                                    })
                            })),
                    )
                })
                .when_some(listing.changelog.clone(), |el, changes| {
                    el.child(line(format!(
                        "New in v{}: {changes}",
                        listing.latest_version
                    )))
                })
                .when_some(listing.example_prompt.clone(), |el, prompt| {
                    el.child(line(format!("Try: “{prompt}”")).italic())
                }),
        )
        .child(action)
        .into_any_element()
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
                .child(
                    Button::new("add-custom-a2a")
                        .small()
                        .icon(Icon::new(IconName::Plus))
                        .label("Add A2A Agent")
                        .on_click(|_, window, cx| {
                            show_add_a2a_dialog(window, cx);
                        }),
                )
                .into_any_element()
        })])
}

// ── Dialogs ────────────────────────────────────────────────────────────────

fn show_login_dialog(window: &mut Window, cx: &mut App) {
    // The Hive address, changeable here (AGE-2129): it was only in the
    // settings file.
    let current_url = cx.global::<HiveSettingsModel>().hive_url.clone();
    let address_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Hive address, e.g. https://hive.example.com")
            .default_value(current_url)
    });
    let email_input = cx.new(|cx| InputState::new(window, cx).placeholder("Email"));
    // Masked by default (AGE-2129); the eye shows it on request.
    let password_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Password")
            .masked(true)
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Sign in to Hive")
            .w(px(400.))
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&address_input))
                    .child(Input::new(&email_input))
                    .child(Input::new(&password_input).mask_toggle()),
            )
            .child(
                Button::new("do-login")
                    .primary()
                    .label("Sign In")
                    .on_click({
                        let address_input = address_input.clone();
                        let email_input = email_input.clone();
                        let password_input = password_input.clone();
                        move |_, window, cx| {
                            let email = email_input.read(cx).value().trim().to_string();
                            let password = password_input.read(cx).value().to_string();
                            if !email.is_empty() && !password.is_empty() {
                                let address = address_input.read(cx).value().to_string();
                                extensions_controller::set_hive_url(&address, cx);
                                extensions_controller::login(email, password, cx);
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}

fn show_register_dialog(window: &mut Window, cx: &mut App) {
    let current_url = cx.global::<HiveSettingsModel>().hive_url.clone();
    let address_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Hive address, e.g. https://hive.example.com")
            .default_value(current_url)
    });
    let username_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Username (3-39 chars, lowercase)"));
    let email_input = cx.new(|cx| InputState::new(window, cx).placeholder("Email"));
    let password_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Password (12+ characters)")
            .masked(true)
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Create a Hive account")
            .w(px(450.))
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&address_input))
                    .child(Input::new(&username_input))
                    .child(Input::new(&email_input))
                    .child(Input::new(&password_input).mask_toggle()),
            )
            .child(
                Button::new("do-register")
                    .primary()
                    .label("Create account")
                    .on_click({
                        let address_input = address_input.clone();
                        let username_input = username_input.clone();
                        let email_input = email_input.clone();
                        let password_input = password_input.clone();
                        move |_, window, cx| {
                            let username = username_input.read(cx).value().trim().to_string();
                            let email = email_input.read(cx).value().trim().to_string();
                            let password = password_input.read(cx).value().to_string();
                            if !username.is_empty() && !email.is_empty() && password.len() >= 12 {
                                let address = address_input.read(cx).value().to_string();
                                extensions_controller::set_hive_url(&address, cx);
                                extensions_controller::register(username, email, password, cx);
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}

fn show_add_a2a_dialog(window: &mut Window, cx: &mut App) {
    let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. voucher-agent"));
    let url_input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("https://agents.example.com/a2a/voucher-agent")
    });
    let key_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Optional API key")
            .masked(true)
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Add A2A Agent")
            .w(px(500.))
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&name_input))
                    .child(Input::new(&url_input))
                    .child(Input::new(&key_input).mask_toggle()),
            )
            .child(
                Button::new("save-add-a2a")
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
                                extensions_controller::add_custom_a2a(name, url, api_key, cx);
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}

fn show_add_mcp_dialog(window: &mut Window, cx: &mut App) {
    let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. github-mcp"));
    let url_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:3000/mcp"));
    let key_input = cx.new(|cx| InputState::new(window, cx).placeholder("Optional API key"));

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
                                extensions_controller::add_custom_mcp(name, url, api_key, cx);
                                window.close_dialog(cx);
                            }
                        }
                    }),
            )
    });
}
