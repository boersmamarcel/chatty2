//! The install record: what chatty knew about a module's `.wasm` when it
//! installed it, kept beside the module and checked at every load
//! (PL-H5a, AGE-703).
//!
//! ```text
//! <module_dir>/echo-agent/
//! ├── module.toml
//! ├── echo-agent.wasm
//! └── .chatty-install.json   {"sha256": "…", "trust_level": "signed", "publisher_key_id": "…"}
//! ```
//!
//! A module with a record loads only while its `.wasm` still hashes to the
//! recorded `sha256`; a mismatch is a load failure, shown in Settings →
//! Extensions as `Failed to load: hash mismatch …`. A module without one was
//! put there by hand and loads as [`TrustLevel::Local`].

use std::path::Path;

use anyhow::{Context, Result};
use hive_client::TrustLevel;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The record's file name, inside the module's directory.
pub const INSTALL_RECORD_FILE: &str = ".chatty-install.json";

/// What the installer recorded about a module's `.wasm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRecord {
    /// Hex SHA-256 of the `.wasm` as installed.
    pub sha256: String,
    /// The trust hive-client gave the download.
    pub trust_level: TrustLevel,
    /// The publisher's key (hex Ed25519 verifying key) the download was
    /// signed with, if it was signed.
    pub publisher_key_id: Option<String>,
}

impl InstallRecord {
    /// The record for installing `wasm` at `trust_level`.
    pub fn new(wasm: &[u8], trust_level: TrustLevel, publisher_key_id: Option<String>) -> Self {
        Self {
            sha256: sha256_hex(wasm),
            trust_level,
            publisher_key_id,
        }
    }

    /// The record in `module_dir`, or `None` when there is none (a module
    /// put there by hand). A record that exists but does not parse is an
    /// error: the module is refused rather than demoted to local.
    pub fn read(module_dir: &Path) -> Result<Option<Self>> {
        let path = module_dir.join(INSTALL_RECORD_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("invalid install record {}", path.display()))
    }

    /// Write the record into `module_dir`.
    pub fn write(&self, module_dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(module_dir.join(INSTALL_RECORD_FILE), json)
    }
}

/// The trust a module's `wasm` bytes load at: the recorded level when they
/// still hash to the record in `module_dir`, [`TrustLevel::Local`] when
/// there is no record, and an error (`hash mismatch …`) when they do not.
pub fn verify_installed(module_dir: &Path, wasm: &[u8]) -> Result<TrustLevel> {
    let Some(record) = InstallRecord::read(module_dir)? else {
        return Ok(TrustLevel::Local);
    };
    let actual = sha256_hex(wasm);
    if !actual.eq_ignore_ascii_case(&record.sha256) {
        anyhow::bail!(
            "hash mismatch: the .wasm in {} hashes to {actual}, but {INSTALL_RECORD_FILE} \
             recorded {} at install; it changed on disk since, so reinstall the module",
            module_dir.display(),
            record.sha256
        );
    }
    Ok(record.trust_level)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let record = InstallRecord::new(b"wasm", TrustLevel::Signed, Some("ab".into()));
        record.write(dir.path()).unwrap();
        assert_eq!(InstallRecord::read(dir.path()).unwrap(), Some(record));
        assert_eq!(
            verify_installed(dir.path(), b"wasm").unwrap(),
            TrustLevel::Signed
        );
        let err = verify_installed(dir.path(), b"wasn").unwrap_err();
        assert!(err.to_string().contains("hash mismatch"), "{err}");
    }

    #[test]
    fn no_record_is_local() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            verify_installed(dir.path(), b"anything").unwrap(),
            TrustLevel::Local
        );
    }

    #[test]
    fn unparsable_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INSTALL_RECORD_FILE), "{").unwrap();
        assert!(verify_installed(dir.path(), b"wasm").is_err());
    }
}
