use anyhow::{Context, Result};
use azure_core::credentials::{Secret, TokenCredential};
use azure_identity::{
    ClientSecretCredential, DeveloperToolsCredential, ManagedIdentityCredential,
    WorkloadIdentityCredential,
};
use std::sync::Arc;
use tracing::info;

const AZURE_OPENAI_SCOPE: &str = "https://cognitiveservices.azure.com/.default";

/// Try to resolve the user's full PATH by running their login shell.
///
/// GUI apps on macOS/Linux don't inherit the shell PATH. This spawns the user's
/// login shell (`$SHELL -l -c '...'`) and reads back the PATH, capturing all
/// modifications from shell config files (.bashrc, .zshrc, .profile, etc.) and
/// tool version managers (asdf, mise, nvm, volta, fnm, sdkman, rbenv, pyenv, etc.).
///
/// Uses markers in the output to isolate the PATH value from any shell startup
/// messages (MOTD, fortune, etc.). Returns None on failure or timeout (5s).
fn resolve_login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());

    // Use markers to isolate PATH from any shell startup noise (MOTD, fortune, etc.)
    let marker_start = "__CHATTY_PATH_START__";
    let marker_end = "__CHATTY_PATH_END__";
    let print_cmd = format!("echo {marker_start}$PATH{marker_end}");

    // Spawn the user's login shell so it sources profile/rc files
    // (.bash_profile, .bashrc, .zshrc, .zprofile, etc.).
    // Only `-l` (login) is used — `-i` (interactive) is avoided because it
    // triggers TTY warnings on headless environments (CI, containers) and is
    // unnecessary for sourcing config files.
    // Stderr is discarded (Stdio::null) to avoid a deadlock: if stderr were
    // piped but never drained, the child could block once the OS pipe buffer
    // fills up (~64 KB), causing a guaranteed timeout on every invocation.
    let mut child = std::process::Command::new(&shell)
        .args(["-l", "-c", &print_cmd])
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            tracing::debug!(shell = %shell, error = ?e, "Failed to spawn login shell for PATH resolution");
        })
        .ok()?;

    // Poll with timeout to avoid hanging if the shell blocks on interactive prompts
    let timeout = std::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut output = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    use std::io::Read;
                    stdout.read_to_string(&mut output).ok()?;
                }

                // Extract PATH from between markers to ignore shell startup messages
                let after_start = output.split(marker_start).nth(1)?;
                let path = after_start.split(marker_end).next()?.trim().to_string();

                if path.is_empty() {
                    tracing::debug!(shell = %shell, "Login shell returned empty PATH");
                    return None;
                }

                tracing::debug!(shell = %shell, "Resolved PATH from login shell");
                return Some(path);
            }
            Ok(Some(_)) => {
                tracing::debug!(shell = %shell, "Login shell exited with non-zero status");
                return None;
            }
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                tracing::debug!(shell = %shell, "Login shell PATH resolution timed out");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(e) => {
                tracing::debug!(shell = %shell, error = ?e, "Failed to check login shell status");
                let _ = child.kill();
                let _ = child.wait(); // Reap to prevent zombie
                return None;
            }
        }
    }
}

/// Augments the process PATH to include common tool installation directories.
///
/// GUI apps on macOS/Linux do not inherit the shell PATH, so executables installed
/// via Homebrew, npm, pip, uv/uvx, cargo, etc. may not be findable.
///
/// Strategy (two layers):
/// 1. **Login shell resolution**: Runs `$SHELL -l -c 'echo $PATH'` to capture
///    the user's full PATH including all config file modifications and version
///    managers (asdf, mise, nvm, volta, fnm, sdkman, rbenv, etc.).
/// 2. **Directory probing (fallback)**: Checks well-known installation directories
///    that might not be in the shell config (e.g., Homebrew keg-only installs).
///
/// Both layers are merged and deduplicated.
pub fn augment_gui_app_path() {
    use std::sync::OnceLock;

    // OnceLock ensures PATH augmentation runs exactly once for the process lifetime,
    // regardless of how many times this function is called.
    static PATH_AUGMENTED: OnceLock<()> = OnceLock::new();

    PATH_AUGMENTED.get_or_init(|| {
        let current = std::env::var("PATH").unwrap_or_default();
        // ':' is the PATH separator on macOS and Linux only.
        let existing: Vec<&str> = current.split(':').collect();

        let mut candidates: Vec<String> = Vec::new();

        // LAYER 1: Login shell PATH resolution.
        // This captures all PATH modifications from shell config files and version
        // managers. These paths get highest priority in the final PATH.
        if let Some(shell_path) = resolve_login_shell_path() {
            for p in shell_path.split(':') {
                if !p.is_empty() {
                    candidates.push(p.to_string());
                }
            }
        }

        // LAYER 2: Probe well-known installation directories as fallback.
        // These are checked even if login shell resolution succeeded, to catch
        // paths that might not be in the shell config (e.g., keg-only Homebrew).

        // Standard system / Homebrew paths
        let static_paths = [
            "/opt/homebrew/bin", // Apple Silicon Homebrew
            "/opt/homebrew/sbin",
            "/usr/local/bin", // Intel Homebrew / system
            "/usr/bin",
            "/bin",
        ];
        for p in &static_paths {
            candidates.push(p.to_string());
        }

        // Homebrew keg-only Node installs: probe /opt/homebrew/opt/node*/bin
        // and /usr/local/opt/node*/bin (Intel). These are not symlinked into
        // /opt/homebrew/bin, so npx/node won't be found without them.
        let homebrew_opt_roots = ["/opt/homebrew/opt", "/usr/local/opt"];
        for root in &homebrew_opt_roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                let mut node_dirs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("node") {
                            let bin = format!("{}/{}/bin", root, name);
                            if std::path::Path::new(&bin).exists() {
                                Some(bin)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                // Sort descending so newer versions (node@22 > node@20) appear first
                node_dirs.sort_by(|a, b| b.cmp(a));
                candidates.extend(node_dirs);
            }
        }

        // nvm-managed Node versions: probe ~/.nvm/versions/node/*/bin
        // Add all installed versions; the default alias is usually first alphabetically
        // (v16 < v20 < v22), so sort descending to prefer newer.
        if let Ok(home) = std::env::var("HOME") {
            let nvm_versions = format!("{}/.nvm/versions/node", home);
            if let Ok(entries) = std::fs::read_dir(&nvm_versions) {
                let mut nvm_dirs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let bin =
                            format!("{}/{}/bin", nvm_versions, e.file_name().to_string_lossy());
                        if std::path::Path::new(&bin).exists() {
                            Some(bin)
                        } else {
                            None
                        }
                    })
                    .collect();
                nvm_dirs.sort_by(|a, b| b.cmp(a));
                candidates.extend(nvm_dirs);
            }
        }

        // Python / uv / uvx paths: uvx is commonly installed via `uv` (Astral),
        // pipx, or cargo. GUI apps won't find these without explicit PATH entries.
        if let Ok(home) = std::env::var("HOME") {
            // ~/.local/bin — default install location for `uv`, pipx, pip --user
            let local_bin = format!("{}/.local/bin", home);
            if std::path::Path::new(&local_bin).exists() {
                candidates.push(local_bin);
            }

            // ~/.cargo/bin — uv can also be installed via `cargo install uv`
            let cargo_bin = format!("{}/.cargo/bin", home);
            if std::path::Path::new(&cargo_bin).exists() {
                candidates.push(cargo_bin);
            }

            // pyenv shims: ~/.pyenv/shims — if user manages Python via pyenv
            let pyenv_shims = format!("{}/.pyenv/shims", home);
            if std::path::Path::new(&pyenv_shims).exists() {
                candidates.push(pyenv_shims);
            }
        }

        // Homebrew keg-only Python installs: probe /opt/homebrew/opt/python*/bin
        // and /usr/local/opt/python*/bin (Intel). Similar to Node keg-only handling.
        for root in &homebrew_opt_roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                let mut python_dirs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("python") {
                            let bin = format!("{}/{}/bin", root, name);
                            if std::path::Path::new(&bin).exists() {
                                Some(bin)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                // Sort descending so newer versions appear first
                python_dirs.sort_by(|a, b| b.cmp(a));
                candidates.extend(python_dirs);
            }
        }

        // Build the new PATH: prepend candidates that aren't already present,
        // in reverse order so that after insertion at 0 the final order matches
        // the candidates order.
        let mut parts: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        for path in candidates.iter().rev() {
            if !parts.iter().any(|p| p == path) {
                parts.insert(0, path.clone());
            }
        }

        let new_path = parts.join(":");
        if new_path != current {
            tracing::debug!(path = %new_path, "Augmented PATH for GUI app");
            // SAFETY: PATH_AUGMENTED guarantees set_var is called at most once.
            // std::env::set_var is unsafe (UB with concurrent env readers), so we
            // minimise the window to a single call during early startup.
            unsafe {
                std::env::set_var("PATH", new_path);
            }
        }
    });
}

/// Which Entra credential `create_entra_credential` will construct.
///
/// azure_identity 1.0 removed `DefaultAzureCredential` (automatic chaining of
/// developer + deployed credentials was considered unsafe). Chatty picks one
/// explicit credential from environment signals instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntraCredentialSource {
    ClientSecret,
    WorkloadIdentity,
    ManagedIdentity,
    DeveloperTools,
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn present(value: Option<&str>) -> bool {
    value.is_some_and(|s| !s.is_empty())
}

/// Pure selection used by `create_entra_credential` and unit tests.
pub(crate) fn entra_credential_source_from_vars(
    client_id: Option<&str>,
    tenant_id: Option<&str>,
    client_secret: Option<&str>,
    federated_token_file: Option<&str>,
    identity_endpoint: Option<&str>,
    msi_endpoint: Option<&str>,
) -> EntraCredentialSource {
    if present(client_id) && present(tenant_id) && present(client_secret) {
        EntraCredentialSource::ClientSecret
    } else if present(federated_token_file) {
        EntraCredentialSource::WorkloadIdentity
    } else if present(identity_endpoint) || present(msi_endpoint) {
        EntraCredentialSource::ManagedIdentity
    } else {
        EntraCredentialSource::DeveloperTools
    }
}

fn detect_entra_credential_source() -> EntraCredentialSource {
    entra_credential_source_from_vars(
        env_nonempty("AZURE_CLIENT_ID").as_deref(),
        env_nonempty("AZURE_TENANT_ID").as_deref(),
        env_nonempty("AZURE_CLIENT_SECRET").as_deref(),
        env_nonempty("AZURE_FEDERATED_TOKEN_FILE").as_deref(),
        env_nonempty("IDENTITY_ENDPOINT").as_deref(),
        env_nonempty("MSI_ENDPOINT").as_deref(),
    )
}

/// Build the Entra credential azure_identity 1.0 expects (no `DefaultAzureCredential`).
///
/// Selection order:
/// 1. Service principal env vars (`AZURE_CLIENT_ID` + `AZURE_TENANT_ID` + `AZURE_CLIENT_SECRET`)
/// 2. Workload identity (`AZURE_FEDERATED_TOKEN_FILE`)
/// 3. Managed identity (`IDENTITY_ENDPOINT` / `MSI_ENDPOINT`)
/// 4. Developer tools (`az login` / `azd auth`) — the desktop default
pub(crate) fn create_entra_credential() -> Result<Arc<dyn TokenCredential>> {
    match detect_entra_credential_source() {
        EntraCredentialSource::ClientSecret => {
            let tenant_id = env_nonempty("AZURE_TENANT_ID")
                .context("AZURE_TENANT_ID is required for ClientSecretCredential")?;
            let client_id = env_nonempty("AZURE_CLIENT_ID")
                .context("AZURE_CLIENT_ID is required for ClientSecretCredential")?;
            let secret = env_nonempty("AZURE_CLIENT_SECRET")
                .context("AZURE_CLIENT_SECRET is required for ClientSecretCredential")?;
            info!("Using ClientSecretCredential for Entra ID");
            let credential =
                ClientSecretCredential::new(&tenant_id, client_id, Secret::new(secret), None)
                    .context("Failed to create ClientSecretCredential")?;
            Ok(credential)
        }
        EntraCredentialSource::WorkloadIdentity => {
            info!("Using WorkloadIdentityCredential for Entra ID");
            let credential = WorkloadIdentityCredential::new(None)
                .context("Failed to create WorkloadIdentityCredential")?;
            Ok(credential)
        }
        EntraCredentialSource::ManagedIdentity => {
            info!("Using ManagedIdentityCredential for Entra ID");
            let credential = ManagedIdentityCredential::new(None)
                .context("Failed to create ManagedIdentityCredential")?;
            Ok(credential)
        }
        EntraCredentialSource::DeveloperTools => {
            info!("Using DeveloperToolsCredential for Entra ID (az / azd)");
            let credential = DeveloperToolsCredential::new(None)
                .context("Failed to create DeveloperToolsCredential")?;
            Ok(credential)
        }
    }
}

/// Fetch Azure Entra ID token for Azure OpenAI
///
/// Picks one azure_identity 1.0 credential (see `create_entra_credential`):
/// 1. Service principal env vars (`AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_CLIENT_SECRET`)
/// 2. Workload identity (`AZURE_FEDERATED_TOKEN_FILE`)
/// 3. Managed identity (`IDENTITY_ENDPOINT` / `MSI_ENDPOINT`)
/// 4. Developer tools (`az login` / `azd auth`)
///
/// # Returns
/// - `Ok(String)`: Valid bearer token (valid for ~1 hour)
/// - `Err`: Authentication failed with actionable error message
pub async fn fetch_entra_id_token() -> Result<String> {
    info!("Fetching Azure Entra ID token for Azure OpenAI");

    augment_gui_app_path();

    let credential = create_entra_credential().context("Failed to create Entra ID credential")?;

    let token_response = credential
        .get_token(&[AZURE_OPENAI_SCOPE], None)
        .await
        .context(
            "Failed to authenticate with Azure Entra ID. \
            Please run 'az login', configure managed identity, \
            or set AZURE_CLIENT_ID/AZURE_TENANT_ID/AZURE_CLIENT_SECRET environment variables.",
        )?;

    Ok(token_response.token.secret().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_login_shell_path_returns_path_with_separators() {
        // On any Unix-like CI or dev machine, the login shell should return
        // a PATH containing at least one ':' separator and `/` characters.
        if let Some(path) = resolve_login_shell_path() {
            assert!(
                path.contains('/'),
                "PATH should contain directory separators"
            );
            assert!(path.contains(':'), "PATH should contain colon separators");
            assert!(
                !path.contains("__CHATTY_PATH_"),
                "Markers should not leak into the resolved PATH"
            );
        }
        // If None, the shell is unavailable in this environment — that's OK,
        // the function is designed to gracefully return None.
    }

    #[test]
    fn test_resolve_login_shell_path_does_not_hang() {
        // Verify the 5-second timeout works: this should complete quickly even
        // if the shell has startup delays.
        let start = std::time::Instant::now();
        let _ = resolve_login_shell_path();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "resolve_login_shell_path should complete within the timeout"
        );
    }

    #[test]
    fn entra_source_prefers_client_secret_when_all_three_env_vars_set() {
        assert_eq!(
            entra_credential_source_from_vars(
                Some("app-id"),
                Some("tenant-id"),
                Some("secret"),
                Some("/var/run/secrets/azure/tokens/azure-identity-token"),
                Some("http://169.254.169.254/metadata/identity"),
                None,
            ),
            EntraCredentialSource::ClientSecret
        );
    }

    #[test]
    fn entra_source_uses_workload_identity_when_federated_token_file_set() {
        assert_eq!(
            entra_credential_source_from_vars(
                Some("app-id"),
                Some("tenant-id"),
                None,
                Some("/var/run/secrets/azure/tokens/azure-identity-token"),
                None,
                None,
            ),
            EntraCredentialSource::WorkloadIdentity
        );
    }

    #[test]
    fn entra_source_uses_managed_identity_when_identity_endpoint_set() {
        assert_eq!(
            entra_credential_source_from_vars(
                None,
                None,
                None,
                None,
                Some("http://169.254.169.254/metadata/identity"),
                None,
            ),
            EntraCredentialSource::ManagedIdentity
        );
    }

    #[test]
    fn entra_source_uses_managed_identity_when_msi_endpoint_set() {
        assert_eq!(
            entra_credential_source_from_vars(
                None,
                None,
                None,
                None,
                None,
                Some("http://localhost")
            ),
            EntraCredentialSource::ManagedIdentity
        );
    }

    #[test]
    fn entra_source_defaults_to_developer_tools() {
        assert_eq!(
            entra_credential_source_from_vars(None, None, None, None, None, None),
            EntraCredentialSource::DeveloperTools
        );
        assert_eq!(
            entra_credential_source_from_vars(Some(""), Some(""), Some(""), None, None, None),
            EntraCredentialSource::DeveloperTools
        );
        // Incomplete service-principal trio is not enough
        assert_eq!(
            entra_credential_source_from_vars(
                Some("app-id"),
                Some("tenant-id"),
                None,
                None,
                None,
                None
            ),
            EntraCredentialSource::DeveloperTools
        );
    }

    /// Live Entra ID token fetch. Requires `az login` or AZURE_CLIENT_* env vars.
    ///
    /// ```text
    /// cargo test -p chatty-core --lib smoke_fetch_entra_id_token -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires Azure credentials (az login or AZURE_CLIENT_* env)"]
    async fn smoke_fetch_entra_id_token() {
        let token = fetch_entra_id_token().await.expect(
            "Entra ID token fetch failed — run `az login` or set \
             AZURE_CLIENT_ID / AZURE_TENANT_ID / AZURE_CLIENT_SECRET",
        );
        let parts: Vec<_> = token.split('.').collect();
        assert_eq!(
            parts.len(),
            3,
            "expected a JWT (three dot-separated parts), got {} parts",
            parts.len()
        );
        assert!(
            parts.iter().all(|p| !p.is_empty()),
            "JWT parts must be non-empty"
        );
    }
}
