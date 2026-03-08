use crate::settings::controllers::training_settings_controller;
use crate::settings::models::training_settings::TrainingSettingsModel;
use gpui::App;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

pub fn training_settings_page() -> SettingPage {
    SettingPage::new("Training Data")
        .description("Configure automatic conversation export for model training")
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("ATIF Export")
                .description(
                    "Export conversations in Agent Trajectory Interchange Format (ATIF) \
                     for fine-tuning and analysis.",
                )
                .items(vec![
                    SettingItem::new(
                        "Auto-export ATIF",
                        SettingField::switch(
                            |cx: &App| cx.global::<TrainingSettingsModel>().atif_auto_export,
                            |_val: bool, cx: &mut App| {
                                training_settings_controller::toggle_atif_auto_export(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "Automatically export each conversation as ATIF JSON after every completed \
                     assistant response. Files are saved to the exports directory.",
                    ),
                ]),
            SettingGroup::new()
                .title("JSONL Export")
                .description(
                    "Export conversations in ChatML JSONL format for supervised fine-tuning \
                     (SFT) and direct preference optimization (DPO).",
                )
                .items(vec![
                    SettingItem::new(
                        "Auto-export JSONL",
                        SettingField::switch(
                            |cx: &App| cx.global::<TrainingSettingsModel>().jsonl_auto_export,
                            |_val: bool, cx: &mut App| {
                                training_settings_controller::toggle_jsonl_auto_export(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "Automatically export SFT and DPO training data after every completed \
                     assistant response. SFT data is appended to sft.jsonl and DPO pairs \
                     to dpo.jsonl in the exports directory.",
                    ),
                ]),
        ])
}
