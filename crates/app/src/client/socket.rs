//! The client end of a run's control socket: `Hello` first, then one request at a time, or a
//! `Subscribe` that streams `Event` frames.

use std::io;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use argonauts_core::control::{ClientKind, ControlReply, ControlRequest, Frame};
use argonauts_core::events::RunEvent;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::run_files;
use crate::wire::{read_frame, write_frame};

/// A supervisor that accepts but never greets is stuck; don't wait on it forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Status, diff, and raw reads, which the supervisor answers from what it holds.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Changes the engine applies before it answers, such as an answer or a stop.
const CHANGE_TIMEOUT: Duration = Duration::from_secs(60);
/// The engine answers a take-over once the card is paused, which can take as long as the
/// agent's current tool call.
const TAKE_OVER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub(crate) struct Connection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    /// The supervisor's pid from its `Welcome`.
    pub(crate) pid: u32,
}

impl Connection {
    /// `None` when no supervisor listens, such as a parked run's stale socket file.
    pub(crate) async fn connect(run_dir: &Path, kind: ClientKind) -> anyhow::Result<Option<Self>> {
        let path = match run_files::socket_address(run_dir) {
            Ok(path) => path,
            Err(err) if nobody_listens(&err) => return Ok(None),
            Err(err) => return Err(err).context("finding the run's control socket"),
        };
        let stream = match UnixStream::connect(&path).await {
            Ok(stream) => stream,
            Err(err) if nobody_listens(&err) => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("connecting to {}", path.display()));
            }
        };
        let (reader, writer) = stream.into_split();
        let mut connection = Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 0,
            pid: 0,
        };
        let hello = ControlRequest::Hello {
            client: kind,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let greeting = tokio::time::timeout(HANDSHAKE_TIMEOUT, connection.request(hello))
            .await
            .map_err(|_| anyhow!("the run's supervisor accepted but didn't answer"))?;
        match greeting {
            Ok(ControlReply::Welcome { pid, .. }) => {
                connection.pid = pid;
                Ok(Some(connection))
            }
            Ok(ControlReply::Error { message }) => Err(anyhow!(
                "the run's supervisor refused this client: {message}"
            )),
            Ok(other) => Err(anyhow!("the run's supervisor greeted with {other:?}")),
            // It exited between accepting and greeting, which is the same as not listening.
            Err(err) if closed(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The reply to `request`. A supervisor that doesn't answer in time counts as stuck.
    pub(crate) async fn request(
        &mut self,
        request: ControlRequest,
    ) -> anyhow::Result<ControlReply> {
        let within = reply_within(&request);
        let id = self.send(request).await?;
        tokio::time::timeout(within, self.reply(id))
            .await
            .map_err(|_| {
                anyhow!(
                    "the run's supervisor didn't answer within {}s",
                    within.as_secs()
                )
            })?
    }

    async fn reply(&mut self, id: u64) -> anyhow::Result<ControlReply> {
        loop {
            let frame: Frame<ControlReply> = read_frame(&mut self.reader)
                .await?
                .ok_or_else(|| anyhow::Error::new(io::Error::from(io::ErrorKind::UnexpectedEof)))
                .context("the run's supervisor closed the connection")?;
            if frame.id == id {
                return Ok(frame.body);
            }
        }
    }

    /// Asks for the log from `from_seq`, or from the latest snapshot, then live events.
    pub(crate) async fn subscribe(
        mut self,
        from_seq: u64,
        from_snapshot: bool,
    ) -> anyhow::Result<Subscription> {
        let id = self
            .send(ControlRequest::Subscribe {
                from_seq,
                from_snapshot,
            })
            .await?;
        Ok(Subscription {
            reader: self.reader,
            _writer: self.writer,
            id,
        })
    }

    async fn send(&mut self, request: ControlRequest) -> anyhow::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = Frame { id, body: request };
        write_frame(&mut self.writer, &frame)
            .await
            .context("writing to the run's supervisor")?;
        Ok(id)
    }
}

/// The events of one `Subscribe`.
pub(crate) struct Subscription {
    reader: BufReader<OwnedReadHalf>,
    // Dropping the write half would half-close the socket, which a server may read as goodbye.
    _writer: OwnedWriteHalf,
    id: u64,
}

impl Subscription {
    /// `None` once the supervisor goes away.
    pub(crate) async fn next(&mut self) -> anyhow::Result<Option<RunEvent>> {
        loop {
            let frame: Option<Frame<ControlReply>> = match read_frame(&mut self.reader).await {
                Ok(frame) => frame,
                Err(err) if err.kind() == io::ErrorKind::ConnectionReset => None,
                Err(err) => return Err(err).context("reading from the run's supervisor"),
            };
            let Some(frame) = frame else {
                return Ok(None);
            };
            if frame.id != self.id {
                continue;
            }
            match frame.body {
                ControlReply::Event { event } => return Ok(Some(*event)),
                ControlReply::Error { message } => {
                    return Err(anyhow!("the run's supervisor ended the stream: {message}"));
                }
                _ => continue,
            }
        }
    }
}

fn reply_within(request: &ControlRequest) -> Duration {
    match request {
        ControlRequest::Hello { .. } => HANDSHAKE_TIMEOUT,
        ControlRequest::Status | ControlRequest::Diff { .. } | ControlRequest::Raw { .. } => {
            READ_TIMEOUT
        }
        ControlRequest::TakeOver { .. } => TAKE_OVER_TIMEOUT,
        ControlRequest::Subscribe { .. }
        | ControlRequest::Steer { .. }
        | ControlRequest::Mention { .. }
        | ControlRequest::Guidance { .. }
        | ControlRequest::Pause { .. }
        | ControlRequest::CancelPause
        | ControlRequest::Stop
        | ControlRequest::StopSession { .. }
        | ControlRequest::TakeBack { .. }
        | ControlRequest::ApprovalMode { .. }
        | ControlRequest::Reassign { .. }
        | ControlRequest::TakeOverEnded { .. }
        | ControlRequest::Answer { .. }
        | ControlRequest::Revert { .. }
        | ControlRequest::SpotCheck { .. }
        | ControlRequest::Goal { .. } => CHANGE_TIMEOUT,
    }
}

fn nobody_listens(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn closed(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<io::Error>().is_some_and(|err| {
            matches!(
                err.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            )
        })
    })
}
