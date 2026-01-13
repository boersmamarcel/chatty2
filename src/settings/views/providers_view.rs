use gpui::App;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

pub fn providers_page() -> SettingPage {
    SettingPage::new("Providers").resettable(true).groups(vec![
        SettingGroup::new().title("API Providers").items(vec![
            SettingItem::new(
                "Enable Providers",
                SettingField::switch(|_cx: &App| false, |_val: bool, _cx: &mut App| {})
                    .default_value(false),
            )
            .description("Provider settings will be added here."),
        ]),
    ])
}
