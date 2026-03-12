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

/// Settings for the web search tool
#[derive(Clone, Serialize, Deserialize)]
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
}

fn default_max_results() -> usize {
    5
}

impl Default for SearchSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            active_provider: SearchProvider::default(),
            tavily_api_key: None,
            brave_api_key: None,
            max_results: default_max_results(),
        }
    }
}
