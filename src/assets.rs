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
    Refresh,       // refresh-ccw.svg - Idle state
    Loader,        // loader.svg - Checking/Installing
    AlertCircle,   // alert-circle.svg - Errors
    CheckCircle,   // check-circle.svg - Update ready
    Copy,          // copy.svg - Copy button
    Lock,          // lock.svg - Sandboxed execution
    TriangleAlert, // triangle-alert.svg - Warning indicator
    CircleX,       // circle-x.svg - Error indicator
    McpServer,     // mcp-server.svg - MCP indicator
    Wrench,        // wrench.svg - Filesystem tools indicator
    Earth,         // earth.svg - Fetch tool online/offline toggle
    Codesandbox,   // codesandbox.svg - Network isolation (sandbox) toggle
    Brain,         // brain.svg - Thinking block header
    Paperclip,     // paperclip.svg - Non-image attachment
}

impl IconNamed for CustomIcon {
    fn path(self) -> SharedString {
        match self {
            // Auto-updater icons
            CustomIcon::Refresh => "icons/refresh-ccw.svg",
            CustomIcon::Loader => "icons/loader.svg",
            CustomIcon::AlertCircle => "icons/alert-circle.svg",
            CustomIcon::CheckCircle => "icons/check-circle.svg",
            CustomIcon::Copy => "icons/copy.svg",
            CustomIcon::Lock => "icons/lock.svg",
            CustomIcon::TriangleAlert => "icons/triangle-alert.svg",
            CustomIcon::CircleX => "icons/circle-x.svg",
            CustomIcon::McpServer => "icons/mcp-server.svg",
            CustomIcon::Wrench => "icons/wrench.svg",
            CustomIcon::Earth => "icons/earth.svg",
            CustomIcon::Codesandbox => "icons/codesandbox.svg",
            CustomIcon::Brain => "icons/brain.svg",
            CustomIcon::Paperclip => "icons/paperclip.svg",
        }
        .into()
    }
}
