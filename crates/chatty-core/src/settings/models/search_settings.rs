use serde::{Deserialize, Serialize};

/// Available web search providers
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum SearchProvider {
    #[default]
    Tavily,
    Brave,
}

impl std::fmt::Display for SearchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchProvider::Tavily => write!(f, "Tavily"),
            SearchProvider::Brave => write!(f, "Brave"),
        }
    }
}

/// A search endpoint that is not the provider's public API, with the bearer
/// token it takes in place of the user's key. A hosted guest gets the lease
/// proxy here (AGE-819/AGE-825), which holds the platform's key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ManagedSearch {
    /// Which provider's request shape the endpoint speaks.
    pub provider: SearchProvider,
    /// Base URL; the provider's own path (`/search` for Tavily,
    /// `/res/v1/web/search` for Brave) is appended to it.
    pub endpoint: String,
    /// Sent where the provider's key would go.
    pub token: String,
}

impl ManagedSearch {
    /// The endpoint and token, when both are non-empty.
    pub fn usable(&self) -> Option<(&str, &str)> {
        let endpoint = self.endpoint.trim().trim_end_matches('/');
        let token = self.token.trim();
        (!endpoint.is_empty() && !token.is_empty()).then_some((endpoint, token))
    }
}

/// Settings for the web search tool and other external services
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchSettingsModel {
    /// Master toggle for web search
    #[serde(default)]
    pub enabled: bool,
    /// Which search provider to use
    #[serde(default)]
    pub active_provider: SearchProvider,
    /// API key for Tavily Search
    #[serde(default)]
    pub tavily_api_key: Option<String>,
    /// API key for Brave Search
    #[serde(default)]
    pub brave_api_key: Option<String>,
    /// Maximum number of search results to return
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Whether browser-use cloud automation is enabled.
    /// Defaults to `true` so that setting an API key is sufficient to activate the tool.
    /// Set to `false` to explicitly disable without removing the key.
    #[serde(default = "default_true")]
    pub browser_use_enabled: bool,
    /// API key for browser-use cloud service (https://browser-use.com)
    #[serde(default)]
    pub browser_use_api_key: Option<String>,
    /// Whether Daytona cloud sandbox execution is enabled.
    /// Defaults to `true` so that setting an API key is sufficient to activate the tool.
    /// Set to `false` to explicitly disable without removing the key.
    #[serde(default = "default_true")]
    pub daytona_enabled: bool,
    /// API key for Daytona cloud service (https://app.daytona.io)
    #[serde(default)]
    pub daytona_api_key: Option<String>,
    /// Cohere/Jina-style `/rerank` endpoint (vLLM `--task score`, llama.cpp
    /// `--reranking`) that orders keyless search results with a
    /// cross-encoder. Used only when `rerank_model` is set too and no API
    /// provider is active.
    #[serde(default)]
    pub rerank_url: Option<String>,
    /// Model name sent to `rerank_url`, e.g. `BAAI/bge-reranker-v2-m3`.
    #[serde(default)]
    pub rerank_model: Option<String>,
    /// Search through this endpoint and token instead of the public provider
    /// API and the user's key. Unset (the desktop default) changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedSearch>,
}

impl SearchSettingsModel {
    /// The configured reranker as `(url, model)`, when both are non-empty.
    pub fn reranker(&self) -> Option<(&str, &str)> {
        let url = self.rerank_url.as_deref().map(str::trim)?;
        let model = self.rerank_model.as_deref().map(str::trim)?;
        (!url.is_empty() && !model.is_empty()).then_some((url, model))
    }
}

fn default_max_results() -> usize {
    5
}

fn default_true() -> bool {
    true
}

impl Default for SearchSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            active_provider: SearchProvider::default(),
            tavily_api_key: None,
            brave_api_key: None,
            max_results: default_max_results(),
            browser_use_enabled: true,
            browser_use_api_key: None,
            daytona_enabled: true,
            daytona_api_key: None,
            rerank_url: None,
            rerank_model: None,
            managed: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_saved_before_the_rerank_fields_still_load() {
        let loaded: SearchSettingsModel =
            serde_json::from_str(r#"{"enabled":true,"max_results":7}"#).unwrap();
        assert_eq!(loaded.rerank_url, None);
        assert_eq!(loaded.rerank_model, None);
        assert_eq!(loaded.reranker(), None);
    }

    #[test]
    fn managed_block_loads_and_unset_is_not_written() {
        let loaded: SearchSettingsModel = serde_json::from_str(
            r#"{"enabled":true,"managed":{"provider":"Tavily","endpoint":"http://172.16.0.1:8080/api/egress/managed/tavily_search/","token":"t"}}"#,
        )
        .unwrap();
        let managed = loaded.managed.unwrap();
        assert_eq!(
            managed.usable(),
            Some((
                "http://172.16.0.1:8080/api/egress/managed/tavily_search",
                "t"
            ))
        );
        let json = serde_json::to_value(SearchSettingsModel::default()).unwrap();
        assert!(json.get("managed").is_none());
    }

    #[test]
    fn reranker_needs_both_url_and_model() {
        let mut settings = SearchSettingsModel {
            rerank_url: Some(" http://127.0.0.1:8001/rerank ".to_string()),
            ..Default::default()
        };
        assert_eq!(settings.reranker(), None);
        settings.rerank_model = Some("  ".to_string());
        assert_eq!(settings.reranker(), None);
        settings.rerank_model = Some("BAAI/bge-reranker-v2-m3".to_string());
        assert_eq!(
            settings.reranker(),
            Some(("http://127.0.0.1:8001/rerank", "BAAI/bge-reranker-v2-m3"))
        );
    }
}
