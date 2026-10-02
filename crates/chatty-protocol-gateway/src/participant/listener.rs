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

use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::calls::Caller;
use super::codec::BrokerCodec;
use super::limits::{
    BoundedLines, CALL_BURST, CALLS_PER_SECOND, MAX_FRAME_BYTES, MAX_IN_FLIGHT_CALLS,
    MAX_PENDING_APPROVALS, MAX_PENDING_QUESTIONS, OUTBOUND_QUEUE_FRAMES, RateLimit,
};
use super::protocol::{BrokerFrame, ParticipantFrame};
use super::registry::{AdmittedNode, ParticipantRegistry};
use chatty_fabric::{AgentOrigin, ApprovalVerdict, CallError, CallEvent, CallStream};
use futures::StreamExt;
use tokio::task::JoinSet;

/// Why every connection on the shared socket is refused.
const SHARED_SOCKET_REFUSAL: &str = "this socket admits no workers: a worker's connection \
                                     is made by the broker that spawns it (ADR-0020)";

/// How long a connection on the shared socket gets to send its first line.
const REFUSAL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Bind a participant socket at `path`.
///
/// Its directory must be this user's alone (created `0700` if missing;
/// refused when it is someone else's or open to group or others, ADR-0021
/// § 4). A stale socket file from a crashed broker is removed first, but
/// only when this user owns it: a Unix socket left on disk is not a running
/// process, and refusing to start because of one would make a crash need
/// manual cleanup. A *live* broker on the same path is a different matter,
/// and `bind` fails with `AddrInUse` for it.
pub fn bind(path: impl AsRef<Path>) -> io::Result<UnixListener> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        crate::access::PrivateDir::open(parent)?;
    }
    crate::access::remove_stale_socket(path, crate::access::current_uid())?;
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

/// Refuse a connection on the shared socket and close it. A
/// `session.hello` is answered with an `error` for its id saying that this
/// socket registers nobody; anything else is closed without a reply.
async fn refuse(stream: UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BoundedLines::new(BufReader::new(read_half), MAX_FRAME_BYTES);
    // Bounded, so a peer that connects and says nothing does not hold a
    // task open for the life of the broker.
    let first = tokio::time::timeout(REFUSAL_READ_TIMEOUT, lines.next_line()).await;
    let Ok(Ok(Some(line))) = first else {
        return;
    };
    let codec = BrokerCodec::new();
    let Ok(Some(ParticipantFrame::Hello { .. })) = codec.decode(&line) else {
        warn!("Closed a connection on the shared participant socket: not a hello");
        return;
    };
    let reason = SHARED_SOCKET_REFUSAL.to_string();
    warn!(%reason, "Refused a hello on the shared participant socket");
    let _ = write_frame(
        &mut write_half,
        &codec,
        &BrokerFrame::Error { reason },
        None,
    )
    .await;
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
///
/// Every line goes through one [`BrokerCodec`] (ADR-0021 § 1): the first
/// must be a `session.hello`, and a second one closes the connection; a
/// line that does not decode — another version, a method a worker may not
/// send, a reused in-flight request id — closes it without a reply, and a
/// response naming nothing in flight is dropped.
///
/// Bounded throughout (EN-0b, see [`limits`](super::limits)): a line over
/// [`MAX_FRAME_BYTES`], or one that does not decode, closes this connection
/// — the worker's own frame is the only thing that does. The outbound queue
/// holds [`OUTBOUND_QUEUE_FRAMES`] and makes producers wait when full; a
/// call past [`MAX_IN_FLIGHT_CALLS`] or the call rate is refused.
pub async fn serve_connection<S>(stream: S, registry: ParticipantRegistry, node: AdmittedNode)
where
    S: tokio::io::AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut lines = BoundedLines::new(BufReader::new(read_half), MAX_FRAME_BYTES);
    let (outbound_tx, outbound_rx) = mpsc::channel::<BrokerFrame>(OUTBOUND_QUEUE_FRAMES);
    let tap = registry.wire_tap();
    let codec = BrokerCodec::new();
    // Every line read is copied to the wire tap, if one is installed.
    let decode = |line: &str| {
        if let Some(tap) = tap.as_ref() {
            let _ = tap.send(format!("<- {line}"));
        }
        codec.decode(line)
    };

    let writer = tokio::spawn(write_frames(
        write_half,
        codec.clone(),
        outbound_rx,
        tap.clone(),
    ));

    // 1. The first message must be a `session.hello`; anything else closes
    // the connection without a reply. The welcome is queued before the node
    // is registered, so it is on the wire ahead of any task.
    let name = match lines.next_line().await {
        Ok(Some(line)) => match decode(&line) {
            Ok(Some(ParticipantFrame::Hello { card })) => {
                let _ = outbound_tx.send(node.welcome()).await;
                registry.register(node, card, outbound_tx.clone())
            }
            refused => {
                match refused {
                    Err(e) => {
                        warn!(node = node.name(), error = %e, "Closing a connection: its first line does not decode")
                    }
                    Ok(_) => warn!(
                        node = node.name(),
                        "Closing a connection: its first message is not a session.hello"
                    ),
                }
                registry.abandon(node);
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
    // Each call's task, by request id, so the worker can withdraw one.
    let mut running: HashMap<u64, tokio::task::AbortHandle> = HashMap::new();
    // The worker's `human.approve`s waiting on the root (EN-2a), by request
    // id, so the worker can withdraw one; the set owns them, so closing the
    // connection withdraws every one.
    let mut approvals = JoinSet::new();
    let mut waiting: HashMap<u64, tokio::task::AbortHandle> = HashMap::new();
    // The worker's `human.ask`s climbing the caller chain (EN-2b), the same
    // way: by request id, owned by the set.
    let mut questions = JoinSet::new();
    let mut asking: HashMap<u64, tokio::task::AbortHandle> = HashMap::new();
    let mut rate = RateLimit::new(CALLS_PER_SECOND, CALL_BURST, tokio::time::Instant::now());
    loop {
        while calls.try_join_next().is_some() {}
        running.retain(|_, handle| !handle.is_finished());
        while approvals.try_join_next().is_some() {}
        waiting.retain(|_, handle| !handle.is_finished());
        while questions.try_join_next().is_some() {}
        asking.retain(|_, handle| !handle.is_finished());
        match lines.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => continue,
            Ok(Some(line)) => match decode(&line) {
                // A peer whose line does not decode — not v3, a method it
                // may not send, a reused id — is not ours to guess at. Its
                // own line closes its own connection, and nothing else.
                Err(e) => {
                    warn!(participant = %name, error = %e, "Closing the connection: an undecodable frame");
                    break;
                }
                // Named nothing in flight: dropped by the codec.
                Ok(None) => continue,
                Ok(Some(ParticipantFrame::Call { id, request })) => {
                    let broker = match registry.calls() {
                        None => Err("this broker takes no calls".to_string()),
                        Some(_) if calls.len() >= MAX_IN_FLIGHT_CALLS => Err(format!(
                            "too many calls in flight: at most {MAX_IN_FLIGHT_CALLS} per connection"
                        )),
                        Some(_) if !rate.try_take(tokio::time::Instant::now()) => Err(format!(
                            "too many calls: at most {CALLS_PER_SECOND} per second per connection"
                        )),
                        Some(broker) => Ok(broker),
                    };
                    match broker {
                        Ok(broker) => {
                            let stream = broker.call(Caller::Node(name.clone()), request);
                            let handle = calls.spawn(reply(id, stream, outbound_tx.clone()));
                            running.insert(id, handle);
                        }
                        Err(reason) => {
                            debug!(participant = %name, call = id, %reason, "Refused a call");
                            let _ = outbound_tx
                                .send(BrokerFrame::CallError {
                                    id,
                                    error: CallError::Refused(reason),
                                })
                                .await;
                        }
                    }
                }
                Ok(Some(ParticipantFrame::Ask { id, request })) => {
                    let raised = match registry.calls() {
                        None => Err(CallError::Refused("this broker takes no questions".into())),
                        Some(_) if asking.len() >= MAX_PENDING_QUESTIONS => {
                            Err(CallError::Refused(format!(
                                "too many questions waiting: at most {MAX_PENDING_QUESTIONS} per connection"
                            )))
                        }
                        Some(broker) => broker.raise_question(&name, request),
                    };
                    match raised {
                        Ok(mut raised) => {
                            let outbound = outbound_tx.clone();
                            let handle = questions.spawn(async move {
                                let answers = raised.answers().await;
                                let _ = outbound.send(BrokerFrame::Answer { id, answers }).await;
                            });
                            asking.insert(id, handle);
                        }
                        Err(error) => {
                            debug!(participant = %name, question = id, %error, "A question nobody can answer");
                            let _ = outbound_tx
                                .send(BrokerFrame::Answer {
                                    id,
                                    answers: Err(error),
                                })
                                .await;
                        }
                    }
                }
                Ok(Some(ParticipantFrame::CancelAsk { id })) => {
                    // Dropping the wait withdraws the question wherever it is.
                    if let Some(handle) = asking.remove(&id) {
                        debug!(participant = %name, question = id, "The worker withdrew a question");
                        handle.abort();
                    }
                }
                Ok(Some(ParticipantFrame::AskReply { question, reply })) => {
                    if let Some(broker) = registry.calls() {
                        broker.question_reply(&name, &question, reply);
                    }
                }
                Ok(Some(ParticipantFrame::Approve { id, request })) => {
                    let raised = match registry.calls() {
                        Some(_) if waiting.len() >= MAX_PENDING_APPROVALS => {
                            warn!(participant = %name, approval = id, "Denying an approval: too many pending on this connection");
                            None
                        }
                        Some(broker) => Some(broker.raise_approval(&name, request)),
                        None => None,
                    };
                    match raised {
                        Some(mut raised) => {
                            let outbound = outbound_tx.clone();
                            let handle = approvals.spawn(async move {
                                let verdict = raised.verdict().await;
                                let _ = outbound.send(BrokerFrame::Approval { id, verdict }).await;
                            });
                            waiting.insert(id, handle);
                        }
                        None => {
                            let _ = outbound_tx
                                .send(BrokerFrame::Approval {
                                    id,
                                    verdict: ApprovalVerdict::Denied,
                                })
                                .await;
                        }
                    }
                }
                Ok(Some(ParticipantFrame::CancelApproval { id })) => {
                    // Dropping the wait withdraws the root's card.
                    if let Some(handle) = waiting.remove(&id) {
                        debug!(participant = %name, approval = id, "The worker withdrew an approval");
                        handle.abort();
                    }
                }
                Ok(Some(ParticipantFrame::CancelCall { id })) => {
                    // Aborting the call's task drops its stream, which
                    // cancels the callee; the worker expects no reply.
                    if let Some(handle) = running.remove(&id) {
                        debug!(participant = %name, call = id, "The worker withdrew a call");
                        handle.abort();
                    }
                }
                Ok(Some(
                    frame @ (ParticipantFrame::Hello { .. }
                    | ParticipantFrame::Status { .. }
                    | ParticipantFrame::Artifact { .. }
                    | ParticipantFrame::Event { .. }),
                )) => {
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
            // A line over the cap, or not UTF-8: the worker's own frame.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                warn!(participant = %name, error = %e, "Closing the connection: an unreadable line");
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
    // they started, and its approvals and questions are withdrawn.
    calls.abort_all();
    // Waited for, not just aborted: each withdrawal is on its way before
    // the task's end is, so the root's card and popover — or a caller's
    // copy of a question — go first.
    approvals.shutdown().await;
    questions.shutdown().await;
    registry.deregister(&name);
    drop(outbound_tx);
    let _ = writer.await;
    info!(participant = %name, "Participant connection closed");
}

/// Send call `id`'s events back as `req.progress`, then one result or
/// error for the request.
///
/// A frame that would be over the worker's frame cap — a callee's result
/// larger than this connection accepts — fails this call with an `error`
/// and ends it; the connection and its other calls go on.
/// Waits while the outbound queue is full, which holds the callee's stream
/// back rather than buffering it.
async fn reply(id: u64, mut stream: CallStream, outbound: mpsc::Sender<BrokerFrame>) {
    while let Some(event) = stream.next().await {
        let (frame, last) = match event {
            Ok(CallEvent::Progress(event)) => (BrokerFrame::CallProgress { id, event }, false),
            Ok(CallEvent::Result(result)) => (BrokerFrame::CallResult { id, result }, true),
            // Only a root call hears its nested runs (TB-1), their approvals
            // (EN-2a) and their questions (EN-2b); a worker's call is one of
            // them, and a question reaches a worker on its own connection.
            Ok(
                CallEvent::Swarm(_)
                | CallEvent::Approve { .. }
                | CallEvent::Ask { .. }
                | CallEvent::InputWithdrawn { .. },
            ) => continue,
            Err(error) => (BrokerFrame::CallError { id, error }, true),
        };
        let size = BrokerCodec::line_len_bound(&frame);
        if size > MAX_FRAME_BYTES {
            warn!(
                call = id,
                size, "Failing a call: its frame is over the worker's frame cap"
            );
            let error = CallError::Failed(format!(
                "the call's {size}-byte reply is over the receiver's {MAX_FRAME_BYTES}-byte frame cap"
            ));
            let _ = outbound.send(BrokerFrame::CallError { id, error }).await;
            return;
        }
        if outbound.send(frame).await.is_err() || last {
            return;
        }
    }
    let _ = outbound
        .send(BrokerFrame::CallError {
            id,
            error: CallError::Failed("the call ended without a result".to_string()),
        })
        .await;
}

/// Write one frame as a line, copying it to `tap` when there is one. A
/// frame the codec does not send — it names nothing in flight — is skipped.
async fn write_frame<W>(
    write_half: &mut W,
    codec: &BrokerCodec,
    frame: &BrokerFrame,
    tap: Option<&mpsc::UnboundedSender<String>>,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let Some(mut line) = codec.encode(frame).map_err(io::Error::other)? else {
        return Ok(());
    };
    // Every producer checks its frame against the cap first, so the worker
    // is never sent a line it would close on; one that got past them is
    // dropped here, with the connection kept.
    if line.len() > MAX_FRAME_BYTES {
        warn!(
            size = line.len(),
            "Dropping a frame over the worker's frame cap"
        );
        return Ok(());
    }
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
    codec: BrokerCodec,
    mut outbound: mpsc::Receiver<BrokerFrame>,
    tap: Option<mpsc::UnboundedSender<String>>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(frame) = outbound.recv().await {
        if let Err(e) = write_frame(&mut write_half, &codec, &frame, tap.as_ref()).await {
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

#[cfg(test)]
#[path = "connection_limits_tests.rs"]
mod connection_limits_tests;
