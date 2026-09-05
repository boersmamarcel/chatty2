//! HTTP layer that puts a current Azure Entra ID token on every request (AGE-245).
//!
//! rig takes the Azure credential once, when the client is built, and copies
//! it into the headers every later request inherits. An Entra token lives
//! about an hour, so a conversation that outlived its token kept sending the
//! expired one, and the mid-stream refresh the desktop did after a 401 never
//! reached the agent. This wrapper sits under rig's client instead — the same
//! shape as `PromptCachingHttpClient` — and asks the [`AzureTokenCache`] for a
//! token as each request goes out. The cache refreshes itself once the token
//! is inside its expiry margin, so no request carries a stale one.
//!
//! rig still needs an auth value to build the client; the provider builder
//! passes a placeholder, and this layer overwrites the header it produced.
//! The token value is never logged.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;

use crate::auth::AzureTokenCache;

/// Something that can produce a bearer token that is valid right now.
pub trait BearerTokenSource: Send + Sync {
    fn bearer_token(&self) -> BoxFuture<'_, anyhow::Result<String>>;
}

impl BearerTokenSource for AzureTokenCache {
    fn bearer_token(&self) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(self.get_token())
    }
}

/// Behind [`Default`]: rig's completion-model bounds require the HTTP client
/// type to be `Default`, but a client built without a credential must fail
/// the request rather than send the placeholder header.
struct NoTokenSource;

impl BearerTokenSource for NoTokenSource {
    fn bearer_token(&self) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async { Err(anyhow::anyhow!("No Azure token source configured")) })
    }
}

/// HTTP client that sets `Authorization: Bearer <token>` on every request,
/// with the token fetched from `tokens` at send time.
#[derive(Clone)]
pub struct AzureAuthHttpClient<I = reqwest::Client> {
    inner: I,
    tokens: Arc<dyn BearerTokenSource>,
}

impl<I> AzureAuthHttpClient<I> {
    pub fn new(inner: I, tokens: Arc<dyn BearerTokenSource>) -> Self {
        Self { inner, tokens }
    }
}

impl<I: Default> Default for AzureAuthHttpClient<I> {
    fn default() -> Self {
        Self::new(I::default(), Arc::new(NoTokenSource))
    }
}

impl<I> fmt::Debug for AzureAuthHttpClient<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureAuthHttpClient")
            .finish_non_exhaustive()
    }
}

/// Replace whatever bearer header rig wrote with the current token.
fn authorize<B>(req: Request<B>, token: &str) -> http_client::Result<Request<B>> {
    let (mut parts, body) = req.into_parts();
    http_client::bearer_auth_header(&mut parts.headers, token)?;
    Ok(Request::from_parts(parts, body))
}

fn token_error(error: anyhow::Error) -> http_client::Error {
    http_client::Error::Instance(error.into())
}

impl<I> HttpClientExt for AzureAuthHttpClient<I>
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
        let tokens = self.tokens.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let token = tokens.bearer_token().await.map_err(token_error)?;
            inner.send(authorize(req, &token)?).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let tokens = self.tokens.clone();
        async move {
            let token = tokens.bearer_token().await.map_err(token_error)?;
            inner.send_multipart(authorize(req, &token)?).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let inner = self.inner.clone();
        let tokens = self.tokens.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let token = tokens.bearer_token().await.map_err(token_error)?;
            inner.send_streaming(authorize(req, &token)?).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use rig_core::http_client::Method;

    use super::*;

    /// Records the `Authorization` header of every request instead of sending it.
    #[derive(Clone, Default)]
    struct RecordingClient {
        auth_headers: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingClient {
        fn record<B>(&self, req: &Request<B>) {
            let header = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            self.auth_headers.lock().unwrap().push(header);
        }
    }

    impl HttpClientExt for RecordingClient {
        fn send<T, U>(
            &self,
            req: Request<T>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
        where
            T: Into<Bytes> + WasmCompatSend,
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            self.record(&req);
            async move {
                let body: LazyBody<U> = Box::pin(async { Ok(U::from(Bytes::new())) });
                Response::builder()
                    .status(200)
                    .body(body)
                    .map_err(http_client::Error::Protocol)
            }
        }

        fn send_multipart<U>(
            &self,
            req: Request<MultipartForm>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
        where
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            self.record(&req);
            async move { Err(http_client::Error::StreamEnded) }
        }

        fn send_streaming<T>(
            &self,
            req: Request<T>,
        ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
        where
            T: Into<Bytes> + WasmCompatSend,
        {
            self.record(&req);
            async move { Err(http_client::Error::StreamEnded) }
        }
    }

    /// Hands out a scripted token per call — one token per "expiry".
    struct ScriptedTokens(Mutex<VecDeque<&'static str>>);

    impl ScriptedTokens {
        fn new(tokens: &[&'static str]) -> Arc<Self> {
            Arc::new(Self(Mutex::new(tokens.iter().copied().collect())))
        }
    }

    impl BearerTokenSource for ScriptedTokens {
        fn bearer_token(&self) -> BoxFuture<'_, anyhow::Result<String>> {
            let next = self.0.lock().unwrap().pop_front();
            Box::pin(async move {
                next.map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("credential expired"))
            })
        }
    }

    /// A chat-completions request the way rig hands it over: with the
    /// placeholder bearer header the provider builder configured.
    fn chat_request() -> Request<Bytes> {
        Request::builder()
            .method(Method::POST)
            .uri("https://example.openai.azure.com/openai/deployments/gpt/chat/completions")
            .header("authorization", "Bearer placeholder")
            .body(Bytes::from_static(b"{}"))
            .unwrap()
    }

    #[tokio::test]
    async fn each_request_carries_the_token_current_at_send_time() {
        let recording = RecordingClient::default();
        let client = AzureAuthHttpClient::new(
            recording.clone(),
            ScriptedTokens::new(&["first-token", "second-token"]),
        );

        client.send::<_, Bytes>(chat_request()).await.unwrap();
        // The token expired in between: the cache hands out a fresh one.
        client.send::<_, Bytes>(chat_request()).await.unwrap();

        assert_eq!(
            *recording.auth_headers.lock().unwrap(),
            vec![
                "Bearer first-token".to_string(),
                "Bearer second-token".to_string()
            ],
            "the placeholder header rig wrote must be replaced on every request"
        );
    }

    #[tokio::test]
    async fn streaming_requests_are_authorized_the_same_way() {
        let recording = RecordingClient::default();
        let client = AzureAuthHttpClient::new(recording.clone(), ScriptedTokens::new(&["tok"]));

        // The recording client has no stream to return; the header is what matters.
        let _ = client.send_streaming(chat_request()).await;

        assert_eq!(
            *recording.auth_headers.lock().unwrap(),
            vec!["Bearer tok".to_string()]
        );
    }

    #[tokio::test]
    async fn a_token_failure_fails_the_request_before_it_is_sent() {
        let recording = RecordingClient::default();
        let client = AzureAuthHttpClient::new(recording.clone(), ScriptedTokens::new(&[]));

        let err = match client.send::<_, Bytes>(chat_request()).await {
            Ok(_) => panic!("no token, no request"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("credential expired"), "{err}");
        assert!(
            recording.auth_headers.lock().unwrap().is_empty(),
            "the placeholder must never reach the provider"
        );
    }

    #[tokio::test]
    async fn default_client_refuses_to_send_without_a_token_source() {
        let client: AzureAuthHttpClient<RecordingClient> = AzureAuthHttpClient::default();
        let err = match client.send::<_, Bytes>(chat_request()).await {
            Ok(_) => panic!("a default client has no credential to send"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("No Azure token source"), "{err}");
    }
}
