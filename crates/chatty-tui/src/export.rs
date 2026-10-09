//! Conversation export from the CLI (AGE-860): `--export-atif`,
//! `/export` and `--export <id>`. All of them render
//! through `chatty_core::exporters::export_run`, the desktop's exporter.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chatty_core::exporters::{ExportRun, export_run};
use chatty_core::repositories::{ConversationData, ConversationRepository};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;

const REDACTED: &str = "[REDACTED]";

/// Values that must never reach an export: provider API keys and the
/// user's secrets. Short values are skipped; redacting them would mangle
/// ordinary text.
pub fn secret_values(
    providers: &[ProviderConfig],
    current: &ProviderConfig,
    user_secrets: &[(String, String)],
) -> Vec<String> {
    providers
        .iter()
        .chain(std::iter::once(current))
        .filter_map(|p| p.api_key.clone())
        .chain(user_secrets.iter().map(|(_, v)| v.clone()))
        .filter(|s| s.trim().len() >= 6)
        .collect()
}

/// Render a run and scrub every secret value from the text.
pub fn render(
    data: &ConversationData,
    model_config: Option<&ModelConfig>,
    secrets: &[String],
) -> Result<String> {
    let mut text = export_run(&ExportRun {
        root: data,
        model_config,
    })?;
    for secret in secrets {
        // The secret as it appears inside JSON strings is escaped.
        let escaped = serde_json::to_string(secret)?;
        let escaped = &escaped[1..escaped.len() - 1];
        text = text.replace(escaped, REDACTED).replace(secret, REDACTED);
    }
    Ok(text)
}

/// `<config dir>/chatty/exports/<id>.<ext>`, next to the desktop's exports.
pub fn default_path(conversation_id: &str) -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("cannot find the config directory")?
        .join("chatty")
        .join("exports");
    Ok(dir.join(format!("{conversation_id}.atif.json")))
}

/// Write `text` to `path`; `-` is stdout. Returns a description of where.
pub fn write(path: &Path, text: &str) -> Result<String> {
    if path == Path::new("-") {
        use std::io::Write;
        std::io::stdout().write_all(text.as_bytes())?;
        return Ok("stdout".to_string());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path.display().to_string())
}

/// `--export <id> [--out path]`: export a saved conversation.
pub async fn export_saved(
    repo: &dyn ConversationRepository,
    id: &str,
    out: Option<&Path>,
) -> Result<String> {
    let Some(data) = repo
        .load_one(id)
        .await
        .map_err(|e| anyhow::anyhow!("cannot load conversation {id}: {e}"))?
    else {
        bail!("no saved conversation with id {id}");
    };
    let text = render(&data, None, &[])?;
    let path = match out {
        Some(p) => p.to_path_buf(),
        None => default_path(id)?,
    };
    write(&path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::repositories::ConversationSqliteRepository;
    use chatty_core::repositories::store_conformance::sample_conversation;

    const KEY: &str = "sk-test-SECRET-1234567890";

    fn data_with_secret() -> ConversationData {
        let mut data = sample_conversation("conv-1", "Title", 100);
        data.message_history = serde_json::json!([
            {"role": "user", "content": [{"type": "text", "text": format!("my key is {KEY}")}]},
            {"role": "assistant", "content": [{"type": "text", "text": format!("noted {KEY}")}]}
        ])
        .to_string();
        data.system_traces = "[null,null]".into();
        data.message_timestamps = "[1700000000,1700000001]".into();
        data.message_feedback = "[null,null]".into();
        data.attachment_paths = "[[],[]]".into();
        data.regeneration_records = "[]".into();
        data
    }

    #[test]
    fn export_contains_no_secrets() {
        let text = render(&data_with_secret(), None, &[KEY.to_string()]).unwrap();
        assert!(!text.contains(KEY));
        assert!(text.contains(REDACTED));
        let mut provider = ProviderConfig::new(
            "p".into(),
            chatty_core::settings::models::providers_store::ProviderType::Ollama,
        );
        provider.api_key = Some(KEY.into());
        assert_eq!(secret_values(&[], &provider, &[]), vec![KEY.to_string()]);
    }

    #[test]
    fn headless_export_atif_writes_valid_atif() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.atif.json");
        write(&path, &render(&data_with_secret(), None, &[]).unwrap()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["schema_version"].as_str().unwrap().starts_with("ATIF"));
        assert_eq!(v["steps"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn export_saved_conversation_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let repo =
            ConversationSqliteRepository::deferred_with_path(dir.path().join("conversations.db"));
        repo.save("conv-1", data_with_secret()).await.unwrap();
        let out = dir.path().join("out.atif.json");
        export_saved(&repo, "conv-1", Some(&out)).await.unwrap();
        assert!(out.exists());
        assert!(export_saved(&repo, "nope", Some(&out)).await.is_err());
    }

    #[test]
    fn export_written_on_failed_run() {
        // A run that failed before the model answered has only the user turn.
        let mut data = data_with_secret();
        data.message_history = serde_json::json!([
            {"role": "user", "content": [{"type": "text", "text": "hi"}]}
        ])
        .to_string();
        data.system_traces = "[null]".into();
        data.message_timestamps = "[1700000000]".into();
        data.message_feedback = "[null]".into();
        data.attachment_paths = "[[]]".into();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("failed.atif.json");
        write(&path, &render(&data, None, &[]).unwrap()).unwrap();
        assert!(path.exists());
    }
}
