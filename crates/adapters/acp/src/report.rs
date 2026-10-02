//! The structured report. ACP has no enforced output schema, so the report comes back through
//! the `argonauts_report` tool, which writes `report.json`, or as a reply holding only the JSON
//! object. The conductor validates it again either way, since the agent could write the file
//! itself.

use std::path::Path;

use argonauts_core::driver::DriverStop;
use argonauts_core::registry::acp::ReportRoute;
use argonauts_core::{AgentEvent, ErrorCategory};
use serde_json::Value;

use crate::driver::Session;
use crate::errors::failed;

const REPORT_TOOL_PROMPT: &str = "Your turn ended without a report. Call the argonauts_report \
tool now with your final report. It is the only way your report reaches Argonauts.";

pub(crate) enum ReportStep {
    Done,
    /// One more prompt that asks for the report.
    Ask(String),
}

impl Session {
    /// After the last turn: emits `Result`, or asks once more, or fails with "no valid report".
    pub(crate) async fn report(&mut self, asked: bool) -> Result<ReportStep, DriverStop> {
        let text = self.mapper.last_message().map(str::to_owned);
        let Some(schema) = self.start.json_schema.clone() else {
            self.emit(AgentEvent::Result {
                text,
                structured: None,
                is_error: false,
            });
            return Ok(ReportStep::Done);
        };
        let report_file = self.start.report_file.clone().filter(|_| {
            self.profile.acp.report == ReportRoute::McpTool && self.profile.acp.mcp_servers
        });
        let found = match &report_file {
            Some(path) => read_report(path).await,
            None => text.as_deref().and_then(json_object),
        };
        if let Some(report) = found {
            self.emit(AgentEvent::Result {
                text,
                structured: Some(report),
                is_error: false,
            });
            return Ok(ReportStep::Done);
        }
        if !asked {
            let prompt = match report_file {
                Some(_) => REPORT_TOOL_PROMPT.to_owned(),
                None => json_prompt(&schema),
            };
            return Ok(ReportStep::Ask(prompt));
        }
        self.emit(AgentEvent::Result {
            text,
            structured: None,
            is_error: true,
        });
        Err(failed(ErrorCategory::Unknown, "no valid report"))
    }
}

fn json_prompt(schema: &Value) -> String {
    let schema = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
    format!(
        "Your turn ended without a report. Reply now with only your final report: one JSON \
         object that matches this schema, with no other text.\n\n{schema}"
    )
}

/// The report file's object. A missing or broken file counts as no report.
async fn read_report(path: &Path) -> Option<Value> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

/// The last JSON object in a reply: the whole reply, a fenced block, or an object inside text.
pub(crate) fn json_object(text: &str) -> Option<Value> {
    let whole = text.trim();
    if let Ok(value @ Value::Object(_)) = serde_json::from_str(whole) {
        return Some(value);
    }
    let fenced = whole
        .split("```")
        .skip(1)
        .step_by(2)
        .filter_map(|block| {
            let body = block.strip_prefix("json").unwrap_or(block);
            serde_json::from_str::<Value>(body.trim())
                .ok()
                .filter(Value::is_object)
        })
        .last();
    if fenced.is_some() {
        return fenced;
    }
    let mut last = None;
    let mut from = 0;
    while let Some(offset) = whole[from..].find('{') {
        let begin = from + offset;
        let mut stream = serde_json::Deserializer::from_str(&whole[begin..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value @ Value::Object(_))) => {
                last = Some(value);
                from = begin + stream.byte_offset();
            }
            _ => from = begin + 1,
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use argonauts_core::registry::acp::AcpProfile;
    use serde_json::json;

    use super::*;
    use crate::testing;

    fn schema() -> Value {
        json!({ "type": "object", "required": ["status"], "properties": { "status": { "type": "string" } } })
    }

    fn results(events: &[AgentEvent]) -> Vec<&AgentEvent> {
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Result { .. }))
            .collect()
    }

    #[tokio::test]
    async fn acp_report_from_tool_then_final_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let report_file = dir.path().join("report.json");
        let mut start = testing::start("/w");
        start.json_schema = Some(schema());
        start.report_file = Some(report_file.clone());

        // The report tool wrote the file during the turn.
        let mut driven = testing::launch(testing::profile(AcpProfile::default()), start.clone());
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        std::fs::write(&report_file, r#"{"status":"done","summary":"Fixed it"}"#).unwrap();
        driven.agent.say("All done.").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "end_turn" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::EndTurn);
        assert_eq!(
            results(&events),
            [&AgentEvent::Result {
                text: Some("All done.".into()),
                structured: Some(json!({ "status": "done", "summary": "Fixed it" })),
                is_error: false,
            }]
        );

        // No file after the turn: one more prompt asks for the tool call, then it's there.
        std::fs::remove_file(&report_file).unwrap();
        let mut driven = testing::launch(testing::profile(AcpProfile::default()), start.clone());
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "end_turn" }))
            .await;
        let ask = driven.agent.expect("session/prompt").await;
        assert!(
            ask["params"]["prompt"][0]["text"]
                .as_str()
                .unwrap()
                .contains("argonauts_report")
        );
        std::fs::write(&report_file, r#"{"status":"blocked"}"#).unwrap();
        driven
            .agent
            .reply(&ask, json!({ "stopReason": "end_turn" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::EndTurn);
        assert!(
            matches!(results(&events)[..], [AgentEvent::Result { structured: Some(report), .. }] if report["status"] == "blocked")
        );

        // Still nothing after the second ask: "no valid report".
        std::fs::write(&report_file, "not json").unwrap();
        let mut driven = testing::launch(testing::profile(AcpProfile::default()), start.clone());
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "end_turn" }))
            .await;
        let ask = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .reply(&ask, json!({ "stopReason": "end_turn" }))
            .await;
        let (end, _) = driven.finish().await;
        assert_eq!(end.stop, failed(ErrorCategory::Unknown, "no valid report"));

        // The final-prompt route reads the last message, and asks for JSON only when it has none.
        let final_prompt = AcpProfile {
            report: ReportRoute::FinalPrompt,
            ..AcpProfile::default()
        };
        let mut driven = testing::launch(testing::profile(final_prompt.clone()), start.clone());
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven.agent.say("I changed the parser.").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "end_turn" }))
            .await;
        let ask = driven.agent.expect("session/prompt").await;
        let asked = ask["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            asked.contains("only your final report") && asked.contains("\"required\""),
            "{asked}"
        );
        driven
            .agent
            .say("```json\n{\"status\": \"done\"}\n```")
            .await;
        driven
            .agent
            .reply(&ask, json!({ "stopReason": "end_turn" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::EndTurn);
        assert!(
            matches!(results(&events)[..], [AgentEvent::Result { structured: Some(report), .. }] if report["status"] == "done")
        );

        // An agent that takes no MCP servers gets the final prompt, whatever the route says.
        let no_mcp = AcpProfile {
            mcp_servers: false,
            ..AcpProfile::default()
        };
        let mut driven = testing::launch(testing::profile(no_mcp), start);
        driven.agent.open(json!({}), json!({})).await;
        let new_session = driven.agent.sent_before_prompt("session/new");
        assert_eq!(new_session["params"]["mcpServers"], json!([]));
        let prompt = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .say(r#"Done. {"status": "done", "notes": "{braces}"}"#)
            .await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "end_turn" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::EndTurn);
        assert!(
            matches!(results(&events)[..], [AgentEvent::Result { structured: Some(report), .. }] if report["notes"] == "{braces}")
        );
    }

    #[test]
    fn json_objects_are_found_in_replies() {
        assert_eq!(json_object(r#" {"a": 1} "#), Some(json!({ "a": 1 })));
        assert_eq!(
            json_object("First {\"a\": 1} then {\"b\": {\"c\": 2}} done"),
            Some(json!({ "b": { "c": 2 } }))
        );
        assert_eq!(
            json_object("```\n{\"a\": 1}\n```\ntext"),
            Some(json!({ "a": 1 }))
        );
        assert_eq!(json_object("no json { here"), None);
        assert_eq!(json_object("[1, 2]"), None);
    }
}
