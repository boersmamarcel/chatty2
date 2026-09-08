//! The Unix-domain-socket listener local participants register over.
//!
//! One connection is one participant for as long as it is open. The
//! connection *is* the liveness signal (ADR-0011): when the read loop ends,
//! for any reason, the participant is deregistered and its open tasks are
//! failed. There is no heartbeat, because a process that has died cannot
//! fail to send one.
//!
//! Unix only. The hosted half of ADR-0011 is a vsock listener (AGE-307) with
//! the same frames on it; nothing here is shaped by the socket family beyond
//! the accept loop, so that listener reuses [`serve_connection`].

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::protocol::{BrokerFrame, ParticipantFrame};
use super::registry::ParticipantRegistry;

/// Bind a participant socket at `path`.
///
/// A stale socket file from a crashed broker is removed first: a Unix socket
/// left on disk is not a running process, and refusing to start because of
/// one would make a crash need manual cleanup. A *live* broker on the same
/// path is a different matter, and `bind` still fails with `AddrInUse` for
/// it — removing the file would not have taken the port from it either.
pub fn bind(path: impl AsRef<Path>) -> io::Result<UnixListener> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A blocking connect is the probe: it either reaches a listener or it
    // does not, and it needs no runtime, unlike tokio's.
    if path.exists() && std::os::unix::net::UnixStream::connect(path).is_err() {
        debug!(socket = %path.display(), "Removing a stale participant socket");
        let _ = std::fs::remove_file(path);
    }
    UnixListener::bind(path)
}

/// Accept participants until `listener` is dropped or the task is aborted.
pub async fn serve(listener: UnixListener, registry: ParticipantRegistry) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let registry = registry.clone();
                tokio::spawn(async move {
                    serve_connection(stream, registry).await;
                });
            }
            Err(e) => {
                warn!(error = %e, "Participant socket accept failed; listener stopping");
                return;
            }
        }
    }
}

/// Drive one participant connection from its first frame to its last.
///
/// Split into read and write halves: the write half drains the registry's
/// outbound queue for this participant, the read half owns registration and
/// routing. When the read half returns, the participant is deregistered and
/// the outbound sender is dropped, which ends the writer.
pub async fn serve_connection<S>(stream: S, registry: ParticipantRegistry)
where
    S: tokio::io::AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<BrokerFrame>();

    let writer = tokio::spawn(write_frames(write_half, outbound_rx));

    // 1. The first frame must be a registration.
    let name = match lines.next_line().await {
        Ok(Some(line)) => match register_from(&line, &registry, outbound_tx.clone()) {
            Ok(name) => {
                let _ = outbound_tx.send(BrokerFrame::Registered { name: name.clone() });
                name
            }
            Err(reason) => {
                warn!(%reason, "Refusing a participant connection");
                let _ = outbound_tx.send(BrokerFrame::Rejected { reason });
                drop(outbound_tx);
                let _ = writer.await;
                return;
            }
        },
        Ok(None) => {
            debug!("A participant connected and closed without registering");
            return;
        }
        Err(e) => {
            warn!(error = %e, "Participant connection failed before registering");
            return;
        }
    };

    // 2. Task traffic until the socket closes.
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => continue,
            Ok(Some(line)) => match serde_json::from_str::<ParticipantFrame>(&line) {
                // A malformed line is dropped, not fatal: one bad frame
                // should not fail every task the participant still owes.
                Err(e) => warn!(participant = %name, error = %e, "Ignoring a malformed frame"),
                Ok(frame) => {
                    if !registry.on_frame(&name, frame) {
                        warn!(participant = %name, "Closing the connection after a protocol error");
                        break;
                    }
                }
            },
            Ok(None) => {
                debug!(participant = %name, "Participant closed its socket");
                break;
            }
            Err(e) => {
                warn!(participant = %name, error = %e, "Participant connection read failed");
                break;
            }
        }
    }

    // 3. However we got here, the participant is gone.
    registry.deregister(&name);
    drop(outbound_tx);
    let _ = writer.await;
    info!(participant = %name, "Participant connection closed");
}

/// Parse and apply the registration frame, returning the claimed name.
fn register_from(
    line: &str,
    registry: &ParticipantRegistry,
    outbound: mpsc::UnboundedSender<BrokerFrame>,
) -> Result<String, String> {
    let frame: ParticipantFrame =
        serde_json::from_str(line).map_err(|e| format!("malformed first frame: {e}"))?;
    let ParticipantFrame::Register { card } = frame else {
        return Err("the first frame on a participant connection must be 'register'".to_string());
    };
    registry.register(card, outbound).map_err(|e| e.to_string())
}

/// Write queued broker frames as newline-delimited JSON until the queue is
/// closed or the socket refuses a write.
async fn write_frames<W>(mut write_half: W, mut outbound: mpsc::UnboundedReceiver<BrokerFrame>)
where
    W: AsyncWrite + Unpin,
{
    while let Some(frame) = outbound.recv().await {
        let Ok(mut line) = serde_json::to_string(&frame) else {
            warn!("Failed to serialize a broker frame; dropping it");
            continue;
        };
        line.push('\n');
        if let Err(e) = write_half.write_all(line.as_bytes()).await {
            debug!(error = %e, "Participant socket write failed; writer stopping");
            return;
        }
        if let Err(e) = write_half.flush().await {
            debug!(error = %e, "Participant socket flush failed; writer stopping");
            return;
        }
    }
}

/// Remove the socket file, if it is still ours to remove.
///
/// Called on gateway shutdown. Failure is logged and ignored: a leftover
/// file is cleaned up by the next [`bind`] on the same path.
pub fn unbind(path: &PathBuf) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        debug!(socket = %path.display(), error = %e, "Could not remove the participant socket");
    }
}
