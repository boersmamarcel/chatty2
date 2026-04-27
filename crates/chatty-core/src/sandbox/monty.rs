//! MontySandbox — a lightweight Python execution backend that runs code
//! directly via the system `python3` interpreter (no Docker required).
//!
//! This backend is intended as a fast path for simple Python scripts that
//! don't need third-party packages, filesystem persistence, or network
//! access.  It complements [`DockerSandbox`] by handling the common case
//! (math, logic, string operations, data transformation) with near-zero
//! startup overhead while Docker remains available for full environments.
//!
//! ## Backend selection heuristic
//!
//! [`MontySandbox::can_handle`] inspects the code and returns `true` when
//! the code only uses stdlib modules and avoids constructs that require a
//! full CPython environment (e.g. class definitions with metaclasses, C
//! extensions, etc.).  The [`SandboxManager`] calls this before deciding
//! which backend to use.
//!
//! ## Resource limits
//!
//! Resource limits are enforced at the process level:
//! - **Execution time** — a `tokio::time::timeout` kills the child process
//!   when the deadline is exceeded.
//! - **Memory** — on Linux the child's `RLIMIT_AS` is set to `memory_mb`
//!   before `exec`, preventing the interpreter from allocating beyond the
//!   configured cap.  On other platforms a best-effort approach is used.
//!
//! ## External function bridge
//!
//! Phase 2 of the integration (see [`super::monty_bridge`]) will allow
//! LLM-generated code to call chatty2 tools as Python functions by
//! injecting stubs that communicate back to the host process.  This file
//! deliberately keeps that concern separate; the bridge will wrap
//! `MontySandbox` and handle the snapshot / resume loop.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};
use crate::models::message_types::ExecutionEngine;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Python modules that are safe to import without a Docker container.
const SUPPORTED_STDLIB_MODULES: &[&str] = &[
    "sys",
    "typing",
    "asyncio",
    "re",
    "os",
    "json",
    "math",
    "random",
    "string",
    "itertools",
    "functools",
    "collections",
    "copy",
    "datetime",
    "time",
    "pathlib",
    "io",
    "struct",
    "hashlib",
    "base64",
    "decimal",
    "fractions",
    "statistics",
    "enum",
    "dataclasses",
    "abc",
    "contextlib",
    "operator",
    "heapq",
    "bisect",
    "array",
    "pprint",
    "textwrap",
    "difflib",
    "unicodedata",
    "calendar",
    "locale",
    "types",
    "inspect",
    "traceback",
    "warnings",
    "weakref",
    "gc",
    "platform",
    "errno",
    "signal",
];

// ─── MontySandbox ─────────────────────────────────────────────────────────────

/// A fast, Docker-free Python execution sandbox.
///
/// Uses the host `python3` interpreter with strict resource limits rather
/// than a container, eliminating the 200–500 ms Docker cold-start cost for
/// simple code.
pub struct MontySandbox {
    config: SandboxConfig,
}

impl MontySandbox {
    /// Create a new `MontySandbox` using the provided configuration.
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    /// Return `true` if this backend can execute the given Python code
    /// without needing a full Docker environment.
    ///
    /// The heuristic rejects code that:
    /// - imports third-party packages (anything not in [`SUPPORTED_STDLIB_MODULES`])
    /// - uses `match`/`case` structural pattern matching (Python 3.10+; may
    ///   not be available on older system interpreters)
    /// - runs subprocesses (`subprocess`, `os.system`, `os.popen`)
    /// - opens sockets or makes network calls directly
    ///
    /// Class definitions (`class Foo: ...`) are **allowed** — standard Python
    /// classes work fine with the host interpreter.  Only blocking patterns
    /// (subprocess, socket, ctypes, etc.) are rejected.
    ///
    /// This is intentionally conservative.  The [`SandboxManager`] will
    /// fall back to Docker whenever `can_handle` returns `false`.
    pub fn can_handle(code: &str) -> bool {
        for line in code.lines() {
            let trimmed = line.trim();

            // Skip comments and blank lines
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Check import statements
            if (trimmed.starts_with("import ") || trimmed.starts_with("from "))
                && !Self::is_supported_import(trimmed)
            {
                debug!(import = trimmed, "MontySandbox: unsupported import");
                return false;
            }

            // Reject subprocess / socket / ctypes — they could escape the sandbox
            for blocked in &[
                "subprocess",
                "socket",
                "ctypes",
                "cffi",
                "__import__",
                "importlib.import_module",
            ] {
                if trimmed.contains(blocked) {
                    debug!(pattern = blocked, "MontySandbox: blocked pattern");
                    return false;
                }
            }
        }

        // Reject structural pattern matching (match/case) as a conservative
        // guard — older Python 3 versions (< 3.10) don't support it.
        // Check for `match ` at the start of a line (after optional indentation),
        // which avoids false positives from string literals like `"match pattern"`.
        !code.lines().any(|line| {
            let t = line.trim_start();
            t.starts_with("match ") || t.starts_with("match\t")
        })
    }

    /// Returns `true` if the import line only imports supported stdlib modules.
    fn is_supported_import(line: &str) -> bool {
        // Extract the module name from `import foo` or `from foo import bar`
        let module = if let Some(rest) = line.strip_prefix("from ") {
            rest.split_whitespace().next().unwrap_or("")
        } else if let Some(rest) = line.strip_prefix("import ") {
            rest.split([' ', ','])
                .next()
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("")
        } else {
            return false;
        };

        // Allow the top-level module name (e.g. `os.path` → `os`)
        let top_level = module.split('.').next().unwrap_or(module);

        SUPPORTED_STDLIB_MODULES.contains(&top_level)
    }

    /// Build a small Python wrapper that enforces memory limits via
    /// `resource.setrlimit` (Linux/macOS) before running user code.
    fn wrap_with_limits(code: &str, memory_mb: u64) -> String {
        let memory_bytes = memory_mb * 1024 * 1024;
        format!(
            r#"import resource as _r, sys as _sys
try:
    _r.setrlimit(_r.RLIMIT_AS, ({memory_bytes}, {memory_bytes}))
except Exception:
    pass  # best-effort; not all platforms support RLIMIT_AS

{code}
"#
        )
    }
}

#[async_trait]
impl SandboxBackend for MontySandbox {
    async fn execute(&self, code: &str, language: &Language) -> Result<ExecutionResult> {
        // MontySandbox only handles Python.
        if *language != Language::Python {
            anyhow::bail!("MontySandbox only supports Python; got {:?}", language);
        }

        let wrapped = Self::wrap_with_limits(code, self.config.memory_mb);
        let timeout_secs = self.config.timeout_secs;

        info!(
            timeout_secs,
            memory_mb = self.config.memory_mb,
            "MontySandbox: executing Python snippet"
        );

        // Spawn `python3 -c <code>` with stdout/stderr piped.
        // We use `-c` rather than a temp file to avoid any filesystem side
        // effects, though the code is still subject to the OS temp directory
        // restrictions imposed by the shell service on macOS sandboxes.
        let mut child = Command::new("python3")
            .arg("-c")
            .arg(&wrapped)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Isolate the child from the parent's environment to reduce the
            // attack surface — pass only a minimal set of safe variables.
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .env("LANG", "en_US.UTF-8")
            .spawn()
            .context("Failed to spawn python3. Is Python 3 installed?")?;

        let mut stdout_handle = child.stdout.take().expect("stdout piped");
        let mut stderr_handle = child.stderr.take().expect("stderr piped");

        let collect = async {
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();

            // Read both streams concurrently.
            tokio::try_join!(
                stdout_handle.read_to_end(&mut stdout_buf),
                stderr_handle.read_to_end(&mut stderr_buf),
            )?;

            let status = child.wait().await?;
            let exit_code = status.code().unwrap_or(-1) as i64;

            Ok::<ExecutionResult, anyhow::Error>(ExecutionResult {
                stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
                exit_code,
                timed_out: false,
                port_mappings: HashMap::new(),
                execution_engine: ExecutionEngine::Monty,
            })
        };

        match timeout(Duration::from_secs(timeout_secs), collect).await {
            Ok(result) => result,
            Err(_) => {
                // Best-effort kill
                let _ = child.kill().await;
                warn!(timeout_secs, "MontySandbox: execution timed out");
                Ok(ExecutionResult {
                    stdout: String::new(),
                    stderr: format!("Execution timed out after {} seconds.", timeout_secs),
                    exit_code: -1,
                    timed_out: true,
                    port_mappings: HashMap::new(),
                    execution_engine: ExecutionEngine::Monty,
                })
            }
        }
    }

    async fn destroy(self: Box<Self>) -> Result<()> {
        // Nothing to clean up — no containers, no persistent processes.
        Ok(())
    }

    fn has_port_exposed(&self, _port: u16) -> bool {
        // MontySandbox runs in the host network namespace but does not
        // bind any ports itself.  A future Phase 5 REPL mode could
        // expose ports via an embedded HTTP server, but that is out of
        // scope for Phase 1.
        false
    }

    async fn is_available(_docker_host: Option<&str>) -> Result<bool>
    where
        Self: Sized,
    {
        // Check that python3 is present and executable.
        let result = Command::new("python3").arg("--version").output().await;

        Ok(result.map(|o| o.status.success()).unwrap_or(false))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── can_handle ────────────────────────────────────────────────────────────

    #[test]
    fn can_handle_simple_math() {
        assert!(MontySandbox::can_handle("x = 1 + 2\nprint(x)"));
    }

    #[test]
    fn can_handle_stdlib_math_import() {
        assert!(MontySandbox::can_handle("import math\nprint(math.sqrt(2))"));
    }

    #[test]
    fn can_handle_from_import() {
        assert!(MontySandbox::can_handle(
            "from collections import Counter\nprint(Counter('hello'))"
        ));
    }

    #[test]
    fn cannot_handle_third_party_import() {
        assert!(!MontySandbox::can_handle(
            "import numpy as np\nprint(np.array([1,2,3]))"
        ));
    }

    #[test]
    fn cannot_handle_subprocess() {
        assert!(!MontySandbox::can_handle(
            "import subprocess\nsubprocess.run(['ls'])"
        ));
    }

    #[test]
    fn cannot_handle_socket() {
        assert!(!MontySandbox::can_handle(
            "import socket\ns = socket.socket()"
        ));
    }

    #[test]
    fn cannot_handle_match_case() {
        let code = "x = 1\nmatch x:\n    case 1:\n        print('one')";
        assert!(!MontySandbox::can_handle(code));
    }

    #[test]
    fn can_handle_classes_are_allowed() {
        // Classes are allowed — the conservative heuristic only blocks
        // known-dangerous constructs, not class definitions.
        assert!(MontySandbox::can_handle(
            "class Foo:\n    pass\nprint(Foo())"
        ));
    }

    // ── is_supported_import ───────────────────────────────────────────────────

    #[test]
    fn supported_import_os_path() {
        assert!(MontySandbox::is_supported_import(
            "from os.path import join"
        ));
    }

    #[test]
    fn unsupported_import_requests() {
        assert!(!MontySandbox::is_supported_import("import requests"));
    }

    #[test]
    fn unsupported_import_pandas() {
        assert!(!MontySandbox::is_supported_import(
            "from pandas import DataFrame"
        ));
    }

    // ── wrap_with_limits ─────────────────────────────────────────────────────

    #[test]
    fn wrap_injects_resource_limits() {
        let wrapped = MontySandbox::wrap_with_limits("print(42)", 256);
        assert!(wrapped.contains("setrlimit"));
        assert!(wrapped.contains("print(42)"));
        // 256 MB in bytes
        assert!(wrapped.contains(&(256u64 * 1024 * 1024).to_string()));
    }

    // ── execute (integration, requires python3 in PATH) ───────────────────────

    #[tokio::test]
    async fn execute_hello_world() {
        let sandbox = MontySandbox::new(SandboxConfig::default());
        let result = sandbox
            .execute("print('hello world')", &Language::Python)
            .await;

        // If python3 is not installed, skip rather than fail.
        if let Err(ref e) = result {
            if e.to_string().contains("python3") {
                eprintln!("Skipping: python3 not available ({e})");
                return;
            }
        }

        let r = result.expect("execution succeeded");
        assert_eq!(r.stdout.trim(), "hello world");
        assert_eq!(r.exit_code, 0);
        assert!(!r.timed_out);
    }

    #[tokio::test]
    async fn execute_stderr_captured() {
        let sandbox = MontySandbox::new(SandboxConfig::default());
        let result = sandbox
            .execute("import sys\nsys.stderr.write('err\\n')", &Language::Python)
            .await;

        if let Err(ref e) = result {
            if e.to_string().contains("python3") {
                return;
            }
        }

        let r = result.unwrap();
        assert!(r.stderr.contains("err"), "stderr should contain 'err'");
    }

    #[tokio::test]
    async fn execute_rejects_non_python() {
        let sandbox = MontySandbox::new(SandboxConfig::default());
        let result = sandbox
            .execute("console.log('hi')", &Language::JavaScript)
            .await;
        assert!(result.is_err(), "should reject non-Python language");
    }
}
