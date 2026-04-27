//! TOML manifest parsing for chatty WASM modules.
//!
//! Each module directory contains a `module.toml` file that declares the
//! module's name, version, capabilities, protocols, and resource limits.
//!
//! # Example `module.toml`
//!
//! ```toml
//! [module]
//! name = "echo-agent"
//! version = "0.1.0"
//! description = "A simple echo agent for testing"
//! wasm = "echo_agent.wasm"
//!
//! [capabilities]
//! tools = ["echo", "reverse"]
//! chat = true
//! agent = true
//!
//! [protocols]
//! openai_compat = true
//! mcp = true
//! a2a = true
//!
//! [resources]
//! max_memory_mb = 64
//! max_execution_ms = 30000
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Raw TOML structures
// ---------------------------------------------------------------------------

/// Top-level structure deserialized from `module.toml`.
#[derive(Debug, Deserialize)]
pub(crate) struct RawManifest {
    pub module: RawModuleSection,
    #[serde(default)]
    pub capabilities: RawCapabilities,
    #[serde(default)]
    pub protocols: RawProtocols,
    #[serde(default)]
    pub resources: RawResources,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawModuleSection {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Path to the WASM binary, relative to the module directory.
    /// Optional for `execution_mode = "remote"` modules which run on the
    /// hive-runner and do not need a local binary.
    #[serde(default)]
    pub wasm: Option<String>,
    /// Where the module is executed: `"local"` (default) or `"remote"`.
    #[serde(default = "default_local")]
    pub execution_mode: String,
}

fn default_local() -> String {
    "local".to_string()
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub chat: bool,
    #[serde(default)]
    pub agent: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawProtocols {
    #[serde(default)]
    pub openai_compat: bool,
    #[serde(default)]
    pub mcp: bool,
    #[serde(default)]
    pub a2a: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawResources {
    /// Maximum memory in megabytes. `0` means use the runtime default.
    #[serde(default)]
    pub max_memory_mb: u64,
    /// Execution timeout in milliseconds. `0` means use the runtime default.
    #[serde(default)]
    pub max_execution_ms: u64,
}

// ---------------------------------------------------------------------------
// Public manifest type
// ---------------------------------------------------------------------------

/// Capabilities declared by a module.
#[derive(Debug, Clone, Default)]
pub struct ModuleCapabilities {
    /// Tool names the module exposes.
    pub tools: Vec<String>,
    /// Whether the module implements the `chat` export.
    pub chat: bool,
    /// Whether the module acts as an autonomous agent.
    pub agent: bool,
}

/// Protocol flags declared by a module.
#[derive(Debug, Clone, Default)]
pub struct ModuleProtocols {
    pub openai_compat: bool,
    pub mcp: bool,
    pub a2a: bool,
}

/// Resource limits declared by a module.
///
/// Values of `0` indicate "use the runtime default".
#[derive(Debug, Clone, Default)]
pub struct ModuleResourceLimits {
    pub max_memory_mb: u64,
    pub max_execution_ms: u64,
}

/// Parsed and validated module manifest loaded from `module.toml`.
#[derive(Debug, Clone)]
pub struct ModuleManifest {
    /// Module name, e.g. `"echo-agent"`.
    pub name: String,
    /// Semver-compatible version string, e.g. `"0.1.0"`.
    pub version: String,
    /// Human-readable description.
    pub description: String,
    /// Path to the `.wasm` file, resolved relative to the manifest directory.
    /// `None` for remote-execution modules which have no local WASM binary.
    pub wasm_path: Option<PathBuf>,
    /// Execution location: `"local"` (default) or `"remote"` / `"remote_only"`.
    pub execution_mode: String,
    /// Capability declarations.
    pub capabilities: ModuleCapabilities,
    /// Protocol declarations.
    pub protocols: ModuleProtocols,
    /// Resource limit declarations.
    pub resources: ModuleResourceLimits,
}

impl ModuleManifest {
    /// Parse and validate a `module.toml` file.
    ///
    /// `manifest_path` must point to the `module.toml` file itself; the
    /// `.wasm` path declared in `[module].wasm` is resolved relative to its
    /// parent directory.
    pub fn from_file(manifest_path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(manifest_path)
            .with_context(|| format!("failed to read manifest at {}", manifest_path.display()))?;

        Self::from_str(&content, manifest_path)
    }

    /// Parse and validate a TOML string.
    ///
    /// `manifest_path` is used to resolve the relative `.wasm` path and to
    /// produce meaningful error messages; it does **not** need to exist on
    /// disk.
    pub fn from_str(content: &str, manifest_path: &Path) -> Result<Self> {
        let raw: RawManifest = toml::from_str(content)
            .with_context(|| format!("invalid TOML in {}", manifest_path.display()))?;

        Self::validate(raw, manifest_path)
    }

    fn validate(raw: RawManifest, manifest_path: &Path) -> Result<Self> {
        // -- [module].name must be non-empty --
        let name = raw.module.name.trim().to_owned();
        if name.is_empty() {
            bail!(
                "manifest {}: [module].name must not be empty",
                manifest_path.display()
            );
        }

        // -- [module].version must be non-empty --
        let version = raw.module.version.trim().to_owned();
        if version.is_empty() {
            bail!(
                "manifest {}: [module].version must not be empty",
                manifest_path.display()
            );
        }

        let execution_mode = raw.module.execution_mode.clone();
        let is_remote = matches!(execution_mode.as_str(), "remote" | "remote_only");

        // -- [module].wasm required for local execution only --
        let wasm_path = if is_remote {
            None
        } else {
            let wasm_rel = raw.module.wasm.as_deref().unwrap_or("").trim().to_owned();
            if wasm_rel.is_empty() {
                bail!(
                    "manifest {}: [module].wasm must not be empty for local execution",
                    manifest_path.display()
                );
            }
            let module_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
            Some(module_dir.join(&wasm_rel))
        };

        Ok(Self {
            name,
            version,
            description: raw.module.description,
            wasm_path,
            execution_mode,
            capabilities: ModuleCapabilities {
                tools: raw.capabilities.tools,
                chat: raw.capabilities.chat,
                agent: raw.capabilities.agent,
            },
            protocols: ModuleProtocols {
                openai_compat: raw.protocols.openai_compat,
                mcp: raw.protocols.mcp,
                a2a: raw.protocols.a2a,
            },
            resources: ModuleResourceLimits {
                max_memory_mb: raw.resources.max_memory_mb,
                max_execution_ms: raw.resources.max_execution_ms,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(toml: &str) -> Result<ModuleManifest> {
        ModuleManifest::from_str(toml, Path::new("/fake/module.toml"))
    }

    const FULL_TOML: &str = r#"
[module]
name = "echo-agent"
version = "0.1.0"
description = "A simple echo agent for testing"
wasm = "echo_agent.wasm"

[capabilities]
tools = ["echo", "reverse"]
chat = true
agent = true

[protocols]
openai_compat = true
mcp = true
a2a = true

[resources]
max_memory_mb = 64
max_execution_ms = 30000
"#;

    #[test]
    fn full_manifest_parses() {
        let m = parse(FULL_TOML).expect("should parse");
        assert_eq!(m.name, "echo-agent");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.description, "A simple echo agent for testing");
        assert_eq!(m.wasm_path, Some(PathBuf::from("/fake/echo_agent.wasm")));
        assert_eq!(m.capabilities.tools, vec!["echo", "reverse"]);
        assert!(m.capabilities.chat);
        assert!(m.capabilities.agent);
        assert!(m.protocols.openai_compat);
        assert!(m.protocols.mcp);
        assert!(m.protocols.a2a);
        assert_eq!(m.resources.max_memory_mb, 64);
        assert_eq!(m.resources.max_execution_ms, 30000);
    }

    #[test]
    fn minimal_manifest_parses() {
        let toml = r#"
[module]
name = "minimal"
version = "1.0.0"
wasm = "minimal.wasm"
"#;
        let m = parse(toml).expect("should parse");
        assert_eq!(m.name, "minimal");
        assert_eq!(m.version, "1.0.0");
        assert!(m.description.is_empty());
        assert!(m.capabilities.tools.is_empty());
        assert!(!m.capabilities.chat);
        assert!(!m.capabilities.agent);
        assert!(!m.protocols.openai_compat);
        assert!(!m.protocols.mcp);
        assert!(!m.protocols.a2a);
        assert_eq!(m.resources.max_memory_mb, 0);
        assert_eq!(m.resources.max_execution_ms, 0);
    }

    #[test]
    fn empty_name_is_rejected() {
        let toml = r#"
[module]
name = ""
version = "1.0.0"
wasm = "x.wasm"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn empty_version_is_rejected() {
        let toml = r#"
[module]
name = "x"
version = ""
wasm = "x.wasm"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn empty_wasm_is_rejected_for_local() {
        let toml = r#"
[module]
name = "x"
version = "1.0.0"
wasm = ""
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn missing_wasm_ok_for_remote() {
        let toml = r#"
[module]
name = "x"
version = "1.0.0"
execution_mode = "remote"

[protocols]
a2a = true
"#;
        let m = parse(toml).expect("remote module without wasm should parse");
        assert_eq!(m.execution_mode, "remote");
        assert!(m.wasm_path.is_none());
    }

    #[test]
    fn invalid_toml_is_rejected() {
        assert!(parse("not = valid [ toml").is_err());
    }

    #[test]
    fn wasm_path_resolved_relative_to_manifest() {
        let m = ModuleManifest::from_str(
            r#"
[module]
name = "x"
version = "1.0.0"
wasm = "sub/mod.wasm"
"#,
            Path::new("/some/dir/module.toml"),
        )
        .unwrap();
        assert_eq!(m.wasm_path, Some(PathBuf::from("/some/dir/sub/mod.wasm")));
    }
}
