use crate::settings::controllers::SettingsView;
use crate::settings::controllers::general_settings_controller;
use crate::settings::models::GeneralSettingsModel;
use crate::settings::views::models_page::{GlobalModelsListView, ModelsListView};
use crate::settings::views::providers_view::providers_page;

use gpui::*;

use gpui_component::{
    ActiveTheme, Root, Sizable, Size, Theme, ThemeMode,
    button::Button,
    group_box::GroupBoxVariant,
    menu::{DropdownMenu, PopupMenuItem},
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
};

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Use cached theme options instead of recomputing on every render
        let theme_options = self.cached_theme_options.clone();
        let dialog_layer = Root::render_dialog_layer(window, cx);

        div()
            .size_full()
            .child(Settings::new("app-settings")
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
                SettingPage::new("Models")
                    .description("Configure AI models and their parameters")
                    .resettable(false)
                    .groups(vec![
                        SettingGroup::new()
                            .title("Models List")
                            .description("All configured AI models")
                            .items(vec![SettingItem::new(
                                "",
                                SettingField::render(|_options, window, cx| {
                                    // Get or create the global singleton view
                                    let view = if let Some(existing_view) = cx.try_global::<GlobalModelsListView>() {
                                        if let Some(view) = existing_view.view.clone() {
                                            view
                                        } else {
                                            let new_view = cx.new(|cx| ModelsListView::new(window, cx));
                                            cx.set_global(GlobalModelsListView {
                                                view: Some(new_view.clone()),
                                            });
                                            new_view
                                        }
                                    } else {
                                        let new_view = cx.new(|cx| ModelsListView::new(window, cx));
                                        cx.set_global(GlobalModelsListView {
                                            view: Some(new_view.clone()),
                                        });
                                        new_view
                                    };

                                    div()
                                        .size_full()
                                        .min_h(px(400.))
                                        .child(view)
                                        .into_any_element()
                                }),
                            )]),
                    ]),
                providers_page(),
            ]))
            .children(dialog_layer)
    }
}
