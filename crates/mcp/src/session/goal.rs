//! `argonauts_set_goal`, the main session's tool for a goal: a condition the work keeps going
//! until it holds, which Argonauts checks itself. The session calls it when you say so in your
//! own words ("keep going until its tests pass"), and the app states the goal back, with what
//! checks it, as the call's answer.
//!
//! What a registry of the main session's tools needs is here by itself: the name, the
//! description, the input schema, the line its standing instructions say about it, and
//! [`parse`], which reads a call's arguments.

use argonauts_core::asking::GoalAsk;
use serde_json::{Value, json};

use super::main_tools::MainTool;

pub const SET_GOAL_TOOL: &str = "argonauts_set_goal";

pub const DESCRIPTION: &str = "Set, show, pause, resume or clear a goal: a condition the work \
    keeps going until it holds. Argonauts runs the check itself after each try and sends back \
    what still fails, so saying it's done is never enough. Call it only when the user asks to \
    keep going until something is true, such as \"keep going until its tests pass\", or asks \
    about the goal, never on your own judgment. The answer states the goal back in one line: \
    say that line to the user before you start.";

/// What the main session's standing lines say about the tool: its name, then what it does.
pub const ABOUT: &str = "`argonauts_set_goal` sets, shows, pauses, resumes or clears a goal: a \
    condition the work keeps going until it holds, which Argonauts checks itself. Use it only \
    when the user asks to keep going until something is true, or asks about the goal.";

/// A JSON object schema for the call's arguments.
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["set", "show", "pause", "resume", "clear"],
                "description": "What to do with the goal. `set` when left out."
            },
            "condition": {
                "type": "string",
                "description": "For `set`: what must hold, in the user's own words. Separate \
                                things that must each hold with semicolons. Put a command that \
                                checks one in backticks, such as \"`cargo test -p export` \
                                passes; the CSV has a header row\". A line with no command is \
                                read by a judge from another provider."
            },
            "from_now_on": {
                "type": "boolean",
                "description": "True when the user wants it for the work that follows in this \
                                conversation (\"from now on\", \"for everything\"). False, the \
                                default, covers this request alone."
            }
        }
    })
}

pub fn tool() -> MainTool {
    MainTool {
        name: SET_GOAL_TOOL,
        description: DESCRIPTION,
        input_schema,
        read_only: false,
    }
}

/// One call's arguments as they were meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCall {
    pub action: GoalAsk,
    /// What must hold. Empty unless a goal is being set.
    pub condition: String,
    /// For the work that follows in this conversation. False covers this request alone.
    pub from_now_on: bool,
}

/// Reads a call's arguments. An action nobody listed sets nothing: it shows the goal.
pub fn parse(arguments: &Value) -> GoalCall {
    let action = match arguments.get("action").and_then(Value::as_str) {
        None | Some("set") => GoalAsk::Set,
        Some("pause") => GoalAsk::Pause,
        Some("resume") => GoalAsk::Resume,
        Some("clear") => GoalAsk::Clear,
        Some(_) => GoalAsk::Show,
    };
    GoalCall {
        action,
        condition: arguments
            .get("condition")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        from_now_on: arguments
            .get("from_now_on")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_sets_a_goal_for_this_request_unless_it_says_more() {
        let asked = parse(&json!({"condition": " its tests pass "}));
        assert_eq!(
            asked,
            GoalCall {
                action: GoalAsk::Set,
                condition: "its tests pass".to_owned(),
                from_now_on: false,
            }
        );
        let kept = parse(&json!({"action": "set", "condition": "x passes", "from_now_on": true}));
        assert!(kept.from_now_on);
    }

    #[test]
    fn the_other_actions_need_no_condition_and_an_unknown_one_only_shows() {
        for (word, action) in [
            ("show", GoalAsk::Show),
            ("pause", GoalAsk::Pause),
            ("resume", GoalAsk::Resume),
            ("clear", GoalAsk::Clear),
            ("delete everything", GoalAsk::Show),
        ] {
            let asked = parse(&json!({"action": word}));
            assert_eq!(asked.action, action, "{word}: {asked:?}");
        }
    }

    #[test]
    fn the_tool_says_when_to_call_it_and_how_to_name_a_command() {
        let tool = tool();
        assert_eq!(tool.name, SET_GOAL_TOOL);
        assert!(ABOUT.starts_with(&format!("`{SET_GOAL_TOOL}`")), "{ABOUT}");
        assert!(tool.description.contains("never on your own judgment"));
        let schema = (tool.input_schema)().to_string();
        assert!(schema.contains("in backticks"), "{schema}");
        assert!(schema.contains("from_now_on"), "{schema}");
    }
}
