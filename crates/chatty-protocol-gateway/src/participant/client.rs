//! The participant side of the socket: what a worker talks.
//!
//! It lives next to the listener so the two halves of the protocol cannot
//! drift. A worker (`chatty-tui --participant-socket …`) owns one of these
//! for its whole life: connect, register, then answer tasks until the broker
//! closes the socket or the process exits.
//!
//! The transport is the caller's, not this type's. It is a Unix socket on
//! the desktop and an `AF_VSOCK` stream from inside a microVM (AGE-307), and
//! the frames are identical over both — which is the whole reason C1's
//! contract could be reused for the hosted half. The halves are boxed rather
//! than the type being generic so that the shared worker loop stays one
//! concrete type regardless of what it is speaking over.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tracing::debug;

use super::protocol::{BrokerFrame, ParticipantCard, ParticipantFrame, TaskState};

type BoxedRead = Box<dyn AsyncRead + Unpin + Send>;
type BoxedWrite = Box<dyn AsyncWrite + Unpin + Send>;

/// A registered connection to the broker.
pub struct ParticipantConnection {
    lines: Lines<BufReader<BoxedRead>>,
    write: BoxedWrite,
    name: String,
}

impl ParticipantConnection {
    /// Connect to `socket` and register `card`.
    ///
    /// Returns once the broker has acknowledged. A rejection — a duplicate
    /// name, a card without one — is an error here rather than a state the
    /// caller has to poll for, because a participant that is not registered
    /// has nothing else it can do.
    pub async fn register(socket: impl AsRef<Path>, card: ParticipantCard) -> Result<Self> {
        let socket = socket.as_ref();
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("failed to reach the broker at {}", socket.display()))?;
        Self::register_over(stream, card).await
    }

    /// Register `card` over an already-connected stream.
    ///
    /// The general form: a microVM's worker reaches the broker over vsock,
    /// which is not a path (AGE-307).
    pub async fn register_over<S>(stream: S, card: ParticipantCard) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read, write) = tokio::io::split(stream);

        let mut conn = Self {
            lines: BufReader::new(Box::new(read) as BoxedRead).lines(),
            write: Box::new(write) as BoxedWrite,
            name: card.name.clone(),
        };
        conn.send(ParticipantFrame::Register { card }).await?;

        match conn.next_frame().await? {
            Some(BrokerFrame::Registered { name }) => {
                debug!(participant = %name, "Registered with the broker");
                conn.name = name;
                Ok(conn)
            }
            Some(BrokerFrame::Rejected { reason }) => {
                bail!("the broker refused the registration: {reason}")
            }
            Some(other) => bail!("expected a registration reply, got {other:?}"),
            None => bail!("the broker closed the socket without replying to the registration"),
        }
    }

    /// The name callers address this participant by. The broker's is
    /// authoritative — it echoes what it actually recorded.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The next frame from the broker, or `None` when it closes the socket.
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
            return Ok(Some(serde_json::from_str(&line).with_context(|| {
                format!("the broker sent a frame this build cannot parse: {line}")
            })?));
        }
    }

    pub async fn send(&mut self, frame: ParticipantFrame) -> Result<()> {
        let mut line = serde_json::to_string(&frame).context("failed to encode a frame")?;
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
