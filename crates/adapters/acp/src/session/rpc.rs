//! Newline-delimited JSON-RPC 2.0 over the agent's pipes. Every frame, both ways, is copied to
//! the worker's raw log.

use argonauts_core::agents::driver::RawFrame;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::sync::mpsc::UnboundedSender;

pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INTERNAL_ERROR: i64 = -32603;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    fn from_value(error: &Value) -> Self {
        Self {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            data: error.get("data").filter(|data| !data.is_null()).cloned(),
        }
    }

    /// The message and any data, for the profile's error patterns.
    pub fn text(&self) -> String {
        match &self.data {
            Some(Value::String(data)) => format!("{} {data}", self.message),
            Some(data) => format!("{} {data}", self.message),
            None => self.message.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    Response {
        id: u64,
        outcome: Result<Value, RpcError>,
    },
    /// A call from the agent, such as `session/request_permission`.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// Anything else, kept raw.
    Other(Value),
}

impl Incoming {
    pub fn parse(line: &str) -> Self {
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return Self::Other(Value::String(line.to_owned()));
        };
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = frame.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (Some(method), Some(id)) => Self::Request {
                id: id.clone(),
                method,
                params,
            },
            (Some(method), None) => Self::Notification { method, params },
            (None, Some(id)) => match (id.as_u64(), frame.get("error"), frame.get("result")) {
                (Some(id), Some(error), _) => Self::Response {
                    id,
                    outcome: Err(RpcError::from_value(error)),
                },
                (Some(id), None, Some(result)) => Self::Response {
                    id,
                    outcome: Ok(result.clone()),
                },
                _ => Self::Other(frame),
            },
            (None, None) => Self::Other(frame),
        }
    }
}

/// The pipes closed, or a write to the agent failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Closed;

pub(crate) struct Wire {
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    lines: Lines<BufReader<Box<dyn AsyncRead + Send + Unpin>>>,
    frames: UnboundedSender<RawFrame>,
    next_id: u64,
}

impl Wire {
    pub fn new(
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        frames: UnboundedSender<RawFrame>,
    ) -> Self {
        Self {
            writer,
            lines: BufReader::new(reader).lines(),
            frames,
            next_id: 0,
        }
    }

    /// Sends a request and returns its id. The answer comes back through [`Wire::next`].
    pub async fn request(&mut self, method: &str, params: Value) -> Result<u64, Closed> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        Ok(id)
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), Closed> {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    pub async fn respond(&mut self, id: Value, result: Value) -> Result<(), Closed> {
        self.write(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await
    }

    pub async fn respond_error(
        &mut self,
        id: Value,
        code: i64,
        message: &str,
    ) -> Result<(), Closed> {
        self.write(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }))
        .await
    }

    async fn write(&mut self, frame: Value) -> Result<(), Closed> {
        let line = frame.to_string();
        self.log(true, &line);
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.map_err(|_| Closed)?;
        self.writer.flush().await.map_err(|_| Closed)
    }

    /// The next frame from the agent, or `None` once its stdout closes or stops being text.
    /// Cancel-safe, so it can sit in a `select!`.
    pub async fn next(&mut self) -> Option<Incoming> {
        loop {
            let line = self.lines.next_line().await.ok().flatten()?;
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            self.log(false, line);
            return Some(Incoming::parse(line));
        }
    }

    fn log(&self, outgoing: bool, line: &str) {
        // The worker stops logging only when it's done with the session, so a closed log has
        // nobody left to tell.
        let _ = self.frames.send(RawFrame {
            outgoing,
            line: line.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_sorted_by_shape() {
        assert!(matches!(
            Incoming::parse(r#"{"jsonrpc":"2.0","id":3,"result":null}"#),
            Incoming::Response {
                id: 3,
                outcome: Ok(Value::Null)
            }
        ));
        let Incoming::Response {
            outcome: Err(error),
            ..
        } = Incoming::parse(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"Authentication required","data":"run hermes login"}}"#,
        )
        else {
            panic!("not an error response");
        };
        assert_eq!(error.code, -32000);
        assert_eq!(error.text(), "Authentication required run hermes login");
        assert!(matches!(
            Incoming::parse(r#"{"jsonrpc":"2.0","id":"p-1","method":"session/request_permission","params":{}}"#),
            Incoming::Request { ref method, .. } if method == "session/request_permission"
        ));
        assert!(matches!(
            Incoming::parse(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#),
            Incoming::Notification { .. }
        ));
        assert_eq!(
            Incoming::parse("not json"),
            Incoming::Other(Value::String("not json".into()))
        );
        assert!(matches!(
            Incoming::parse(r#"{"jsonrpc":"2.0","id":"x","result":{}}"#),
            Incoming::Other(_)
        ));
    }
}
