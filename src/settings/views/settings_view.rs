use crate::settings::controllers::SettingsView;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::*;

use gpui_component::{
    ActiveTheme, Sizable, Size, Theme, ThemeMode,
    group_box::GroupBoxVariant,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
};

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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
                                        cx.refresh_windows();
                                    },
                                )
                                .default_value(false),
                            )
                            .description("Switch between light and dark themes."),
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
                                        cx.global_mut::<GeneralSettingsModel>().font_size =
                                            val as f32;
                                        cx.refresh_windows();
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
                                        cx.global_mut::<GeneralSettingsModel>().line_height =
                                            val as f32;
                                    },
                                )
                                .default_value(20.0),
                            )
                            .description("Adjust the line height for text."),
                        ]),
                    ]),
            ])
    }
}
