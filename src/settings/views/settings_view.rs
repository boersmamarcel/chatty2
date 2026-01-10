use crate::settings::controllers::SettingsView;
use gpui::*;

use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, Size, Theme, ThemeMode,
    button::Button,
    group_box::GroupBoxVariant,
    h_flex,
    label::Label,
    setting::{
        NumberFieldOptions, RenderOptions, SettingField, SettingFieldElement, SettingGroup,
        SettingItem, SettingPage, Settings,
    },
    v_flex,
};

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        Settings::new("app-settings")
            .with_size(Size::default())
            .with_group_variant(GroupBoxVariant::Outline)
            .pages(vec![
                SettingPage::new("General")
                    .resettable(true)
                    .default_open(true)
                    .groups(vec![SettingGroup::new().title("Appearance").items(
                        vec![SettingItem::new(
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
                    .description("Switch between light and dark themes.")],
                    )]),
            ])
    }
}
