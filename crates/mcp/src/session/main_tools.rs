//! The main session's tools: what the CLI you talk to calls to make something happen outside
//! itself, such as starting a workflow. Nothing leaves that session until it calls one.
//!
//! This is the one place they are registered. A tool has a file of its own beside this one,
//! which gives its name, its description and its input schema, and one line in [`all`]. A call
//! goes to the app as [`Ask::Tool`] under that name, where the app's own registry
//! (`controller/conversation/tools`) does the work and writes the reply.

use std::sync::Arc;

use argonauts_core::runs::asking::{Ask, AskReply};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations};
use serde_json::Value;

use super::{ask, goal, guide, lookup, setup, start_workflow};
use crate::AskTool;

/// One tool of the main session, as its CLI lists it.
#[derive(Debug, Clone, Copy)]
pub struct MainTool {
    pub name: &'static str,
    pub description: &'static str,
    /// A JSON object schema for the call's arguments.
    pub input_schema: fn() -> Value,
    /// Changes nothing, so the CLI may call it without asking you.
    pub read_only: bool,
}

/// Every tool the main session can be offered, in the order it lists them.
pub fn all() -> Vec<MainTool> {
    vec![
        start_workflow::tool(),
        goal::tool(),
        MainTool {
            name: setup::NAME,
            description: setup::DESCRIPTION,
            input_schema: setup::input_schema,
            read_only: setup::READ_ONLY,
        },
        MainTool {
            name: guide::NAME,
            description: guide::DESCRIPTION,
            input_schema: guide::input_schema,
            read_only: guide::READ_ONLY,
        },
        MainTool {
            name: lookup::NAME,
            description: lookup::DESCRIPTION,
            input_schema: lookup::input_schema,
            read_only: lookup::READ_ONLY,
        },
    ]
}

/// The tools this session was offered, by the names `--tool` gave.
pub(super) fn offered(asking: &AskTool) -> Vec<Tool> {
    all()
        .into_iter()
        .filter(|tool| asking.offers(tool.name))
        .map(|tool| {
            let listed = Tool::new(
                tool.name,
                tool.description,
                Arc::new(object((tool.input_schema)())),
            );
            if tool.read_only {
                listed.annotate(ToolAnnotations::new().read_only(true))
            } else {
                listed
            }
        })
        .collect()
}

/// Sends the call to the app and returns its reply. `None` when `name` isn't a tool this
/// session was offered.
pub(super) async fn call(
    asking: &AskTool,
    name: &str,
    arguments: &Value,
) -> Option<CallToolResult> {
    let known = all().iter().any(|tool| tool.name == name);
    if !known || !asking.offers(name) {
        return None;
    }
    let asked = Ask::Tool {
        name: name.to_owned(),
        arguments: arguments.clone(),
    };
    let reply = ask::send(asking, asked)
        .await
        .unwrap_or_else(|err| AskReply {
            text: format!("Argonauts Code couldn't be reached: {err}"),
            is_error: true,
        });
    let content = vec![ContentBlock::text(reply.text)];
    Some(if reply.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    })
}

pub(super) fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_tool_has_its_own_name_and_an_object_schema() {
        let tools = all();
        let names: BTreeSet<&str> = tools.iter().map(|tool| tool.name).collect();
        assert_eq!(names.len(), tools.len(), "two tools share a name");
        for tool in tools {
            assert!(tool.name.starts_with("argonauts_"), "{}", tool.name);
            assert!(!tool.description.trim().is_empty(), "{}", tool.name);
            let schema = (tool.input_schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
        }
    }

    #[tokio::test]
    async fn a_tool_that_was_not_offered_does_not_exist() {
        let asking = AskTool {
            endpoint: "127.0.0.1:9".to_owned(),
            token: "t".to_owned(),
            approve: false,
            tools: Vec::new(),
            roles: Vec::new(),
        };
        assert!(offered(&asking).is_empty());
        let called = call(&asking, start_workflow::NAME, &Value::Null).await;
        assert!(called.is_none());
    }
}
