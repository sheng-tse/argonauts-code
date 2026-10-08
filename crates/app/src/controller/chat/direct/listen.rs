//! The loopback listener a direct turn's questions come in on. Each connection carries one
//! question and waits for its answer, which the loop sends back through a oneshot once you
//! answer the card. The listener lives as long as the turn.

use std::net::{Ipv4Addr, SocketAddr};

use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::names::TurnId;
use argonauts_core::worker::Asking;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::controller::Internal;
use crate::controller::chat::TurnUpdate;

/// A question longer than this is refused; a permission prompt's input is a tool call's.
const MAX_QUESTION_BYTES: u64 = 4 * 1024 * 1024;

/// One question from the turn's session, held until you answer.
#[derive(Debug)]
pub(crate) struct Asked {
    pub ask: Ask,
    pub reply: oneshot::Sender<AskReply>,
}

/// Stops listening when dropped, which ends every question still open with a no.
#[derive(Debug)]
pub(crate) struct Listener {
    pub endpoint: String,
    pub token: String,
    task: JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Listener {
    /// The session's way to ask: `tools` are the main session's tools it is offered, and
    /// `standing` the lines that tell it about them.
    pub(crate) fn asking(
        &self,
        asks: argonauts_core::Asks,
        tools: Vec<String>,
        standing: Option<String>,
    ) -> Asking {
        Asking {
            asks,
            endpoint: self.endpoint.clone(),
            token: self.token.clone(),
            tools,
            standing,
            roles: Vec::new(),
        }
    }
}

/// Listens on a free loopback port for `turn`'s questions.
pub(crate) fn listen(
    internal: mpsc::UnboundedSender<Internal>,
    turn: TurnId,
) -> std::io::Result<Listener> {
    let std_listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener)?;
    let endpoint = listener.local_addr()?.to_string();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let expected = token.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let internal = internal.clone();
            let turn = turn.clone();
            let expected = expected.clone();
            tokio::spawn(async move {
                // A broken connection is the session's to report; nothing waits on it here.
                let _ = answer(stream, &expected, internal, turn).await;
            });
        }
    });
    Ok(Listener {
        endpoint,
        token,
        task,
    })
}

async fn answer(
    stream: TcpStream,
    token: &str,
    internal: mpsc::UnboundedSender<Internal>,
    turn: TurnId,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read.take(MAX_QUESTION_BYTES))
        .read_line(&mut line)
        .await?;
    let reply = match serde_json::from_str::<AskLine>(line.trim()) {
        Ok(asked) if asked.token == token => {
            let (reply, answered) = oneshot::channel();
            let update = TurnUpdate::Asked {
                turn,
                asked: Asked {
                    ask: asked.ask,
                    reply,
                },
            };
            if internal.send(Internal::Chat(update)).is_err() {
                return Ok(());
            }
            // The question closed without an answer, such as when the turn ended: a no.
            answered
                .await
                .unwrap_or_else(|_| AskReply::deny("The turn ended before the user answered."))
        }
        Ok(_) => AskReply {
            text: "That token doesn't belong to this turn.".to_owned(),
            is_error: true,
        },
        Err(err) => AskReply {
            text: format!("That question couldn't be read: {err}"),
            is_error: true,
        },
    };
    let mut text = serde_json::to_string(&reply).map_err(std::io::Error::other)?;
    text.push('\n');
    write.write_all(text.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_question_reaches_the_loop_and_its_answer_goes_back() {
        let (internal, mut updates) = mpsc::unbounded_channel();
        let listener = listen(internal, TurnId::numbered(1)).unwrap();
        let ask = AskLine {
            token: listener.token.clone(),
            ask: Ask::Permission {
                tool_name: "Bash".into(),
                input: json!({"command": "ls"}),
                tool_use_id: None,
            },
        };
        let endpoint = listener.endpoint.clone();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(endpoint).await.unwrap();
            let line = format!("{}\n", serde_json::to_string(&ask).unwrap());
            stream.write_all(line.as_bytes()).await.unwrap();
            let mut answer = String::new();
            BufReader::new(stream).read_line(&mut answer).await.unwrap();
            serde_json::from_str::<AskReply>(answer.trim()).unwrap()
        });
        let Some(Internal::Chat(TurnUpdate::Asked { turn, asked })) = updates.recv().await else {
            panic!("no question came in");
        };
        assert_eq!(turn, TurnId::numbered(1));
        asked
            .reply
            .send(AskReply::allow(&json!({"command": "ls"})))
            .unwrap();
        let answer = client.await.unwrap();
        assert!(answer.text.contains("\"allow\""), "{answer:?}");
    }

    #[tokio::test]
    async fn a_question_without_the_token_never_reaches_you() {
        let (internal, mut updates) = mpsc::unbounded_channel();
        let listener = listen(internal, TurnId::numbered(1)).unwrap();
        let mut stream = TcpStream::connect(&listener.endpoint).await.unwrap();
        let line = r#"{"token":"wrong","kind":"permission","tool_name":"Bash","input":{}}"#;
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).await.unwrap();
        let reply: AskReply = serde_json::from_str(answer.trim()).unwrap();
        assert!(reply.is_error);
        drop(listener);
        assert!(updates.try_recv().is_err());
    }
}
