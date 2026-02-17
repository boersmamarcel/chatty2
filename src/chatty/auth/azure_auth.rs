use anyhow::{Context, Result};
use azure_core::auth::TokenCredential;
use azure_identity::{DefaultAzureCredential, TokenCredentialOptions};
use tracing::info;

const AZURE_OPENAI_SCOPE: &str = "https://cognitiveservices.azure.com/.default";

/// Augments the process PATH to include common Azure CLI installation directories.
///
/// GUI apps on macOS do not inherit the shell PATH, so `az` may not be findable
/// even after `az login`. This adds standard Homebrew and system bin paths.
pub fn augment_path_for_az_cli() {
    let extra_paths = [
        "/opt/homebrew/bin", // Apple Silicon Homebrew
        "/usr/local/bin",    // Intel Homebrew / system
        "/usr/bin",
        "/bin",
    ];

    let current = std::env::var("PATH").unwrap_or_default();
    let mut parts: Vec<&str> = current.split(':').collect();

    for path in extra_paths.iter().rev() {
        if !parts.contains(path) {
            parts.insert(0, path);
        }
    }

    let new_path = parts.join(":");
    if new_path != current {
        tracing::debug!(path = %new_path, "Augmented PATH for Azure CLI discovery");
        // SAFETY: no other thread reads PATH concurrently at this point;
        // this is a one-time augmentation to fix GUI app PATH on macOS
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("PATH", new_path)
        };
    }
}

/// Fetch Azure Entra ID token for Azure OpenAI
///
/// Uses DefaultAzureCredential which tries:
/// 1. Environment variables (AZURE_CLIENT_ID, AZURE_TENANT_ID, AZURE_CLIENT_SECRET)
/// 2. Managed Identity (if running on Azure)
/// 3. Azure CLI (`az login`)
/// 4. Interactive browser authentication (if configured)
///
/// # Returns
/// - `Ok(String)`: Valid bearer token (valid for ~1 hour)
/// - `Err`: Authentication failed with actionable error message
pub async fn fetch_entra_id_token() -> Result<String> {
    info!("Fetching Azure Entra ID token for Azure OpenAI");

    augment_path_for_az_cli();

    let credential = DefaultAzureCredential::create(TokenCredentialOptions::default())
        .context("Failed to create DefaultAzureCredential")?;

    let token_response = credential.get_token(&[AZURE_OPENAI_SCOPE]).await.context(
        "Failed to authenticate with Azure Entra ID. \
            Please run 'az login', configure managed identity, \
            or set AZURE_CLIENT_ID/AZURE_TENANT_ID/AZURE_CLIENT_SECRET environment variables.",
    )?;

    Ok(token_response.token.secret().to_string())
}
