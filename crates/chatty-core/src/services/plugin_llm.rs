//! The host side of a WASM plugin's `llm::complete` (PL-H2, AGE-605).
//!
//! A plugin's LLM call is part of the calling agent's turn (PL-D1 option B),
//! so it goes through the same provider client an agent uses —
//! [`completion_model`](crate::factories::agent_factory) with its connect
//! retry, prompt caching and Azure auth layers — rather than a hand-built
//! HTTP request. Every frontend that hosts plugins (desktop, TUI broker)
//! builds one [`PluginLlmProvider`].
//!
//! Model selection: an empty `model` is the calling agent's model, given
//! when the provider is built. A named model must be one of the user's
//! configured models; anything else is refused before a request is sent.
//!
//! Usage: each call's provider usage is normalised
//! ([`normalize_usage`]) and both returned to the guest
//! (`completion-response.usage`) and appended to a [`PluginUsage`] the host
//! holds, so the calling turn can account for it.

use std::sync::{Arc, Mutex};

use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, Role, TokenUsage, ToolCall};
use rig_core::completion::{AssistantContent, CompletionRequestBuilder, ToolDefinition};
use tokio::runtime::Handle;
use tracing::debug;

use crate::factories::agent_factory::{completion_model, ensure_tools_reachable, request_params};
use crate::models::token_usage::{ApiCallUsage, ModelRef};
use crate::services::llm_service::normalize_usage;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;

/// Usage of every `llm::complete` call a [`PluginLlmProvider`] served, oldest
/// first, until the host takes it. Cloning shares the log.
#[derive(Clone, Debug, Default)]
pub struct PluginUsage(Arc<Mutex<Vec<ApiCallUsage>>>);

impl PluginUsage {
    /// Remove and return everything recorded so far.
    pub fn take(&self) -> Vec<ApiCallUsage> {
        std::mem::take(&mut *self.0.lock().expect("plugin usage lock"))
    }

    /// Append one call's usage, numbering it after those still held.
    fn record(&self, mut call: ApiCallUsage) {
        let mut calls = self.0.lock().expect("plugin usage lock");
        call.turn = u32::try_from(calls.len() + 1).unwrap_or(u32::MAX);
        calls.push(call);
    }
}

/// The [`LlmProvider`] a plugin host hands the WASM runtime.
pub struct PluginLlmProvider {
    /// The model an empty `model` argument means: the calling agent's.
    calling_model: ModelConfig,
    /// The user's configured models; the only ones a plugin may name.
    models: Vec<ModelConfig>,
    providers: Vec<ProviderConfig>,
    /// The runtime the provider's async client runs on (see
    /// [`LlmProvider::complete`] below).
    runtime: Handle,
    usage: PluginUsage,
}

impl PluginLlmProvider {
    /// A provider whose default is `calling_model`, allowed to reach any of
    /// `models` through the matching entry of `providers`. `runtime` drives
    /// the requests; capture it where the host has one (`Handle::current()`).
    pub fn new(
        calling_model: ModelConfig,
        models: Vec<ModelConfig>,
        providers: Vec<ProviderConfig>,
        runtime: Handle,
    ) -> Self {
        Self {
            calling_model,
            models,
            providers,
            runtime,
            usage: PluginUsage::default(),
        }
    }

    /// The log this provider records each call's usage into.
    pub fn usage(&self) -> PluginUsage {
        self.usage.clone()
    }

    /// The model config and provider config one call runs on.
    fn resolve(&self, model: &str) -> Result<(ModelConfig, ProviderConfig), String> {
        let model_config = if model.is_empty() || model == self.calling_model.model_identifier {
            &self.calling_model
        } else {
            self.models
                .iter()
                .find(|m| m.model_identifier == model || m.id == model)
                .ok_or_else(|| {
                    format!("llm::complete: model `{model}` is not one of the configured models")
                })?
        };
        let provider_config = self
            .providers
            .iter()
            .find(|p| p.provider_type == model_config.provider_type)
            .ok_or_else(|| {
                format!(
                    "llm::complete: no {:?} provider is configured for model `{}`",
                    model_config.provider_type, model_config.model_identifier
                )
            })?;
        Ok((model_config.clone(), provider_config.clone()))
    }
}

impl LlmProvider for PluginLlmProvider {
    /// Synchronous bridge to the async provider client.
    ///
    /// The runtime calls this off the async executor: on the guest call's
    /// `spawn_blocking` thread, or on the helper thread that enforces the
    /// call's deadline (PL-H1), which gives up waiting — and hands the guest
    /// `deadline exceeded` — when the call's remaining time runs out. Either
    /// way this thread is not driving a runtime, so `Handle::block_on` on the
    /// handle captured at construction is sound on any runtime flavour.
    /// `block_in_place` is not: it panics on a current-thread runtime and
    /// stalls a worker on a multi-thread one (F3).
    fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let (model_config, provider_config) = self.resolve(model)?;
        debug!(
            provider = ?provider_config.provider_type,
            model = %model_config.model_identifier,
            message_count = messages.len(),
            has_tools = tools.is_some(),
            "plugin llm::complete"
        );
        let started = std::time::Instant::now();
        let (response, mut usage) = self.runtime.block_on(send(
            &model_config,
            &provider_config,
            messages,
            tools.as_deref(),
        ))?;
        // A usage line is a fact (AGE-682): the model that served the call,
        // when it finished and how long it took; its price is read later.
        usage.model = Some(ModelRef {
            provider: provider_config.provider_type.clone(),
            model_id: model_config.model_identifier.clone(),
        });
        usage.at = Some(std::time::SystemTime::now());
        usage.duration_ms = started.elapsed().as_millis() as u64;
        self.usage.record(usage);
        Ok(response)
    }
}

/// Send one completion request and convert the reply for the guest.
async fn send(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    messages: Vec<Message>,
    tools: Option<&str>,
) -> Result<(CompletionResponse, ApiCallUsage), String> {
    let model = completion_model(model_config, provider_config)
        .map_err(|e| format!("llm::complete: {e:#}"))?;

    let mut history = to_rig_messages(messages);
    let prompt = history
        .pop()
        .ok_or_else(|| "llm::complete: no messages".to_string())?;

    let mut request = CompletionRequestBuilder::new(model, prompt).messages(history);
    if let Some(tools) = tools {
        ensure_tools_reachable(model_config, provider_config)
            .map_err(|e| format!("llm::complete: {e:#}"))?;
        request = request.tools(normalize_tools(tools));
    }
    if model_config.supports_temperature {
        request = request.temperature(model_config.temperature as f64);
    }
    if let Some(max_tokens) = model_config.max_tokens {
        request = request.max_tokens(max_tokens as u64);
    }
    if let Some(params) = request_params(model_config, &provider_config.provider_type) {
        request = request.additional_params(params);
    }

    let reply = request
        .send()
        .await
        .map_err(|e| format!("llm::complete: {e}"))?;

    let usage = normalize_usage(
        provider_config.provider_type.usage_semantics(),
        0,
        &reply.usage,
    );
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for part in reply.choice {
        match part {
            AssistantContent::Text(text) => content.push_str(&text.text),
            AssistantContent::ToolCall(call) => tool_calls.push(ToolCall {
                id: call
                    .provider
                    .as_ref()
                    .map(|p| p.call_id.clone())
                    .unwrap_or_else(|| call.id.to_string()),
                name: call.function.name,
                arguments: match call.function.arguments {
                    serde_json::Value::String(raw) => raw,
                    other => other.to_string(),
                },
            }),
            AssistantContent::Reasoning(_) | AssistantContent::Image(_) => {}
        }
    }
    let response = CompletionResponse {
        content,
        tool_calls,
        // The guest sees the whole prompt as its input; the host keeps the
        // cache split in `usage`.
        usage: Some(TokenUsage {
            input_tokens: usage.prompt_tokens(),
            output_tokens: usage.output_tokens,
        }),
    };
    Ok((response, usage))
}

/// The guest's conversation as rig messages. An `assistant` message keeps
/// the tool calls it made; a `tool` message is the result of the call its
/// `tool-call-id` names, under the tool name that call used.
fn to_rig_messages(messages: Vec<Message>) -> Vec<rig_core::completion::Message> {
    use rig_core::completion::Message as RigMessage;
    use rig_core::message::{ToolResultContent, UserContent};

    let mut tool_names = std::collections::HashMap::new();
    messages
        .into_iter()
        .map(|message| match message.role {
            Role::System => RigMessage::system(message.content),
            Role::User => RigMessage::user(message.content),
            Role::Assistant if message.tool_calls.is_empty() => {
                RigMessage::assistant(message.content)
            }
            Role::Assistant => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(AssistantContent::text(message.content));
                }
                for call in message.tool_calls {
                    tool_names.insert(call.id.clone(), call.name.clone());
                    let arguments = serde_json::from_str(&call.arguments)
                        .unwrap_or(serde_json::Value::String(call.arguments));
                    content.push(AssistantContent::tool_call(call.id, call.name, arguments));
                }
                RigMessage::Assistant { id: None, content }
            }
            Role::Tool => {
                let call_id = message.tool_call_id.unwrap_or_default();
                let name = tool_names.get(&call_id).cloned().unwrap_or_default();
                RigMessage::User {
                    content: vec![UserContent::tool_result_from_wire(
                        call_id,
                        name,
                        vec![ToolResultContent::text(message.content)],
                    )],
                }
            }
        })
        .collect()
}

/// Normalize a JSON tools blob from a WASM module into tool definitions.
///
/// Accepts either:
/// - OpenAI wrapped form: `[{"type":"function","function":{name,description,parameters}}]`
/// - Flat form: `[{name, description, parameters}]` (or `parameters_schema` /
///   `input_schema`)
/// - A single object instead of an array
///
/// Tools missing a `name` are dropped, as is a blob that is not JSON.
fn normalize_tools(tools_json: &str) -> Vec<ToolDefinition> {
    let parsed: serde_json::Value = match serde_json::from_str(tools_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(a) => a,
        v @ serde_json::Value::Object(_) => vec![v],
        _ => return Vec::new(),
    };
    arr.into_iter()
        .filter_map(|t| {
            let inner = t.get("function").cloned().unwrap_or(t);
            let name = inner.get("name")?.as_str()?.to_string();
            let description = inner
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = inner
                .get("parameters")
                .or_else(|| inner.get("parameters_schema"))
                .or_else(|| inner.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            Some(ToolDefinition {
                name,
                description,
                parameters,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// PL-E7 (AGE-602) rows 4.1 and 4.2, moved here from chatty-gpui with the
// provider (PL-H2, AGE-605). Every request goes to a wiremock server; none
// reaches a real provider.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use chatty_wasm_runtime::test_support::fixture_path;
    use chatty_wasm_runtime::{
        Capability, ModuleManifest, ResourceLimits, ToolCallRequest, WasmModule,
    };
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::factories::agent_factory::openrouter_base_url;
    use crate::settings::models::providers_store::ProviderType;

    fn model(provider_type: ProviderType, identifier: &str) -> ModelConfig {
        let mut config = ModelConfig::new(
            identifier.to_string(),
            identifier.to_string(),
            provider_type,
            identifier.to_string(),
        );
        config.temperature = 0.0;
        config
    }

    fn provider_config(
        provider_type: ProviderType,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> ProviderConfig {
        let mut config = ProviderConfig::new("test".to_string(), provider_type);
        config.api_key = api_key.map(str::to_string);
        config.base_url = base_url.map(str::to_string);
        config
    }

    /// A provider whose calling model is `test-model` on `provider`.
    fn plugin_provider(provider: ProviderConfig, runtime: Handle) -> PluginLlmProvider {
        let calling = model(provider.provider_type.clone(), "test-model");
        PluginLlmProvider::new(calling.clone(), vec![calling], vec![provider], runtime)
    }

    fn one_message() -> Vec<Message> {
        vec![Message {
            role: Role::User,
            content: "hi".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        }]
    }

    fn ok_body() -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })
    }

    fn ollama_ok_body() -> serde_json::Value {
        serde_json::json!({
            "model": "test-model",
            "created_at": "2026-09-27T00:00:00Z",
            "message": {"role": "assistant", "content": "hi"},
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 1,
            "eval_count": 1,
        })
    }

    /// Call `complete` the way the WASM host does: from a blocking thread.
    async fn complete_off_executor(
        provider: PluginLlmProvider,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> (Result<CompletionResponse, String>, PluginLlmProvider) {
        let model = model.to_string();
        tokio::task::spawn_blocking(move || {
            let result = provider.complete(&model, messages, tools);
            (result, provider)
        })
        .await
        .expect("the blocking call does not panic")
    }

    // -- 4.1: URL + auth header per provider -------------------------------

    /// OpenRouter with no base URL uses `https://openrouter.ai/api/v1` as the
    /// API root and appends only `/chat/completions` (F3: the old client
    /// appended `/v1/chat/completions`, doubling the segment). The default
    /// itself cannot be called from a test, so the same-shaped root
    /// (`…/api/v1`) is served by wiremock and the path it receives checked.
    #[tokio::test(flavor = "multi_thread")]
    async fn row_4_1_openrouter_default_base_url_is_not_doubled() {
        assert_eq!(
            openrouter_base_url(&provider_config(
                ProviderType::OpenRouter,
                Some("sk-test"),
                None
            )),
            "https://openrouter.ai/api/v1"
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .and(header("Authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let root = format!("{}/api/v1", server.uri());
        let provider = plugin_provider(
            provider_config(ProviderType::OpenRouter, Some("sk-test"), Some(&root)),
            Handle::current(),
        );
        let (result, _) = complete_off_executor(provider, "", one_message(), None).await;
        assert_eq!(
            result
                .expect("wiremock answers /api/v1/chat/completions")
                .content,
            "hi"
        );
    }

    /// Ollama speaks its native `/api/chat` and sends no auth header.
    #[tokio::test(flavor = "multi_thread")]
    async fn row_4_1_ollama_posts_api_chat_without_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ollama_ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = plugin_provider(
            provider_config(ProviderType::Ollama, None, Some(&server.uri())),
            Handle::current(),
        );
        let (result, _) = complete_off_executor(provider, "", one_message(), None).await;
        assert_eq!(result.expect("wiremock answers /api/chat").content, "hi");

        let requests = server.received_requests().await.expect("recording on");
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "no api key configured, so no bearer header should be sent"
        );
    }

    /// A custom OpenRouter-compatible base URL is the API root, with a
    /// bearer auth header.
    #[tokio::test(flavor = "multi_thread")]
    async fn row_4_1_custom_base_url_and_bearer_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer sk-custom"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = plugin_provider(
            provider_config(
                ProviderType::OpenRouter,
                Some("sk-custom"),
                Some(&server.uri()),
            ),
            Handle::current(),
        );
        let (result, _) = complete_off_executor(provider, "", one_message(), None).await;
        assert_eq!(result.expect("wiremock should answer 200").content, "hi");
    }

    /// Azure OpenAI authenticates with an `api-key` header (never a bearer
    /// token) on the resource's Responses API (AGE-858).
    #[tokio::test(flavor = "multi_thread")]
    async fn row_4_1_azure_uses_api_key_header_not_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/v1/responses"))
            .and(header("api-key", "sk-azure"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_1", "object": "response", "created_at": 0,
                "status": "completed", "model": "test-model", "tools": [],
                "output": [{ "type": "message", "id": "msg_1", "role": "assistant",
                    "status": "completed",
                    "content": [{ "type": "output_text", "text": "hi", "annotations": [] }] }],
                "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 },
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = plugin_provider(
            provider_config(
                ProviderType::AzureOpenAI,
                Some("sk-azure"),
                Some(&server.uri()),
            ),
            Handle::current(),
        );
        let (result, _) = complete_off_executor(provider, "", one_message(), None).await;
        assert_eq!(
            result
                .expect("Azure should authenticate with api-key, not bearer")
                .content,
            "hi"
        );

        let requests = server.received_requests().await.expect("recording on");
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "an Azure API key must not also go out as a bearer token"
        );
    }

    // -- 4.2: current-thread runtime -----------------------------------

    /// The provider's runtime is a `current_thread` one, and `complete` is
    /// called the way the WASM host calls it — from a blocking thread while
    /// the runtime's only thread awaits the result. `block_in_place` panics
    /// unconditionally there; `Handle::block_on` must answer.
    #[test]
    fn row_4_2_current_thread_runtime_does_not_panic() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime should build");
        let result = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/chat"))
                .respond_with(ResponseTemplate::new(200).set_body_json(ollama_ok_body()))
                .expect(1)
                .mount(&server)
                .await;
            let provider = plugin_provider(
                provider_config(ProviderType::Ollama, None, Some(&server.uri())),
                Handle::current(),
            );
            tokio::task::spawn_blocking(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    LlmProvider::complete(&provider, "", one_message(), None)
                }))
            })
            .await
            .expect("the blocking task finishes")
        });
        let reply = result.expect(
            "complete panicked on a current-thread runtime (block_in_place always panics there)",
        );
        assert_eq!(reply.expect("wiremock answers").content, "hi");
    }

    // -- model selection, usage, tools --------------------------------------

    /// An empty `model` is the calling agent's model, not the first one in
    /// the roster; a model outside the roster is refused without a request.
    #[tokio::test(flavor = "multi_thread")]
    async fn empty_model_is_the_calling_agents_and_unknown_models_are_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(
                serde_json::json!({"model": "vendor/caller"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let first = model(ProviderType::OpenRouter, "vendor/first");
        let caller = model(ProviderType::OpenRouter, "vendor/caller");
        let provider = PluginLlmProvider::new(
            caller.clone(),
            vec![first, caller],
            vec![provider_config(
                ProviderType::OpenRouter,
                Some("sk"),
                Some(&server.uri()),
            )],
            Handle::current(),
        );
        let (result, provider) = complete_off_executor(provider, "", one_message(), None).await;
        result.expect("the calling agent's model is used");

        let (result, _) =
            complete_off_executor(provider, "vendor/not-configured", one_message(), None).await;
        let err = result.expect_err("a model outside the roster is refused");
        assert!(err.contains("not one of the configured models"), "{err}");
        // `.expect(1)` above: the refused call never reached the server.
    }

    /// Usage comes back to the guest (whole prompt as input) and into the
    /// host's log, normalised (cache reads split out of the input count).
    #[tokio::test(flavor = "multi_thread")]
    async fn usage_is_normalised_and_logged_for_the_host() {
        let server = MockServer::start().await;
        let mut body = ok_body();
        body["usage"] = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 7,
            "total_tokens": 107,
            "prompt_tokens_details": {"cached_tokens": 40},
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let provider = plugin_provider(
            provider_config(ProviderType::OpenRouter, Some("sk"), Some(&server.uri())),
            Handle::current(),
        );
        let log = provider.usage();
        let (result, provider) = complete_off_executor(provider, "", one_message(), None).await;
        let usage = result.expect("reply").usage.expect("usage is returned");
        assert_eq!((usage.input_tokens, usage.output_tokens), (100, 7));
        let (_, _) = complete_off_executor(provider, "", one_message(), None).await;

        let calls = log.take();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].turn, 1);
        assert_eq!(calls[1].turn, 2);
        assert_eq!(calls[0].input_tokens, 60);
        assert_eq!(calls[0].cache_read_tokens, 40);
        assert_eq!(calls[0].output_tokens, 7);
        // Each call names the model that served it and when (AGE-682).
        assert_eq!(
            calls[0].model,
            Some(ModelRef {
                provider: ProviderType::OpenRouter,
                model_id: "test-model".to_string(),
            })
        );
        assert!(calls[0].at.is_some());
        assert!(log.take().is_empty(), "take drains the log");
    }

    /// The guest's tools reach the request as function tools, and a tool
    /// call in the reply comes back with its id, name and raw arguments.
    #[tokio::test(flavor = "multi_thread")]
    async fn tools_go_out_and_tool_calls_come_back() {
        let server = MockServer::start().await;
        let mut body = ok_body();
        body["choices"][0]["message"] = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"},
            }],
        });
        body["choices"][0]["finish_reason"] = serde_json::json!("tool_calls");
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "tools": [{"type": "function", "function": {"name": "lookup"}}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let provider = plugin_provider(
            provider_config(ProviderType::OpenRouter, Some("sk"), Some(&server.uri())),
            Handle::current(),
        );
        let tools =
            r#"[{"name":"lookup","description":"Look up.","parameters":{"type":"object"}}]"#;
        let (result, _) =
            complete_off_executor(provider, "", one_message(), Some(tools.to_string())).await;
        let reply = result.expect("reply");
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].id, "call_1");
        assert_eq!(reply.tool_calls[0].name, "lookup");
        let args: serde_json::Value =
            serde_json::from_str(&reply.tool_calls[0].arguments).expect("arguments are JSON");
        assert_eq!(args, serde_json::json!({"q": "x"}));
    }

    #[test]
    fn normalize_tools_accepts_wrapped_flat_and_single_forms() {
        let wrapped =
            r#"[{"type":"function","function":{"name":"a","parameters":{"type":"object"}}}]"#;
        let flat = r#"[{"name":"b","description":"B","input_schema":{"type":"object"}}]"#;
        let single = r#"{"name":"c"}"#;
        assert_eq!(normalize_tools(wrapped)[0].name, "a");
        assert_eq!(normalize_tools(flat)[0].description, "B");
        assert_eq!(
            normalize_tools(single)[0].parameters,
            serde_json::json!({"type": "object"})
        );
        assert!(normalize_tools("not json").is_empty());
        assert!(normalize_tools(r#"[{"description":"no name"}]"#).is_empty());
    }

    // -- through the WASM host ----------------------------------------------

    /// A fixture tool call with `{"input": input}`.
    fn call(tool: &str, input: &str) -> ToolCallRequest {
        ToolCallRequest {
            name: tool.to_string(),
            arguments_json: serde_json::json!({ "input": input }).to_string(),
            call_id: "plugin-llm".to_string(),
            caller: None,
        }
    }

    fn load(name: &str, llm: Arc<dyn LlmProvider>, limits: ResourceLimits) -> WasmModule {
        let engine = WasmModule::build_engine(&limits).expect("engine");
        WasmModule::from_file(
            &engine,
            &fixture_path(name),
            // The fixtures here call `llm::complete`.
            ModuleManifest::new(name).with_grants([Capability::Llm]),
            llm,
            limits,
        )
        .unwrap_or_else(|e| panic!("fixture `{name}` failed to load: {e:#}"))
    }

    /// A plugin tool's `llm::complete` goes through the WASM host to an
    /// OpenRouter endpoint of the default shape, on default settings (no
    /// model named), and the call is accounted for.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_tool_calls_the_model_against_openrouter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let root = format!("{}/api/v1", server.uri());
        let llm = Arc::new(plugin_provider(
            provider_config(ProviderType::OpenRouter, Some("sk"), Some(&root)),
            Handle::current(),
        ));
        let usage = llm.usage();
        let mut module = load("slow-host", llm, ResourceLimits::default());
        let reply = module
            .invoke_tool(call("ask", "2+2?"))
            .await
            .expect("the tool gets the model's reply");
        assert_eq!(reply.content, "hi");
        assert_eq!(usage.take().len(), 1, "the model call is accounted for");
        let sent: serde_json::Value = server.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        assert_eq!(sent["messages"][0]["role"], "user", "{sent}");
        assert_eq!(sent["messages"][0]["content"][0]["text"], "2+2?", "{sent}");
    }

    /// A `tool` message (the role 0.3.0 added) reaches the provider as the
    /// result of the call it names, after the assistant message that made
    /// the call — not as a user message.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tool_message_reaches_the_provider_as_a_tool_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let root = format!("{}/api/v1", server.uri());
        let provider = plugin_provider(
            provider_config(ProviderType::OpenRouter, Some("sk"), Some(&root)),
            Handle::current(),
        );
        let messages = vec![
            Message {
                role: Role::User,
                content: "count".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_7".to_string(),
                    name: "count".to_string(),
                    arguments: "{}".to_string(),
                }],
                tool_call_id: None,
            },
            Message {
                role: Role::Tool,
                content: "3".to_string(),
                tool_calls: vec![],
                tool_call_id: Some("call_7".to_string()),
            },
        ];
        let (result, _) = complete_off_executor(provider, "", messages, None).await;
        assert_eq!(result.expect("the provider answers").content, "hi");
        let sent: serde_json::Value = server.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        let messages = sent["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 3, "{sent}");
        assert_eq!(messages[1]["role"], "assistant", "{sent}");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_7", "{sent}");
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "count");
        assert_eq!(messages[2]["role"], "tool", "{sent}");
        assert_eq!(messages[2]["tool_call_id"], "call_7", "{sent}");
        assert_eq!(messages[2]["content"], "3", "{sent}");
    }

    /// The call's deadline still bounds a provider that answers late: the
    /// host stops waiting at `max_execution_ms` and the guest call fails with
    /// `deadline exceeded` (PL-H1) instead of waiting out the response.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_provider_is_cut_off_at_the_call_deadline() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(ollama_ok_body())
                    .set_delay(Duration::from_millis(3_000)),
            )
            .mount(&server)
            .await;

        let limits = ResourceLimits {
            max_execution_ms: 1_000,
            ..ResourceLimits::default()
        };
        let llm = Arc::new(plugin_provider(
            provider_config(ProviderType::Ollama, None, Some(&server.uri())),
            Handle::current(),
        ));
        let mut module = load("slow-host", llm, limits);

        let start = Instant::now();
        let err = module
            .invoke_tool(call("ask", "x"))
            .await
            .expect_err("the late reply is cut off");
        let elapsed = start.elapsed();
        assert!(
            format!("{err:#}").contains("deadline exceeded"),
            "expected `deadline exceeded`, got: {err:#}"
        );
        assert!(
            elapsed < Duration::from_millis(1_400),
            "the host waited {elapsed:?}, past the 1000 ms deadline"
        );
    }
}
