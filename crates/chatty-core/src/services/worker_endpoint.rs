//! Resolving which model endpoint the broker's local workers share, and how
//! many of them may run against it at once (ADR-0011 C6).
//!
//! The endpoint's budget type itself (`EndpointBudget`) lives in
//! `chatty-protocol-gateway`, which this crate does not depend on — that
//! crate's optional `worker` feature depends back on `chatty-core`, so the
//! edge stays acyclic. This resolves the decision (which endpoint, what
//! limit) and leaves wrapping it in an `EndpointBudget` to the caller;
//! chatty-gpui's `broker_runner::worker_endpoint` and chatty-tui's
//! `--broker` wiring both do that in a couple of lines (AGE-376).

use crate::settings::models::ModuleSettingsModel;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;

/// The endpoint every worker will share, and how many may hold it at once.
///
/// A worker resolves its own model exactly as `chatty-tui` does when it is
/// spawned without `--model`: the first model in the roster. So the endpoint
/// to meter is that model's provider's, and no configured model means
/// nothing to meter — the delegation would fail in the child anyway.
///
/// The limit is the provider's own parallel-request setting where it is
/// known, an explicit per-endpoint override where there is one, and
/// otherwise the configured default.
pub fn resolve_worker_endpoint(
    models: &[ModelConfig],
    providers: &[ProviderConfig],
    module_settings: &ModuleSettingsModel,
) -> Option<(String, usize)> {
    let model = models.first()?;
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
        assert!(resolve_worker_endpoint(&[], &providers, &settings).is_none());
    }

    #[test]
    fn no_matching_provider_means_nothing_to_meter() {
        let models = vec![model(ProviderType::Ollama)];
        let providers = vec![ProviderConfig::new(
            "p".to_string(),
            ProviderType::AzureOpenAI,
        )];
        let settings = ModuleSettingsModel::default();
        assert!(resolve_worker_endpoint(&models, &providers, &settings).is_none());
    }

    #[test]
    fn resolves_the_first_models_provider_endpoint_and_default_limit() {
        let models = vec![model(ProviderType::Ollama)];
        let mut provider = ProviderConfig::new("p".to_string(), ProviderType::Ollama);
        provider.base_url = Some("http://localhost:11434".to_string());
        let providers = vec![provider];
        let settings = ModuleSettingsModel::default();

        let (endpoint, limit) = resolve_worker_endpoint(&models, &providers, &settings)
            .expect("a model and its provider resolve");
        assert_eq!(endpoint, "http://localhost:11434");
        assert_eq!(limit, settings.default_endpoint_budget);
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

        let (_, limit) = resolve_worker_endpoint(&models, &providers, &settings)
            .expect("a model and its provider resolve");
        assert_eq!(limit, 4);
    }
}
