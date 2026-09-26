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
    /// Attach a short query-relevant extract of the page itself to the top
    /// search results, so a fact on the page often needs no `fetch` round
    /// trip. The pages are read like `fetch` reads them (same address
    /// filtering, session page cache); off = titles and snippets only.
    #[serde(default = "default_true")]
    pub page_extracts: bool,
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
            page_extracts: true,
            browser_use_enabled: true,
            browser_use_api_key: None,
            daytona_enabled: true,
            daytona_api_key: None,
            rerank_url: None,
            rerank_model: None,
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
        assert!(loaded.page_extracts, "page extracts default to on");
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
