//! The models Claude Code lists in its `initialize` reply, with what `default` runs today, so
//! the picker can mirror the CLI and a model it adds shows up the same day.

use argonauts_core::{AgentEvent, ReportedModel};
use serde_json::Value;

use crate::json::{array_at, str_at, string_at, strings_at};

/// A `control_response` line. It names the models only when it answers `initialize`.
pub fn from_control_response(line: &Value) -> Option<AgentEvent> {
    let reply = line.get("response")?;
    if str_at(reply, "subtype") != Some("success") {
        return None;
    }
    let models: Vec<ReportedModel> = array_at(reply.get("response")?, "models")
        .iter()
        .filter_map(model)
        .collect();
    (!models.is_empty()).then_some(AgentEvent::ModelsReported { models })
}

fn model(entry: &Value) -> Option<ReportedModel> {
    let value = string_at(entry, "value")?;
    let efforts = if entry.get("supportsEffort").and_then(Value::as_bool) == Some(true) {
        strings_at(entry, "supportedEffortLevels")
    } else {
        Vec::new()
    };
    Some(ReportedModel {
        display_name: string_at(entry, "displayName").unwrap_or_else(|| value.clone()),
        resolved: string_at(entry, "resolvedModel"),
        description: string_at(entry, "description").unwrap_or_default(),
        efforts,
        value,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_initialize_reply_names_each_model_and_what_default_runs() {
        let line = json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": "init-1",
        "response": {"commands": [], "models": [
            {"value": "default", "resolvedModel": "claude-opus-5-5",
             "displayName": "Default (recommended)",
             "description": "Opus 5.5 · Best for everyday, complex tasks",
             "supportsEffort": true,
             "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
            {"value": "haiku", "resolvedModel": "claude-haiku-4-5-20251001",
             "displayName": "Haiku 4.5", "description": "Fastest for quick answers"},
            {"displayName": "no value"}
        ]}}});
        let Some(AgentEvent::ModelsReported { models }) = from_control_response(&line) else {
            panic!("no models");
        };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].value, "default");
        assert_eq!(models[0].resolved.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(models[0].efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(models[1].display_name, "Haiku 4.5");
        assert!(models[1].efforts.is_empty());

        let other = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": "x", "response": {"status": "delivering"}}});
        assert!(from_control_response(&other).is_none());
        let failed = json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": "x", "error": "no"}});
        assert!(from_control_response(&failed).is_none());
    }
}
