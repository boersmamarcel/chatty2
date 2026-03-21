//! Login profile repository — JSON file persistence for [`LoginProfile`] metadata.
//!
//! Follows the same pattern as other chatty settings repositories:
//! - XDG-compliant path (`~/.config/chatty/login_profiles.json`)
//! - Atomic writes via temp file + rename
//! - Returns `Default` if the file doesn't exist

use std::path::PathBuf;

use crate::credential::types::LoginProfile;
use anyhow::{Context, Result};

/// Repository for persisting login profile metadata (no secrets).
pub struct LoginProfileRepository {
    file_path: PathBuf,
}

impl LoginProfileRepository {
    /// Create a new repository with the default config path.
    pub fn new() -> Result<Self> {
        let config_dir = dirs::config_dir().context("Cannot determine config directory")?;
        let file_path = config_dir.join("chatty").join("login_profiles.json");
        Ok(Self { file_path })
    }

    /// Load all login profiles from disk.
    pub async fn load_all(&self) -> Result<Vec<LoginProfile>> {
        if !tokio::fs::try_exists(&self.file_path)
            .await
            .unwrap_or(false)
        {
            return Ok(Vec::new());
        }
        let contents = tokio::fs::read_to_string(&self.file_path)
            .await
            .context("Failed to read login profiles")?;
        let profiles: Vec<LoginProfile> =
            serde_json::from_str(&contents).context("Failed to parse login profiles")?;
        Ok(profiles)
    }

    /// Save all login profiles to disk (atomic write).
    pub async fn save_all(&self, profiles: &[LoginProfile]) -> Result<()> {
        if let Some(parent) = self.file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json =
            serde_json::to_string_pretty(profiles).context("Failed to serialize login profiles")?;
        let temp = self
            .file_path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        tokio::fs::write(&temp, &json).await?;
        tokio::fs::rename(&temp, &self.file_path).await?;
        Ok(())
    }

    /// Add or update a login profile (matched by name).
    pub async fn upsert(&self, profile: LoginProfile) -> Result<()> {
        let mut profiles = self.load_all().await?;
        if let Some(existing) = profiles.iter_mut().find(|p| p.name == profile.name) {
            *existing = profile;
        } else {
            profiles.push(profile);
        }
        self.save_all(&profiles).await
    }

    /// Delete a login profile by name.
    pub async fn delete(&self, name: &str) -> Result<bool> {
        let mut profiles = self.load_all().await?;
        let initial_len = profiles.len();
        profiles.retain(|p| p.name != name);
        if profiles.len() < initial_len {
            self.save_all(&profiles).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Find a login profile by name.
    pub async fn find_by_name(&self, name: &str) -> Result<Option<LoginProfile>> {
        let profiles = self.load_all().await?;
        Ok(profiles.into_iter().find(|p| p.name == name))
    }
}
