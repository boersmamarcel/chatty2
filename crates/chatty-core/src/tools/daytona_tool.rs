use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Base URL for the Daytona cloud API
const DAYTONA_API_BASE: &str = "https://app.daytona.io/api";

/// Default timeout for sandbox creation, polling, and deletion (seconds)
const DAYTONA_TIMEOUT_SECS: u64 = 30;

/// Longer timeout for code execution which may run for extended periods (seconds)
const DAYTONA_EXEC_TIMEOUT_SECS: u64 = 300;

/// Maximum polling attempts when waiting for sandbox to reach "started" state (~120s)
const DAYTONA_SANDBOX_POLL_ATTEMPTS: u64 = 60;

/// Polling interval in milliseconds while waiting for sandbox start
const DAYTONA_SANDBOX_POLL_INTERVAL_MS: u64 = 2000;

/// File extensions to auto-discover and download from sandbox
const DOWNLOADABLE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", // images
    "pdf", "csv", "html",                               // documents
];

/// Maximum number of files to download from a single sandbox run
const MAX_DOWNLOAD_FILES: usize = 5;

/// Maximum file size to download (5 MB)
const MAX_DOWNLOAD_SIZE: usize = 5 * 1024 * 1024;

// ── Tool Args / Output ──────────────────────────────────────────────────────

/// Arguments for the daytona_run tool
#[derive(Deserialize, Serialize)]
pub struct DaytonaToolArgs {
    /// The code to execute in the Daytona sandbox
    pub code: String,
    /// Programming language hint (e.g. "python", "javascript", "bash")
    #[serde(default)]
    pub language: Option<String>,
}

/// Output from the daytona_run tool
#[derive(Debug, Serialize)]
pub struct DaytonaToolOutput {
    /// The code that was executed
    pub code: String,
    /// Standard output from the code execution
    pub result: String,
    /// Exit code (0 = success)
    pub exit_code: i32,
    /// Whether the sandbox was cleaned up after use
    pub sandbox_cleaned_up: bool,
    /// Local file paths of downloaded artifacts (images, CSVs, etc.)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub downloaded_files: Vec<String>,
}

/// Error type for the daytona_run tool
#[derive(Debug, thiserror::Error)]
pub enum DaytonaToolError {
    #[error("Daytona error: {0}")]
    ApiError(String),
    #[error("Daytona authentication failed: {0}")]
    AuthenticationFailed(String),
    #[error("Daytona quota exceeded: {0}")]
    QuotaExceeded(String),
}

// ── Daytona API types ────────────────────────────────────────────────────────

/// Response from sandbox creation. Only the `id` is needed; other fields are
/// accepted but ignored via `deny_unknown_fields` being absent.
#[derive(Deserialize, Debug)]
struct SandboxCreateResponse {
    id: String,
}

/// Lightweight response used when polling sandbox state.
#[derive(Deserialize, Debug)]
struct SandboxStateResponse {
    state: String,
}

/// Request body for the toolbox `process/execute` endpoint.
#[derive(Serialize)]
struct ExecuteRequest {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
}

/// Response from the toolbox `process/execute` endpoint.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ExecuteResponse {
    result: String,
    exit_code: i32,
}

/// Daytona API error body (for structured error parsing).
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct DaytonaApiError {
    #[serde(default)]
    status_code: u16,
    #[serde(default)]
    message: String,
}

/// Entry from the toolbox files listing endpoint.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    name: String,
    #[serde(default)]
    is_dir: bool,
    #[serde(default)]
    size: u64,
}

/// Check whether a filename has a recognized downloadable extension.
fn has_downloadable_extension(name: &str) -> bool {
    let lower = name.to_lowercase();
    DOWNLOADABLE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

/// Common Python standard library modules that should NOT be pip-installed.
const PYTHON_STDLIB: &[&str] = &[
    "abc", "argparse", "ast", "asyncio", "base64", "bisect", "calendar",
    "cmath", "collections", "colorsys", "concurrent", "configparser",
    "contextlib", "copy", "csv", "ctypes", "dataclasses", "datetime",
    "decimal", "difflib", "email", "enum", "errno", "fcntl", "fileinput",
    "fnmatch", "fractions", "ftplib", "functools", "gc", "getpass", "glob",
    "gzip", "hashlib", "heapq", "hmac", "html", "http", "imaplib", "importlib",
    "inspect", "io", "ipaddress", "itertools", "json", "keyword", "linecache",
    "locale", "logging", "lzma", "math", "mimetypes", "multiprocessing",
    "numbers", "operator", "os", "pathlib", "pickle", "platform", "plistlib",
    "pprint", "pdb", "queue", "random", "re", "readline", "reprlib",
    "secrets", "select", "shelve", "shlex", "shutil", "signal", "site",
    "smtplib", "socket", "sqlite3", "ssl", "stat", "statistics", "string",
    "struct", "subprocess", "sys", "syslog", "tempfile", "textwrap",
    "threading", "time", "timeit", "tkinter", "token", "tokenize", "tomllib",
    "traceback", "tty", "turtle", "types", "typing", "unicodedata", "unittest",
    "urllib", "uuid", "venv", "warnings", "wave", "weakref", "webbrowser",
    "xml", "xmlrpc", "zipfile", "zipimport", "zlib",
    // Also exclude _ prefixed and __future__
    "__future__", "_thread",
];

/// Map import names to pip package names for common mismatches.
fn pip_package_name(import_name: &str) -> &str {
    match import_name {
        "cv2" => "opencv-python",
        "PIL" => "Pillow",
        "sklearn" => "scikit-learn",
        "bs4" => "beautifulsoup4",
        "yaml" => "pyyaml",
        "attr" => "attrs",
        "dateutil" => "python-dateutil",
        "dotenv" => "python-dotenv",
        "gi" => "PyGObject",
        "lxml" => "lxml",
        "wx" => "wxPython",
        _ => import_name,
    }
}

/// Extract top-level Python import names from source code.
fn extract_python_imports(code: &str) -> Vec<String> {
    let mut imports = std::collections::HashSet::new();
    for line in code.lines() {
        let trimmed = line.trim();
        // `import foo`, `import foo.bar`, `import foo as f`
        if let Some(rest) = trimmed.strip_prefix("import ") {
            for part in rest.split(',') {
                let module = part.split_whitespace().next().unwrap_or("");
                let top = module.split('.').next().unwrap_or("");
                if !top.is_empty() {
                    imports.insert(top.to_string());
                }
            }
        }
        // `from foo import bar`, `from foo.bar import baz`
        if let Some(rest) = trimmed.strip_prefix("from ") {
            let module = rest.split_whitespace().next().unwrap_or("");
            let top = module.split('.').next().unwrap_or("");
            if !top.is_empty() {
                imports.insert(top.to_string());
            }
        }
    }

    imports
        .into_iter()
        .filter(|name| !PYTHON_STDLIB.contains(&name.as_str()))
        .collect()
}

// ── Tool implementation ──────────────────────────────────────────────────────

/// Code execution tool powered by the Daytona cloud sandbox service.
///
/// Creates an isolated Daytona sandbox, runs the provided code, returns the
/// output, and cleans up the sandbox afterwards. Generated files (images, etc.)
/// are automatically downloaded to the local workspace directory.
#[derive(Clone)]
pub struct DaytonaTool {
    /// HTTP client for short operations (create, poll, delete)
    client: reqwest::Client,
    /// HTTP client with longer timeout for code execution
    exec_client: reqwest::Client,
    api_key: String,
    api_base: String,
    /// Local directory where downloaded sandbox files are saved
    workspace_dir: Option<String>,
}

impl DaytonaTool {
    /// Create a new DaytonaTool with the given API key.
    pub fn new(api_key: String, workspace_dir: Option<String>) -> Self {
        Self::new_with_base(api_key, DAYTONA_API_BASE.to_string(), workspace_dir)
    }

    /// Create a DaytonaTool with a custom API base URL (useful for self-hosted Daytona).
    pub fn new_with_base(api_key: String, api_base: String, workspace_dir: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DAYTONA_TIMEOUT_SECS))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .build()
            .expect("Failed to build HTTP client");
        let exec_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DAYTONA_EXEC_TIMEOUT_SECS))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .build()
            .expect("Failed to build HTTP exec client");
        Self {
            client,
            exec_client,
            api_key,
            api_base,
            workspace_dir,
        }
    }

    /// Parse a Daytona API error response into a typed error.
    fn parse_api_error(&self, status: reqwest::StatusCode, body: &str) -> DaytonaToolError {
        if let Ok(api_err) = serde_json::from_str::<DaytonaApiError>(body) {
            if status.as_u16() == 401 || api_err.status_code == 401 {
                return DaytonaToolError::AuthenticationFailed(format!(
                    "Check your Daytona API key — {}",
                    api_err.message
                ));
            }
            if (status.as_u16() == 403 || api_err.status_code == 403)
                && api_err.message.to_lowercase().contains("quota")
            {
                return DaytonaToolError::QuotaExceeded(api_err.message);
            }
            return DaytonaToolError::ApiError(format!(
                "Daytona API {} ({}): {}",
                status,
                api_err.status_code,
                api_err.message
            ));
        }

        if status.as_u16() == 401 {
            return DaytonaToolError::AuthenticationFailed(
                "Check your Daytona API key".to_string(),
            );
        }

        DaytonaToolError::ApiError(format!("Daytona API returned {}: {}", status, body))
    }

    /// Create a new Daytona sandbox and return its ID.
    async fn create_sandbox(&self) -> Result<String, DaytonaToolError> {
        let response = self
            .client
            .post(format!("{}/sandbox", self.api_base))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| {
                DaytonaToolError::ApiError(format!("Failed to create Daytona sandbox: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(self.parse_api_error(status, &body));
        }

        let sandbox: SandboxCreateResponse = response.json().await.map_err(|e| {
            DaytonaToolError::ApiError(format!("Failed to parse sandbox response: {}", e))
        })?;

        Ok(sandbox.id)
    }

    /// Poll until the sandbox reaches the "started" state (up to ~120 seconds).
    async fn wait_for_started(&self, sandbox_id: &str) -> Result<(), DaytonaToolError> {
        for attempt in 0..DAYTONA_SANDBOX_POLL_ATTEMPTS {
            let response = self
                .client
                .get(format!("{}/sandbox/{}", self.api_base, sandbox_id))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .send()
                .await
                .map_err(|e| {
                    DaytonaToolError::ApiError(format!("Failed to poll sandbox state: {}", e))
                })?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "(failed to read body)".to_string());
                return Err(self.parse_api_error(status, &body));
            }

            let state_resp: SandboxStateResponse = response.json().await.map_err(|e| {
                DaytonaToolError::ApiError(format!("Failed to parse sandbox state: {}", e))
            })?;

            info!(
                sandbox_id,
                attempt,
                state = %state_resp.state,
                "Waiting for Daytona sandbox to start"
            );

            match state_resp.state.as_str() {
                "started" => return Ok(()),
                "error" | "build_failed" => {
                    return Err(DaytonaToolError::ApiError(format!(
                        "Daytona sandbox {} entered error state: {}",
                        sandbox_id, state_resp.state
                    )));
                }
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        DAYTONA_SANDBOX_POLL_INTERVAL_MS,
                    ))
                    .await;
                }
            }
        }

        let timeout_secs = DAYTONA_SANDBOX_POLL_ATTEMPTS * DAYTONA_SANDBOX_POLL_INTERVAL_MS / 1000;
        Err(DaytonaToolError::ApiError(format!(
            "Daytona sandbox {} did not reach 'started' state within ~{}s",
            sandbox_id, timeout_secs
        )))
    }

    /// Upload a code file to the sandbox via the toolbox bulk-upload endpoint.
    ///
    /// URL: `{api_base}/toolbox/{sandbox_id}/toolbox/files/bulk-upload`
    async fn upload_code_file(
        &self,
        sandbox_id: &str,
        remote_path: &str,
        code: &str,
    ) -> Result<(), DaytonaToolError> {
        let url = format!(
            "{}/toolbox/{}/toolbox/files/bulk-upload",
            self.api_base, sandbox_id
        );

        let part = reqwest::multipart::Part::bytes(code.as_bytes().to_vec())
            .file_name(remote_path.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| DaytonaToolError::ApiError(format!("Failed to build multipart: {}", e)))?;

        let form = reqwest::multipart::Form::new()
            .text("files[0].path", remote_path.to_string())
            .part("files[0].file", part);

        info!(url = %url, path = remote_path, "Uploading code file to sandbox");

        let response = self
            .exec_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                DaytonaToolError::ApiError(format!("Failed to upload code to sandbox: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(self.parse_api_error(status, &body));
        }

        Ok(())
    }

    /// Execute a shell command in the sandbox via the toolbox process/execute endpoint.
    ///
    /// URL: `{api_base}/toolbox/{sandbox_id}/toolbox/process/execute`
    async fn execute_command(
        &self,
        sandbox_id: &str,
        command: &str,
    ) -> Result<ExecuteResponse, DaytonaToolError> {
        let request = ExecuteRequest {
            command: command.to_string(),
            timeout: Some(DAYTONA_EXEC_TIMEOUT_SECS),
        };

        let url = format!(
            "{}/toolbox/{}/toolbox/process/execute",
            self.api_base, sandbox_id
        );

        info!(url = %url, command = %command, "Executing command in sandbox");

        let response = self
            .exec_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                DaytonaToolError::ApiError(format!(
                    "Failed to execute command in sandbox: {}",
                    e
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(self.parse_api_error(status, &body));
        }

        // Parse as raw JSON first for debugging, then extract typed response
        let raw_text = response.text().await.map_err(|e| {
            DaytonaToolError::ApiError(format!("Failed to read execute response: {}", e))
        })?;

        debug!(raw_response = %raw_text, "Daytona execute raw response");

        let exec_response: ExecuteResponse =
            serde_json::from_str(&raw_text).map_err(|e| {
                DaytonaToolError::ApiError(format!(
                    "Failed to parse execute response: {} — raw: {}",
                    e, raw_text
                ))
            })?;

        Ok(exec_response)
    }

    /// Execute code in the sandbox by uploading it as a file and running it.
    ///
    /// For Python code, automatically detects imports and installs missing
    /// packages via pip before execution.
    async fn execute_code(
        &self,
        sandbox_id: &str,
        code: &str,
        language: &str,
    ) -> Result<ExecuteResponse, DaytonaToolError> {
        let file_id = uuid::Uuid::new_v4();
        let (ext, interpreter) = match language {
            "python" | "python3" => ("py", "python3"),
            "javascript" | "js" | "node" => ("js", "node"),
            "bash" | "sh" | "shell" => ("sh", "bash"),
            "typescript" | "ts" => ("ts", "ts-node"),
            "ruby" | "rb" => ("rb", "ruby"),
            _ => ("py", "python3"), // Default to Python
        };

        // Step 1: Auto-install Python dependencies if needed
        if ext == "py" {
            let imports = extract_python_imports(code);
            if !imports.is_empty() {
                let packages: Vec<&str> = imports.iter().map(|i| pip_package_name(i)).collect();
                let pip_cmd = format!("pip install --quiet {}", packages.join(" "));
                info!(sandbox_id, packages = ?packages, "Auto-installing Python dependencies");
                match self.execute_command(sandbox_id, &pip_cmd).await {
                    Ok(resp) if resp.exit_code != 0 => {
                        warn!(
                            sandbox_id,
                            exit_code = resp.exit_code,
                            result = %resp.result,
                            "pip install had non-zero exit (continuing anyway)"
                        );
                    }
                    Err(e) => {
                        warn!(sandbox_id, error = %e, "pip install failed (continuing anyway)");
                    }
                    Ok(_) => {
                        info!(sandbox_id, "Python dependencies installed");
                    }
                }
            }
        }

        let remote_path = format!("/tmp/chatty_code_{}.{}", file_id, ext);
        let command = format!("{} {}", interpreter, remote_path);

        // Step 2: Upload the code file
        self.upload_code_file(sandbox_id, &remote_path, code)
            .await?;

        // Step 3: Execute the file
        self.execute_command(sandbox_id, &command).await
    }

    /// Delete a Daytona sandbox to free resources.
    async fn delete_sandbox(&self, sandbox_id: &str) -> bool {
        let result = self
            .client
            .delete(format!("{}/sandbox/{}", self.api_base, sandbox_id))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                info!(sandbox_id, "Daytona sandbox deleted");
                true
            }
            Ok(resp) => {
                warn!(
                    sandbox_id,
                    status = %resp.status(),
                    "Failed to delete Daytona sandbox"
                );
                false
            }
            Err(e) => {
                warn!(sandbox_id, error = %e, "Error deleting Daytona sandbox");
                false
            }
        }
    }

    /// Discover downloadable files generated by code execution in the sandbox.
    ///
    /// Uses the Daytona toolbox filesystem REST API to list files in common
    /// output directories and filter by recognized extensions.
    async fn discover_files(&self, sandbox_id: &str) -> Vec<String> {
        let mut found = Vec::new();

        for dir in &["/tmp", "/root", "/home"] {
            if found.len() >= MAX_DOWNLOAD_FILES {
                break;
            }
            self.list_downloadable_files(sandbox_id, dir, &mut found, 2)
                .await;
        }

        found.truncate(MAX_DOWNLOAD_FILES);
        found
    }

    /// Recursively list files in a sandbox directory via the toolbox files API,
    /// collecting paths with downloadable extensions.
    async fn list_downloadable_files(
        &self,
        sandbox_id: &str,
        dir: &str,
        out: &mut Vec<String>,
        depth: u8,
    ) {
        if depth == 0 || out.len() >= MAX_DOWNLOAD_FILES {
            return;
        }

        let url = format!(
            "{}/toolbox/{}/toolbox/files",
            self.api_base, sandbox_id
        );

        let response = match self
            .client
            .get(&url)
            .query(&[("path", dir)])
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp,
            Ok(resp) => {
                // Directory might not exist — that's fine
                if resp.status().as_u16() != 404 {
                    warn!(sandbox_id, dir, status = %resp.status(), "File listing failed");
                }
                return;
            }
            Err(e) => {
                warn!(sandbox_id, dir, error = %e, "File listing request failed");
                return;
            }
        };

        let raw_text = match response.text().await {
            Ok(text) => text,
            Err(e) => {
                warn!(sandbox_id, dir, error = %e, "Failed to read file listing response");
                return;
            }
        };

        debug!(sandbox_id, dir, raw_response = %raw_text, "File listing raw response");

        let entries: Vec<FileEntry> = match serde_json::from_str(&raw_text) {
            Ok(entries) => entries,
            Err(e) => {
                warn!(sandbox_id, dir, error = %e, raw = %raw_text, "Failed to parse file listing");
                return;
            }
        };

        info!(sandbox_id, dir, count = entries.len(), "Listed files in sandbox directory");

        for entry in entries {
            if out.len() >= MAX_DOWNLOAD_FILES {
                return;
            }

            let full_path = if dir.ends_with('/') {
                format!("{}{}", dir, entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };

            if entry.is_dir {
                // Recurse into subdirectories
                Box::pin(self.list_downloadable_files(sandbox_id, &full_path, out, depth - 1))
                    .await;
            } else if has_downloadable_extension(&entry.name) {
                if entry.size <= MAX_DOWNLOAD_SIZE as u64 {
                    out.push(full_path);
                } else {
                    warn!(
                        sandbox_id,
                        path = full_path,
                        size = entry.size,
                        "File too large to download"
                    );
                }
            }
        }
    }

    /// Download a single file from the sandbox via the toolbox files API.
    ///
    /// Returns the raw file bytes, or `None` on failure.
    async fn download_file_bytes(
        &self,
        sandbox_id: &str,
        remote_path: &str,
    ) -> Option<Vec<u8>> {
        let url = format!(
            "{}/toolbox/{}/toolbox/files/download",
            self.api_base, sandbox_id
        );

        let response = match self
            .exec_client
            .get(&url)
            .query(&[("path", remote_path)])
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp,
            Ok(resp) => {
                warn!(sandbox_id, path = remote_path, status = %resp.status(), "File download failed");
                return None;
            }
            Err(e) => {
                warn!(sandbox_id, path = remote_path, error = %e, "File download request failed");
                return None;
            }
        };

        match response.bytes().await {
            Ok(bytes) => {
                info!(sandbox_id, path = remote_path, size = bytes.len(), "Downloaded file from sandbox");
                Some(bytes.to_vec())
            }
            Err(e) => {
                warn!(sandbox_id, path = remote_path, error = %e, "Failed to read file bytes");
                None
            }
        }
    }

    /// Discover and download files generated by code execution.
    ///
    /// Saves files to the workspace directory (or a fallback location).
    /// Returns the list of local file paths.
    async fn download_generated_files(&self, sandbox_id: &str) -> Vec<String> {
        let remote_files = self.discover_files(sandbox_id).await;
        if remote_files.is_empty() {
            return Vec::new();
        }

        info!(
            sandbox_id,
            count = remote_files.len(),
            files = ?remote_files,
            "Found downloadable files in sandbox"
        );

        let output_dir = match &self.workspace_dir {
            Some(dir) => PathBuf::from(dir),
            None => match dirs::home_dir() {
                Some(home) => home,
                None => {
                    warn!("No workspace directory or home directory available for file download");
                    return Vec::new();
                }
            },
        };

        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            warn!(error = %e, dir = %output_dir.display(), "Failed to create output directory");
            return Vec::new();
        }

        let mut local_paths = Vec::new();

        for remote_path in &remote_files {
            let filename = std::path::Path::new(remote_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            if let Some(bytes) = self.download_file_bytes(sandbox_id, remote_path).await {
                let local_path = output_dir.join(filename);
                match std::fs::write(&local_path, &bytes) {
                    Ok(()) => {
                        info!(path = %local_path.display(), size = bytes.len(), "Saved sandbox file locally");
                        local_paths.push(local_path.to_string_lossy().to_string());
                    }
                    Err(e) => {
                        warn!(path = %local_path.display(), error = %e, "Failed to save sandbox file");
                    }
                }
            }
        }

        local_paths
    }
}

impl Tool for DaytonaTool {
    const NAME: &'static str = "daytona_run";
    type Error = DaytonaToolError;
    type Args = DaytonaToolArgs;
    type Output = DaytonaToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "daytona_run".to_string(),
            description: "Execute code in an isolated Daytona cloud sandbox. \
                         Creates a secure, ephemeral sandbox environment, runs your code, \
                         returns the output, and cleans up automatically. \
                         Generated files (images, PDFs, CSVs) saved to /tmp/ are automatically \
                         downloaded to the user's local working directory — do NOT call \
                         add_attachment or any file tool afterwards; the files are already local. \
                         Use this for running code snippets, scripts, or commands in a \
                         fully isolated cloud environment with internet access."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The code or script to execute in the sandbox"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language hint (e.g. 'python', 'javascript', 'bash'). Optional."
                    }
                },
                "required": ["code"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let code = args.code.trim().to_string();
        if code.is_empty() {
            return Err(DaytonaToolError::ApiError(
                "Code cannot be empty".to_string(),
            ));
        }

        let lang = args
            .language
            .as_deref()
            .unwrap_or("unknown")
            .to_string();

        info!(language = %lang, "Creating Daytona sandbox");
        let sandbox_id = self.create_sandbox().await?;
        info!(sandbox_id = %sandbox_id, language = %lang, "Daytona sandbox created, waiting for start");

        // Wait for the sandbox to reach "started" state before running code.
        if let Err(e) = self.wait_for_started(&sandbox_id).await {
            self.delete_sandbox(&sandbox_id).await;
            return Err(e);
        }

        info!(sandbox_id = %sandbox_id, language = %lang, "Daytona sandbox started, running code");

        // Execute code by uploading as a file and running it
        let (result_text, exit_code) = match self.execute_code(&sandbox_id, &code, &lang).await {
            Ok(resp) => {
                info!(
                    sandbox_id = %sandbox_id,
                    exit_code = resp.exit_code,
                    result_len = resp.result.len(),
                    "Code execution complete"
                );
                (resp.result, resp.exit_code)
            }
            Err(e) => {
                self.delete_sandbox(&sandbox_id).await;
                return Err(e);
            }
        };

        // Download any generated files before sandbox cleanup
        let downloaded_files = if exit_code == 0 {
            self.download_generated_files(&sandbox_id).await
        } else {
            Vec::new()
        };

        // Always attempt cleanup regardless of execution result
        let cleaned_up = self.delete_sandbox(&sandbox_id).await;

        if exit_code != 0 {
            warn!(
                sandbox_id = %sandbox_id,
                exit_code,
                "Daytona code execution exited with non-zero code"
            );
        } else {
            info!(sandbox_id = %sandbox_id, "Daytona code execution completed successfully");
        }

        // Append download info with FULL paths so the LLM can reference them
        let mut final_result = result_text;
        if !downloaded_files.is_empty() {
            final_result.push_str(
                "\n\n[Files automatically downloaded and displayed inline to the user. \
                 Do NOT call add_attachment — the images are already visible. Paths:",
            );
            for f in &downloaded_files {
                final_result.push_str(&format!("\n  - {}", f));
            }
            final_result.push(']');
        }

        Ok(DaytonaToolOutput {
            code,
            result: final_result,
            exit_code,
            sandbox_cleaned_up: cleaned_up,
            downloaded_files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_daytona_tool_definition() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "daytona_run");
        assert!(def.description.contains("sandbox"));
    }

    #[tokio::test]
    async fn test_daytona_tool_empty_code() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let args = DaytonaToolArgs {
            code: "   ".to_string(),
            language: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_api_error_auth() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let body = r#"{"statusCode":401,"message":"Invalid API key"}"#;
        let err = tool.parse_api_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(matches!(err, DaytonaToolError::AuthenticationFailed(_)));
    }

    #[test]
    fn test_parse_api_error_quota() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let body = r#"{"statusCode":403,"message":"quota exceeded for your account"}"#;
        let err = tool.parse_api_error(reqwest::StatusCode::FORBIDDEN, body);
        assert!(matches!(err, DaytonaToolError::QuotaExceeded(_)));
    }

    #[test]
    fn test_parse_api_error_generic() {
        let tool = DaytonaTool::new("test-key".into(), None);
        let body = "Internal Server Error";
        let err = tool.parse_api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, body);
        assert!(matches!(err, DaytonaToolError::ApiError(_)));
    }

    #[test]
    fn test_has_downloadable_extension() {
        assert!(has_downloadable_extension("chart.png"));
        assert!(has_downloadable_extension("PHOTO.JPG"));
        assert!(has_downloadable_extension("data.csv"));
        assert!(has_downloadable_extension("report.pdf"));
        assert!(!has_downloadable_extension("script.py"));
        assert!(!has_downloadable_extension("data.json"));
        assert!(!has_downloadable_extension("readme.txt"));
    }

    #[test]
    fn test_file_entry_deserialization() {
        let json = r#"[
            {"name": "chart.png", "isDir": false, "size": 12345},
            {"name": "subdir", "isDir": true, "size": 0}
        ]"#;
        let entries: Vec<FileEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "chart.png");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, 12345);
        assert!(entries[1].is_dir);
    }

    #[test]
    fn test_extract_python_imports() {
        let code = r#"
import matplotlib.pyplot as plt
import plotly.express as px
from numpy import array
import os
import json
import pandas as pd
from PIL import Image
"#;
        let imports = extract_python_imports(code);
        assert!(imports.contains(&"matplotlib".to_string()));
        assert!(imports.contains(&"plotly".to_string()));
        assert!(imports.contains(&"numpy".to_string()));
        assert!(imports.contains(&"pandas".to_string()));
        assert!(imports.contains(&"PIL".to_string()));
        // stdlib should be excluded
        assert!(!imports.contains(&"os".to_string()));
        assert!(!imports.contains(&"json".to_string()));
    }

    #[test]
    fn test_pip_package_name_mapping() {
        assert_eq!(pip_package_name("PIL"), "Pillow");
        assert_eq!(pip_package_name("sklearn"), "scikit-learn");
        assert_eq!(pip_package_name("cv2"), "opencv-python");
        assert_eq!(pip_package_name("yaml"), "pyyaml");
        // Unmapped names pass through
        assert_eq!(pip_package_name("plotly"), "plotly");
        assert_eq!(pip_package_name("pandas"), "pandas");
    }
}
