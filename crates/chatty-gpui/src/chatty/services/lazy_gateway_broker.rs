//! The desktop's `LazyBroker` (BI-2, AGE-634): unlike `broker_runner`, this
//! is not Unix-only, because the module gateway itself (WASM modules, MCP,
//! OpenAI-compat) is not — only the fleet-broker socket riding on it is.
//!
//! Building and starting the gateway needs `gpui::AsyncApp`, which is
//! `!Send` (GPUI is single-threaded), while this handle must be
//! `Send + Sync` to live behind `Arc<dyn LazyBroker>` and be called from a
//! tool running on tokio's own threads. So `ensure_started` does not do the
//! work itself: it asks the task `module_settings_controller::refresh_runtime`
//! spawned on GPUI's own executor to do it — that task is still on
//! `AsyncApp`, and answers over a one-shot reply channel — and waits for the
//! answer. Settings changing before anything ever asks simply drops this and
//! the task it talks to; there was nothing to tear down.

use std::net::SocketAddr;

/// A reply to one [`LazyGatewayBroker::ensure_started`] request: the port
/// the gateway bound, or the reason it failed to.
pub type StartReply = tokio::sync::oneshot::Sender<Result<u16, String>>;

/// See the module docs.
pub struct LazyGatewayBroker {
    request_tx: tokio::sync::mpsc::UnboundedSender<StartReply>,
    once: tokio::sync::OnceCell<u16>,
}

impl LazyGatewayBroker {
    pub fn new(request_tx: tokio::sync::mpsc::UnboundedSender<StartReply>) -> Self {
        Self {
            request_tx,
            once: tokio::sync::OnceCell::new(),
        }
    }
}

#[async_trait::async_trait]
impl chatty_core::services::lazy_broker::LazyBroker for LazyGatewayBroker {
    async fn ensure_started(&self) -> anyhow::Result<String> {
        let port = self
            .once
            .get_or_try_init(|| async {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                self.request_tx
                    .send(reply_tx)
                    .map_err(|_| anyhow::anyhow!("the gateway's start task is gone"))?;
                reply_rx
                    .await
                    .map_err(|_| anyhow::anyhow!("the gateway's start task dropped the reply"))?
                    .map_err(|e| anyhow::anyhow!(e))
            })
            .await?;
        Ok(format!("http://localhost:{port}"))
    }

    fn bound_addrs(&self) -> Vec<SocketAddr> {
        match self.once.get() {
            Some(&port) => vec![SocketAddr::from(([127, 0, 0, 1], port))],
            None => Vec::new(),
        }
    }
}
