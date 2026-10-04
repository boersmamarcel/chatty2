//! Extension install/uninstall service.
//!
//! Orchestrates downloading WASM modules from the Hive registry, writing them
//! to the configured module directory, and updating the `ExtensionsModel`.
//!
//! Install hardening (PL-H5a, AGE-703): a registry-supplied name and version
//! are validated before anything touches the filesystem, the download is
//! capped at [`hive_client::MAX_DOWNLOAD_BYTES`] while it streams, and every
//! WASM install leaves an [`InstallRecord`] beside the module, which the
//! module registry checks at every load.

use std::path::Path;

use crate::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use crate::settings::models::mcp_store::McpServerConfig;
use chatty_module_registry::{INSTALL_RECORD_FILE, InstallRecord};
use hive_client::HiveRegistryClient;
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
    #[error("invalid module name {name:?}: {reason}")]
    InvalidName { name: String, reason: &'static str },
    #[error("invalid module version {version:?}: not semver ({reason})")]
    InvalidVersion { version: String, reason: String },
}

/// Longest module name the registry accepts.
const MAX_NAME_LEN: usize = 50;

/// Check `name` against the registry's own rule for module names,
/// `^[a-z][a-z0-9-]{1,48}[a-z0-9]$` with no `--`, before it is joined into
/// any path. A name that passes is a single, plain path component.
pub fn validate_module_name(name: &str) -> Result<(), InstallError> {
    let invalid = |reason| {
        Err(InstallError::InvalidName {
            name: name.to_string(),
            reason,
        })
    };
    if name.len() < 3 || name.len() > MAX_NAME_LEN {
        return invalid("must be 3 to 50 characters");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return invalid("only lowercase letters, digits and '-' are allowed");
    }
    if !name.as_bytes()[0].is_ascii_lowercase() {
        return invalid("must start with a lowercase letter");
    }
    if name.ends_with('-') {
        return invalid("must not end with '-'");
    }
    if name.contains("--") {
        return invalid("must not contain '--'");
    }
    Ok(())
}

/// Check that `version` is semver (`1.2.3`, `1.0.0-rc.1`).
pub fn validate_version(version: &str) -> Result<(), InstallError> {
    semver::Version::parse(version)
        .map(|_| ())
        .map_err(|e| InstallError::InvalidVersion {
            version: version.to_string(),
            reason: e.to_string(),
        })
}

/// Download `name@version`'s `.wasm` from the registry, the one way the
/// desktop does it: the name and version are validated first, the body is
/// capped at [`hive_client::MAX_DOWNLOAD_BYTES`] while it streams, then
/// hive-client verifies the signing chain against its trusted registry root
/// key and checks the signed manifest names `name@version` (PL-H5): an
/// unsigned or unverifiable download is refused. `on_progress` gets
/// `(bytes read, Content-Length or 0)` after each chunk.
pub async fn download_wasm_module(
    client: &HiveRegistryClient,
    name: &str,
    version: &str,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, InstallError> {
    validate_module_name(name)?;
    validate_version(version)?;
    let mut begun = client.begin_download(name, version).await?;
    let total = begun.total_size;
    let wasm = begun.read_body(|read| on_progress(read, total)).await?;
    let download = client
        .finalize_download(wasm, &begun.chain, name, version)
        .await?;
    Ok(download)
}

/// Install a WASM module from a [`DownloadResult`] into `module_dir` (the
/// configured `ModuleSettingsModel::module_dir`) and register it in the
/// extensions model.
///
/// Writes `<module_dir>/<name>/{<name>.wasm, module.toml,
/// .chatty-install.json}`. The name and version are validated before any
/// write. Returns the `InstalledExtension` that was created.
#[allow(clippy::too_many_arguments)]
pub fn install_wasm_module(
    download: &DownloadResult,
    name: &str,
    version: &str,
    display_name: &str,
    description: &str,
    pricing_model: &str,
    module_dir: &Path,
    extensions: &mut ExtensionsModel,
) -> Result<InstalledExtension, InstallError> {
    validate_module_name(name)?;
    validate_version(version)?;
    if extensions.is_installed(name) {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let dest = module_dir.join(name);
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
        &with_signed_capabilities(&download.manifest, &download.signed_manifest.capabilities),
    );
    std::fs::write(dest.join("module.toml"), toml_content)?;

    // Last, so a half-written install fails its hash check at load.
    InstallRecord::new(
        &download.wasm,
        download.trust_level.clone(),
        Some(download.publisher_public_key.clone()),
    )
    .write(&dest)?;

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

/// Install a remote module (no WASM download) into `module_dir` — writes a
/// `module.toml` that declares `execution_mode = "remote"` so the gateway
/// routes calls to the hive-runner. Removes any stale `.wasm` and install
/// record left from a previous local install of the same module.
#[allow(clippy::too_many_arguments)]
pub fn install_remote_module(
    name: &str,
    version: &str,
    display_name: &str,
    description: &str,
    pricing_model: &str,
    version_manifest: &serde_json::Value,
    module_dir: &Path,
    extensions: &mut ExtensionsModel,
) -> Result<InstalledExtension, InstallError> {
    validate_module_name(name)?;
    validate_version(version)?;
    if extensions.is_installed(name) {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let dest = module_dir.join(name);
    std::fs::create_dir_all(&dest)?;

    // Remove any stale WASM binary and its record from a previous local install.
    for stale in [format!("{name}.wasm"), INSTALL_RECORD_FILE.to_string()] {
        let path = dest.join(stale);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }

    // Write module.toml with execution_mode = "remote" (no wasm field).
    let toml_content =
        build_module_toml(name, version, description, None, "remote", version_manifest);
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

/// Uninstall an extension by ID. Removes a WASM module's directory under
/// `module_dir` (only for an ID that is a valid module name).
pub fn uninstall_extension(
    id: &str,
    module_dir: &Path,
    extensions: &mut ExtensionsModel,
) -> Result<(), InstallError> {
    // If it's a WASM module, clean up files on disk
    if let Some(ext) = extensions.find(id)
        && matches!(ext.kind, ExtensionKind::WasmModule)
    {
        validate_module_name(id)?;
        let dir = module_dir.join(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
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

/// Errors that can occur when changing a module's execution mode.
#[derive(Debug, thiserror::Error)]
pub enum SetExecutionModeError {
    #[error("Module '{0}' is not installed")]
    NotInstalled(String),
    #[error(transparent)]
    InvalidName(#[from] InstallError),
    #[error("Mode '{0}' is not valid; expected \"local\" or \"remote\"")]
    InvalidMode(String),
    #[error("Cannot switch to local: no WASM file found for module '{0}'")]
    NoWasmFile(String),
    #[error("Failed to update module.toml: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse module.toml: {0}")]
    Toml(String),
}

/// Switch the WASM module `name` under `module_dir` between `"local"` and
/// `"remote"` execution.
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
    module_dir: &Path,
) -> Result<(), SetExecutionModeError> {
    if new_mode != "local" && new_mode != "remote" {
        return Err(SetExecutionModeError::InvalidMode(new_mode.to_string()));
    }
    validate_module_name(name)?;

    let module_dir = module_dir.join(name);
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

/// `manifest` (the version record's, which nobody signed) with its
/// `capabilities` replaced by the ones the publisher signed, so the tools a
/// module requests are exactly what the verified chain covers (PL-H5).
fn with_signed_capabilities(
    manifest: &serde_json::Value,
    signed: &hive_client::verify::Capabilities,
) -> serde_json::Value {
    let mut manifest = match manifest {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    manifest.remove("capabilities");
    if !signed.tools.is_empty() {
        manifest.insert(
            "capabilities".to_string(),
            serde_json::json!({ "tools": signed.tools }),
        );
    }
    serde_json::Value::Object(manifest)
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

    // Capabilities — only the tools the Hive manifest declares. A
    // `chatty:plugin@0.4.0` plugin is never an agent: it has no `chat`
    // (PL-U3) and no `agent` (PL-U5), and `module.toml` refuses both keys,
    // so a manifest still carrying them is not copied over.
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
    }

    // Protocols — a plugin is used through agent specs whatever it says,
    // and served over MCP only when it asks. `openai_compat` went with
    // `chat` (PL-U3) and `a2a` with `agent` (PL-U5); neither is copied over.
    if let Some(protos) = manifest.get("protocols") {
        toml.push_str("\n[protocols]\n");
        if protos.get("mcp").and_then(|v| v.as_bool()).unwrap_or(false) {
            toml.push_str("mcp = true\n");
        }
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

    // Config — the registry stores only the key names the author declared
    // (never values, API spec §6.1). Each key is written empty so the user
    // sees exactly what the plugin expects them to fill in before it loads;
    // an author who declared no `[config]` keys gets no `[config]` table.
    if let Some(keys) = manifest.get("config_keys").and_then(|v| v.as_array()) {
        let names: Vec<&str> = keys.iter().filter_map(|k| k.as_str()).collect();
        if !names.is_empty() {
            toml.push_str("\n[config]\n");
            for key in names {
                toml.push_str(&format!("{key} = \"\"\n"));
            }
        }
    }

    toml
}

/// Well-known extension ID for the built-in Hive MCP server.
pub const HIVE_MCP_EXT_ID: &str = "mcp-hive";

// ── Curated external MCP catalog ──────────────────────────────────────────
//
// A small, hand-picked catalog of well-known external MCP servers that ship
// with Chatty. Each entry is seeded into the user's `ExtensionsModel` and
// `McpServersModel` on first launch as **disabled**, so the user opts in
// explicitly. Once seeded, the entry participates in the shared MCP
// enable/disable flow exactly like any other MCP server — including OAuth
// discovery, persistence, and error reporting.
//
// To add a new curated provider, append a `CuratedMcpServer` entry to
// `CURATED_MCP_SERVERS`. No other changes are required.

/// Static metadata for one curated external MCP server.
#[derive(Debug, Clone)]
pub struct CuratedMcpServer {
    /// Stable extension ID (e.g. `"mcp-notion"`). Used as the
    /// `InstalledExtension.id` and is the primary idempotency key.
    pub ext_id: &'static str,
    /// Short slug used as the `McpServerConfig.name` (e.g. `"notion"`).
    pub server_name: &'static str,
    /// Human-readable display name shown in the UI.
    pub display_name: &'static str,
    /// Short, user-facing description.
    pub description: &'static str,
    /// Remote MCP endpoint URL the client connects to.
    pub url: &'static str,
    /// Documentation / setup URL surfaced in the UI for setup guidance.
    pub docs_url: &'static str,
    /// Notes about authentication behaviour (e.g. OAuth flow expectations,
    /// failure modes). Surfaced to the user as setup guidance.
    pub auth_notes: &'static str,
}

/// The curated catalog of external MCP servers seeded at first launch.
///
/// New entries can be added here without touching the seeding logic; the
/// entry will appear automatically and participate in the shared
/// enable/disable flow.
pub const CURATED_MCP_SERVERS: &[CuratedMcpServer] = &[CuratedMcpServer {
    ext_id: "mcp-notion",
    server_name: "notion",
    display_name: "Notion",
    description: "Read and update Notion pages, databases, and search via Notion's hosted MCP server.",
    url: "https://mcp.notion.com/sse",
    docs_url: "https://developers.notion.com/docs/mcp",
    auth_notes: "Notion's hosted MCP server uses an OAuth flow. On first connect Chatty discovers the \
             OAuth metadata from the server, opens Notion's authorization page in your browser, \
             and caches the resulting tokens locally. If the browser flow is cancelled, the network \
             is unavailable, or your Notion workspace administrator has not granted the integration \
             access, the connection will report a `Failed` auth status and can be retried from the \
             extension settings.",
}];

/// Ensure every entry in [`CURATED_MCP_SERVERS`] is present in the
/// `ExtensionsModel` and `McpServersModel`.
///
/// New entries are added **disabled** so users must opt in explicitly. The
/// function is idempotent: existing entries (including the user's
/// enabled/disabled choice and any cached API key) are left untouched, so
/// it is safe to call on every launch.
///
/// Returns `true` if at least one new entry was added (caller should
/// persist both stores).
pub fn ensure_curated_mcp_servers(
    extensions: &mut ExtensionsModel,
    mcp_servers: &mut Vec<McpServerConfig>,
) -> bool {
    let mut added_any = false;

    for entry in CURATED_MCP_SERVERS {
        if extensions.is_installed(entry.ext_id) {
            continue;
        }

        let config = McpServerConfig {
            name: entry.server_name.to_string(),
            url: entry.url.to_string(),
            api_key: None,
            enabled: false,
            is_module: false,
        };

        extensions.add(InstalledExtension {
            id: entry.ext_id.to_string(),
            display_name: entry.display_name.to_string(),
            description: entry.description.to_string(),
            kind: ExtensionKind::McpServer(config.clone()),
            source: ExtensionSource::Custom,
            pricing_model: None,
            enabled: false,
        });

        // Mirror into the legacy McpServersModel only if no entry with this
        // name already exists (preserves any user-managed override).
        if !mcp_servers.iter().any(|s| s.name == entry.server_name) {
            mcp_servers.push(config);
        }

        added_any = true;
    }

    added_any
}

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
    use sha2::Digest as _;

    #[test]
    fn build_module_toml_minimal() {
        let manifest = serde_json::json!({});
        let toml = build_module_toml(
            "test-mod",
            "0.1.0",
            "A test",
            Some("test-mod.wasm"),
            "local",
            &manifest,
        );
        assert!(toml.contains("name = \"test-mod\""));
        assert!(toml.contains("wasm = \"test-mod.wasm\""));
        // local is default — execution_mode should NOT be written
        assert!(!toml.contains("execution_mode"));
        // Nothing declared, nothing written: a plugin is not an agent and
        // is served over no protocol unless it asks.
        assert!(!toml.contains("[capabilities]"));
        assert!(!toml.contains("[protocols]"));
        chatty_module_registry::ModuleManifest::from_str(&toml, Path::new("/m/module.toml"))
            .expect("the written module.toml parses");
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
            },
            "config_keys": ["api_key", "model"]
        });
        let toml = build_module_toml(
            "echo-agent",
            "1.0.0",
            "Echo",
            Some("echo.wasm"),
            "local",
            &manifest,
        );
        assert!(toml.contains("tools = [\"echo\", \"reverse\"]"));
        assert!(toml.contains("mcp = true"));
        assert!(!toml.contains("a2a = true"));
        assert!(toml.contains("max_memory_mb = 32"));
        assert!(toml.contains("api_key = \"\""));
        assert!(toml.contains("model = \"\""));
        // The 0.2.0 agent-world keys a Hive manifest may still carry are
        // dropped: `module.toml` refuses them.
        assert!(!toml.contains("chat"), "{toml}");
        assert!(!toml.contains("openai_compat"), "{toml}");
        assert!(!toml.contains("agent ="), "{toml}");
        chatty_module_registry::ModuleManifest::from_str(&toml, Path::new("/m/module.toml"))
            .expect("the written module.toml parses");
    }

    #[test]
    fn build_module_toml_config_keys_never_carry_values() {
        // The registry stores only the key names an author declared under
        // `[config]` (API spec §6.1); it never stores the values (an author
        // never publishes secrets to the registry). The installed
        // `module.toml` writes each key with an empty string so the user
        // sees exactly what to fill in — never a value from anywhere else.
        let manifest = serde_json::json!({
            "config_keys": ["greeting", "endpoint_url"]
        });
        let toml = build_module_toml(
            "configurable-mod",
            "1.0.0",
            "Needs config",
            Some("m.wasm"),
            "local",
            &manifest,
        );
        assert!(toml.contains("[config]"));
        assert!(toml.contains("greeting = \"\""));
        assert!(toml.contains("endpoint_url = \"\""));
        let parsed =
            chatty_module_registry::ModuleManifest::from_str(&toml, Path::new("/m/module.toml"))
                .expect("the written module.toml parses");
        assert_eq!(parsed.config.get("greeting").map(String::as_str), Some(""));
        assert_eq!(
            parsed.config.get("endpoint_url").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn build_module_toml_no_config_keys_writes_no_config_table() {
        let manifest = serde_json::json!({ "config_keys": [] });
        let toml = build_module_toml(
            "no-config-mod",
            "1.0.0",
            "No config",
            Some("m.wasm"),
            "local",
            &manifest,
        );
        assert!(!toml.contains("[config]"));
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
        // PL-U5: a Hive-installed module never becomes an agent, whatever
        // its Hive manifest still says.
        assert!(!toml.contains("agent ="), "{toml}");
        assert!(!toml.contains("a2a ="), "{toml}");
        chatty_module_registry::ModuleManifest::from_str(&toml, Path::new("/m/module.toml"))
            .expect("the written module.toml parses");
    }

    // ── Install hardening (PL-H5a, AGE-703) ──────────────────────────────

    use chatty_module_registry::ModuleRegistry;
    use chatty_wasm_runtime::test_support::fixture_path;
    use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
    use hive_client::TrustLevel;

    struct NoLlm;

    impl LlmProvider for NoLlm {
        fn complete(
            &self,
            _: &str,
            _: Vec<Message>,
            _: Option<String>,
        ) -> Result<CompletionResponse, String> {
            Err("no LLM in install tests".to_string())
        }
    }

    fn download_of(wasm: Vec<u8>) -> DownloadResult {
        DownloadResult {
            wasm_hash: String::new(),
            wasm,
            trust_level: TrustLevel::Signed,
            publisher_public_key: "ab".repeat(32),
            signed_manifest: hive_client::verify::SignedManifest {
                capabilities: Default::default(),
                name: String::new(),
                sha256: String::new(),
                version: String::new(),
                wit_version: "0.3.0".to_string(),
            },
            manifest: serde_json::json!({}),
        }
    }

    fn install_into(
        dir: &Path,
        name: &str,
        version: &str,
    ) -> Result<InstalledExtension, InstallError> {
        let mut extensions = ExtensionsModel::default();
        install_wasm_module(
            &download_of(b"\0asm".to_vec()),
            name,
            version,
            name,
            "",
            "free",
            dir,
            &mut extensions,
        )
    }

    /// Everything under `dir`, relative, sorted.
    fn tree(dir: &Path) -> Vec<String> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                out.push(path.strip_prefix(root).unwrap().display().to_string());
                if path.is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn install_refuses_path_escaping_names() {
        let root = tempfile::tempdir().unwrap();
        let modules = root.path().join("modules");
        std::fs::create_dir_all(&modules).unwrap();
        let too_long = format!("a{}", "b".repeat(MAX_NAME_LEN));
        for name in [
            "../x",
            "../../pwned",
            "/abs",
            "a/b",
            "a--b",
            "Upper",
            "UPPER",
            "a.b",
            "ab-",
            "1ab",
            "ab",
            "",
            too_long.as_str(),
        ] {
            let result = install_into(&modules, name, "1.0.0");
            assert!(
                matches!(result, Err(InstallError::InvalidName { .. })),
                "{name:?} was not refused as a name: {result:?}"
            );
        }
        assert_eq!(
            tree(root.path()),
            vec!["modules".to_string()],
            "nothing was written"
        );

        // The boundaries of the rule are accepted.
        for name in [
            "abc",
            "a-b",
            "a1-b2-c3",
            &format!("a{}", "b".repeat(MAX_NAME_LEN - 1)),
        ] {
            validate_module_name(name).unwrap_or_else(|e| panic!("{name:?}: {e}"));
        }
    }

    #[test]
    fn install_refuses_non_semver_version() {
        let modules = tempfile::tempdir().unwrap();
        for version in ["1", "1.0", "v1.0.0", "latest", "../1.0.0", "1.0.0/../x", ""] {
            let result = install_into(modules.path(), "echo-agent", version);
            assert!(
                matches!(result, Err(InstallError::InvalidVersion { .. })),
                "{version:?} was not refused as a version: {result:?}"
            );
        }
        assert!(tree(modules.path()).is_empty(), "nothing was written");
        validate_version("1.0.0-rc.1+build.5").unwrap();
    }

    #[test]
    fn install_uses_configured_module_dir() {
        let configured = tempfile::tempdir().unwrap();
        install_into(configured.path(), "echo-agent", "1.0.0").unwrap();
        assert_eq!(
            tree(configured.path()),
            vec![
                "echo-agent".to_string(),
                "echo-agent/.chatty-install.json".to_string(),
                "echo-agent/echo-agent.wasm".to_string(),
                "echo-agent/module.toml".to_string(),
            ]
        );
        let record = InstallRecord::read(&configured.path().join("echo-agent"))
            .unwrap()
            .expect("an install record");
        assert_eq!(record.trust_level, TrustLevel::Signed);
        assert_eq!(record.publisher_key_id, Some("ab".repeat(32)));
        assert_eq!(record.sha256, hex::encode(sha2::Sha256::digest(b"\0asm")));

        // Remote installs land there too, and uninstall removes from there.
        let mut extensions = ExtensionsModel::default();
        install_remote_module(
            "remote-mod",
            "1.0.0",
            "Remote",
            "",
            "free",
            &serde_json::json!({}),
            configured.path(),
            &mut extensions,
        )
        .unwrap();
        assert!(configured.path().join("remote-mod/module.toml").is_file());
        uninstall_extension("remote-mod", configured.path(), &mut extensions).unwrap();
        assert!(!configured.path().join("remote-mod").exists());
    }

    #[test]
    fn installed_capabilities_are_the_signed_ones() {
        let modules = tempfile::tempdir().unwrap();
        let mut download = download_of(b"\0asm".to_vec());
        download.manifest = serde_json::json!({ "capabilities": { "tools": ["unsigned-tool"] } });
        download.signed_manifest.capabilities.tools = vec!["signed-tool".to_string()];
        install_wasm_module(
            &download,
            "caps-mod",
            "1.0.0",
            "caps-mod",
            "",
            "free",
            modules.path(),
            &mut ExtensionsModel::default(),
        )
        .unwrap();
        let toml = std::fs::read_to_string(modules.path().join("caps-mod/module.toml")).unwrap();
        assert!(toml.contains("tools = [\"signed-tool\"]"), "{toml}");
        assert!(!toml.contains("unsigned-tool"), "{toml}");
    }

    #[tokio::test]
    async fn download_capped_while_streaming() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = vec![0u8; (hive_client::MAX_DOWNLOAD_BYTES + 1) as usize];
        Mock::given(method("GET"))
            .and(path("/api/modules/echo-agent/1.0.0"))
            .respond_with(
                // Chain headers are present (the body is capped before
                // anything is verified); their content does not matter here.
                ResponseTemplate::new(200)
                    .insert_header(hive_client::verify::HEADER_MANIFEST, "e30=")
                    .insert_header(hive_client::verify::HEADER_MANIFEST_SIGNATURE, "AA==")
                    .insert_header(hive_client::verify::HEADER_CERTIFICATE, "e30=")
                    .insert_header(hive_client::verify::HEADER_CERTIFICATE_SIGNATURE, "AA==")
                    .set_body_bytes(body),
            )
            .mount(&server)
            .await;
        let modules = tempfile::tempdir().unwrap();
        let client = HiveRegistryClient::new(server.uri()).with_local_root_key(&"00".repeat(32));

        let mut read = 0;
        let result = download_wasm_module(&client, "echo-agent", "1.0.0", |n, _| read = n).await;
        assert!(
            matches!(
                result,
                Err(InstallError::Client(
                    hive_client::ClientError::TooLarge { .. }
                ))
            ),
            "a 64 MiB + 1 byte body was not refused: {:?}",
            result.map(|d| d.wasm.len())
        );
        assert!(read <= hive_client::MAX_DOWNLOAD_BYTES, "read {read} bytes");
        assert!(tree(modules.path()).is_empty(), "nothing was written");
    }

    #[test]
    fn tampered_module_refused_at_load() {
        let modules = tempfile::tempdir().unwrap();
        let wasm = std::fs::read(fixture_path("tool-args")).expect("the tool-args fixture");
        let mut extensions = ExtensionsModel::default();
        install_wasm_module(
            &download_of(wasm),
            "tool-args",
            "1.0.0",
            "Tool args",
            "",
            "free",
            modules.path(),
            &mut extensions,
        )
        .unwrap();
        let dir = modules.path().join("tool-args");
        let mut registry =
            ModuleRegistry::new(std::sync::Arc::new(NoLlm), ResourceLimits::default()).unwrap();
        registry.load(&dir).expect("the untouched install loads");
        assert_eq!(registry.trust_level("tool-args"), Some(TrustLevel::Signed));
        registry.unload("tool-args").unwrap();

        // Flip one byte of the installed .wasm.
        let wasm_path = dir.join("tool-args.wasm");
        let mut bytes = std::fs::read(&wasm_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&wasm_path, bytes).unwrap();

        let err = registry
            .load(&dir)
            .expect_err("a tampered module must not load");
        assert!(format!("{err:#}").contains("hash mismatch"), "{err:#}");
        assert!(registry.get("tool-args").is_none());
    }

    #[test]
    fn uninstall_nonexistent_is_noop() {
        let mut model = ExtensionsModel::default();
        let dir = tempfile::tempdir().unwrap();
        let result = uninstall_extension("nonexistent", dir.path(), &mut model);
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

    #[test]
    fn curated_catalog_includes_notion() {
        let notion = CURATED_MCP_SERVERS
            .iter()
            .find(|c| c.ext_id == "mcp-notion")
            .expect("Notion entry must exist in the curated catalog");
        assert_eq!(notion.server_name, "notion");
        assert_eq!(notion.display_name, "Notion");
        assert_eq!(notion.url, "https://mcp.notion.com/sse");
        assert!(!notion.docs_url.is_empty());
        assert!(notion.auth_notes.to_lowercase().contains("oauth"));
    }

    #[test]
    fn curated_catalog_entries_are_unique() {
        // Both ext_id and server_name must be unique across the catalog,
        // otherwise seeding would create duplicates or silently skip entries.
        let mut ext_ids = std::collections::HashSet::new();
        let mut server_names = std::collections::HashSet::new();
        for entry in CURATED_MCP_SERVERS {
            assert!(
                ext_ids.insert(entry.ext_id),
                "duplicate ext_id: {}",
                entry.ext_id
            );
            assert!(
                server_names.insert(entry.server_name),
                "duplicate server_name: {}",
                entry.server_name
            );
        }
    }

    #[test]
    fn ensure_curated_mcp_servers_seeds_disabled() {
        let mut ext = ExtensionsModel::default();
        let mut servers = vec![];
        let added = ensure_curated_mcp_servers(&mut ext, &mut servers);
        assert!(added);

        let notion_ext = ext
            .find("mcp-notion")
            .expect("Notion extension should be installed");
        assert!(
            !notion_ext.enabled,
            "curated entries must be seeded as disabled"
        );

        let notion_server = servers
            .iter()
            .find(|s| s.name == "notion")
            .expect("Notion MCP server should be present");
        assert!(!notion_server.enabled);
        assert_eq!(notion_server.url, "https://mcp.notion.com/sse");
        assert!(notion_server.api_key.is_none());
    }

    #[test]
    fn ensure_curated_mcp_servers_idempotent() {
        let mut ext = ExtensionsModel::default();
        let mut servers = vec![];
        ensure_curated_mcp_servers(&mut ext, &mut servers);
        let added = ensure_curated_mcp_servers(&mut ext, &mut servers);
        assert!(!added, "second call must not add duplicates");
        assert_eq!(ext.extensions.len(), CURATED_MCP_SERVERS.len());
        assert_eq!(servers.len(), CURATED_MCP_SERVERS.len());
    }

    #[test]
    fn ensure_curated_mcp_servers_preserves_user_state() {
        let mut ext = ExtensionsModel::default();
        let mut servers = vec![];
        ensure_curated_mcp_servers(&mut ext, &mut servers);

        // Simulate the user enabling Notion and providing an API key.
        if let Some(installed) = ext.find_mut("mcp-notion") {
            installed.enabled = true;
            if let ExtensionKind::McpServer(ref mut cfg) = installed.kind {
                cfg.enabled = true;
                cfg.api_key = Some("user-token".to_string());
            }
        }
        if let Some(server) = servers.iter_mut().find(|s| s.name == "notion") {
            server.enabled = true;
            server.api_key = Some("user-token".to_string());
        }

        // Subsequent seeding must not clobber the user's choices.
        let added = ensure_curated_mcp_servers(&mut ext, &mut servers);
        assert!(!added);

        let notion_ext = ext.find("mcp-notion").unwrap();
        assert!(notion_ext.enabled, "user enabled state must be preserved");
        if let ExtensionKind::McpServer(ref cfg) = notion_ext.kind {
            assert_eq!(cfg.api_key.as_deref(), Some("user-token"));
        } else {
            panic!("expected McpServer kind");
        }

        let notion_server = servers.iter().find(|s| s.name == "notion").unwrap();
        assert!(notion_server.enabled);
        assert_eq!(notion_server.api_key.as_deref(), Some("user-token"));
    }
}
