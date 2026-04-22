//! Extension install/uninstall service.
//!
//! Orchestrates downloading WASM modules from the Hive registry, writing them
//! to the platform module directory, and updating the `ExtensionsModel`.

use std::path::{Path, PathBuf};

use crate::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::models::module_settings::default_module_dir;
use hive_client::models::DownloadResult;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("extension '{0}' is already installed")]
    AlreadyInstalled(String),
    #[error("registry client error: {0}")]
    Client(#[from] hive_client::ClientError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid manifest: {0}")]
    BadManifest(String),
}

/// Install a WASM module from a [`DownloadResult`] into the local module
/// directory and register it in the extensions model.
///
/// Returns the `InstalledExtension` that was created.
pub fn install_wasm_module(
    download: &DownloadResult,
    name: &str,
    version: &str,
    display_name: &str,
    description: &str,
    pricing_model: &str,
    extensions: &mut ExtensionsModel,
) -> Result<InstalledExtension, InstallError> {
    if extensions.is_installed(name) {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let module_dir = default_module_dir();
    let dest = PathBuf::from(&module_dir).join(name);
    std::fs::create_dir_all(&dest)?;

    // Write the .wasm binary
    let wasm_filename = format!("{name}.wasm");
    std::fs::write(dest.join(&wasm_filename), &download.wasm)?;

    // Build module.toml from the Hive manifest JSON (or generate a minimal one)
    let toml_content = build_module_toml(
        name,
        version,
        description,
        Some(&wasm_filename),
        "local",
        &download.manifest,
    );
    std::fs::write(dest.join("module.toml"), toml_content)?;

    let ext = InstalledExtension {
        id: name.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        kind: ExtensionKind::WasmModule,
        source: ExtensionSource::Hive {
            module_name: name.to_string(),
            version: version.to_string(),
        },
        pricing_model: Some(pricing_model.to_string()),
        enabled: true,
    };

    extensions.add(ext.clone());
    Ok(ext)
}

/// Install a remote module (no WASM download) — writes a `module.toml` that
/// declares `execution_mode = "remote"` so the gateway routes calls to the
/// hive-runner.  Removes any stale `.wasm` file left from a previous local
/// install of the same module.
pub fn install_remote_module(
    name: &str,
    version: &str,
    display_name: &str,
    description: &str,
    pricing_model: &str,
    version_manifest: &serde_json::Value,
    extensions: &mut ExtensionsModel,
) -> Result<InstalledExtension, InstallError> {
    if extensions.is_installed(name) {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let module_dir = default_module_dir();
    let dest = PathBuf::from(&module_dir).join(name);
    std::fs::create_dir_all(&dest)?;

    // Remove any stale WASM binary left from a previous local install.
    let wasm_path = dest.join(format!("{name}.wasm"));
    if wasm_path.exists() {
        std::fs::remove_file(&wasm_path)?;
    }

    // Write module.toml with execution_mode = "remote" (no wasm field).
    let toml_content = build_module_toml(
        name,
        version,
        description,
        None,
        "remote",
        version_manifest,
    );
    std::fs::write(dest.join("module.toml"), toml_content)?;

    let ext = InstalledExtension {
        id: name.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        kind: ExtensionKind::WasmModule,
        source: ExtensionSource::Hive {
            module_name: name.to_string(),
            version: version.to_string(),
        },
        pricing_model: Some(pricing_model.to_string()),
        enabled: true,
    };

    extensions.add(ext.clone());
    Ok(ext)
}

/// Install an MCP server extension from registry metadata.
pub fn install_mcp_extension(
    name: &str,
    display_name: &str,
    description: &str,
    mcp_config: McpServerConfig,
    version: &str,
    extensions: &mut ExtensionsModel,
) -> Result<InstalledExtension, InstallError> {
    if extensions.is_installed(name) {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let ext = InstalledExtension {
        id: name.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        kind: ExtensionKind::McpServer(mcp_config),
        source: ExtensionSource::Hive {
            module_name: name.to_string(),
            version: version.to_string(),
        },
        pricing_model: None,
        enabled: true,
    };

    extensions.add(ext.clone());
    Ok(ext)
}

/// Uninstall an extension by ID. Removes WASM files for module extensions.
pub fn uninstall_extension(id: &str, extensions: &mut ExtensionsModel) -> Result<(), InstallError> {
    // If it's a WASM module, clean up files on disk
    if let Some(ext) = extensions.find(id)
        && matches!(ext.kind, ExtensionKind::WasmModule)
    {
        let module_dir = PathBuf::from(default_module_dir()).join(id);
        if module_dir.exists() {
            std::fs::remove_dir_all(&module_dir)?;
        }
    }

    extensions.remove(id);
    Ok(())
}

/// Check whether an update is available for a Hive-sourced extension.
pub fn needs_update(ext: &InstalledExtension, latest_version: &str) -> bool {
    match &ext.source {
        ExtensionSource::Hive { version, .. } => version != latest_version,
        ExtensionSource::Custom => false,
    }
}

/// Returns the on-disk path for an installed WASM module.
pub fn module_path(name: &str) -> PathBuf {
    Path::new(&default_module_dir()).join(name)
}

/// Errors that can occur when changing a module's execution mode.
#[derive(Debug, thiserror::Error)]
pub enum SetExecutionModeError {
    #[error("Module '{0}' is not installed")]
    NotInstalled(String),
    #[error("Mode '{0}' is not valid; expected \"local\" or \"remote\"")]
    InvalidMode(String),
    #[error("Cannot switch to local: no WASM file found for module '{0}'")]
    NoWasmFile(String),
    #[error("Failed to update module.toml: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse module.toml: {0}")]
    Toml(String),
}

/// Switch a WASM module's execution mode between `"local"` and `"remote"`.
///
/// - `"local"` → module runs in the in-process WASM runtime. Requires a `.wasm`
///   file to be present in the module directory.
/// - `"remote"` → gateway routes calls to the Hive runner; the local WASM file
///   (if any) is left on disk but not loaded.
///
/// Rewrites only the `execution_mode` line of `module.toml`.  A rescan must
/// be triggered afterwards to apply the new mode.
pub fn set_module_execution_mode(
    name: &str,
    new_mode: &str,
) -> Result<(), SetExecutionModeError> {
    if new_mode != "local" && new_mode != "remote" {
        return Err(SetExecutionModeError::InvalidMode(new_mode.to_string()));
    }

    let module_dir = PathBuf::from(default_module_dir()).join(name);
    if !module_dir.is_dir() {
        return Err(SetExecutionModeError::NotInstalled(name.to_string()));
    }

    // Switching to local requires a WASM binary.
    if new_mode == "local" {
        let wasm = module_dir.join(format!("{name}.wasm"));
        if !wasm.exists() {
            return Err(SetExecutionModeError::NoWasmFile(name.to_string()));
        }
    }

    let toml_path = module_dir.join("module.toml");
    let content = std::fs::read_to_string(&toml_path)
        .map_err(|e| SetExecutionModeError::Toml(e.to_string()))?;

    // Rewrite the execution_mode line (or add it if absent).
    let mut new_lines: Vec<String> = Vec::new();
    let mut found = false;
    for line in content.lines() {
        if line.trim_start().starts_with("execution_mode") {
            if new_mode == "local" {
                // "local" is the default — omit the line to keep manifests clean.
            } else {
                new_lines.push(format!("execution_mode = \"{}\"", new_mode));
            }
            found = true;
        } else {
            new_lines.push(line.to_string());
        }
    }
    // If the line was absent and we're switching to remote, append it.
    if !found && new_mode != "local" {
        new_lines.push(format!("execution_mode = \"{}\"", new_mode));
    }

    let new_content = new_lines.join("\n") + "\n";
    std::fs::write(&toml_path, new_content)?;

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Escape a string for use inside a TOML basic string (`"..."`).
/// Handles backslashes, double quotes, and control characters.
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                // TOML unicode escape: \uXXXX
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{unit:04X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Build a `module.toml` from the Hive manifest JSON. Falls back to a
/// minimal manifest if the JSON doesn't contain the expected fields.
///
/// `wasm_filename` is `None` for remote modules (no local binary).
/// `execution_mode` is `"local"` (default) or `"remote"`.
fn build_module_toml(
    name: &str,
    version: &str,
    description: &str,
    wasm_filename: Option<&str>,
    execution_mode: &str,
    manifest: &serde_json::Value,
) -> String {
    let name = toml_escape(name);
    let version = toml_escape(version);
    let description = toml_escape(description);

    let mut toml = format!(
        "[module]\nname = \"{name}\"\nversion = \"{version}\"\ndescription = \"{description}\"\n"
    );

    if let Some(wasm) = wasm_filename {
        toml.push_str(&format!("wasm = \"{}\"\n", toml_escape(wasm)));
    }

    if execution_mode != "local" {
        toml.push_str(&format!(
            "execution_mode = \"{}\"\n",
            toml_escape(execution_mode)
        ));
    }

    // Capabilities — use Hive manifest if present, otherwise default to
    // chat + agent since marketplace modules are expected to be usable.
    if let Some(caps) = manifest.get("capabilities") {
        toml.push_str("\n[capabilities]\n");
        if let Some(tools) = caps.get("tools").and_then(|v| v.as_array()) {
            let tool_list: Vec<String> = tools
                .iter()
                .filter_map(|t| t.as_str())
                .map(|s| format!("\"{}\"", toml_escape(s)))
                .collect();
            if !tool_list.is_empty() {
                toml.push_str(&format!("tools = [{}]\n", tool_list.join(", ")));
            }
        }
        if caps.get("chat").and_then(|v| v.as_bool()).unwrap_or(false) {
            toml.push_str("chat = true\n");
        }
        if caps.get("agent").and_then(|v| v.as_bool()).unwrap_or(false) {
            toml.push_str("agent = true\n");
        }
    } else {
        // No capabilities declared — assume the module supports chat and
        // agent mode. The module scanner will validate by loading the WASM.
        toml.push_str("\n[capabilities]\nchat = true\nagent = true\n");
    }

    // Protocols — use Hive manifest if present, otherwise default to a2a = true
    // since every chatty-module-sdk module implements the WIT agent interface
    // which IS the A2A protocol.
    if let Some(protos) = manifest.get("protocols") {
        toml.push_str("\n[protocols]\n");
        for key in &["openai_compat", "mcp", "a2a"] {
            if protos.get(key).and_then(|v| v.as_bool()).unwrap_or(false) {
                toml.push_str(&format!("{key} = true\n"));
            }
        }
    } else {
        toml.push_str("\n[protocols]\na2a = true\n");
    }

    // Resources
    if let Some(res) = manifest.get("resources") {
        let mem = res
            .get("max_memory_mb")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let exec = res
            .get("max_execution_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if mem > 0 || exec > 0 {
            toml.push_str("\n[resources]\n");
            if mem > 0 {
                toml.push_str(&format!("max_memory_mb = {mem}\n"));
            }
            if exec > 0 {
                toml.push_str(&format!("max_execution_ms = {exec}\n"));
            }
        }
    }

    toml
}

/// Well-known extension ID for the built-in Hive MCP server.
pub const HIVE_MCP_EXT_ID: &str = "mcp-hive";

/// Ensure the built-in Hive registry MCP server exists in the extensions model
/// and the MCP server list. Called on first launch so users can enable it once
/// the Hive MCP endpoint is deployed (see hive issue #55).
///
/// Returns `true` if a new entry was added (caller should persist both stores).
pub fn ensure_default_hive_mcp(
    registry_url: &str,
    extensions: &mut ExtensionsModel,
    mcp_servers: &mut Vec<McpServerConfig>,
) -> bool {
    if extensions.is_installed(HIVE_MCP_EXT_ID) {
        return false;
    }

    let mcp_url = format!("{registry_url}/mcp");
    let config = McpServerConfig {
        name: "hive".to_string(),
        url: mcp_url,
        api_key: None,
        enabled: false,
        is_module: false,
    };

    extensions.add(InstalledExtension {
        id: HIVE_MCP_EXT_ID.to_string(),
        display_name: "Hive Registry".to_string(),
        description: "Search, browse, and manage Hive modules via MCP.".to_string(),
        kind: ExtensionKind::McpServer(config.clone()),
        source: ExtensionSource::Hive {
            module_name: "hive-mcp".to_string(),
            version: "built-in".to_string(),
        },
        pricing_model: None,
        enabled: false,
    });

    if !mcp_servers.iter().any(|s| s.name == "hive") {
        mcp_servers.push(config);
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_module_toml_minimal() {
        let manifest = serde_json::json!({});
        let toml = build_module_toml("test-mod", "0.1.0", "A test", Some("test-mod.wasm"), "local", &manifest);
        assert!(toml.contains("name = \"test-mod\""));
        assert!(toml.contains("wasm = \"test-mod.wasm\""));
        // local is default — execution_mode should NOT be written
        assert!(!toml.contains("execution_mode"));
        // No capabilities in manifest → defaults to chat + agent
        assert!(toml.contains("chat = true"));
        assert!(toml.contains("agent = true"));
        // No protocols in manifest → defaults to a2a = true
        assert!(toml.contains("a2a = true"));
    }

    #[test]
    fn build_module_toml_full() {
        let manifest = serde_json::json!({
            "capabilities": {
                "tools": ["echo", "reverse"],
                "chat": true,
                "agent": true
            },
            "protocols": {
                "openai_compat": true,
                "mcp": true,
                "a2a": false
            },
            "resources": {
                "max_memory_mb": 32,
                "max_execution_ms": 5000
            }
        });
        let toml = build_module_toml("echo-agent", "1.0.0", "Echo", Some("echo.wasm"), "local", &manifest);
        assert!(toml.contains("tools = [\"echo\", \"reverse\"]"));
        assert!(toml.contains("chat = true"));
        assert!(toml.contains("mcp = true"));
        assert!(!toml.contains("a2a = true"));
        assert!(toml.contains("max_memory_mb = 32"));
    }

    #[test]
    fn build_module_toml_remote() {
        let manifest = serde_json::json!({
            "capabilities": { "agent": true },
            "protocols": { "a2a": true }
        });
        let toml = build_module_toml("benford-law", "0.1.0", "Benford", None, "remote", &manifest);
        assert!(toml.contains("name = \"benford-law\""));
        // Remote modules must NOT have a wasm field
        assert!(!toml.contains("wasm ="));
        assert!(toml.contains("execution_mode = \"remote\""));
        assert!(toml.contains("agent = true"));
        assert!(toml.contains("a2a = true"));
    }

    #[test]
    fn uninstall_nonexistent_is_noop() {
        let mut model = ExtensionsModel::default();
        let result = uninstall_extension("nonexistent", &mut model);
        assert!(result.is_ok());
    }

    #[test]
    fn needs_update_detects_version_change() {
        let ext = InstalledExtension {
            id: "test".into(),
            display_name: "Test".into(),
            description: "".into(),
            kind: ExtensionKind::WasmModule,
            source: ExtensionSource::Hive {
                module_name: "test".into(),
                version: "0.1.0".into(),
            },
            pricing_model: None,
            enabled: true,
        };
        assert!(needs_update(&ext, "0.2.0"));
        assert!(!needs_update(&ext, "0.1.0"));
    }

    #[test]
    fn needs_update_custom_never_updates() {
        let ext = InstalledExtension {
            id: "custom".into(),
            display_name: "Custom".into(),
            description: "".into(),
            kind: ExtensionKind::WasmModule,
            source: ExtensionSource::Custom,
            pricing_model: None,
            enabled: true,
        };
        assert!(!needs_update(&ext, "99.0.0"));
    }

    #[test]
    fn ensure_default_hive_mcp_adds_on_first_run() {
        let mut ext = ExtensionsModel::default();
        let mut servers = vec![];
        let added = ensure_default_hive_mcp("http://localhost:8080", &mut ext, &mut servers);
        assert!(added);
        assert!(ext.is_installed(HIVE_MCP_EXT_ID));
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "hive");
        assert!(!servers[0].enabled);
    }

    #[test]
    fn ensure_default_hive_mcp_idempotent() {
        let mut ext = ExtensionsModel::default();
        let mut servers = vec![];
        ensure_default_hive_mcp("http://localhost:8080", &mut ext, &mut servers);
        let added = ensure_default_hive_mcp("http://localhost:8080", &mut ext, &mut servers);
        assert!(!added);
        assert_eq!(ext.extensions.len(), 1);
    }
}
