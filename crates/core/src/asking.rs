//! A direct turn's questions, from the session's MCP server to the app: one JSON line each way
//! over the loopback connection the app listens on for that turn. The server holds the tool
//! call until the answer comes, so a card waits as long as you take.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Where a session's questions go: the app's listener for the turn, and its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskTarget {
    /// `127.0.0.1:<port>`.
    pub endpoint: String,
    pub token: String,
}

impl AskTarget {
    /// Sends one question and waits for its answer, however long you take.
    pub async fn ask(&self, ask: Ask) -> std::io::Result<AskReply> {
        let line = AskLine {
            token: self.token.clone(),
            ask,
        };
        let mut stream = TcpStream::connect(&self.endpoint).await?;
        let mut text = serde_json::to_string(&line).map_err(std::io::Error::other)?;
        text.push('\n');
        stream.write_all(text.as_bytes()).await?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).await?;
        if answer.trim().is_empty() {
            return Err(std::io::Error::other("the app closed the question"));
        }
        serde_json::from_str(answer.trim()).map_err(std::io::Error::other)
    }
}

/// One question, with the turn's token so nothing else on this machine can ask in its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskLine {
    pub token: String,
    #[serde(flatten)]
    pub ask: Ask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ask {
    /// Claude Code's permission prompt for one tool call, as its permission prompt tool gets it.
    Permission {
        tool_name: String,
        #[serde(default)]
        input: Value,
        #[serde(default)]
        tool_use_id: Option<String>,
    },
    /// One of the main session's tools, by the name it's registered under, such as
    /// `argonauts_start_workflow`. The app's own registry decides what a call does.
    Tool {
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    /// `argonauts_ask_reviewers` or `argonauts_ask_judge`: a role of the crew, on its own
    /// model, reads the change in the session's folder and answers.
    Role {
        role: crate::settings::crew::CrewRole,
        /// The change's name in the asking workflow, such as `review:store`.
        #[serde(default)]
        label: String,
        /// What the change is meant to do, and what to look at.
        #[serde(default)]
        what: String,
    },
}

/// What `argonauts_set_goal` does with the goal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalAsk {
    #[default]
    Set,
    Show,
    Pause,
    Resume,
    Clear,
}

/// What the tool call returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskReply {
    pub text: String,
    #[serde(default)]
    pub is_error: bool,
}

impl AskReply {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    /// Claude Code runs the call as asked.
    pub fn allow(input: &Value) -> Self {
        Self::text(json!({"behavior": "allow", "updatedInput": input}).to_string())
    }

    /// Claude Code skips the call, and the model reads `message`.
    pub fn deny(message: &str) -> Self {
        Self::text(json!({"behavior": "deny", "message": message}).to_string())
    }

    /// `None` when the answer allows the call, else what a denial says.
    pub fn denial(&self) -> Option<String> {
        let answer: Value = serde_json::from_str(&self.text).unwrap_or(Value::Null);
        match answer.get("behavior").and_then(Value::as_str) {
            Some("allow") if !self.is_error => None,
            _ => Some(
                answer
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the user said no")
                    .to_owned(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_reads_as_the_permission_prompt_tool_sends_it() {
        let line = r#"{"token":"t","kind":"permission","tool_name":"Edit","input":{"file_path":"a.txt"},"tool_use_id":"toolu_1"}"#;
        let parsed: AskLine = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.token, "t");
        assert_eq!(
            parsed.ask,
            Ask::Permission {
                tool_name: "Edit".into(),
                input: json!({"file_path": "a.txt"}),
                tool_use_id: Some("toolu_1".into()),
            }
        );
        let allow: Value = serde_json::from_str(&AskReply::allow(&json!({"a": 1})).text).unwrap();
        assert_eq!(
            allow,
            json!({"behavior": "allow", "updatedInput": {"a": 1}})
        );
        let deny: Value = serde_json::from_str(&AskReply::deny("no").text).unwrap();
        assert_eq!(deny, json!({"behavior": "deny", "message": "no"}));
    }
}
