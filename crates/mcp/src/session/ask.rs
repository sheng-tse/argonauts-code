//! A direct turn's questions. Each call opens a connection to the app, sends one line, and
//! waits for one line back, however long you take on the question. This file holds Claude
//! Code's permission prompt and the way to the app; the main session's own tools are in
//! `main_tools`.

use std::sync::Arc;

use argonauts_core::asking::{Ask, AskLine, AskReply};
use rmcp::model::{CallToolResult, ContentBlock, Tool};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::main_tools::{self, object};
use crate::AskTool;

/// Claude Code's permission prompt tool, which it calls instead of asking in its own UI. It
/// never shows in the model's tool list (2.1.289).
pub const APPROVE_TOOL: &str = "approve";

pub(super) fn tools(asking: &AskTool) -> Vec<Tool> {
    let mut tools = Vec::new();
    if asking.approve {
        tools.push(approve_tool());
    }
    tools.extend(main_tools::offered(asking));
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

/// `None` when `name` isn't one of these tools.
pub(super) async fn call(
    asking: &AskTool,
    name: &str,
    arguments: &Value,
) -> Option<CallToolResult> {
    if name != APPROVE_TOOL || !asking.approve {
        return main_tools::call(asking, name, arguments).await;
    }
    let ask = Ask::Permission {
        tool_name: string(arguments, "tool_name"),
        input: arguments.get("input").cloned().unwrap_or(Value::Null),
        tool_use_id: arguments
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    // Nothing to ask means no, so a lost app never lets a call through.
    let reply = send(asking, ask).await.unwrap_or_else(|err| {
        AskReply::deny(&format!(
            "Argonauts couldn't ask the user ({err}), so this wasn't allowed."
        ))
    });
    let content = vec![ContentBlock::text(reply.text)];
    Some(if reply.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    })
}

pub(super) async fn send(asking: &AskTool, ask: Ask) -> anyhow::Result<AskReply> {
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

pub(super) fn string(arguments: &Value, key: &str) -> String {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::START_WORKFLOW_TOOL;
    use tokio::net::TcpListener;

    async fn listener() -> (TcpListener, AskTool) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let asking = AskTool {
            endpoint: listener.local_addr().unwrap().to_string(),
            token: "secret".into(),
            approve: true,
            tools: vec![START_WORKFLOW_TOOL.to_owned()],
            roles: Vec::new(),
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
        assert_eq!(names, [APPROVE_TOOL, START_WORKFLOW_TOOL]);
    }

    #[tokio::test]
    async fn a_main_tool_s_call_reaches_the_app_under_its_name() {
        let (listener, asking) = listener().await;
        let app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let asked: AskLine = serde_json::from_str(&line).unwrap();
            let reply = serde_json::to_string(&AskReply::text("The workflow started.")).unwrap();
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            asked
        });
        let arguments = json!({"request": "add due dates to tasks"});
        let result = call(&asking, START_WORKFLOW_TOOL, &arguments)
            .await
            .unwrap();
        assert_eq!(text(&result), "The workflow started.");
        assert_ne!(result.is_error, Some(true));
        let asked = app.await.unwrap();
        assert_eq!(
            asked.ask,
            Ask::Tool {
                name: START_WORKFLOW_TOOL.to_owned(),
                arguments,
            }
        );
    }

    #[tokio::test]
    async fn without_the_app_a_main_tool_says_so_and_starts_nothing() {
        let (listener, asking) = listener().await;
        drop(listener);
        let result = call(&asking, START_WORKFLOW_TOOL, &json!({"request": "x"}))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("couldn't be reached"),
            "{}",
            text(&result)
        );
    }
}
