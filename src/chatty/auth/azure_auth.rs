use anyhow::{Context, Result};
use azure_core::auth::TokenCredential;
use azure_identity::{DefaultAzureCredential, TokenCredentialOptions};
use tracing::info;

const AZURE_OPENAI_SCOPE: &str = "https://cognitiveservices.azure.com/.default";

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

    let credential = DefaultAzureCredential::create(TokenCredentialOptions::default())
        .context("Failed to create DefaultAzureCredential")?;

    let token_response = credential.get_token(&[AZURE_OPENAI_SCOPE]).await.context(
        "Failed to authenticate with Azure Entra ID. \
            Please run 'az login', configure managed identity, \
            or set AZURE_CLIENT_ID/AZURE_TENANT_ID/AZURE_CLIENT_SECRET environment variables.",
    )?;

    Ok(token_response.token.secret().to_string())
}
