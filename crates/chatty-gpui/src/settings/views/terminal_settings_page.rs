//! Settings → Terminal: the bottom terminal dock (AGE-582).

use crate::settings::controllers::general_settings_controller::update_terminal_settings;
use crate::settings::models::GeneralSettingsModel;
use chatty_core::settings::models::general_model::{
    DEFAULT_TERMINAL_DOCK_HEIGHT, DEFAULT_TERMINAL_SCROLLBACK,
};
use gpui::{App, SharedString};
use gpui_component::ActiveTheme;
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};

/// An empty or blank text field means "not set".
fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub fn terminal_settings_page() -> SettingPage {
    SettingPage::new("Terminal")
        .description("The terminal dock under the chat (Ctrl+J, or Cmd+J on macOS)")
        .resettable(false)
        .groups(vec![
            SettingGroup::new().title("Font").items(vec![
                SettingItem::new(
                    "Font Family",
                    SettingField::input(
                        |cx: &App| {
                            cx.global::<GeneralSettingsModel>()
                                .terminal
                                .font_family
                                .clone()
                                .unwrap_or_default()
                                .into()
                        },
                        |val: SharedString, cx: &mut App| {
                            update_terminal_settings(cx, |t| t.font_family = non_empty(&val));
                        },
                    ),
                )
                .description("A monospace font. Leave empty for the theme's code font."),
                SettingItem::new(
                    "Font Size",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 6.0,
                            max: 48.0,
                            ..Default::default()
                        },
                        |cx: &App| {
                            let theme_size = f32::from(cx.theme().mono_font_size);
                            cx.global::<GeneralSettingsModel>()
                                .terminal
                                .font_size
                                .unwrap_or(theme_size)
                                .into()
                        },
                        |val: f64, cx: &mut App| {
                            update_terminal_settings(cx, |t| t.font_size = Some(val as f32));
                        },
                    ),
                )
                .description("In pixels. Applies to open terminals at once."),
            ]),
            SettingGroup::new().title("Shell").items(vec![
                SettingItem::new(
                    "Shell",
                    SettingField::input(
                        |cx: &App| {
                            cx.global::<GeneralSettingsModel>()
                                .terminal
                                .shell
                                .clone()
                                .unwrap_or_default()
                                .into()
                        },
                        |val: SharedString, cx: &mut App| {
                            update_terminal_settings(cx, |t| t.shell = non_empty(&val));
                        },
                    ),
                )
                .description(
                    "Program to run, e.g. /bin/zsh or pwsh. Leave empty for your login shell \
                     ($SHELL on macOS and Linux, PowerShell on Windows). Applies to new terminals.",
                ),
                SettingItem::new(
                    "Scrollback Lines",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 0.0,
                            max: 100_000.0,
                            step: 1000.0,
                        },
                        |cx: &App| {
                            cx.global::<GeneralSettingsModel>()
                                .terminal
                                .scrollback_lines as f64
                        },
                        |val: f64, cx: &mut App| {
                            update_terminal_settings(cx, |t| t.scrollback_lines = val as usize);
                        },
                    )
                    .default_value(DEFAULT_TERMINAL_SCROLLBACK as f64),
                )
                .description("Lines kept above the screen. Applies to new terminals."),
            ]),
            SettingGroup::new().title("Dock").items(vec![
                SettingItem::new(
                    "Dock Height",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 100.0,
                            max: 2000.0,
                            step: 10.0,
                        },
                        |cx: &App| {
                            cx.global::<GeneralSettingsModel>().terminal.dock_height as f64
                        },
                        |val: f64, cx: &mut App| {
                            update_terminal_settings(cx, |t| t.dock_height = val as f32);
                        },
                    )
                    .default_value(DEFAULT_TERMINAL_DOCK_HEIGHT as f64),
                )
                .description(
                    "Height the dock opens at, in pixels. Dragging the dock's top edge changes it too.",
                ),
            ]),
        ])
}

#[cfg(test)]
mod tests {
    use super::non_empty;

    #[test]
    fn blank_fields_mean_unset() {
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("   "), None);
        assert_eq!(non_empty(" /bin/zsh "), Some("/bin/zsh".to_string()));
    }
}
