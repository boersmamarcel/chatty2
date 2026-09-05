use anyhow::anyhow;
use gpui::*;
use gpui_component::IconNamed;
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "**/*.svg"]
pub struct ChattyAssets;

impl AssetSource for ChattyAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }

        // `Application::with_assets` replaces the asset source rather than chaining,
        // so fall back to gpui-component's bundle for the built-in `IconName` icons
        // we don't vendor ourselves.
        if let Some(f) = Self::get(path) {
            return Ok(Some(f.data));
        }

        gpui_component_assets::Assets
            .load(path)
            .map_err(|_| anyhow!("could not find asset at path \"{path}\""))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths: Vec<SharedString> = Self::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        paths.extend(gpui_component_assets::Assets.list(path)?);
        Ok(paths)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CustomIcon {
    // Auto-updater icons
    Refresh,       // refresh-ccw.svg - Idle state
    Loader,        // loader.svg - Checking/Installing
    AlertCircle,   // alert-circle.svg - Errors
    CheckCircle,   // check-circle.svg - Update ready
    CircleDashed,  // circle-dashed.svg - Planned item
    CircleDot,     // circle-dot.svg - Completed item
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
    Image,         // image.svg - Copy as image (PNG)
    Download,      // download.svg - Conversation download action
    FolderOpen,    // folder-open.svg - Working directory picker
    Ollama,        // ollama.svg - Ollama provider badge
    OpenRouter,    // openrouter.svg - OpenRouter provider badge
    Azure,         // azure.svg - Azure provider badge
    GitMerge,      // git-merge.svg - PR status bar, merged pull request
    GitPr,         // git-pull-request.svg - PR status bar, open pull request
}

impl IconNamed for CustomIcon {
    fn path(self) -> SharedString {
        match self {
            // Auto-updater icons
            CustomIcon::Refresh => "icons/refresh-ccw.svg",
            CustomIcon::Loader => "icons/loader.svg",
            CustomIcon::AlertCircle => "icons/alert-circle.svg",
            CustomIcon::CheckCircle => "icons/check-circle.svg",
            CustomIcon::CircleDashed => "icons/circle-dashed.svg",
            CustomIcon::CircleDot => "icons/circle-dot.svg",
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
            CustomIcon::Image => "icons/image.svg",
            CustomIcon::Download => "icons/download.svg",
            CustomIcon::FolderOpen => "icons/folder-open.svg",
            CustomIcon::Ollama => "icons/ollama.svg",
            CustomIcon::OpenRouter => "icons/openrouter.svg",
            CustomIcon::Azure => "icons/azure.svg",
            CustomIcon::GitMerge => "icons/git-merge.svg",
            CustomIcon::GitPr => "icons/git-pull-request.svg",
        }
        .into()
    }
}
