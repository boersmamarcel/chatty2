//! TOML manifest parsing for chatty WASM modules.
//!
//! Each module directory contains a `module.toml` file that declares the
//! module's name, version, capabilities, protocols, resource limits, the
//! config values its guest reads through `config::get`, and the directory its
//! guest may read through `file::read-bytes`.
//!
//! Parsing is strict (PL-H3): an unknown key or table is an error, an
//! `execution_mode` must be one of `local`, `remote`, `remote_only`, and the
//! `wasm` and `[files].root` paths must be plain relative paths inside the
//! module directory. `[resources]` values above the host ceilings
//! (`chatty_wasm_runtime::MAX_*_CEILING`) are clamped, with a warning in
//! [`ModuleManifest::warnings`].
//!
//! # Example `module.toml`
//!
//! ```toml
//! [module]
//! name = "echo"
//! version = "0.2.0"
//! description = "A simple echo plugin for testing"
//! wasm = "echo.wasm"
//!
//! [capabilities]
//! tools = ["echo", "reverse"]
//!
//! [protocols]
//! mcp = true
//!
//! [resources]
//! max_memory_mb = 64
//! max_execution_ms = 30000
//!
//! [config]
//! greeting = "hello"
//!
//! [files]
//! root = "weights"
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use chatty_wasm_runtime::{MAX_EXECUTION_MS_CEILING, MAX_MEMORY_BYTES_CEILING};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Raw TOML structures
// ---------------------------------------------------------------------------

/// Top-level structure deserialized from `module.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawManifest {
    pub module: RawModuleSection,
    #[serde(default)]
    pub capabilities: RawCapabilities,
    #[serde(default)]
    pub protocols: RawProtocols,
    #[serde(default)]
    pub resources: RawResources,
    /// String → string values the guest reads through `config::get`.
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    #[serde(default)]
    pub files: Option<RawFiles>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Where the module is executed.
    #[serde(default)]
    pub execution_mode: ExecutionMode,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub agent: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawProtocols {
    #[serde(default)]
    pub mcp: bool,
    #[serde(default)]
    pub a2a: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawResources {
    /// Maximum memory in megabytes. `0` means use the runtime default.
    #[serde(default)]
    pub max_memory_mb: u64,
    /// Execution timeout in milliseconds. `0` means use the runtime default.
    #[serde(default)]
    pub max_execution_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFiles {
    /// Directory the guest may read, relative to the module directory.
    pub root: String,
}

// ---------------------------------------------------------------------------
// Public manifest type
// ---------------------------------------------------------------------------

/// Where a module is executed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// In-process, in the local WASM runtime (the default).
    #[default]
    Local,
    /// On the hive-runner; the local `.wasm`, if any, is not loaded.
    Remote,
    /// On the hive-runner only; the module ships no local binary.
    RemoteOnly,
}

impl ExecutionMode {
    /// `true` for [`Remote`](Self::Remote) and [`RemoteOnly`](Self::RemoteOnly).
    pub fn is_remote(self) -> bool {
        !matches!(self, Self::Local)
    }

    /// The `module.toml` spelling: `"local"`, `"remote"` or `"remote_only"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::RemoteOnly => "remote_only",
        }
    }
}

impl fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Capabilities declared by a module.
#[derive(Debug, Clone, Default)]
pub struct ModuleCapabilities {
    /// Tool names the module exposes.
    pub tools: Vec<String>,
    /// Read only by the module-agent listing PL-U5 removes; a
    /// `chatty:plugin@0.3.0` plugin is never an agent.
    pub agent: bool,
}

/// Protocol flags declared by a module.
#[derive(Debug, Clone, Default)]
pub struct ModuleProtocols {
    /// Serve the plugin's tools to external MCP clients at `/mcp/{name}`.
    pub mcp: bool,
    /// Read only by the module-agent listing PL-U5 removes and by remote
    /// forwarding (PL-H8b); local plugins have no A2A route.
    pub a2a: bool,
}

/// Resource limits declared by a module, already clamped to the host
/// ceilings.
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
    /// Module name, e.g. `"echo"`.
    pub name: String,
    /// Semver-compatible version string, e.g. `"0.1.0"`.
    pub version: String,
    /// Human-readable description.
    pub description: String,
    /// Path to the `.wasm` file, resolved relative to the manifest directory.
    /// `None` for remote-execution modules which have no local WASM binary.
    pub wasm_path: Option<PathBuf>,
    /// Execution location.
    pub execution_mode: ExecutionMode,
    /// Capability declarations.
    pub capabilities: ModuleCapabilities,
    /// Protocol declarations.
    pub protocols: ModuleProtocols,
    /// Resource limit declarations, clamped to the host ceilings.
    pub resources: ModuleResourceLimits,
    /// `[config]`: the values the guest reads through `config::get`.
    pub config: BTreeMap<String, String>,
    /// `[files].root`, resolved relative to the manifest directory: the only
    /// directory the guest may read through `file::read-bytes`. `None`
    /// grants no files.
    pub files_root: Option<PathBuf>,
    /// Non-fatal findings, e.g. a `[resources]` value clamped to a ceiling.
    pub warnings: Vec<String>,
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
        let module_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));

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

        let execution_mode = raw.module.execution_mode;

        // -- [module].wasm: a plain relative path inside the module dir,
        // required for local execution only --
        let wasm_rel = match raw.module.wasm.as_deref().map(str::trim) {
            Some(wasm) if !wasm.is_empty() => {
                Some(plain_relative_path("[module].wasm", wasm, manifest_path)?)
            }
            _ => None,
        };
        let wasm_path = if execution_mode.is_remote() {
            None
        } else {
            let wasm_rel = wasm_rel.with_context(|| {
                format!(
                    "manifest {}: [module].wasm must not be empty for local execution",
                    manifest_path.display()
                )
            })?;
            Some(module_dir.join(wasm_rel))
        };

        // -- [files].root: a plain relative path inside the module dir --
        let files_root = match raw.files {
            Some(files) => Some(module_dir.join(plain_relative_path(
                "[files].root",
                files.root.trim(),
                manifest_path,
            )?)),
            None => None,
        };

        // -- [resources]: clamped to the host ceilings, with a warning --
        let mut warnings = Vec::new();
        let max_memory_mb = clamp_with_warning(
            "[resources].max_memory_mb",
            raw.resources.max_memory_mb,
            MAX_MEMORY_BYTES_CEILING / (1024 * 1024),
            &mut warnings,
        );
        let max_execution_ms = clamp_with_warning(
            "[resources].max_execution_ms",
            raw.resources.max_execution_ms,
            MAX_EXECUTION_MS_CEILING,
            &mut warnings,
        );

        Ok(Self {
            name,
            version,
            description: raw.module.description,
            wasm_path,
            execution_mode,
            capabilities: ModuleCapabilities {
                tools: raw.capabilities.tools,
                agent: raw.capabilities.agent,
            },
            protocols: ModuleProtocols {
                mcp: raw.protocols.mcp,
                a2a: raw.protocols.a2a,
            },
            resources: ModuleResourceLimits {
                max_memory_mb,
                max_execution_ms,
            },
            config: raw.config,
            files_root,
            warnings,
        })
    }
}

/// `value` as a relative path that stays inside the module directory
/// lexically: `/`-separated, no root or drive, no `..`, no `\` or `:`.
fn plain_relative_path(field: &str, value: &str, manifest_path: &Path) -> Result<PathBuf> {
    let reject = |why: &str| {
        anyhow::anyhow!(
            "manifest {}: {field} = {value:?} must be a plain relative path inside the \
             module directory ({why})",
            manifest_path.display()
        )
    };
    if value.contains('\\') || value.contains(':') {
        return Err(reject("use `/` as the separator; no drive letters"));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err(reject("`..` is not allowed")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(reject("absolute paths are not allowed"));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(reject("it names the module directory itself"));
    }
    Ok(relative)
}

/// `value` lowered to `ceiling` when above it, recording a warning.
fn clamp_with_warning(field: &str, value: u64, ceiling: u64, warnings: &mut Vec<String>) -> u64 {
    if value > ceiling {
        warnings.push(format!(
            "{field} = {value} is above the host ceiling; clamped to {ceiling}"
        ));
        ceiling
    } else {
        value
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
name = "echo"
version = "0.2.0"
description = "A simple echo plugin for testing"
wasm = "echo.wasm"

[capabilities]
tools = ["echo", "reverse"]

[protocols]
mcp = true

[resources]
max_memory_mb = 64
max_execution_ms = 30000
"#;

    #[test]
    fn full_manifest_parses() {
        let m = parse(FULL_TOML).expect("should parse");
        assert_eq!(m.name, "echo");
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.description, "A simple echo plugin for testing");
        assert_eq!(m.wasm_path, Some(PathBuf::from("/fake/echo.wasm")));
        assert_eq!(m.capabilities.tools, vec!["echo", "reverse"]);
        assert!(!m.capabilities.agent);
        assert!(m.protocols.mcp);
        assert!(!m.protocols.a2a);
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
        assert!(!m.capabilities.agent);
        assert!(!m.protocols.mcp);
        assert!(!m.protocols.a2a);
        assert_eq!(m.resources.max_memory_mb, 0);
        assert_eq!(m.resources.max_execution_ms, 0);
        assert_eq!(m.execution_mode, ExecutionMode::Local);
        assert!(m.config.is_empty());
        assert!(m.files_root.is_none());
        assert!(m.warnings.is_empty());
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
        assert_eq!(m.execution_mode, ExecutionMode::Remote);
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

    const MINIMAL: &str = "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"x.wasm\"\n";

    #[test]
    fn config_and_files_root_parse() {
        let m = ModuleManifest::from_str(
            &format!(
                "{MINIMAL}\n[config]\ngreeting = \"hi\"\nmodel = \"small\"\n\n[files]\nroot = \"weights\"\n"
            ),
            Path::new("/some/dir/module.toml"),
        )
        .unwrap();
        assert_eq!(m.config.get("greeting").map(String::as_str), Some("hi"));
        assert_eq!(m.config.get("model").map(String::as_str), Some("small"));
        assert_eq!(m.files_root, Some(PathBuf::from("/some/dir/weights")));
    }

    #[test]
    fn config_values_must_be_strings() {
        assert!(parse(&format!("{MINIMAL}\n[config]\nthreshold = 0.8\n")).is_err());
    }

    #[test]
    fn unknown_keys_are_rejected_in_every_section() {
        for extra in [
            "\n[weird]\nsurprise = true\n",
            "\n[capabilities]\ntool = [\"echo\"]\n",
            "\n[protocols]\nhttp = true\n",
            "\n[resources]\nmax_fuel = 5\n",
            "\n[files]\nroot = \"w\"\nwritable = true\n",
            "pricing_model = \"paid\"\n",
        ] {
            let toml = format!("{MINIMAL}{extra}");
            let err = parse(&toml).expect_err(&toml);
            assert!(format!("{err:#}").contains("unknown field"), "{err:#}");
        }
    }

    /// `chat` and `openai_compat` described the 0.2.0 agent world, which
    /// PL-U3 removed without a compatibility window: naming them is an error
    /// that names the field.
    #[test]
    fn removed_agent_world_keys_are_refused() {
        for (extra, field) in [
            ("\n[capabilities]\nchat = true\n", "chat"),
            ("\n[protocols]\nopenai_compat = true\n", "openai_compat"),
        ] {
            let toml = format!("{MINIMAL}{extra}");
            let err = format!("{:#}", parse(&toml).expect_err(&toml));
            assert!(err.contains(&format!("unknown field `{field}`")), "{err}");
        }
    }

    #[test]
    fn execution_mode_is_an_enum() {
        for (text, mode) in [
            ("local", ExecutionMode::Local),
            ("remote", ExecutionMode::Remote),
            ("remote_only", ExecutionMode::RemoteOnly),
        ] {
            let m = parse(&format!("{MINIMAL}execution_mode = \"{text}\"\n")).unwrap();
            assert_eq!(m.execution_mode, mode);
            assert_eq!(m.execution_mode.as_str(), text);
        }
        for typo in ["remtoe", "Remote", "LOCAL", ""] {
            assert!(parse(&format!("{MINIMAL}execution_mode = \"{typo}\"\n")).is_err());
        }
    }

    #[test]
    fn paths_must_stay_inside_the_module_dir() {
        for bad in [
            "../other.wasm",
            "sub/../../x.wasm",
            "/etc/passwd",
            "C:/x.wasm",
            "sub\\x.wasm",
            ".",
        ] {
            let wasm = format!("[module]\nname = \"x\"\nversion = \"1\"\nwasm = {bad:?}\n");
            assert!(parse(&wasm).is_err(), "wasm = {bad:?} must be rejected");
            let files = format!("{MINIMAL}\n[files]\nroot = {bad:?}\n");
            assert!(
                parse(&files).is_err(),
                "[files].root = {bad:?} must be rejected"
            );
        }
        let m =
            parse("[module]\nname = \"x\"\nversion = \"1\"\nwasm = \"./sub/x.wasm\"\n").unwrap();
        assert_eq!(m.wasm_path, Some(PathBuf::from("/fake/sub/x.wasm")));
    }

    #[test]
    fn remote_modules_still_validate_a_declared_wasm_path() {
        let toml = "[module]\nname = \"x\"\nversion = \"1\"\nexecution_mode = \"remote\"\n\
                    wasm = \"/etc/passwd\"\n";
        assert!(parse(toml).is_err());
    }

    #[test]
    fn resources_above_the_ceilings_are_clamped_with_a_warning() {
        let m = parse(&format!(
            "{MINIMAL}\n[resources]\nmax_memory_mb = 9007199254740992\nmax_execution_ms = 600000\n"
        ))
        .unwrap();
        assert_eq!(m.resources.max_memory_mb, 256);
        assert_eq!(m.resources.max_execution_ms, 60_000);
        assert_eq!(m.warnings.len(), 2, "{:?}", m.warnings);
        assert!(m.warnings[0].contains("max_memory_mb"), "{:?}", m.warnings);
        assert!(
            m.warnings[1].contains("max_execution_ms"),
            "{:?}",
            m.warnings
        );

        let at = parse(&format!(
            "{MINIMAL}\n[resources]\nmax_memory_mb = 256\nmax_execution_ms = 60000\n"
        ))
        .unwrap();
        assert!(at.warnings.is_empty(), "{:?}", at.warnings);
    }
}
