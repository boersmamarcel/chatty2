//! Which OpenAI wire a provider config speaks (AGE-858).
//!
//! OpenAI's reasoning models take function tools only on the Responses API
//! (`/responses`): `/chat/completions` rejects a request that carries both
//! reasoning and tools. So on OpenAI's own API and on Azure OpenAI resources
//! a request with tools goes to Responses, through rig's OpenAI Responses
//! client ([`ToolsOnResponses`]); a request without tools keeps chat
//! completions, so an older Azure deployment without Responses still chats.
//! OpenAI-compatible servers (vLLM, OpenRouter, Ollama compat) and Azure's
//! model-inference surface keep chat completions for everything; their base
//! URL decides, and nothing about them changes.
//!
//! Users paste whatever endpoint their portal shows, so the Azure URL is
//! read for the resource it names: a trailing `/responses` or
//! `/chat/completions` is dropped, a deployment URL becomes the resource's
//! v1 base (the deployment is already the model identifier), and a dated
//! `/openai/responses?api-version=…` URL keeps its `api-version`. A path
//! chatty cannot place is not guessed at.

use std::fmt;
use std::future::Future;

use anyhow::anyhow;
use bytes::Bytes;
use reqwest::Url;
use rig_core::http_client::{
    self, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, Response,
    StreamingResponse, Uri,
};
use rig_agent::ModelHandle;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    ProviderCapabilities,
};
use rig_core::streaming::StreamingCompletionResponse;
use rig_core::wasm_compat::WasmCompatSend;

use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

/// The host of OpenAI's own API.
const OPENAI_API_HOST: &str = "api.openai.com";

/// The URL users should configure when their Azure endpoint can't reach
/// the Responses API.
pub(crate) const AZURE_V1_BASE_HINT: &str = "https://<resource>.openai.azure.com/openai/v1/";

/// Where an Azure OpenAI endpoint sends a chat request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AzureWire {
    /// The resource's Responses API: rig appends `/responses` to `base_url`;
    /// `api_version` is the dated preview's query parameter, if any.
    Responses {
        base_url: String,
        api_version: Option<String>,
    },
    /// An endpoint that serves chat completions only (the model-inference
    /// surface, or a path chatty cannot place on a resource).
    ChatCompletionsOnly,
}

fn parse(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    if raw.contains("://") {
        Url::parse(raw).ok()
    } else {
        Url::parse(&format!("https://{raw}")).ok()
    }
}

/// `path` without trailing slashes and without a pasted operation suffix.
fn strip_operation(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix("/responses")
        .or_else(|| path.strip_suffix("/chat/completions"))
        .unwrap_or(path)
        .trim_end_matches('/')
}

/// Where the Azure endpoint `raw_endpoint` sends a chat request.
pub(crate) fn azure_wire(raw_endpoint: &str) -> AzureWire {
    let Some(url) = parse(raw_endpoint) else {
        return AzureWire::ChatCompletionsOnly;
    };
    let origin = url.origin().ascii_serialization();
    let api_version = url
        .query_pairs()
        .find(|(key, _)| key == "api-version")
        .map(|(_, value)| value.into_owned());
    let v1 = || AzureWire::Responses {
        base_url: format!("{origin}/openai/v1"),
        api_version: None,
    };
    match strip_operation(url.path()) {
        "" => v1(),
        "/openai/v1" => AzureWire::Responses {
            base_url: format!("{origin}/openai/v1"),
            api_version,
        },
        "/openai" => match api_version {
            Some(version) => AzureWire::Responses {
                base_url: format!("{origin}/openai"),
                api_version: Some(version),
            },
            None => v1(),
        },
        // The deployment URL's api-version dates the chat-completions API;
        // the v1 surface needs none.
        path if path.starts_with("/openai/deployments/") => v1(),
        _ => AzureWire::ChatCompletionsOnly,
    }
}

/// The Responses API root when `base_url` is OpenAI's own API, else `None`
/// (an OpenAI-compatible server, which keeps chat completions).
pub(crate) fn openai_platform_responses_base(base_url: &str) -> Option<String> {
    let url = parse(base_url)?;
    if url.host_str()? != OPENAI_API_HOST {
        return None;
    }
    let path = match strip_operation(url.path()) {
        "" => "/v1",
        path => path,
    };
    Some(format!("{}{path}", url.origin().ascii_serialization()))
}

/// Whether `model_config` is an OpenAI reasoning model, as far as chatty
/// knows: the reasoning family rejects a sampling temperature.
fn is_reasoning_model(model_config: &ModelConfig) -> bool {
    !model_config.supports_temperature
}

/// Refuse a request with tools that its endpoint would reject: a reasoning
/// model on an Azure endpoint that only serves chat completions. The error
/// names the URL that works, rather than letting every turn fail on the
/// provider's 400.
pub(crate) fn ensure_tools_reachable(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
) -> anyhow::Result<()> {
    if provider_config.provider_type != ProviderType::AzureOpenAI
        || !is_reasoning_model(model_config)
    {
        return Ok(());
    }
    let endpoint = provider_config.base_url.as_deref().unwrap_or_default();
    match azure_wire(endpoint) {
        AzureWire::Responses { .. } => Ok(()),
        AzureWire::ChatCompletionsOnly => Err(anyhow!(
            "The Azure endpoint '{endpoint}' serves chat completions only, and reasoning \
             model '{}' takes tools only on the Responses API. Set the Azure provider's \
             endpoint to your resource's v1 base URL, {AZURE_V1_BASE_HINT}",
            model_config.model_identifier
        )),
    }
}

/// A model that sends each request on the wire it needs: a request with
/// tools to the Responses API, one without (titles, summaries, a plain
/// chat) to chat completions.
#[derive(Clone, Debug)]
pub(crate) struct ToolsOnResponses {
    pub(crate) chat: ModelHandle,
    pub(crate) responses: ModelHandle,
}

impl ToolsOnResponses {
    fn pick(&self, request: &CompletionRequest) -> &ModelHandle {
        if request.tools.is_empty() {
            &self.chat
        } else {
            &self.responses
        }
    }
}

impl CompletionModel for ToolsOnResponses {
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend {
        self.pick(&request).completion(request)
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<StreamingCompletionResponse, CompletionError>> + WasmCompatSend
    {
        self.pick(&request).stream(request)
    }

    /// The tool-carrying wire's: capabilities describe how tools and
    /// structured output compose, which only the Responses side sees.
    fn capabilities(&self) -> ProviderCapabilities {
        self.responses.capabilities()
    }
}

/// HTTP layer for Azure's Responses API under rig's OpenAI client: moves
/// the API key from the bearer header rig writes to Azure's `api-key`
/// header, and adds the dated preview's `api-version` query parameter.
#[derive(Clone, Default)]
pub(crate) struct AzureResponsesHttpClient<I = reqwest::Client> {
    inner: I,
    api_key: Option<String>,
    api_version: Option<String>,
}

impl<I> AzureResponsesHttpClient<I> {
    /// `api_key` is `None` under Entra ID, whose bearer token stays.
    pub(crate) fn new(inner: I, api_key: Option<String>, api_version: Option<String>) -> Self {
        Self {
            inner,
            api_key,
            api_version,
        }
    }

    fn prepare<B>(&self, req: Request<B>) -> http_client::Result<Request<B>> {
        let (mut parts, body) = req.into_parts();
        if let Some(key) = &self.api_key {
            parts.headers.remove("authorization");
            let value = HeaderValue::from_str(key).map_err(layer_error)?;
            parts.headers.insert("api-key", value);
        }
        if let Some(version) = &self.api_version {
            let uri = parts.uri.to_string();
            let separator = if uri.contains('?') { '&' } else { '?' };
            parts.uri = format!("{uri}{separator}api-version={version}")
                .parse::<Uri>()
                .map_err(layer_error)?;
        }
        Ok(Request::from_parts(parts, body))
    }
}

fn layer_error(error: impl std::error::Error + Send + Sync + 'static) -> http_client::Error {
    http_client::Error::Instance(anyhow::Error::new(error).into())
}

impl<I> fmt::Debug for AzureResponsesHttpClient<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureResponsesHttpClient")
            .field("api_version", &self.api_version)
            .finish_non_exhaustive()
    }
}

impl<I> HttpClientExt for AzureResponsesHttpClient<I>
where
    I: HttpClientExt + Clone + 'static,
{
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let (parts, body) = req.into_parts();
        let req = self.prepare(Request::from_parts(parts, body.into()));
        async move { inner.send(req?).await }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let req = self.prepare(req);
        async move { inner.send_multipart(req?).await }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let inner = self.inner.clone();
        let (parts, body) = req.into_parts();
        let req = self.prepare(Request::from_parts(parts, body.into()));
        async move { inner.send_streaming(req?).await }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use rig_agent::agent::AgentBuilder;
    use rig_agent::streaming::StreamingPrompt;
    use rig_agent::test_utils::MockAddTool;
    use rig_core::completion::{CompletionRequestBuilder, ToolDefinition};
    use rig_core::message::{AssistantContent, Message, UserContent};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request as WireRequest, ResponseTemplate};

    use super::*;
    use crate::factories::agent_factory::completion_model;
    use crate::factories::agent_factory::provider_builder::openai_responses_model;
    use crate::services::message_helpers::tool_round_trips_intact;

    fn model(provider_type: ProviderType, reasoning: bool) -> ModelConfig {
        let mut config = ModelConfig::new(
            "m".to_string(),
            "m".to_string(),
            provider_type,
            "gpt-6.1-sol".to_string(),
        );
        config.supports_temperature = !reasoning;
        config
    }

    fn provider(provider_type: ProviderType, key: &str, base_url: &str) -> ProviderConfig {
        let mut config = ProviderConfig::new("test".to_string(), provider_type);
        config.api_key = Some(key.to_string());
        config.base_url = Some(base_url.to_string());
        config
    }

    fn add_tool() -> ToolDefinition {
        ToolDefinition {
            name: "add".to_string(),
            description: "add two numbers".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
                "required": ["x", "y"],
            }),
        }
    }

    fn usage() -> serde_json::Value {
        serde_json::json!({
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 4},
            "output_tokens": 5,
            "output_tokens_details": {"reasoning_tokens": 2},
            "total_tokens": 15,
        })
    }

    fn response(output: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 0,
            "status": "completed",
            "model": "gpt-6.1-sol",
            "output": output,
            "tools": [],
            "usage": usage(),
        })
    }

    fn text_output(text: &str) -> serde_json::Value {
        serde_json::json!([{
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }])
    }

    fn chat_completion_body() -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })
    }

    /// One non-streaming request with the `add` tool through `model`.
    async fn send_with_tools(model: rig_agent::ModelHandle) -> rig_core::completion::Usage {
        CompletionRequestBuilder::new(model, Message::user("hi"))
            .tools(vec![add_tool()])
            .send()
            .await
            .expect("the fake server answers")
            .usage
    }

    async fn only_body(server: &MockServer) -> serde_json::Value {
        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 1, "exactly one request");
        serde_json::from_slice(&requests[0].body).expect("a JSON body")
    }

    /// OpenAI's own API is routed to Responses, and the Responses client
    /// sends the tools to `/v1/responses` with the key as a bearer token,
    /// reading usage (cached and reasoning tokens) back for metering. The
    /// real host can't be served from a test, so the routing is checked on
    /// the URL and the client it picks on a fake server.
    #[tokio::test(flavor = "multi_thread")]
    async fn openai_reasoning_model_with_tools_uses_responses_api() {
        assert_eq!(
            openai_platform_responses_base("https://api.openai.com/v1").as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            openai_platform_responses_base("https://api.openai.com/v1/chat/completions").as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            openai_platform_responses_base("api.openai.com").as_deref(),
            Some("https://api.openai.com/v1")
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer sk-openai"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response(text_output("hi"))))
            .expect(1)
            .mount(&server)
            .await;

        let base = format!("{}/v1", server.uri());
        let model = openai_responses_model(&base, "sk-openai", "gpt-6.1-sol").unwrap();
        let usage = send_with_tools(model).await;
        assert_eq!(usage.cached_input_tokens, 4);
        assert_eq!(usage.reasoning_tokens, 2);

        let body = only_body(&server).await;
        assert_eq!(body["tools"][0]["name"], "add");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    /// OpenAI-compatible servers (vLLM, OpenRouter) keep chat completions,
    /// tools and all.
    #[tokio::test(flavor = "multi_thread")]
    async fn compat_provider_keeps_chat_completions() {
        assert_eq!(
            openai_platform_responses_base("https://openrouter.ai/api/v1"),
            None
        );
        assert_eq!(
            openai_platform_responses_base("http://localhost:8000/v1"),
            None
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion_body()))
            .expect(1)
            .mount(&server)
            .await;

        let model_config = model(ProviderType::OpenRouter, true);
        let provider_config = provider(
            ProviderType::OpenRouter,
            "sk-compat",
            &format!("{}/v1", server.uri()),
        );
        ensure_tools_reachable(&model_config, &provider_config).unwrap();
        send_with_tools(completion_model(&model_config, &provider_config).unwrap()).await;
        assert_eq!(
            only_body(&server).await["tools"][0]["function"]["name"],
            "add"
        );
    }

    /// No chat-completions request chatty sends carries `reasoning_effort`
    /// next to tools: not the compat arm with its thinking switch on, not
    /// Azure's model-inference surface. A reasoning model there is refused
    /// before anything is sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn chat_completions_never_sends_reasoning_effort_with_tools() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion_body()))
            .mount(&server)
            .await;

        let mut compat = model(ProviderType::OpenRouter, true);
        compat
            .extra_params
            .insert("think".to_string(), "true".to_string());
        let compat_provider = provider(
            ProviderType::OpenRouter,
            "sk-compat",
            &format!("{}/v1", server.uri()),
        );
        let mut request = CompletionRequestBuilder::new(
            completion_model(&compat, &compat_provider).unwrap(),
            "hi",
        )
        .tools(vec![add_tool()]);
        if let Some(params) =
            crate::factories::agent_factory::request_params(&compat, &ProviderType::OpenRouter)
        {
            request = request.additional_params(params);
        }
        request.send().await.unwrap();

        let inference = format!("{}/models", server.uri());
        let azure_provider = provider(ProviderType::AzureOpenAI, "sk-azure", &inference);
        let azure = model(ProviderType::AzureOpenAI, false);
        assert_eq!(azure_wire(&inference), AzureWire::ChatCompletionsOnly);
        send_with_tools(completion_model(&azure, &azure_provider).unwrap()).await;

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert!(request.url.path().ends_with("/chat/completions"));
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body["tools"].is_array(), "the tools went out");
            assert!(
                body.get("reasoning_effort").is_none() && body.get("reasoning").is_none(),
                "chat completions carried reasoning next to tools: {body}"
            );
        }

        let reasoning = model(ProviderType::AzureOpenAI, true);
        assert!(ensure_tools_reachable(&reasoning, &azure_provider).is_err());
    }

    /// Marcel's case: an Azure resource with a reasoning deployment and
    /// tools goes to `/openai/v1/responses`, authenticated with `api-key`
    /// and no bearer token.
    #[tokio::test(flavor = "multi_thread")]
    async fn azure_reasoning_model_with_tools_uses_responses_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/v1/responses"))
            .and(header("api-key", "sk-azure"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response(text_output("hi"))))
            .expect(1)
            .mount(&server)
            .await;

        let model_config = model(ProviderType::AzureOpenAI, true);
        let provider_config = provider(
            ProviderType::AzureOpenAI,
            "sk-azure",
            &format!("{}/openai/v1/responses", server.uri()),
        );
        ensure_tools_reachable(&model_config, &provider_config).unwrap();
        let usage =
            send_with_tools(completion_model(&model_config, &provider_config).unwrap()).await;
        assert_eq!(usage.cached_input_tokens, 4);

        let requests = server.received_requests().await.unwrap();
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "an Azure API key must not also go out as a bearer token"
        );
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["model"], "gpt-6.1-sol", "the deployment is the model");
        assert_eq!(body["tools"][0]["name"], "add");
    }

    /// The dated preview keeps the `api-version` the user pasted.
    #[tokio::test(flavor = "multi_thread")]
    async fn azure_dated_responses_url_keeps_its_api_version() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/responses"))
            .and(wiremock::matchers::query_param(
                "api-version",
                "2025-04-01-preview",
            ))
            .and(header("api-key", "sk-azure"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response(text_output("hi"))))
            .expect(1)
            .mount(&server)
            .await;

        let model_config = model(ProviderType::AzureOpenAI, true);
        let provider_config = provider(
            ProviderType::AzureOpenAI,
            "sk-azure",
            &format!(
                "{}/openai/responses?api-version=2025-04-01-preview",
                server.uri()
            ),
        );
        send_with_tools(completion_model(&model_config, &provider_config).unwrap()).await;
    }

    /// Every Azure URL a portal shows is read for its resource: the v1 base,
    /// a pasted Responses or chat-completions URL, a deployment URL. The
    /// model-inference surface and unknown paths are not guessed at.
    #[test]
    fn azure_endpoint_urls_normalise_to_the_responses_base() {
        let v1 = |base: &str| AzureWire::Responses {
            base_url: base.to_string(),
            api_version: None,
        };
        let res = "https://res.openai.azure.com";
        assert_eq!(
            azure_wire(res),
            v1("https://res.openai.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire("res.openai.azure.com/"),
            v1("https://res.openai.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire("https://res.openai.azure.com/openai/v1/"),
            v1("https://res.openai.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire("https://res.openai.azure.com/openai/v1/responses"),
            v1("https://res.openai.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire("https://res.openai.azure.com/openai/v1/chat/completions"),
            v1("https://res.openai.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire(
                "https://res.cognitiveservices.azure.com/openai/deployments/gpt-6/chat/completions?api-version=2024-10-21"
            ),
            v1("https://res.cognitiveservices.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire(
                "https://res.openai.azure.com/openai/responses?api-version=2025-04-01-preview"
            ),
            AzureWire::Responses {
                base_url: "https://res.openai.azure.com/openai".to_string(),
                api_version: Some("2025-04-01-preview".to_string()),
            }
        );
        assert_eq!(
            azure_wire("https://res.openai.azure.com/openai/responses"),
            v1("https://res.openai.azure.com/openai/v1")
        );
        assert_eq!(
            azure_wire(
                "https://res.services.ai.azure.com/models/chat/completions?api-version=2024-05-01-preview"
            ),
            AzureWire::ChatCompletionsOnly
        );
        assert_eq!(
            azure_wire("https://gateway.example.com/team/openai-proxy"),
            AzureWire::ChatCompletionsOnly
        );
    }

    /// A reasoning model with tools on a chat-completions-only Azure URL is
    /// refused with an error that names the v1 base URL to use; a
    /// non-reasoning model there, or a reasoning model on a resource URL,
    /// is not.
    #[test]
    fn azure_chat_completions_only_url_names_the_v1_base_for_reasoning_tools() {
        let inference = provider(
            ProviderType::AzureOpenAI,
            "sk",
            "https://res.services.ai.azure.com/models",
        );
        let error = ensure_tools_reachable(&model(ProviderType::AzureOpenAI, true), &inference)
            .expect_err("a reasoning model with tools can't run there");
        let message = error.to_string();
        assert!(message.contains(AZURE_V1_BASE_HINT), "{message}");
        assert!(message.contains("Responses API"), "{message}");

        ensure_tools_reachable(&model(ProviderType::AzureOpenAI, false), &inference).unwrap();
        let resource = provider(
            ProviderType::AzureOpenAI,
            "sk",
            "https://res.openai.azure.com",
        );
        ensure_tools_reachable(&model(ProviderType::AzureOpenAI, true), &resource).unwrap();
    }

    /// A full agent turn on Azure's Responses API: the model calls `add`,
    /// the tool runs, and the follow-up request carries the call and its
    /// output under the same `call_id`, in that order. The history rig
    /// hands back passes the AGE-512 round-trip check.
    #[tokio::test(flavor = "multi_thread")]
    async fn responses_tool_round_trip_is_well_formed() {
        let sse = |events: Vec<serde_json::Value>| -> String {
            events
                .iter()
                .map(|event| {
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap()
                    )
                })
                .collect()
        };
        let call = serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "add",
            "arguments": "{\"x\":2,\"y\":3}",
            "status": "completed",
        });
        let first = sse(vec![
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "sequence_number": 1,
                "item": call,
            }),
            serde_json::json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": response(serde_json::json!([call])),
            }),
        ]);
        let second = sse(vec![
            serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 1,
                "delta": "5",
            }),
            serde_json::json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": response(text_output("5")),
            }),
        ]);

        let server = MockServer::start().await;
        let turn = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder = {
            let turn = turn.clone();
            move |_: &WireRequest| {
                let body = if turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    first.clone()
                } else {
                    second.clone()
                };
                ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
            }
        };
        Mock::given(method("POST"))
            .and(path("/openai/v1/responses"))
            .and(header("api-key", "sk-azure"))
            .respond_with(responder)
            .expect(2)
            .mount(&server)
            .await;

        let model_config = model(ProviderType::AzureOpenAI, true);
        let provider_config = provider(ProviderType::AzureOpenAI, "sk-azure", &server.uri());
        let agent = AgentBuilder::from_model_handle(
            completion_model(&model_config, &provider_config).unwrap(),
        )
        .tool(MockAddTool)
        .build();

        let mut stream = agent.stream_prompt("add 2 and 3").max_turns(3).await;
        let mut history = None;
        while let Some(item) = stream.next().await {
            if let rig_agent::agent::MultiTurnStreamItem::FinalResponse(done) =
                item.expect("the run succeeds")
            {
                history = done.messages;
            }
        }

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        let input = body["input"].as_array().expect("an input list");
        let call_at = input
            .iter()
            .position(|item| item["type"] == "function_call")
            .expect("the call is replayed");
        let output_at = input
            .iter()
            .position(|item| item["type"] == "function_call_output")
            .expect("the tool output is sent");
        assert!(call_at < output_at, "the output follows its call: {body}");
        assert_eq!(input[call_at]["call_id"], "call_1");
        assert_eq!(input[output_at]["call_id"], "call_1");
        assert_eq!(input[output_at]["output"], "5");

        let history = history.expect("the run hands back its messages");
        assert!(
            history
                .iter()
                .any(|m| matches!(m, Message::Assistant { content, .. }
            if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)))))
        );
        assert!(history.iter().any(|m| matches!(m, Message::User { content }
            if content.iter().any(|c| matches!(c, UserContent::ToolResult(_))))));
        assert!(tool_round_trips_intact(&history, &[]));
    }
}
