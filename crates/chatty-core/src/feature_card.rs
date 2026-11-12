//! The feature card (RO-2, AGE-659, vault `fabric-roster.md` §3.2): an A2A
//! `AgentCard` for one [`AgentSpec`], plus one entry in
//! `capabilities.extensions[]` under [`EXTENSION_URI`] that says what the
//! spec wires: model, profile, plugins and their grants, skills, budget,
//! who it may call and who may call it, and a price hint.
//!
//! A card is built from the spec's declared names and numbers only. It
//! never reads a provider, an MCP server, a plugin's `config` or the
//! preamble, so no key can reach it (CLAUDE.md, "MCP API Key Masking").

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agent_spec::AgentSpec;
use crate::models::token_usage::PriceBook;
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};

/// The URI the feature card extension is declared under.
pub const EXTENSION_URI: &str = "https://chatty.dev/a2a/ext/feature-card/v1";

/// An A2A agent card with the chatty extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureCard {
    pub name: String,
    pub description: String,
    pub version: String,
    /// Required by A2A 1.0: where and how the agent is reached.
    pub supported_interfaces: Vec<AgentInterface>,
    /// Not in 1.0; kept for readers of the 0.3 card.
    pub url: String,
    pub capabilities: Capabilities,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<CardSkill>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    pub protocol_binding: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub extensions: Vec<CardExtension>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardExtension {
    pub uri: String,
    pub description: String,
    pub required: bool,
    pub params: FeatureParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// The extension's `params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureParams {
    /// The model the spec names, or the roster's default; `None` when the
    /// roster has no model to resolve it to.
    pub model: Option<String>,
    pub profile: Option<String>,
    pub plugins: Vec<CardPlugin>,
    pub skills: Vec<String>,
    pub budget: CardBudget,
    pub delegates_to: Vec<String>,
    /// `None`: anyone may call it. `Some`: only these.
    pub callers: Option<Vec<String>>,
    /// `None` when the model has no input and output price in the book.
    pub price_hint_usd_per_mtok: Option<PriceHint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardPlugin {
    pub module: String,
    pub version: Option<String>,
    pub grants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardBudget {
    pub max_agent_turns: Option<u32>,
    pub max_duration: Option<String>,
    pub cap_usd: Option<f64>,
}

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriceHint {
    #[serde(rename = "in")]
    pub input: f64,
    pub out: f64,
}

impl FeatureCard {
    /// The card for `spec`, served at `url`. `models` resolves the spec's
    /// model and `book` prices it (AGE-682).
    pub fn from_spec(
        spec: &AgentSpec,
        url: &str,
        models: &[ModelConfig],
        book: &PriceBook,
    ) -> Self {
        let resolved = resolve_model_query(models, spec.agent.model.as_deref());
        let price_hint_usd_per_mtok = resolved
            .and_then(|model| book.pricing(&model.model_ref(), None))
            .map(|pricing| PriceHint {
                input: pricing.input_per_million,
                out: pricing.output_per_million,
            });
        let skills = spec.tools.skills.clone();
        let params = FeatureParams {
            model: resolved
                .map(|model| model.model_identifier.clone())
                .or_else(|| spec.agent.model.clone()),
            profile: spec.tools.profile.clone(),
            plugins: spec
                .plugins
                .iter()
                .map(|plugin| CardPlugin {
                    module: plugin.module.clone(),
                    version: plugin.version.clone(),
                    grants: plugin.grants.iter().cloned().map(String::from).collect(),
                })
                .collect(),
            skills: skills.clone(),
            budget: CardBudget {
                max_agent_turns: spec.budget.max_agent_turns,
                max_duration: spec.budget.max_duration.clone(),
                cap_usd: spec.budget.cap_usd,
            },
            delegates_to: spec.swarm.delegates_to.clone(),
            callers: spec.swarm.callers.clone(),
            price_hint_usd_per_mtok,
        };
        Self {
            name: spec.agent.name.clone(),
            description: spec.agent.description.clone().unwrap_or_default(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            supported_interfaces: vec![AgentInterface {
                url: url.to_string(),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "1.0".to_string(),
            }],
            url: url.to_string(),
            capabilities: Capabilities {
                streaming: true,
                extensions: vec![CardExtension {
                    uri: EXTENSION_URI.to_string(),
                    description: "What this chatty agent is wired with".to_string(),
                    required: false,
                    params,
                }],
            },
            default_input_modes: vec!["text/plain".to_string()],
            default_output_modes: vec!["text/plain".to_string()],
            skills: skills
                .into_iter()
                .map(|skill| CardSkill {
                    id: skill.clone(),
                    name: skill.clone(),
                    description: skill,
                    tags: Vec::new(),
                })
                .collect(),
        }
    }

    /// The extension entry, when this card carries one.
    pub fn feature_params(&self) -> Option<&FeatureParams> {
        self.capabilities
            .extensions
            .iter()
            .find(|extension| extension.uri == EXTENSION_URI)
            .map(|extension| &extension.params)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a card is plain data")
    }

    /// One card per spec, keyed by name, as the gateway serves them.
    /// `base_url` is stripped to scheme, host, port and path: userinfo
    /// (`user:pass@`), a query and a fragment never reach a card.
    pub fn json_by_name(
        specs: &[AgentSpec],
        base_url: &str,
        models: &[ModelConfig],
        book: &PriceBook,
    ) -> BTreeMap<String, serde_json::Value> {
        let base_url = credential_free(base_url);
        specs
            .iter()
            .map(|spec| {
                let url = format!("{}/a2a/{}", base_url, spec.agent.name);
                (
                    spec.agent.name.clone(),
                    Self::from_spec(spec, &url, models, book).to_json(),
                )
            })
            .collect()
    }
}

/// `url` without userinfo, query or fragment, and without a trailing `/`.
/// Text that is not a URL is cut at the first `?` or `#` and loses anything
/// before an `@` in its authority.
fn credential_free(url: &str) -> String {
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    let url = &url[..cut];
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), url),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}{host}{path}")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
#[path = "feature_card_tests.rs"]
mod tests;
