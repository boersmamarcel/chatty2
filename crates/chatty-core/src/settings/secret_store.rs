//! Where Chatty keeps secrets: provider API keys, MCP and A2A bearer tokens,
//! and the Hive access and refresh tokens (AGE-741).
//!
//! The settings JSON files hold only a reference (`"api_key_ref":
//! "provider/<name>"`); the value lives in a [`SecretStore`]. The default
//! store is the OS keychain — macOS Keychain, Windows Credential Manager,
//! Secret Service on Linux. A machine without one (headless Linux, a Harbor
//! container) falls back to an owner-only `secrets.json` next to the settings
//! and says so in one warning line. `CHATTY_SECRET_STORE` overrides the
//! choice: `keychain` refuses the fallback, `file` skips the keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The environment variable that picks the secret store explicitly.
pub const SECRET_STORE_ENV: &str = "CHATTY_SECRET_STORE";

/// The file-fallback store's file name, in the chatty config directory.
pub const SECRETS_FILE_NAME: &str = "secrets.json";

/// A failed secret-store operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStoreError(pub String);

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretStoreError {}

pub type SecretResult<T> = Result<T, SecretStoreError>;

/// A key → secret map that lives outside the settings files.
///
/// Calls may block (a keychain is an IPC round trip); async callers run
/// them on the blocking pool.
pub trait SecretStore: Send + Sync + 'static {
    /// The secret stored under `key`, or `None` if there is none.
    fn get(&self, key: &str) -> SecretResult<Option<String>>;
    /// Store `value` under `key`, replacing any previous value.
    fn set(&self, key: &str, value: &str) -> SecretResult<()>;
    /// Remove `key`. Removing a key that is not there is not an error.
    fn delete(&self, key: &str) -> SecretResult<()>;
    /// A short human name for logs ("OS keychain", "secrets.json").
    fn describe(&self) -> String;
}

// ── In-memory ────────────────────────────────────────────────────────────────

/// A process-local store, for tests and for hosts that keep nothing on disk.
#[derive(Default)]
pub struct InMemorySecretStore {
    entries: Mutex<BTreeMap<String, String>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every key currently stored, sorted.
    pub fn keys(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &str) -> SecretResult<Option<String>> {
        Ok(self.lock().get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> SecretResult<()> {
        self.lock().insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> SecretResult<()> {
        self.lock().remove(key);
        Ok(())
    }

    fn describe(&self) -> String {
        "in-memory store".to_string()
    }
}

// ── File fallback ────────────────────────────────────────────────────────────

/// The fallback for machines without a keychain: one JSON object in a file
/// only its owner can read. Plaintext on disk, which is why it is only used
/// when no keychain answers, and never silently.
///
/// Read fresh on every call, so several processes sharing one config
/// directory (a Harbor container's workers) see each other's writes.
pub struct FileSecretStore {
    path: PathBuf,
    /// Serializes read-modify-write within this process.
    write_lock: Mutex<()>,
}

impl FileSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_map(&self) -> SecretResult<BTreeMap<String, String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).map_err(|e| {
                SecretStoreError(format!("{} is not a JSON object: {e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(SecretStoreError(format!(
                "cannot read {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn write_map(&self, map: &BTreeMap<String, String>) -> SecretResult<()> {
        let json = serde_json::to_string_pretty(map)
            .map_err(|e| SecretStoreError(format!("cannot serialize secrets: {e}")))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SecretStoreError(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        let temp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        write_owner_only(&temp, json.as_bytes())
            .map_err(|e| SecretStoreError(format!("cannot write {}: {e}", temp.display())))?;
        std::fs::rename(&temp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            SecretStoreError(format!("cannot replace {}: {e}", self.path.display()))
        })
    }
}

/// Create `path` readable by this user only (mode 0600 on Unix; on Windows
/// the per-user profile ACL already keeps other users out).
fn write_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> SecretResult<Option<String>> {
        Ok(self.read_map()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> SecretResult<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_map()?;
        if map.get(key).map(String::as_str) == Some(value) {
            return Ok(());
        }
        map.insert(key.to_string(), value.to_string());
        self.write_map(&map)
    }

    fn delete(&self, key: &str) -> SecretResult<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_map()?;
        if map.remove(key).is_some() {
            self.write_map(&map)?;
        }
        Ok(())
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

// ── OS keychain ──────────────────────────────────────────────────────────────

/// The OS keychain via the `keyring` crate.
///
/// Entries are filed under one service per config directory
/// (`chatty:<config dir>`), so a throwaway `HOME` or `XDG_CONFIG_HOME` (test
/// harnesses, team smoke runs) never reads or overwrites the keys of the
/// user's real install.
pub struct KeychainSecretStore {
    service: String,
}

impl KeychainSecretStore {
    pub fn for_config_dir(config_dir: &Path) -> Self {
        Self {
            service: format!("chatty:{}", config_dir.display()),
        }
    }

    fn entry(&self, key: &str) -> SecretResult<keyring::Entry> {
        keyring::Entry::new(&self.service, key)
            .map_err(|e| SecretStoreError(format!("OS keychain entry {key}: {e}")))
    }

    /// Whether the keychain answers at all: write, read back and delete a
    /// probe entry. A Linux box without a Secret Service fails here.
    pub fn probe(&self) -> SecretResult<()> {
        const PROBE_KEY: &str = "chatty/keychain-probe";
        self.set(PROBE_KEY, "ok")?;
        let read = self.get(PROBE_KEY);
        let _ = self.delete(PROBE_KEY);
        match read? {
            Some(value) if value == "ok" => Ok(()),
            _ => Err(SecretStoreError(
                "OS keychain did not return the probe value it was given".to_string(),
            )),
        }
    }
}

impl SecretStore for KeychainSecretStore {
    fn get(&self, key: &str) -> SecretResult<Option<String>> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretStoreError(format!("OS keychain read {key}: {e}"))),
        }
    }

    fn set(&self, key: &str, value: &str) -> SecretResult<()> {
        self.entry(key)?
            .set_password(value)
            .map_err(|e| SecretStoreError(format!("OS keychain write {key}: {e}")))
    }

    fn delete(&self, key: &str) -> SecretResult<()> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretStoreError(format!("OS keychain delete {key}: {e}"))),
        }
    }

    fn describe(&self) -> String {
        "OS keychain".to_string()
    }
}

// ── Choosing the store ───────────────────────────────────────────────────────

/// Pick the secret store for `config_dir`, honouring [`SECRET_STORE_ENV`]:
///
/// - unset or `auto`: the OS keychain if it answers a probe, otherwise the
///   owner-only `secrets.json` file, with one warning line;
/// - `keychain`: the OS keychain or an error, never the file;
/// - `file`: the file, without touching the keychain.
pub fn select_secret_store(config_dir: &Path) -> SecretResult<Arc<dyn SecretStore>> {
    let choice = std::env::var(SECRET_STORE_ENV).unwrap_or_default();
    let file = || FileSecretStore::new(config_dir.join(SECRETS_FILE_NAME));
    match choice.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => {
            let keychain = KeychainSecretStore::for_config_dir(config_dir);
            match keychain.probe() {
                Ok(()) => Ok(Arc::new(keychain)),
                Err(e) => {
                    let file = file();
                    tracing::warn!(
                        "no OS keychain available ({e}); keeping provider keys and Hive tokens in the owner-only file {} instead (set {SECRET_STORE_ENV}=keychain to refuse this fallback)",
                        file.path().display()
                    );
                    Ok(Arc::new(file))
                }
            }
        }
        "keychain" => {
            let keychain = KeychainSecretStore::for_config_dir(config_dir);
            keychain.probe().map_err(|e| {
                SecretStoreError(format!(
                    "{SECRET_STORE_ENV}=keychain but the OS keychain is unavailable: {e}. Start a Secret Service (e.g. gnome-keyring) or unset {SECRET_STORE_ENV} to allow the owner-only file fallback"
                ))
            })?;
            Ok(Arc::new(keychain))
        }
        "file" => {
            let file = file();
            tracing::info!(
                "{SECRET_STORE_ENV}=file: keeping secrets in {}",
                file.path().display()
            );
            Ok(Arc::new(file))
        }
        other => Err(SecretStoreError(format!(
            "{SECRET_STORE_ENV}={other} is not one of auto, keychain, file"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_store_set_get_delete() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get("provider/a").unwrap(), None);
        store.set("provider/a", "k1").unwrap();
        store.set("provider/a", "k2").unwrap();
        assert_eq!(store.get("provider/a").unwrap().as_deref(), Some("k2"));
        store.delete("provider/a").unwrap();
        store.delete("provider/a").unwrap();
        assert_eq!(store.get("provider/a").unwrap(), None);
    }

    #[test]
    fn file_store_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_FILE_NAME);
        FileSecretStore::new(path.clone())
            .set("hive/token", "jwt")
            .unwrap();
        let again = FileSecretStore::new(path.clone());
        assert_eq!(again.get("hive/token").unwrap().as_deref(), Some("jwt"));
        again.delete("hive/token").unwrap();
        assert_eq!(FileSecretStore::new(path).get("hive/token").unwrap(), None);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != SECRETS_FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    /// Talks to the real OS keychain, which CI does not have. Run by hand on
    /// a desktop: `cargo test -p chatty-core keychain_smoke -- --ignored`.
    #[test]
    #[ignore = "needs a real OS keychain"]
    fn keychain_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeychainSecretStore::for_config_dir(dir.path());
        store.probe().expect("keychain answers the probe");
        store.set("provider/smoke", "sk-smoke").unwrap();
        assert_eq!(
            store.get("provider/smoke").unwrap().as_deref(),
            Some("sk-smoke")
        );
        store.delete("provider/smoke").unwrap();
        assert_eq!(store.get("provider/smoke").unwrap(), None);
    }
}
