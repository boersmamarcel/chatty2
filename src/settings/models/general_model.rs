use gpui::Global;

#[derive(Clone)]
pub struct GeneralSettingsModel {
    pub font_size: f32,
    pub line_height: f32,
}

impl Default for GeneralSettingsModel {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            line_height: 20.0,
        }
    }
}

impl Global for GeneralSettingsModel {}
