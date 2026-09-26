//! Settings → Terminal: the bottom terminal dock (AGE-582).

use crate::settings::controllers::general_settings_controller::update_terminal_settings;
use crate::settings::models::GeneralSettingsModel;
use chatty_core::settings::models::general_model::{
    DEFAULT_TERMINAL_DOCK_HEIGHT, DEFAULT_TERMINAL_SCROLLBACK, TerminalShareDefault,
};
use gpui::{App, SharedString};
use gpui_component::ActiveTheme;
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};

/// The "When sharing a terminal" choices, as (value, label).
const SHARE_DEFAULTS: [(TerminalShareDefault, &str, &str); 3] = [
    (TerminalShareDefault::Ask, "ask", "Ask"),
    (TerminalShareDefault::ReadOnly, "read_only", "Read only"),
    (TerminalShareDefault::ReadRun, "read_run", "Read + run"),
];

fn share_default_key(value: TerminalShareDefault) -> &'static str {
    SHARE_DEFAULTS
        .iter()
        .find(|(v, _, _)| *v == value)
        .map_or("ask", |(_, key, _)| key)
}

fn share_default_from_key(key: &str) -> TerminalShareDefault {
    SHARE_DEFAULTS
        .iter()
        .find(|(_, k, _)| *k == key)
        .map_or(TerminalShareDefault::Ask, |(v, _, _)| *v)
}

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
            SettingGroup::new().title("Agent access").items(vec![
                SettingItem::new(
                    "When sharing a terminal",
                    SettingField::dropdown(
                        SHARE_DEFAULTS
                            .iter()
                            .map(|(_, key, label)| ((*key).into(), (*label).into()))
                            .collect(),
                        |cx: &App| {
                            share_default_key(
                                cx.global::<GeneralSettingsModel>().terminal.share_default,
                            )
                            .into()
                        },
                        |val: SharedString, cx: &mut App| {
                            update_terminal_settings(cx, |t| {
                                t.share_default = share_default_from_key(&val)
                            });
                        },
                    ),
                )
                .description(
                    "What the eye icon on a terminal tab does. Ask shows the Read only / \
                     Read + run choice each time; the other two share at that level at once. \
                     Terminals are never shared with the agent until you click the eye.",
                ),
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
    use super::*;

    #[test]
    fn share_default_keys_round_trip() {
        for (value, key, _) in SHARE_DEFAULTS {
            assert_eq!(share_default_key(value), key);
            assert_eq!(share_default_from_key(key), value);
        }
        assert_eq!(share_default_from_key("junk"), TerminalShareDefault::Ask);
    }

    #[test]
    fn blank_fields_mean_unset() {
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("   "), None);
        assert_eq!(non_empty(" /bin/zsh "), Some("/bin/zsh".to_string()));
    }
}
