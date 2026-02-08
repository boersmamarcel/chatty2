use anyhow::anyhow;
use gpui::*;
use gpui_component::IconNamed;
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "./assets"]
#[include = "**/*.svg"]
pub struct ChattyAssets;

impl AssetSource for ChattyAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }

        Self::get(path)
            .map(|f| Some(f.data))
            .ok_or_else(|| anyhow!("could not find asset at path \"{path}\""))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CustomIcon {
    // Auto-updater icons
    Refresh,     // refresh-ccw.svg - Idle state
    Loader,      // loader.svg - Checking/Installing
    Download,    // download.svg - Downloading
    AlertCircle, // alert-circle.svg - Errors
    CheckCircle, // check-circle.svg - Update ready
    Copy,        // copy.svg - Copy button
}

impl IconNamed for CustomIcon {
    fn path(self) -> SharedString {
        match self {
            // Auto-updater icons
            CustomIcon::Refresh => "icons/refresh-ccw.svg",
            CustomIcon::Loader => "icons/loader.svg",
            CustomIcon::Download => "icons/download.svg",
            CustomIcon::AlertCircle => "icons/alert-circle.svg",
            CustomIcon::CheckCircle => "icons/check-circle.svg",
            CustomIcon::Copy => "icons/copy.svg",
        }
        .into()
    }
}
