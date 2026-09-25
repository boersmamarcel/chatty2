//! HTTP layer that re-sends a model request the connection lost before any
//! response arrived.
//!
//! A local server (vLLM, Ollama) under load drops or refuses connections now
//! and then: reqwest reports `error sending request for url (…)` before a
//! single response byte. rig ends the whole multi-turn run on that error, so
//! every tool round-trip of the run so far went with it and headless had to
//! continue from a history that no longer held them. Nothing was sent back
//! yet, so sending the identical request again is safe, and doing it here —
//! under rig — keeps the run itself alive.
//!
//! Only the send is retried. An error after the response started (a body
//! stream cut mid-answer) has already reached the model loop as partial
//! output and is the session's to recover.

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;

/// Re-sends after the first failed send. With the default backoff the
/// retries wait 1 + 2 + 4 + 8 = 15 s in all: long enough for a pooled
/// connection race or a brief overload, short of a server restart, which
/// headless's own (slower) retry of the turn covers.
pub const CONNECT_RETRY_ATTEMPTS: usize = 4;
const DEFAULT_BACKOFF: Duration = Duration::from_secs(1);

/// Wraps an HTTP client so a request whose send failed at the connection
/// level is sent again, up to [`CONNECT_RETRY_ATTEMPTS`] times with
/// doubling backoff.
///
/// `Default` is required by rig's completion-model bounds on the HTTP
/// client type, not used by chatty itself.
#[derive(Clone, Debug)]
pub struct ConnectRetryHttpClient<I = reqwest::Client> {
    inner: I,
    backoff: Duration,
}

impl<I> ConnectRetryHttpClient<I> {
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            backoff: DEFAULT_BACKOFF,
        }
    }

    /// The first retry's delay (each later one doubles it).
    #[cfg(test)]
    fn with_backoff(inner: I, backoff: Duration) -> Self {
        Self { inner, backoff }
    }
}

impl<I: Default> Default for ConnectRetryHttpClient<I> {
    fn default() -> Self {
        Self::new(I::default())
    }
}

/// Whether `error` is a send that never got a response: a refused, reset or
/// dropped connection. An HTTP status (the server answered) is not one.
fn is_connection_failure(error: &http_client::Error) -> bool {
    let http_client::Error::Instance(inner) = error else {
        return false;
    };
    inner
        .downcast_ref::<reqwest::Error>()
        .is_some_and(|e| e.status().is_none() && (e.is_connect() || e.is_request()))
}

/// Run `send` until it succeeds, fails with anything but a connection
/// failure, or has been retried [`CONNECT_RETRY_ATTEMPTS`] times.
async fn with_connect_retries<R, F, Fut>(backoff: Duration, mut send: F) -> http_client::Result<R>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = http_client::Result<R>>,
{
    let mut delay = backoff;
    let mut retries = 0;
    loop {
        match send().await {
            Err(error) if retries < CONNECT_RETRY_ATTEMPTS && is_connection_failure(&error) => {
                retries += 1;
                tracing::warn!(
                    %error,
                    retry = retries,
                    of = CONNECT_RETRY_ATTEMPTS,
                    delay_ms = delay.as_millis() as u64,
                    "Model request failed before a response; sending it again"
                );
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            result => return result,
        }
    }
}

impl<I> HttpClientExt for ConnectRetryHttpClient<I>
where
    I: HttpClientExt + Clone + Send + Sync + 'static,
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
        let backoff = self.backoff;
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        async move {
            with_connect_retries(backoff, || {
                inner.send(Request::from_parts(parts.clone(), body.clone()))
            })
            .await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        // A multipart form is consumed by the send; it cannot go twice.
        self.inner.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let inner = self.inner.clone();
        let backoff = self.backoff;
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        async move {
            with_connect_retries(backoff, || {
                inner.send_streaming(Request::from_parts(parts.clone(), body.clone()))
            })
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A local server that drops the first `drops` connections without a
    /// response (what reqwest reports as "error sending request") and then
    /// answers 200. Returns its URL and the count of connections accepted.
    fn flaky_server(drops: usize) -> (String, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                if n < drops {
                    drop(stream);
                    continue;
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                      Content-Length: 12\r\nConnection: close\r\n\r\ndata: [DONE]",
                );
            }
        });
        (url, accepted)
    }

    fn post(url: &str) -> Request<Bytes> {
        Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "application/json")
            .body(Bytes::from_static(br#"{"messages":[]}"#))
            .unwrap()
    }

    fn client() -> ConnectRetryHttpClient {
        // No pooling: every attempt is a fresh connection the server counts.
        let inner = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        ConnectRetryHttpClient::with_backoff(inner, Duration::from_millis(5))
    }

    /// The failure seen in the offline benchmark: the connection dropped
    /// before a response. The same request goes out again and succeeds.
    #[tokio::test]
    async fn a_dropped_connection_is_sent_again() {
        let (url, accepted) = flaky_server(2);
        let response = client()
            .send_streaming(post(&url))
            .await
            .expect("the third attempt answers");
        assert_eq!(response.status(), 200);
        assert_eq!(accepted.load(Ordering::SeqCst), 3);
    }

    /// A server that never answers: the retries stop after the bound and
    /// the connection error is what the caller sees.
    #[tokio::test]
    async fn retries_are_bounded() {
        let (url, accepted) = flaky_server(usize::MAX);
        let error = client()
            .send_streaming(post(&url))
            .await
            .map(|response| response.status())
            .expect_err("a server that never answers still fails");
        assert!(is_connection_failure(&error), "{error}");
        assert_eq!(accepted.load(Ordering::SeqCst), 1 + CONNECT_RETRY_ATTEMPTS);
    }

    /// Nothing listening at all (connection refused) is retried too, and
    /// still ends in an error.
    #[tokio::test]
    async fn a_refused_connection_is_a_connection_failure() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = client()
            .send(post(&format!(
                "http://127.0.0.1:{port}/v1/chat/completions"
            )))
            .await
            .map(|_: Response<LazyBody<Bytes>>| ())
            .expect_err("nothing listens there");
        assert!(is_connection_failure(&error), "{error}");
    }

    /// A status the server sent back is an answer, not a lost connection.
    #[test]
    fn an_http_status_is_not_a_connection_failure() {
        let error = http_client::Error::InvalidStatusCodeWithMessage(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "overloaded".into(),
        );
        assert!(!is_connection_failure(&error));
    }
}
