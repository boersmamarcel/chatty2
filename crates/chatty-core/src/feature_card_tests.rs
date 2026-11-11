use super::*;
use crate::agent_spec::{
    AgentSection, BudgetSection, Grant, PluginSpec, SwarmSection, ToolsSection,
};
use crate::services::a2a_client::A2aClient;
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

const SCHEMA: &str = include_str!("../tests/fixtures/a2a-agent-card.schema.json");

const PROVIDER_KEY: &str = "sk-live-provider-9f3a1c77";
const MCP_KEY: &str = "mcp-live-key-51d2e8b0";
const PLUGIN_SECRET: &str = "plugin-config-secret-0badc0de";

fn model() -> ModelConfig {
    let mut model = ModelConfig::new(
        "m1".into(),
        "Reviewer model".into(),
        ProviderType::OpenRouter,
        "vendor/reviewer-1".into(),
    );
    model.cost_per_million_input_tokens = Some(3.0);
    model.cost_per_million_output_tokens = Some(15.0);
    model
}

fn book(models: &[ModelConfig]) -> PriceBook {
    let mut book = PriceBook::default();
    for model in models {
        if let Some(pricing) = model.token_pricing() {
            book.insert(model.model_ref(), pricing);
        }
    }
    book
}

fn spec() -> AgentSpec {
    AgentSpec {
        agent: AgentSection {
            name: "local-reviewer".into(),
            description: Some("Reviews a worker's branch".into()),
            model: Some("vendor/reviewer-1".into()),
            preamble: Some("Verify, do not trust.".into()),
            ..AgentSection::default()
        },
        tools: ToolsSection {
            profile: Some("reviewer".into()),
            skills: vec!["coder-reviewer".into()],
            ..ToolsSection::default()
        },
        plugins: vec![PluginSpec {
            module: "benford".into(),
            version: Some("^0.2".into()),
            grants: vec![Grant::Llm],
            config: [("token".to_string(), PLUGIN_SECRET.to_string())].into(),
            ..PluginSpec::default()
        }],
        swarm: SwarmSection {
            delegates_to: vec!["local-coder".into()],
            callers: Some(vec!["coding-lead".into()]),
            ..SwarmSection::default()
        },
        budget: BudgetSection {
            max_agent_turns: Some(20),
            max_duration: Some("30m".into()),
            cap_usd: Some(2.0),
        },
    }
}

fn card() -> FeatureCard {
    let models = [model()];
    FeatureCard::from_spec(
        &spec(),
        "http://127.0.0.1:1/a2a/local-reviewer",
        &models,
        &book(&models),
    )
}

#[tokio::test]
async fn feature_card_is_valid_a2a() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let json = card().to_json();
    let errors: Vec<String> = validator
        .iter_errors(&json)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "card does not validate: {errors:?}");

    let extension = &json["capabilities"]["extensions"][0];
    assert_eq!(extension["uri"], EXTENSION_URI);
    let params = &extension["params"];
    assert_eq!(params["model"], "vendor/reviewer-1");
    assert_eq!(params["profile"], "reviewer");
    assert_eq!(params["plugins"][0]["module"], "benford");
    assert_eq!(params["plugins"][0]["version"], "^0.2");
    assert_eq!(params["plugins"][0]["grants"][0], "llm");
    assert_eq!(params["skills"][0], "coder-reviewer");
    assert_eq!(params["budget"]["cap_usd"], 2.0);
    assert_eq!(params["delegates_to"][0], "local-coder");
    assert_eq!(params["callers"][0], "coding-lead");
    assert_eq!(params["price_hint_usd_per_mtok"]["in"], 3.0);
    assert_eq!(params["price_hint_usd_per_mtok"]["out"], 15.0);

    // And the client that reads remote cards reads this one.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = json.to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    });

    let config = A2aAgentConfig {
        name: "local-reviewer".into(),
        url: format!("http://127.0.0.1:{port}"),
        api_key: None,
        enabled: true,
        skills: vec![],
        allow_private_network: false,
    };
    let parsed = A2aClient::new().fetch_agent_card(&config).await.unwrap();
    assert_eq!(parsed.name, "local-reviewer");
    assert_eq!(parsed.description, "Reviews a worker's branch");
    assert_eq!(parsed.skills, vec!["coder-reviewer".to_string()]);
    assert!(parsed.supports_streaming);
}

#[test]
fn feature_card_has_no_secrets() {
    // The settings an agent runs against hold real keys; the card is built
    // from the spec and the model's name and prices only.
    let mut provider = ProviderConfig::new("OpenRouter".into(), ProviderType::OpenRouter);
    provider.api_key = Some(PROVIDER_KEY.into());
    let mcp = McpServerConfig {
        name: "github".into(),
        url: "http://localhost:3000/mcp".into(),
        api_key: Some(MCP_KEY.into()),
        enabled: true,
        is_module: false,
    };
    assert!(provider.api_key.is_some() && mcp.has_api_key());

    let text = card().to_json().to_string();
    for secret in [PROVIDER_KEY, MCP_KEY, PLUGIN_SECRET] {
        assert!(!text.contains(secret), "the card leaks {secret}");
    }
    let lower = text.to_lowercase();
    assert!(!lower.contains("api_key") && !lower.contains("bearer"));
    assert!(
        !text.contains("Verify, do not trust."),
        "the preamble is not on the card"
    );
}

#[test]
fn feature_card_round_trips() {
    let card = card();
    let text = serde_json::to_string(&card).unwrap();
    let back: FeatureCard = serde_json::from_str(&text).unwrap();
    assert_eq!(back, card);
    assert_eq!(back.feature_params().unwrap().plugins[0].module, "benford");
}

#[test]
fn an_unpriced_model_has_no_price_hint() {
    let mut unpriced = model();
    unpriced.cost_per_million_output_tokens = None;
    let models = [unpriced];
    let card = FeatureCard::from_spec(&spec(), "http://x/a2a/y", &models, &book(&models));
    assert_eq!(card.feature_params().unwrap().price_hint_usd_per_mtok, None);
}

#[test]
fn the_card_is_a_1_0_card() {
    let json = card().to_json();
    let interface = &json["supportedInterfaces"][0];
    assert_eq!(interface["protocolBinding"], "JSONRPC");
    assert_eq!(interface["protocolVersion"], "1.0");
    assert_eq!(interface["url"], "http://127.0.0.1:1/a2a/local-reviewer");
    assert!(json.get("protocolVersion").is_none());
}

#[test]
fn json_by_name_drops_credentials_from_the_base_url() {
    let models = [model()];
    let cards = FeatureCard::json_by_name(
        &[spec()],
        "https://user:hunter2@example.com:8443/base/?token=abc123#frag",
        &models,
        &book(&models),
    );
    let text = cards["local-reviewer"].to_string();
    for leaked in ["user", "hunter2", "token", "abc123", "frag"] {
        assert!(!text.contains(leaked), "the card leaks {leaked}: {text}");
    }
    assert_eq!(
        cards["local-reviewer"]["supportedInterfaces"][0]["url"],
        "https://example.com:8443/base/a2a/local-reviewer"
    );
}
