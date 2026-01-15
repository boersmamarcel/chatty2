use crate::settings::controllers::SettingsView;
use crate::settings::controllers::general_settings_controller;
use crate::settings::models::GeneralSettingsModel;
use crate::settings::views::providers_view::providers_page;

use gpui::*;

use gpui_component::{
    ActiveTheme, Sizable, Size, Theme, ThemeMode, ThemeRegistry,
    button::Button,
    group_box::GroupBoxVariant,
    menu::{DropdownMenu, PopupMenuItem},
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
};

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Build theme options list - extract unique base names
        let all_themes: Vec<SharedString> =
            ThemeRegistry::global(cx).themes().keys().cloned().collect();

        // Extract base theme names (remove " Light" and " Dark" suffixes)
        let mut theme_bases: std::collections::HashSet<String> = std::collections::HashSet::new();
        for theme_name in &all_themes {
            let name_str = theme_name.to_string();
            let base_name = if name_str.ends_with(" Light") {
                name_str.strip_suffix(" Light").unwrap().to_string()
            } else if name_str.ends_with(" Dark") {
                name_str.strip_suffix(" Dark").unwrap().to_string()
            } else {
                name_str
            };
            theme_bases.insert(base_name);
        }

        // Convert to sorted Vec for dropdown
        let mut theme_options: Vec<(SharedString, SharedString)> = theme_bases
            .into_iter()
            .map(|name| {
                let shared: SharedString = name.clone().into();
                (shared.clone(), shared)
            })
            .collect();

        theme_options.sort_by(|a, b| a.0.as_ref().cmp(b.0.as_ref()));

        Settings::new("app-settings")
            .with_size(Size::default())
            .with_group_variant(GroupBoxVariant::Outline)
            .pages(vec![
                SettingPage::new("General")
                    .resettable(true)
                    .default_open(true)
                    .groups(vec![
                        SettingGroup::new().title("Appearance").items(vec![
                            SettingItem::new(
                                "Theme",
                                SettingField::render(move |_options, _window, cx| {
                                    let theme_opts = theme_options.clone();
                                    let current_theme = cx
                                        .global::<GeneralSettingsModel>()
                                        .theme_name
                                        .clone()
                                        .unwrap_or_else(|| "Ayu".to_string());

                                    let current_label = current_theme.clone();

                                    Button::new("theme-dropdown")
                                        .label(current_label)
                                        .dropdown_caret(true)
                                        .outline()
                                        .w_full()
                                        .dropdown_menu_with_anchor(Corner::BottomLeft, move |menu, _, _| {
                                            let mut scrollable_menu = menu.max_h(px(300.0)).scrollable(true);

                                            for (value, label) in &theme_opts {
                                                let is_selected = value.to_string() == current_theme;
                                                let val_clone = value.clone();

                                                scrollable_menu = scrollable_menu.item(
                                                    PopupMenuItem::new(label.clone())
                                                        .checked(is_selected)
                                                        .on_click(move |_, _, cx| {
                                                            general_settings_controller::update_theme(
                                                                cx,
                                                                val_clone.clone(),
                                                            );
                                                        }),
                                                );
                                            }

                                            scrollable_menu
                                        })
                                        .into_any_element()
                                }),
                            )
                            .description("Select a theme family (use Dark Mode toggle for light/dark variant)"),
                            SettingItem::new(
                                "Dark Mode",
                                SettingField::switch(
                                    |cx: &App| cx.theme().mode.is_dark(),
                                    |val: bool, cx: &mut App| {
                                        let mode = if val {
                                            ThemeMode::Dark
                                        } else {
                                            ThemeMode::Light
                                        };
                                        Theme::global_mut(cx).mode = mode;
                                        Theme::change(mode, None, cx);

                                        // Re-apply current theme with new mode
                                        let base_theme = cx
                                            .global::<GeneralSettingsModel>()
                                            .theme_name
                                            .clone()
                                            .unwrap_or_else(|| "Ayu".to_string());
                                        general_settings_controller::update_theme(
                                            cx,
                                            base_theme.into(),
                                        );
                                    },
                                )
                                .default_value(false),
                            )
                            .description(
                                "Switch between light and dark variants of the selected theme.",
                            ),
                        ]),
                        SettingGroup::new().title("Text Settings").items(vec![
                            SettingItem::new(
                                "Font Size",
                                SettingField::number_input(
                                    NumberFieldOptions {
                                        min: 8.0,
                                        max: 32.0,
                                        ..Default::default()
                                    },
                                    |cx: &App| cx.global::<GeneralSettingsModel>().font_size.into(),
                                    |val: f64, cx: &mut App| {
                                        general_settings_controller::update_font_size(
                                            cx, val as f32,
                                        );
                                    },
                                )
                                .default_value(14.0),
                            )
                            .description("Adjust the default font size."),
                            SettingItem::new(
                                "Line Height",
                                SettingField::number_input(
                                    NumberFieldOptions {
                                        min: 12.0,
                                        max: 48.0,
                                        ..Default::default()
                                    },
                                    |cx: &App| {
                                        cx.global::<GeneralSettingsModel>().line_height.into()
                                    },
                                    |val: f64, cx: &mut App| {
                                        general_settings_controller::update_line_height(
                                            cx, val as f32,
                                        );
                                    },
                                )
                                .default_value(20.0),
                            )
                            .description("Adjust the line height for text."),
                        ]),
                    ]),
                providers_page(),
            ])
    }
}
