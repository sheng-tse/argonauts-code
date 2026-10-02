//! One JSON `Frame` per line on the control socket, for both ends.

use std::io;

use argonauts_core::control::Frame;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// A longer line is refused rather than buffered, so a stuck peer can't grow memory without end.
pub(crate) const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub(crate) async fn write_frame<W, T>(writer: &mut W, frame: &Frame<T>) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_vec(frame).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

/// `None` when the peer closed the connection between frames.
pub(crate) async fn read_frame<R, T>(reader: &mut R) -> io::Result<Option<Frame<T>>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the connection closed mid-frame",
            ));
        }
        let (taken, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => (end + 1, true),
            None => (available.len(), false),
        };
        line.extend_from_slice(&available[..taken]);
        reader.consume(taken);
        if line.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a control frame is too long",
            ));
        }
        if done {
            break;
        }
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::control::{ClientKind, ControlRequest};
    use tokio::io::BufReader;

    #[tokio::test]
    async fn frames_round_trip_one_per_line() {
        let (client, server) = tokio::io::duplex(1024);
        let (_, mut write) = tokio::io::split(client);
        let (read, _) = tokio::io::split(server);
        let mut read = BufReader::new(read);
        let hello = Frame {
            id: 1,
            body: ControlRequest::Hello {
                client: ClientKind::App,
                version: "0.0.1".to_owned(),
            },
        };
        write_frame(&mut write, &hello).await.unwrap();
        write_frame(
            &mut write,
            &Frame {
                id: 2,
                body: ControlRequest::Status,
            },
        )
        .await
        .unwrap();
        drop(write);
        let first: Frame<ControlRequest> = read_frame(&mut read).await.unwrap().unwrap();
        assert_eq!(first, hello);
        let second: Frame<ControlRequest> = read_frame(&mut read).await.unwrap().unwrap();
        assert_eq!(second.body, ControlRequest::Status);
        let end: Option<Frame<ControlRequest>> = read_frame(&mut read).await.unwrap();
        assert!(end.is_none());
    }
}
