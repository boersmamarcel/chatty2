//! Broker-made connections, and the shared socket nobody registers on.
//!
//! One connection is one participant for as long as it is open. The
//! connection *is* the liveness signal (ADR-0011): when the read loop ends,
//! for any reason, the participant is deregistered and its open tasks are
//! failed. There is no heartbeat, because a process that has died cannot
//! fail to send one.
//!
//! The connection is also the identity (ADR-0020). The broker admits a node
//! and makes the connection for it — a socket pair for a local worker
//! ([`open_connection`]) — so what speaks on it is whoever the broker handed
//! the other end to. The worker's `hello` claims nothing; the broker answers
//! with `welcome` and the name it admitted.
//!
//! The shared socket ([`bind`], [`serve`]) is where workers used to register
//! themselves, first come first named. It stays bound, and refuses every
//! connection: nothing registers there, so no local process can take a name
//! the broker is about to route a task to.
//!
//! Unix only. The hosted half is a vsock connection the hosted broker makes
//! per microVM (HS-4, AGE-678); [`serve_connection`] is not shaped by the
//! socket family, so it reuses it.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::calls::Caller;
use super::protocol::{
    BrokerFrame, FrameError, ParticipantFrame, TaskInput, decode_frame, encode_frame,
};
use super::registry::{AdmittedNode, ParticipantRegistry};
use chatty_fabric::{AgentOrigin, CallError, CallEvent, CallStream};
use futures::StreamExt;
use tokio::task::JoinSet;

/// Why every connection on the shared socket is refused.
const SHARED_SOCKET_REFUSAL: &str = "this socket admits no workers: a worker's connection \
                                     is made by the broker that spawns it (ADR-0020)";

/// How long a connection on the shared socket gets to send its first line.
const REFUSAL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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

/// Accept connections on the shared socket until `listener` is dropped or
/// the task is aborted, refusing each one.
pub async fn serve(listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tokio::spawn(refuse(stream));
            }
            Err(e) => {
                warn!(error = %e, "Participant socket accept failed; listener stopping");
                return;
            }
        }
    }
}

/// Answer a connection on the shared socket with an `error` frame and
/// close it. A frame that is not v2 is told so; anything else — `hello`
/// included — is told that this socket registers nobody.
async fn refuse(stream: UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    // Bounded, so a peer that connects and says nothing does not hold a
    // task open for the life of the broker.
    let first = tokio::time::timeout(REFUSAL_READ_TIMEOUT, lines.next_line()).await;
    let reason = match first {
        Ok(Ok(Some(line))) => match decode_frame::<ParticipantFrame>(&line) {
            Err(e) if e.is_version() => e.to_string(),
            _ => SHARED_SOCKET_REFUSAL.to_string(),
        },
        Ok(Ok(None)) | Ok(Err(_)) => return,
        Err(_) => SHARED_SOCKET_REFUSAL.to_string(),
    };
    warn!(%reason, "Refused a connection on the shared participant socket");
    let _ = write_frame(&mut write_half, &BrokerFrame::Error { reason }, None).await;
}

/// A connection the broker made for a local worker: the node's name, and
/// the worker's end, which the caller hands to the process it spawns.
///
/// The broker's end is already being served. Dropping the worker's end
/// without handing it on closes the connection, which abandons the node.
#[derive(Debug)]
pub struct LocalConnection {
    pub name: String,
    pub worker_end: std::os::unix::net::UnixStream,
}

/// Admit a node started as `spec` for `owner` (see
/// [`ParticipantRegistry::admit`]), make a socket pair for it, and serve the
/// broker's end. Both ends are close-on-exec, so neither leaks into a
/// process nobody meant to give it to; the spawner clears the flag on the
/// worker's end in the one child that gets it.
///
/// Must be called within a Tokio runtime.
pub fn open_connection(
    registry: &ParticipantRegistry,
    spec: &str,
    owner: Option<&str>,
) -> io::Result<LocalConnection> {
    let node = registry
        .admit(spec, AgentOrigin::Local, owner)
        .map_err(io::Error::other)?;
    let name = node.name().to_string();
    let (broker_end, worker_end) = std::os::unix::net::UnixStream::pair()?;
    broker_end.set_nonblocking(true)?;
    let broker_end = UnixStream::from_std(broker_end)?;
    tokio::spawn(serve_connection(broker_end, registry.clone(), node));
    Ok(LocalConnection { name, worker_end })
}

/// Drive one broker-made connection from its `hello` to its last frame.
///
/// Split into read and write halves: the write half drains the registry's
/// outbound queue for this participant, the read half owns registration and
/// routing. When the read half returns, the participant is deregistered and
/// the outbound sender is dropped, which ends the writer.
pub async fn serve_connection<S>(stream: S, registry: ParticipantRegistry, node: AdmittedNode)
where
    S: tokio::io::AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<BrokerFrame>();
    let tap = registry.wire_tap();
    // Every line read is copied to the wire tap, if one is installed.
    let decode = |line: &str| {
        if let Some(tap) = tap.as_ref() {
            let _ = tap.send(format!("<- {line}"));
        }
        decode_frame::<ParticipantFrame>(line)
    };

    let writer = tokio::spawn(write_frames(write_half, outbound_rx, tap.clone()));

    // 1. The first frame must be a v2 `hello`. The welcome is queued before
    // the node is registered, so it is on the wire ahead of any task.
    let name = match lines.next_line().await {
        Ok(Some(line)) => match decode(&line) {
            Ok(ParticipantFrame::Hello { card }) => {
                let _ = outbound_tx.send(node.welcome());
                registry.register(node, card, outbound_tx.clone())
            }
            refused => {
                let reason = match refused {
                    Err(e) if e.is_version() => e.to_string(),
                    Err(e) => format!("the first frame must be a v2 'hello': {e}"),
                    Ok(_) => "the first frame must be a v2 'hello'".to_string(),
                };
                warn!(node = node.name(), %reason, "Refusing a participant connection");
                registry.abandon(node);
                let _ = outbound_tx.send(BrokerFrame::Error { reason });
                drop(outbound_tx);
                let _ = writer.await;
                return;
            }
        },
        Ok(None) => {
            debug!(
                node = node.name(),
                "A connection closed before its worker said hello"
            );
            registry.abandon(node);
            return;
        }
        Err(e) => {
            warn!(node = node.name(), error = %e, "A connection failed before its worker said hello");
            registry.abandon(node);
            return;
        }
    };

    // 2. Task traffic and calls until the socket closes. Each call runs on
    // its own task, so several can be in flight and finish in any order;
    // the set owns them, so closing the connection cancels every one.
    let mut calls = JoinSet::new();
    // Which callee task each of this connection's calls has parked on a
    // question (BI-5): an answer is delivered only to the task its call
    // names, so a worker answers its own callees and nobody else's.
    let parked: Parked = Arc::default();
    loop {
        while calls.try_join_next().is_some() {}
        match lines.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => continue,
            Ok(Some(line)) => match decode(&line) {
                // A peer that stops speaking v2 is not ours to guess at.
                Err(e @ (FrameError::MissingVersion | FrameError::WrongVersion(_))) => {
                    warn!(participant = %name, error = %e, "Closing the connection: not v2");
                    let _ = outbound_tx.send(BrokerFrame::Error {
                        reason: e.to_string(),
                    });
                    break;
                }
                // A malformed line is dropped, not fatal: one bad frame
                // should not fail every task the participant still owes.
                Err(e) => warn!(participant = %name, error = %e, "Ignoring a malformed frame"),
                Ok(ParticipantFrame::Call { id, request }) => match registry.calls() {
                    Some(broker) => {
                        let stream = broker.call(Caller::Node(name.clone()), request);
                        calls.spawn(reply(id, stream, outbound_tx.clone(), parked.clone()));
                    }
                    None => {
                        let _ = outbound_tx.send(BrokerFrame::CallError {
                            id,
                            error: CallError::Refused("this broker takes no calls".to_string()),
                        });
                    }
                },
                Ok(ParticipantFrame::CallInput { id, task, input }) => {
                    answer_callee(&registry, &parked, &name, id, &task, input);
                }
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

    // 3. However we got here, the participant is gone, and so is everything
    // it had asked for: its calls are cancelled, which reaps the workers
    // they started.
    calls.abort_all();
    registry.deregister(&name);
    drop(outbound_tx);
    let _ = writer.await;
    info!(participant = %name, "Participant connection closed");
}

/// Call id → the callee task that call has parked on a question.
type Parked = Arc<Mutex<HashMap<u64, String>>>;

/// Send call `id`'s events back as `call_progress` (and
/// `call_input_required` when its callee asks a question), then one
/// `call_result` or `call_error`.
async fn reply(
    id: u64,
    mut stream: CallStream,
    outbound: mpsc::UnboundedSender<BrokerFrame>,
    parked: Parked,
) {
    // However the call ends, it has nothing parked any more.
    struct Unpark(Parked, u64);
    impl Drop for Unpark {
        fn drop(&mut self) {
            lock(&self.0).remove(&self.1);
        }
    }
    let _unpark = Unpark(parked.clone(), id);

    while let Some(event) = stream.next().await {
        let (frame, last) = match event {
            Ok(CallEvent::Progress(event)) => (BrokerFrame::CallProgress { id, event }, false),
            Ok(CallEvent::InputRequired { task, request }) => {
                lock(&parked).insert(id, task.clone());
                (BrokerFrame::CallInputRequired { id, task, request }, false)
            }
            Ok(CallEvent::Result(result)) => (BrokerFrame::CallResult { id, result }, true),
            // Only a root call hears its nested runs (TB-1); a worker's
            // call is one of them.
            Ok(CallEvent::Swarm(_)) => continue,
            Err(error) => (BrokerFrame::CallError { id, error }, true),
        };
        if outbound.send(frame).is_err() || last {
            return;
        }
    }
    let _ = outbound.send(BrokerFrame::CallError {
        id,
        error: CallError::Failed("the call ended without a result".to_string()),
    });
}

/// Deliver `node`'s answer to the question its call `id`'s callee parked
/// `task` on. Anything else — a call with nothing parked, another task — is
/// refused with a log line: the connection names who may answer what.
fn answer_callee(
    registry: &ParticipantRegistry,
    parked: &Parked,
    node: &str,
    id: u64,
    task: &str,
    input: TaskInput,
) {
    if lock(parked).get(&id).map(String::as_str) != Some(task) {
        warn!(participant = %node, call = id, task, "Refused an answer for a task its call did not park");
        return;
    }
    if let Err(error) = registry.answer_task(task, input) {
        // The callee's task is gone; its call ends on its own.
        warn!(participant = %node, call = id, task, %error, "Could not deliver a worker's answer");
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write one frame as a line, copying it to `tap` when there is one.
async fn write_frame<W>(
    write_half: &mut W,
    frame: &BrokerFrame,
    tap: Option<&mpsc::UnboundedSender<String>>,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut line = encode_frame(frame).map_err(io::Error::other)?;
    if let Some(tap) = tap {
        let _ = tap.send(format!("-> {line}"));
    }
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await
}

/// Write queued broker frames as newline-delimited JSON until the queue is
/// closed or the socket refuses a write.
async fn write_frames<W>(
    mut write_half: W,
    mut outbound: mpsc::UnboundedReceiver<BrokerFrame>,
    tap: Option<mpsc::UnboundedSender<String>>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(frame) = outbound.recv().await {
        if let Err(e) = write_frame(&mut write_half, &frame, tap.as_ref()).await {
            debug!(error = %e, "Participant socket write failed; writer stopping");
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
