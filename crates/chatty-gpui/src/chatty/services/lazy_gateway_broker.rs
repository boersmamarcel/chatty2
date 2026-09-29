//! The desktop's `LazyBroker` (BI-2, AGE-634): unlike `broker_runner`, this
//! is not Unix-only, because the module gateway itself (WASM modules, MCP,
//! OpenAI-compat) is not — only the fleet-broker socket riding on it is.
//!
//! Building and starting the gateway needs `gpui::AsyncApp`, which is
//! `!Send` (GPUI is single-threaded), while this handle must be
//! `Send + Sync` to live behind `Arc<dyn LazyBroker>` and be called from a
//! tool running on tokio's own threads. So `transport` does not do the
//! work itself: it asks the task `module_settings_controller::refresh_runtime`
//! spawned on GPUI's own executor to do it — that task is still on
//! `AsyncApp`, and answers over a one-shot reply channel — and waits for the
//! answer. Settings changing before anything ever asks simply drops this and
//! the task it talks to; there was nothing to tear down.
//!
//! The desktop's root conversation is its broker's root (ADR-0020: one
//! broker per root process), so what the start task answers with is the
//! gateway's direct [`Transport`] as well as its socket: `invoke_agent`
//! reaches the local roles through that handle, never over the gateway's
//! socket (AGE-744).

use std::path::PathBuf;
use std::sync::Arc;

use chatty_fabric::Transport;

/// What a started gateway hands back: the socket it serves on, and this
/// process's direct handle into it (`ProtocolGateway::transport`).
#[derive(Clone)]
pub struct StartedGateway {
    pub socket: PathBuf,
    pub transport: Arc<dyn Transport>,
}

/// A reply to one start request: the started
/// gateway, or the reason it failed to start.
pub type StartReply = tokio::sync::oneshot::Sender<Result<StartedGateway, String>>;

/// Start `gateway` as the desktop's broker. Its direct handle is taken
/// here, after every virtual agent is on it, so the call path reaches all
/// of them.
pub async fn start(
    gateway: &mut chatty_protocol_gateway::ProtocolGateway,
) -> anyhow::Result<StartedGateway> {
    let transport = gateway.transport();
    gateway.start().await?;
    let socket = gateway
        .socket_path()
        .ok_or_else(|| anyhow::anyhow!("the gateway started with no socket"))?
        .to_path_buf();
    Ok(StartedGateway { socket, transport })
}

/// See the module docs.
pub struct LazyGatewayBroker {
    request_tx: tokio::sync::mpsc::UnboundedSender<StartReply>,
    once: tokio::sync::OnceCell<StartedGateway>,
}

impl LazyGatewayBroker {
    pub fn new(request_tx: tokio::sync::mpsc::UnboundedSender<StartReply>) -> Self {
        Self {
            request_tx,
            once: tokio::sync::OnceCell::new(),
        }
    }
}

impl LazyGatewayBroker {
    async fn started(&self) -> anyhow::Result<&StartedGateway> {
        self.once
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
            .await
    }
}

#[async_trait::async_trait]
impl chatty_core::services::lazy_broker::LazyBroker for LazyGatewayBroker {
    /// The root reaches its broker directly (ADR-0020, BI-4).
    async fn transport(&self) -> anyhow::Result<Option<Arc<dyn Transport>>> {
        Ok(Some(self.started().await?.transport.clone()))
    }

    fn bound_sockets(&self) -> Vec<PathBuf> {
        self.once
            .get()
            .map(|started| vec![started.socket.clone()])
            .unwrap_or_default()
    }

    /// The root's messages come from its broker's direct handle; a broker
    /// that has not started has none (TM-2).
    fn take_run_messages(&self) -> Vec<String> {
        self.once
            .get()
            .map(|started| started.transport.take_run_messages())
            .unwrap_or_default()
    }

    /// The root stops one run of its swarm through its direct handle
    /// (TB-7); a broker that has not started runs nothing.
    fn cancel(&self, node: &str) -> anyhow::Result<()> {
        let Some(started) = self.once.get() else {
            anyhow::bail!("the broker is not running, so nothing is named '{node}'");
        };
        started.transport.cancel(node).map_err(anyhow::Error::from)
    }
}
