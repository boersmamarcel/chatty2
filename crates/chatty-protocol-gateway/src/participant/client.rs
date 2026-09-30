//! The participant side of the socket: what a worker talks.
//!
//! It lives next to the listener so the two halves of the protocol cannot
//! drift. A worker (`chatty-tui --participant-fd N`) owns one of these for
//! its whole life: say hello over the connection the broker made for it,
//! learn its name from the welcome, then answer tasks until the broker
//! closes the connection or the process exits.
//!
//! The transport is the caller's, not this type's. It is the worker's end of
//! a socket pair on the desktop and an `AF_VSOCK` stream from inside a
//! microVM (AGE-307, HS-4), and
//! the frames are identical over both — which is the whole reason C1's
//! contract could be reused for the hosted half. The halves are boxed rather
//! than the type being generic so that the shared worker loop stays one
//! concrete type regardless of what it is speaking over.

use anyhow::{Context, Result, bail};
use chatty_fabric::{ConversationScope, NodeName};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tracing::debug;

use super::codec::WorkerCodec;
use super::limits::{BoundedLines, MAX_FRAME_BYTES};
use super::protocol::{BrokerFrame, ParticipantCard, ParticipantFrame, TaskState};

type BoxedRead = Box<dyn AsyncRead + Unpin + Send>;
type BoxedWrite = Box<dyn AsyncWrite + Unpin + Send>;

/// A welcomed connection to the broker.
pub struct ParticipantConnection {
    reader: ParticipantReader,
    writer: ParticipantWriter,
    name: NodeName,
    scope: ConversationScope,
    owner: Option<NodeName>,
}

/// The broker's frames, read one at a time.
///
/// One half of [`ParticipantConnection::into_split`]: a worker running a
/// task reports on the other half while it waits here for the answer to a
/// question it asked (AGE-306), and neither side may hold the other up.
pub struct ParticipantReader {
    lines: BoundedLines<BufReader<BoxedRead>>,
    codec: WorkerCodec,
}

/// The participant's frames, written one per line.
pub struct ParticipantWriter {
    write: BoxedWrite,
    codec: WorkerCodec,
}

impl ParticipantConnection {
    /// Say `hello` with `card` over `stream`, the connection the broker made
    /// for this worker, and wait for the `welcome`.
    ///
    /// A refusal is an error here rather than a state the caller has to poll
    /// for, because a worker the broker did not welcome has nothing else it
    /// can do. The card's `name` is ignored by the broker; [`name`](Self::name)
    /// is what it assigned.
    pub async fn hello_over<S>(stream: S, card: ParticipantCard) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read, write) = tokio::io::split(stream);
        // One codec for the connection, shared by its two halves.
        let codec = WorkerCodec::new();
        let mut reader = ParticipantReader {
            lines: BoundedLines::new(BufReader::new(Box::new(read) as BoxedRead), MAX_FRAME_BYTES),
            codec: codec.clone(),
        };
        let mut writer = ParticipantWriter {
            write: Box::new(write) as BoxedWrite,
            codec,
        };
        writer.send(ParticipantFrame::Hello { card }).await?;

        match reader.next_frame().await? {
            Some(BrokerFrame::Welcome { name, scope, owner }) => {
                debug!(participant = %name, %scope, "Welcomed by the broker");
                Ok(Self {
                    reader,
                    writer,
                    name,
                    scope,
                    owner,
                })
            }
            Some(BrokerFrame::Error { reason }) => {
                bail!("the broker refused the connection: {reason}")
            }
            Some(other) => bail!("expected a welcome, got {other:?}"),
            None => bail!("the broker closed the connection without a welcome"),
        }
    }

    /// The name callers address this participant by, as the broker
    /// assigned it.
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The conversation this participant works for.
    pub fn scope(&self) -> &ConversationScope {
        &self.scope
    }

    /// The node that asked for this participant; `None` when the root did.
    pub fn owner(&self) -> Option<&NodeName> {
        self.owner.as_ref()
    }

    /// The next frame from the broker, or `None` when it closes the socket.
    pub async fn next_frame(&mut self) -> Result<Option<BrokerFrame>> {
        self.reader.next_frame().await
    }

    pub async fn send(&mut self, frame: ParticipantFrame) -> Result<()> {
        self.writer.send(frame).await
    }

    /// Take the two halves apart so they can be driven by different tasks.
    pub fn into_split(self) -> (ParticipantReader, ParticipantWriter) {
        (self.reader, self.writer)
    }

    pub async fn status(
        &mut self,
        task_id: &str,
        state: TaskState,
        message: Option<String>,
    ) -> Result<()> {
        self.send(ParticipantFrame::Status {
            task_id: task_id.to_string(),
            state,
            message,
            metadata: None,
            input: None,
        })
        .await
    }

    /// A terminal status carrying the turn's ledger data (see
    /// [`ParticipantFrame::Status`]).
    pub async fn finish(
        &mut self,
        task_id: &str,
        state: TaskState,
        message: Option<String>,
        metadata: Option<Value>,
    ) -> Result<()> {
        self.send(ParticipantFrame::Status {
            task_id: task_id.to_string(),
            state,
            message,
            metadata,
            input: None,
        })
        .await
    }

    pub async fn artifact(&mut self, task_id: &str, text: String, last_chunk: bool) -> Result<()> {
        self.send(ParticipantFrame::Artifact {
            task_id: task_id.to_string(),
            text,
            last_chunk,
        })
        .await
    }
}

impl ParticipantReader {
    /// The next frame from the broker, or `None` when it closes the socket.
    /// A message naming nothing in flight is dropped by the codec and
    /// skipped here.
    pub async fn next_frame(&mut self) -> Result<Option<BrokerFrame>> {
        loop {
            let Some(line) = self
                .lines
                .next_line()
                .await
                .context("the broker connection failed")?
            else {
                return Ok(None);
            };
            if line.trim().is_empty() {
                continue;
            }
            let frame = self.codec.decode(&line).with_context(|| {
                format!("the broker sent a frame this build cannot accept: {line}")
            })?;
            if let Some(frame) = frame {
                return Ok(Some(frame));
            }
        }
    }
}

impl ParticipantWriter {
    /// Send `frame`. One that names nothing in flight — a status for a task
    /// the broker never ran — is dropped by the codec, not sent.
    pub async fn send(&mut self, frame: ParticipantFrame) -> Result<()> {
        let Some(mut line) = self
            .codec
            .encode(&frame)
            .context("failed to encode a frame")?
        else {
            return Ok(());
        };
        line.push('\n');
        self.write
            .write_all(line.as_bytes())
            .await
            .context("failed to write to the broker")?;
        self.write
            .flush()
            .await
            .context("failed to flush to the broker")
    }
}
