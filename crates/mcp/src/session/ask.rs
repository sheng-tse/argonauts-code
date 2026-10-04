//! A direct turn's questions. Each call opens a connection to the app, sends one line, and
//! waits for one line back, however long you take on the card.

use std::sync::Arc;

use argonauts_core::asking::{Ask, AskLine, AskReply};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::AskTool;

/// Claude Code's permission prompt tool, which it calls instead of asking in its own UI. It
/// never shows in the model's tool list (2.1.289).
pub const APPROVE_TOOL: &str = "approve";
pub const START_RUN_TOOL: &str = "argonauts_start_run";

const START_RUN_DESCRIPTION: &str = "Start an Argonauts run for the user's request. Call it only \
    when the user asks for the crew, for other agents, or for a checked copy, never on your own \
    judgment.";

pub(super) fn tools(asking: &AskTool) -> Vec<Tool> {
    let mut tools = Vec::new();
    if asking.approve {
        tools.push(approve_tool());
    }
    start_run_tools(asking, &mut tools);
    tools
}

fn approve_tool() -> Tool {
    Tool::new(
        APPROVE_TOOL,
        "Asks the user whether a tool call may run.",
        Arc::new(object(json!({
            "type": "object",
            "properties": {
                "tool_name": {"type": "string"},
                "input": {"type": "object"},
                "tool_use_id": {"type": "string"}
            },
            "required": ["tool_name", "input"]
        }))),
    )
}

fn start_run_tools(asking: &AskTool, tools: &mut Vec<Tool>) {
    if asking.start_run {
        tools.push(Tool::new(
            START_RUN_TOOL,
            START_RUN_DESCRIPTION,
            Arc::new(object(json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The user's request, as they said it."
                    },
                    "how_to_run": {
                        "type": "array",
                        "description": "Each thing the user said about how to run it, with \
                                        their own words in `says`, copied exactly: the whole \
                                        phrase, two words at least.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "says": {"type": "string"},
                                "kind": {"type": "string"},
                                "who": {"type": "string"},
                                "model": {"type": "string"},
                                "effort": {"type": "string"},
                                "value": {"type": "string"},
                                "scope": {"type": "string"}
                            },
                            "required": ["says", "kind"]
                        }
                    }
                },
                "required": ["request"]
            }))),
        ));
    }
}

/// `None` when `name` isn't one of these tools.
pub(super) async fn call(
    asking: &AskTool,
    name: &str,
    arguments: &Value,
) -> Option<CallToolResult> {
    let ask = match name {
        APPROVE_TOOL if asking.approve => Ask::Permission {
            tool_name: string(arguments, "tool_name"),
            input: arguments.get("input").cloned().unwrap_or(Value::Null),
            tool_use_id: arguments
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        START_RUN_TOOL if asking.start_run => Ask::StartRun {
            request: string(arguments, "request"),
            how_to_run: arguments
                .get("how_to_run")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        },
        _ => return None,
    };
    let denies = matches!(ask, Ask::Permission { .. });
    let reply = match send(asking, ask).await {
        Ok(reply) => reply,
        // Nothing to ask means no, so a lost app never lets a call through.
        Err(err) if denies => AskReply::deny(&format!(
            "Argonauts couldn't ask the user ({err}), so this wasn't allowed."
        )),
        Err(err) => AskReply {
            text: format!("Argonauts couldn't be reached: {err}"),
            is_error: true,
        },
    };
    let content = vec![ContentBlock::text(reply.text)];
    Some(if reply.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    })
}

async fn send(asking: &AskTool, ask: Ask) -> anyhow::Result<AskReply> {
    let line = AskLine {
        token: asking.token.clone(),
        ask,
    };
    let mut stream = TcpStream::connect(&asking.endpoint).await?;
    let mut text = serde_json::to_string(&line)?;
    text.push('\n');
    stream.write_all(text.as_bytes()).await?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await?;
    anyhow::ensure!(!answer.trim().is_empty(), "the app closed the question");
    Ok(serde_json::from_str(answer.trim())?)
}

fn string(arguments: &Value, key: &str) -> String {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn listener() -> (TcpListener, AskTool) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let asking = AskTool {
            endpoint: listener.local_addr().unwrap().to_string(),
            token: "secret".into(),
            approve: true,
            start_run: true,
        };
        (listener, asking)
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect()
    }

    #[tokio::test]
    async fn a_permission_prompt_waits_for_the_apps_answer() {
        let (listener, asking) = listener().await;
        let app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let asked: AskLine = serde_json::from_str(&line).unwrap();
            let reply = serde_json::to_string(&AskReply::deny("not now")).unwrap();
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            asked
        });
        let arguments =
            json!({"tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1"});
        let result = call(&asking, APPROVE_TOOL, &arguments).await.unwrap();
        let answer: Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(answer, json!({"behavior": "deny", "message": "not now"}));
        let asked = app.await.unwrap();
        assert_eq!(asked.token, "secret");
        assert!(matches!(asked.ask, Ask::Permission { ref tool_name, .. } if tool_name == "Bash"));
    }

    #[tokio::test]
    async fn without_the_app_a_call_is_denied() {
        let (listener, asking) = listener().await;
        drop(listener);
        let arguments = json!({"tool_name": "Edit", "input": {}});
        let result = call(&asking, APPROVE_TOOL, &arguments).await.unwrap();
        let answer: Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(answer["behavior"], "deny");
        assert!(call(&asking, "other", &arguments).await.is_none());
        let tools = tools(&asking);
        let names: Vec<_> = tools.iter().map(|tool| tool.name.to_string()).collect();
        assert_eq!(names, [APPROVE_TOOL, START_RUN_TOOL]);
    }
}
