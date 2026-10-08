//! Which OpenAI wire a provider config speaks (AGE-858).
//!
//! OpenAI's reasoning models take function tools only on the Responses API
//! (`/responses`): `/chat/completions` rejects a request that carries both
//! reasoning and tools. So OpenAI's own API and Azure OpenAI resources go to
//! Responses, through rig's OpenAI Responses client. OpenAI-compatible
//! servers (vLLM, OpenRouter, Ollama compat) and Azure's model-inference
//! surface keep chat completions; their base URL decides, and nothing about
//! them changes.
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
