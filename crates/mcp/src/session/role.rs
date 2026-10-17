//! `argonauts_ask_reviewer` and `argonauts_ask_judge`: a session asks a role of the crew, and
//! Argonauts Code runs that role's model on the change in the session's folder and returns its
//! answer. Claude Code's own workflow calls them at each review step when the
//! reviewers or the judge are on another CLI, and nothing merges that didn't go through them.
//! The main session has them too, for when you ask a role yourself.

use std::sync::Arc;

use argonauts_core::asking::{Ask, AskReply};
use argonauts_core::settings::crew::CrewRole;
use rmcp::model::{CallToolResult, ContentBlock, Tool};
use serde_json::{Value, json};

use super::ask::{send, string};
use super::main_tools::object;
use crate::AskTool;

const PREFIX: &str = "argonauts_ask_";

/// The tool that asks `role`.
pub fn tool_name(role: CrewRole) -> String {
    format!("{PREFIX}{}", role.as_str())
}

pub(super) fn tools(asking: &AskTool) -> Vec<Tool> {
    asking
        .roles
        .iter()
        .map(|role| {
            Tool::new(
                tool_name(*role),
                description(*role),
                Arc::new(object(json!({
                    "type": "object",
                    "properties": {
                        "label": {
                            "type": "string",
                            "description": label_hint(*role)
                        },
                        "what": {
                            "type": "string",
                            "description": "What the change is meant to do, and anything to \
                                            look at closely."
                        }
                    },
                    "required": ["what"]
                }))),
            )
        })
        .collect()
}

/// What the label is, with an example in the role's own word.
fn label_hint(role: CrewRole) -> &'static str {
    match role {
        CrewRole::Judge => "The change's name in your workflow, such as sign-off:store.",
        _ => "The change's name in your workflow, such as review:store.",
    }
}

fn description(role: CrewRole) -> String {
    let who = match role {
        CrewRole::Judge => "the crew's judge to sign off on",
        _ => "the crew's reviewers to review",
    };
    format!(
        "Asks {who} the change now in your folder. Argonauts Code runs that role's own model on \
         the change and returns its answer, each finding marked blocking or a note. In a workflow \
         of your own, call it at the review step with the label your workflow gives that agent. \
         Argonauts Code goes through the final change itself before it merges, so don't call it \
         again for that, and when it says the role was asked as often as the task allows, report. \
         Otherwise call it only when the user asks for it."
    )
}

/// `None` when `name` isn't one of these tools.
pub(super) async fn call(
    asking: &AskTool,
    name: &str,
    arguments: &Value,
) -> Option<CallToolResult> {
    let role = asking
        .roles
        .iter()
        .copied()
        .find(|role| tool_name(*role) == name)?;
    let ask = Ask::Role {
        role,
        label: string(arguments, "label"),
        what: string(arguments, "what"),
    };
    let reply = send(asking, ask).await.unwrap_or_else(|err| AskReply {
        text: format!("Argonauts Code couldn't be reached, so nobody reviewed it: {err}"),
        is_error: true,
    });
    let content = vec![ContentBlock::text(reply.text)];
    Some(if reply.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::asking::AskLine;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn the_reviewers_answer_comes_back_to_the_workflow() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let asking = AskTool {
            endpoint: listener.local_addr().unwrap().to_string(),
            token: "secret".into(),
            approve: false,
            tools: Vec::new(),
            roles: vec![CrewRole::Reviewer, CrewRole::Judge],
        };
        let names: Vec<String> = tools(&asking).iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, ["argonauts_ask_reviewer", "argonauts_ask_judge"]);
        let app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let asked: AskLine = serde_json::from_str(&line).unwrap();
            let reply = serde_json::to_string(&AskReply::text("Accepted by Astra.")).unwrap();
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            asked
        });
        let arguments = json!({"label": "review:store", "what": "due dates in the store"});
        let result = call(&asking, "argonauts_ask_reviewer", &arguments)
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false));
        let asked = app.await.unwrap();
        assert_eq!(
            asked.ask,
            Ask::Role {
                role: CrewRole::Reviewer,
                label: "review:store".into(),
                what: "due dates in the store".into(),
            }
        );
        assert!(
            call(&asking, "argonauts_ask_worker", &arguments)
                .await
                .is_none()
        );
    }

    #[test]
    fn each_tool_names_its_label_in_its_own_role_word() {
        let asking = AskTool {
            endpoint: "127.0.0.1:1".into(),
            token: "secret".into(),
            approve: false,
            tools: Vec::new(),
            roles: vec![CrewRole::Reviewer, CrewRole::Judge],
        };
        let hints: Vec<String> = tools(&asking)
            .iter()
            .map(|tool| tool.input_schema["properties"]["label"]["description"].to_string())
            .collect();
        assert!(hints[0].contains("review:store"), "{}", hints[0]);
        assert!(hints[1].contains("sign-off:store"), "{}", hints[1]);
    }

    /// The final review is Argonauts Code's own, so the tool never asks for one more.
    #[test]
    fn the_tool_asks_for_no_final_round_of_its_own() {
        for role in [CrewRole::Reviewer, CrewRole::Judge] {
            let text = description(role);
            assert!(!text.contains("once more"), "{text}");
            assert!(text.contains("blocking or a note"), "{text}");
            assert!(text.contains("the label your workflow gives"), "{text}");
        }
    }
}
