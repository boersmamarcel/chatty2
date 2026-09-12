//! Resolving which model endpoint a broker worker talks to, and how many
//! workers may run against it at once (ADR-0011 C6).
//!
//! The endpoint's budget type itself (`EndpointBudget`) lives in
//! `chatty-protocol-gateway`, which this crate does not depend on — that
//! crate's optional `worker` feature depends back on `chatty-core`, so the
//! edge stays acyclic. This resolves the decision (which endpoint, what
//! limit) and leaves wrapping it in an `EndpointBudget` to the caller;
//! `services::virtual_agents` asks it once per declared agent (ADR-0011
//! C10), and both frontends' broker wiring wrap the answers (AGE-376).

use crate::settings::models::ModuleSettingsModel;
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};
use crate::settings::models::providers_store::ProviderConfig;

/// The endpoint a worker spawned with `--model <model>` (or without, for
/// `None`) will talk to, and how many workers may hold it at once.
///
/// A worker resolves its model exactly as `chatty-tui` does from that flag
/// ([`resolve_model_query`]), so the endpoint to meter is that model's
/// provider's. A model that resolves to nothing means nothing to meter — the
/// delegation would fail in the child anyway.
///
/// The limit is the provider's own parallel-request setting where it is
/// known, an explicit per-endpoint override where there is one, and
/// otherwise the configured default.
pub fn resolve_worker_endpoint(
    models: &[ModelConfig],
    providers: &[ProviderConfig],
    module_settings: &ModuleSettingsModel,
    model: Option<&str>,
) -> Option<(String, usize)> {
    let model = resolve_model_query(models, model)?;
    let provider = providers
        .iter()
        .find(|p| p.provider_type == model.provider_type)?;

    let endpoint = provider.endpoint_key();
    let limit = module_settings.endpoint_budget(&endpoint, provider.parallel_requests());
    Some((endpoint, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::providers_store::ProviderType;

    fn model(provider_type: ProviderType) -> ModelConfig {
        ModelConfig::new(
            "m1".to_string(),
            "Model One".to_string(),
            provider_type,
            "vendor/model-one".to_string(),
        )
    }

    #[test]
    fn no_models_means_nothing_to_meter() {
        let providers = vec![ProviderConfig::new("p".to_string(), ProviderType::Ollama)];
        let settings = ModuleSettingsModel::default();
        assert!(resolve_worker_endpoint(&[], &providers, &settings, None).is_none());
    }

    #[test]
    fn no_matching_provider_means_nothing_to_meter() {
        let models = vec![model(ProviderType::Ollama)];
        let providers = vec![ProviderConfig::new(
            "p".to_string(),
            ProviderType::AzureOpenAI,
        )];
        let settings = ModuleSettingsModel::default();
        assert!(resolve_worker_endpoint(&models, &providers, &settings, None).is_none());
    }

    #[test]
    fn resolves_the_first_models_provider_endpoint_and_default_limit() {
        let models = vec![model(ProviderType::Ollama)];
        let mut provider = ProviderConfig::new("p".to_string(), ProviderType::Ollama);
        provider.base_url = Some("http://localhost:11434".to_string());
        let providers = vec![provider];
        let settings = ModuleSettingsModel::default();

        let (endpoint, limit) = resolve_worker_endpoint(&models, &providers, &settings, None)
            .expect("a model and its provider resolve");
        assert_eq!(endpoint, "http://localhost:11434");
        assert_eq!(limit, settings.default_endpoint_budget);
    }

    /// ADR-0011 C10: a declared agent's `--model` decides the endpoint, so
    /// a reviewer on another server is metered on that server, not on the
    /// roster head's.
    #[test]
    fn a_named_model_resolves_its_own_providers_endpoint() {
        let models = vec![
            model(ProviderType::Ollama),
            ModelConfig::new(
                "m2".to_string(),
                "Model Two".to_string(),
                ProviderType::OpenRouter,
                "vendor/model-two".to_string(),
            ),
        ];
        let mut ollama = ProviderConfig::new("o".to_string(), ProviderType::Ollama);
        ollama.base_url = Some("http://localhost:11434".to_string());
        let mut router = ProviderConfig::new("r".to_string(), ProviderType::OpenRouter);
        router.base_url = Some("http://x/v1".to_string());
        let providers = vec![ollama, router];
        let settings = ModuleSettingsModel::default();

        let (endpoint, _) = resolve_worker_endpoint(&models, &providers, &settings, Some("m2"))
            .expect("the named model resolves");
        assert_eq!(endpoint, "http://x/v1");
        assert!(
            resolve_worker_endpoint(&models, &providers, &settings, Some("no-such")).is_none(),
            "a model the child cannot resolve is nothing to meter"
        );
    }

    #[test]
    fn an_explicit_endpoint_override_beats_the_default() {
        let models = vec![model(ProviderType::Ollama)];
        let mut provider = ProviderConfig::new("p".to_string(), ProviderType::Ollama);
        provider.base_url = Some("http://localhost:11434".to_string());
        let providers = vec![provider];
        let mut settings = ModuleSettingsModel::default();
        settings
            .endpoint_budgets
            .insert("http://localhost:11434".to_string(), 4);

        let (_, limit) = resolve_worker_endpoint(&models, &providers, &settings, None)
            .expect("a model and its provider resolve");
        assert_eq!(limit, 4);
    }
}
