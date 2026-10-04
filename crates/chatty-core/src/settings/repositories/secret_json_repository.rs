//! JSON settings files whose secret fields live in a [`SecretStore`]
//! (AGE-741).
//!
//! The model keeps its plain field (`api_key: Option<String>`); only the file
//! changes. On save each non-empty secret field is written to the store and
//! replaced in the JSON by a reference (`"api_key_ref": "provider/<name>"`);
//! on load the reference is resolved back. A secret whose reference
//! disappears from the file (provider removed or renamed, Hive logout) is
//! deleted from the store.
//!
//! **One-time migration.** A file written by an older version still has the
//! plaintext field. Loading it moves each value into the store and rewrites
//! the file with references, so after the first start of this version no key
//! is left in the JSON. Remove this move code in v0.7.0 (AGE-832).

use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use super::generic_json_repository::SaveTicket;
use super::provider_repository::{BoxFuture, RepositoryError, RepositoryResult};
use crate::settings::secret_store::{SecretStore, SecretStoreError};

/// Where a secret field's value is filed in the store.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SecretKey {
    /// Always this key (a single-object file: `hive/token`).
    Fixed(&'static str),
    /// `<prefix>/<the item's "name">` (a list file: `provider/openrouter`).
    Named(&'static str),
}

/// One secret field of a settings object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SecretField {
    /// The model's field, e.g. `api_key`. In the file it becomes
    /// `<field>_ref`.
    pub field: &'static str,
    pub key: SecretKey,
}

impl SecretField {
    fn ref_field(&self) -> String {
        format!("{}_ref", self.field)
    }

    fn key_for(&self, object: &Map<String, Value>) -> RepositoryResult<String> {
        match self.key {
            SecretKey::Fixed(key) => Ok(key.to_string()),
            SecretKey::Named(prefix) => match object.get("name").and_then(Value::as_str) {
                Some(name) => Ok(format!("{prefix}/{name}")),
                None => Err(RepositoryError::SerializationError(format!(
                    "cannot file {} in the secret store: the entry has no name",
                    self.field
                ))),
            },
        }
    }
}

fn store_error(e: SecretStoreError) -> RepositoryError {
    RepositoryError::IoError(format!("secret store: {e}"))
}

/// The objects a file holds: the root object, or each object of a root array.
fn objects_mut(root: &mut Value) -> Vec<&mut Map<String, Value>> {
    match root {
        Value::Object(map) => vec![map],
        Value::Array(items) => items.iter_mut().filter_map(Value::as_object_mut).collect(),
        _ => Vec::new(),
    }
}

/// Every secret reference a file's JSON names.
fn refs_in(root: &mut Value, fields: &[SecretField]) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for object in objects_mut(root) {
        for field in fields {
            if let Some(Value::String(key)) = object.get(&field.ref_field()) {
                refs.insert(key.clone());
            }
        }
    }
    refs
}

/// Move every non-empty secret field into `store`, leaving a reference.
/// Returns the references written.
fn externalize(
    root: &mut Value,
    fields: &[SecretField],
    store: &dyn SecretStore,
) -> RepositoryResult<BTreeSet<String>> {
    let mut refs = BTreeSet::new();
    for object in objects_mut(root) {
        for field in fields {
            object.remove(&field.ref_field());
            let secret = match object.get(field.field) {
                Some(Value::String(secret)) if !secret.is_empty() => secret.clone(),
                _ => continue,
            };
            let key = field.key_for(object)?;
            store.set(&key, &secret).map_err(store_error)?;
            object.remove(field.field);
            object.insert(field.ref_field(), Value::String(key.clone()));
            refs.insert(key);
        }
    }
    Ok(refs)
}

/// Resolve every reference back into its plain field. Returns whether the
/// file still had a plaintext secret that is now in the store (the one-time
/// migration, AGE-832), in which case the caller rewrites the file.
fn resolve(root: &mut Value, fields: &[SecretField], store: &dyn SecretStore) -> bool {
    let mut migrated = false;
    for object in objects_mut(root) {
        for field in fields {
            let reference = object.remove(&field.ref_field());

            // One-time migration (AGE-832): a plaintext value from an older
            // version goes into the store; the rewrite drops it from the file.
            if let Some(Value::String(plain)) = object.get(field.field)
                && !plain.is_empty()
            {
                let plain = plain.clone();
                match field
                    .key_for(object)
                    .and_then(|key| store.set(&key, &plain).map_err(store_error))
                {
                    Ok(()) => migrated = true,
                    Err(e) => tracing::warn!(
                        "could not move a plaintext {} into the secret store, leaving it in place: {e}",
                        field.field
                    ),
                }
                continue;
            }

            let Some(Value::String(key)) = reference else {
                continue;
            };
            match store.get(&key) {
                Ok(Some(secret)) => {
                    object.insert(field.field.to_string(), Value::String(secret));
                }
                Ok(None) => tracing::warn!(
                    "secret {key} is missing from the {}; enter it again in Settings",
                    store.describe()
                ),
                Err(e) => tracing::warn!("cannot read secret {key}: {e}"),
            }
        }
    }
    migrated
}

/// A JSON settings file with secret fields; the typed repositories below
/// wrap it.
struct SecretJsonFile {
    path: PathBuf,
    store: Arc<dyn SecretStore>,
    fields: &'static [SecretField],
}

impl SecretJsonFile {
    /// The file's JSON with every secret resolved, or `None` if there is no
    /// file yet.
    async fn load(&self) -> RepositoryResult<Option<Value>> {
        if !tokio::fs::try_exists(&self.path).await.unwrap_or(false) {
            return Ok(None);
        }
        let contents = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|e| RepositoryError::IoError(e.to_string()))?;
        let value: Value = serde_json::from_str(&contents)
            .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

        let store = self.store.clone();
        let fields = self.fields;
        let (value, migrated) = tokio::task::spawn_blocking(move || {
            let mut value = value;
            let migrated = resolve(&mut value, fields, store.as_ref());
            (value, migrated)
        })
        .await
        .map_err(|e| RepositoryError::IoError(e.to_string()))?;

        if migrated {
            match self.save(value.clone()).await {
                Ok(()) => tracing::info!(
                    "moved the plaintext secrets of {} into the {}",
                    self.path.display(),
                    self.store.describe()
                ),
                Err(e) => tracing::warn!(
                    "could not rewrite {} without its plaintext secrets: {e}",
                    self.path.display()
                ),
            }
        }
        Ok(Some(value))
    }

    /// Write `value`, its secrets to the store and references to the file,
    /// then drop the secrets the file no longer references. In save order:
    /// the ticket is taken now and held across the store writes.
    fn save(&self, value: Value) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.path.clone();
        let store = self.store.clone();
        let fields = self.fields;
        let ticket = SaveTicket::take(&path);

        Box::pin(async move {
            let Some(turn) = ticket.turn().await else {
                return Ok(());
            };
            let mut previous = referenced_in_file(&path, fields).await;

            let (value, current) = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || {
                    let mut value = value;
                    externalize(&mut value, fields, store.as_ref()).map(|refs| (value, refs))
                })
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))??
            };
            let json = serde_json::to_string_pretty(&value)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;
            turn.write(&path, json).await?;

            previous.retain(|key| !current.contains(key));
            if !previous.is_empty() {
                let _ = tokio::task::spawn_blocking(move || {
                    for key in previous {
                        if let Err(e) = store.delete(&key) {
                            tracing::warn!("cannot delete the unused secret {key}: {e}");
                        }
                    }
                })
                .await;
            }
            Ok(())
        })
    }
}

/// The references the file on disk names now (empty when it is missing or
/// unreadable: a secret is then left behind rather than deleted wrongly).
async fn referenced_in_file(path: &Path, fields: &[SecretField]) -> BTreeSet<String> {
    let Ok(contents) = tokio::fs::read_to_string(path).await else {
        return BTreeSet::new();
    };
    match serde_json::from_str::<Value>(&contents) {
        Ok(mut value) => refs_in(&mut value, fields),
        Err(_) => BTreeSet::new(),
    }
}

/// A single settings object (`load` / `save`) with secret fields.
pub struct SecretJsonRepository<T> {
    file: SecretJsonFile,
    _marker: PhantomData<T>,
}

impl<T> SecretJsonRepository<T>
where
    T: Serialize + DeserializeOwned + Default + Send + 'static,
{
    pub(crate) fn with_store(
        path: PathBuf,
        store: Arc<dyn SecretStore>,
        fields: &'static [SecretField],
    ) -> Self {
        Self {
            file: SecretJsonFile {
                path,
                store,
                fields,
            },
            _marker: PhantomData,
        }
    }

    pub fn load(&self) -> BoxFuture<'static, RepositoryResult<T>> {
        let file = self.file.clone_handle();
        Box::pin(async move {
            match file.load().await? {
                Some(value) => serde_json::from_value(value)
                    .map_err(|e| RepositoryError::SerializationError(e.to_string())),
                None => Ok(T::default()),
            }
        })
    }

    pub fn save(&self, value: T) -> BoxFuture<'static, RepositoryResult<()>> {
        match serde_json::to_value(&value) {
            Ok(value) => self.file.save(value),
            Err(e) => {
                let e = RepositoryError::SerializationError(e.to_string());
                Box::pin(async move { Err(e) })
            }
        }
    }
}

/// A list of settings objects (`load_all` / `save_all`) with secret fields.
pub struct SecretJsonListRepository<T> {
    file: SecretJsonFile,
    _marker: PhantomData<T>,
}

impl<T> SecretJsonListRepository<T>
where
    T: Serialize + DeserializeOwned + Send + 'static,
{
    pub(crate) fn with_store(
        path: PathBuf,
        store: Arc<dyn SecretStore>,
        fields: &'static [SecretField],
    ) -> Self {
        Self {
            file: SecretJsonFile {
                path,
                store,
                fields,
            },
            _marker: PhantomData,
        }
    }

    pub fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<T>>> {
        let file = self.file.clone_handle();
        Box::pin(async move {
            match file.load().await? {
                Some(value) => serde_json::from_value(value)
                    .map_err(|e| RepositoryError::SerializationError(e.to_string())),
                None => Ok(Vec::new()),
            }
        })
    }

    pub fn save_all(&self, items: Vec<T>) -> BoxFuture<'static, RepositoryResult<()>> {
        match serde_json::to_value(&items) {
            Ok(value) => self.file.save(value),
            Err(e) => {
                let e = RepositoryError::SerializationError(e.to_string());
                Box::pin(async move { Err(e) })
            }
        }
    }
}

impl SecretJsonFile {
    fn clone_handle(&self) -> Self {
        Self {
            path: self.path.clone(),
            store: self.store.clone(),
            fields: self.fields,
        }
    }
}

// ── The secret fields of each settings file ──────────────────────────────────

/// `providers.json`: each provider's API key.
pub(crate) const PROVIDER_SECRETS: &[SecretField] = &[SecretField {
    field: "api_key",
    key: SecretKey::Named("provider"),
}];

/// `mcp_servers.json`: each server's bearer token.
pub(crate) const MCP_SECRETS: &[SecretField] = &[SecretField {
    field: "api_key",
    key: SecretKey::Named("mcp"),
}];

/// `a2a_agents.json`: each agent's bearer token.
pub(crate) const A2A_SECRETS: &[SecretField] = &[SecretField {
    field: "api_key",
    key: SecretKey::Named("a2a"),
}];

/// `hive_settings.json`: the access JWT and the 30-day refresh token.
pub(crate) const HIVE_SECRETS: &[SecretField] = &[
    SecretField {
        field: "token",
        key: SecretKey::Fixed("hive/token"),
    },
    SecretField {
        field: "refresh_token",
        key: SecretKey::Fixed("hive/refresh_token"),
    },
];

#[cfg(test)]
mod tests {
    use super::super::{
        A2aJsonRepository, A2aRepository, HiveSettingsJsonRepository, HiveSettingsRepository,
        JsonFileRepository, JsonMcpRepository, McpRepository, ProviderRepository,
    };
    use crate::settings::models::a2a_store::A2aAgentConfig;
    use crate::settings::models::hive_settings::HiveSettingsModel;
    use crate::settings::models::mcp_store::McpServerConfig;
    use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
    use crate::settings::secret_store::{InMemorySecretStore, SecretStore};
    use std::sync::Arc;

    fn read(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn providers(
        dir: &tempfile::TempDir,
    ) -> (
        std::path::PathBuf,
        Arc<InMemorySecretStore>,
        JsonFileRepository,
    ) {
        let path = dir.path().join("providers.json");
        let store = Arc::new(InMemorySecretStore::new());
        let repo = JsonFileRepository::with_path_and_store(path.clone(), store.clone());
        (path, store, repo)
    }

    #[tokio::test]
    async fn provider_key_goes_to_the_store_and_the_file_keeps_a_reference() {
        let dir = tempfile::tempdir().unwrap();
        let (path, store, repo) = providers(&dir);

        repo.save_all(vec![
            ProviderConfig::new("openrouter".into(), ProviderType::OpenRouter)
                .with_api_key("sk-or-secret".into()),
            ProviderConfig::new("ollama".into(), ProviderType::Ollama),
        ])
        .await
        .unwrap();

        let file = read(&path);
        assert!(!file.contains("sk-or-secret"), "{file}");
        assert!(!file.contains("\"api_key\""), "{file}");
        assert!(
            file.contains("\"api_key_ref\": \"provider/openrouter\""),
            "{file}"
        );
        assert_eq!(
            store.get("provider/openrouter").unwrap().as_deref(),
            Some("sk-or-secret")
        );
        assert_eq!(store.keys(), vec!["provider/openrouter".to_string()]);

        let loaded = repo.load_all().await.unwrap();
        assert_eq!(loaded[0].api_key.as_deref(), Some("sk-or-secret"));
        assert_eq!(loaded[1].api_key, None);
    }

    /// The AGE-741 "done when": an existing plaintext `providers.json` has no
    /// key left in it after the first start, and the key still works.
    #[tokio::test]
    async fn plaintext_provider_key_moves_to_the_store_on_first_load() {
        let dir = tempfile::tempdir().unwrap();
        let (path, store, repo) = providers(&dir);
        std::fs::write(
            &path,
            r#"[{"name":"openrouter","provider_type":"open_router","api_key":"sk-legacy"},
                {"name":"azure","provider_type":"azure_openai","api_key":"az-legacy","base_url":"https://x.openai.azure.com"}]"#,
        )
        .unwrap();

        let loaded = repo.load_all().await.unwrap();
        assert_eq!(loaded[0].api_key.as_deref(), Some("sk-legacy"));
        assert_eq!(loaded[1].api_key.as_deref(), Some("az-legacy"));

        let file = read(&path);
        assert!(!file.contains("legacy"), "plaintext left behind: {file}");
        assert!(!file.contains("\"api_key\""), "{file}");
        assert_eq!(
            store.get("provider/openrouter").unwrap().as_deref(),
            Some("sk-legacy")
        );
        assert_eq!(
            store.get("provider/azure").unwrap().as_deref(),
            Some("az-legacy")
        );

        // The second start reads the references; nothing is rewritten.
        let again = repo.load_all().await.unwrap();
        assert_eq!(again[0].api_key.as_deref(), Some("sk-legacy"));
        assert_eq!(read(&path), file);
    }

    #[tokio::test]
    async fn hive_tokens_move_to_the_store_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive_settings.json");
        let store = Arc::new(InMemorySecretStore::new());
        let repo = HiveSettingsJsonRepository::with_path_and_store(path.clone(), store.clone());
        std::fs::write(
            &path,
            r#"{"registry_url":"https://hive.example","runner_url":"https://runner.example","token":"jwt-old","refresh_token":"rt-old","username":"marcel"}"#,
        )
        .unwrap();

        let loaded = repo.load().await.unwrap();
        assert_eq!(loaded.token.as_deref(), Some("jwt-old"));
        assert_eq!(loaded.refresh_token.as_deref(), Some("rt-old"));
        let file = read(&path);
        assert!(
            !file.contains("jwt-old") && !file.contains("rt-old"),
            "{file}"
        );
        assert!(file.contains("\"token_ref\": \"hive/token\""), "{file}");
        assert!(
            file.contains("\"refresh_token_ref\": \"hive/refresh_token\""),
            "{file}"
        );

        // A refresh rotates both tokens; logout clears them from the store.
        repo.save(HiveSettingsModel {
            token: Some("jwt-new".into()),
            refresh_token: Some("rt-new".into()),
            ..loaded.clone()
        })
        .await
        .unwrap();
        assert_eq!(store.get("hive/token").unwrap().as_deref(), Some("jwt-new"));
        assert_eq!(
            repo.load().await.unwrap().refresh_token.as_deref(),
            Some("rt-new")
        );

        repo.save(HiveSettingsModel {
            token: None,
            refresh_token: None,
            ..loaded
        })
        .await
        .unwrap();
        assert!(store.keys().is_empty(), "{:?}", store.keys());
        assert_eq!(repo.load().await.unwrap().token, None);
    }

    #[tokio::test]
    async fn removed_or_renamed_provider_secret_is_deleted_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let (_path, store, repo) = providers(&dir);
        let a = ProviderConfig::new("a".into(), ProviderType::OpenRouter).with_api_key("ka".into());
        let b = ProviderConfig::new("b".into(), ProviderType::OpenRouter).with_api_key("kb".into());
        repo.save_all(vec![a.clone(), b]).await.unwrap();
        assert_eq!(store.keys(), vec!["provider/a", "provider/b"]);

        let mut renamed = a;
        renamed.name = "a2".into();
        repo.save_all(vec![renamed]).await.unwrap();
        assert_eq!(store.keys(), vec!["provider/a2"]);
        assert_eq!(store.get("provider/a2").unwrap().as_deref(), Some("ka"));

        repo.save_all(vec![]).await.unwrap();
        assert!(store.keys().is_empty(), "{:?}", store.keys());
    }

    #[tokio::test]
    async fn mcp_and_a2a_tokens_live_in_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let mcp_path = dir.path().join("mcp_servers.json");
        let a2a_path = dir.path().join("a2a_agents.json");
        let mcp = JsonMcpRepository::with_path_and_store(mcp_path.clone(), store.clone());
        let a2a = A2aJsonRepository::with_path_and_store(a2a_path.clone(), store.clone());

        mcp.save_all(vec![McpServerConfig {
            name: "github".into(),
            url: "https://mcp.example/mcp".into(),
            api_key: Some("ghp-secret".into()),
            enabled: true,
            is_module: false,
        }])
        .await
        .unwrap();
        a2a.save_all(vec![A2aAgentConfig {
            name: "voucher".into(),
            url: "https://hive.dev/a2a/voucher".into(),
            api_key: Some("a2a-secret".into()),
            enabled: true,
            skills: vec![],
            allow_private_network: false,
        }])
        .await
        .unwrap();

        assert!(!read(&mcp_path).contains("ghp-secret"));
        assert!(!read(&a2a_path).contains("a2a-secret"));
        assert_eq!(store.keys(), vec!["a2a/voucher", "mcp/github"]);
        assert_eq!(
            mcp.load_all().await.unwrap()[0].api_key.as_deref(),
            Some("ghp-secret")
        );
        assert_eq!(
            a2a.load_all().await.unwrap()[0].api_key.as_deref(),
            Some("a2a-secret")
        );
    }

    /// A reference whose secret is gone (keychain wiped, file copied to a new
    /// machine) loads as "no key" instead of failing the whole file.
    #[tokio::test]
    async fn missing_secret_loads_as_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _store, repo) = providers(&dir);
        std::fs::write(
            &path,
            r#"[{"name":"openrouter","provider_type":"open_router","api_key_ref":"provider/openrouter"}]"#,
        )
        .unwrap();

        let loaded = repo.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].api_key, None);
    }

    /// Overlapping saves (a key field saves per keystroke) leave the store
    /// holding the value of the last save *called* (AGE-562 ordering).
    #[tokio::test]
    async fn overlapping_saves_leave_the_last_key_in_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let (_path, store, repo) = providers(&dir);
        let saves: Vec<_> = (0..20)
            .map(|i| {
                repo.save_all(vec![
                    ProviderConfig::new("p".into(), ProviderType::OpenRouter)
                        .with_api_key(format!("k{i}")),
                ])
            })
            .collect();
        let results = futures::future::join_all(saves.into_iter().rev()).await;
        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
        assert_eq!(store.get("provider/p").unwrap().as_deref(), Some("k19"));
        assert_eq!(
            repo.load_all().await.unwrap()[0].api_key.as_deref(),
            Some("k19")
        );
    }
}
