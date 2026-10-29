//! `argonauts_start_workflow`: the main session hands work to the crew. Argonauts Code starts the
//! workflow you chose, its agents do the work, and its closing answer lands in the
//! conversation. The app decides whether a call may start one: with Captain in use, only when your
//! own words asked for it.

use argonauts_core::steps::intake::InstructionKind;
use serde_json::{Value, json};

use super::main_tools::MainTool;

pub const NAME: &str = "argonauts_start_workflow";

const DESCRIPTION: &str = "Hands work to the user's crew. Argonauts Code starts the workflow the \
    user chose: other agents plan, build and review the work in their own worktrees while the user \
    keeps talking to you, and its closing answer lands in this conversation. Call it for real \
    work, or when the user asks for the crew, for other agents or for a workflow. Never call it \
    for a question, or while the user is only talking something over. When it says the workflow \
    started, tell the user in one line and stop: the workflow does the work.";

pub fn tool() -> MainTool {
    MainTool {
        name: NAME,
        description: DESCRIPTION,
        input_schema: schema,
        read_only: false,
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "request": {
                "type": "string",
                "description": "The work to hand over, complete enough for agents who haven't \
                                read this conversation: what to change, and what done looks \
                                like. Keep the user's own words where they gave them."
            },
            "how_to_run": {
                "type": "array",
                "description": "Each thing the user said about how to run it, with their own \
                                words in `says`, copied exactly: the whole phrase, two words \
                                at least. \"Use a workflow\" or \"use the crew\" is kind \
                                `mode` with value `crew`. \"With Codex as the reviewer\" is \
                                kind `review` with `who` \"codex\". \"Use only Claude\" is \
                                kind `crew` with `who` \"claude\".",
                "items": {
                    "type": "object",
                    "properties": {
                        "says": {"type": "string"},
                        "kind": instruction_kinds(),
                        "who": {
                            "type": "string",
                            "description": "The CLI or model the user named for it, as they \
                                            named it, such as \"codex\" or \"opus\". Never a \
                                            role such as \"reviewer\": `kind` says the role."
                        },
                        "model": {"type": "string"},
                        "effort": {"type": "string"},
                        "value": {
                            "type": "string",
                            "description": "For `mode`, `captain` or `crew`. For other kinds, \
                                            what was asked, such as \"$2\"."
                        },
                        "scope": {"type": "string"}
                    },
                    "required": ["says", "kind"]
                }
            },
            "checks": {
                "type": "array",
                "description": "The commands that show the change works, such as the project's \
                                tests, when the user has set no checks of their own. They hold \
                                for this run alone, on each task and on the merge, inside the \
                                run's sandbox, and nothing is saved. Name each one and give its \
                                argument list, which runs with no shell. Never write checks \
                                into a settings file yourself.",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "A short name, such as \"tests\" or \"lint\"."
                        },
                        "command": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "The program and its arguments, such as \
                                            [\"python3\", \"-m\", \"unittest\", \"-q\"]."
                        }
                    },
                    "required": ["name", "command"]
                }
            }
        },
        "required": ["request"]
    })
}

/// The kinds the checks know, each with what it means, as the estimate's schema lists them,
/// so words such as "all the other agents need to be Opus" are sorted as the checks read them.
fn instruction_kinds() -> Value {
    let schema = schemars::schema_for!(InstructionKind).to_value();
    let variants = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut names = Vec::new();
    let mut meanings = Vec::new();
    for variant in &variants {
        let name = variant
            .get("const")
            .or_else(|| variant.get("enum").and_then(|values| values.get(0)))
            .and_then(Value::as_str);
        let Some(name) = name else { continue };
        names.push(name.to_owned());
        if let Some(meaning) = variant.get("description").and_then(Value::as_str) {
            meanings.push(format!("{name}: {}", meaning.replace('\n', " ")));
        }
    }
    if names.is_empty() {
        return schema;
    }
    json!({"type": "string", "enum": names, "description": meanings.join(" ")})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start tool lists every kind the checks know, with what each means, so the main
    /// session can sort "all the other agents" and "the judge" as the checks read them.
    /// `who` takes the CLI or model named, never the role, so "Codex as the reviewer" reaches
    /// Codex.
    #[test]
    fn who_takes_the_agent_named_never_the_role() {
        let schema = schema();
        let item = &schema["properties"]["how_to_run"]["items"]["properties"];
        let who = item["who"]["description"].as_str().unwrap();
        assert!(who.contains("CLI or model the user named"), "{who}");
        assert!(who.contains("Never a role"), "{who}");
        let list = schema["properties"]["how_to_run"]["description"]
            .as_str()
            .unwrap();
        assert!(list.contains("\"Use a workflow\""), "{list}");
    }

    /// Checks come as a name and an argument list each, for this run alone, so the session
    /// never writes them into a settings file.
    #[test]
    fn checks_come_with_the_call_never_in_a_file() {
        let schema = schema();
        let checks = &schema["properties"]["checks"];
        assert_eq!(checks["items"]["required"], json!(["name", "command"]));
        let about = checks["description"].as_str().unwrap();
        assert!(about.contains("this run alone"), "{about}");
        assert!(about.contains("Never write checks"), "{about}");
    }

    #[test]
    fn the_start_tool_names_every_kind_of_instruction() {
        let kinds = instruction_kinds();
        let names: Vec<&str> = kinds["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for kind in ["mode", "review", "judge", "others", "for_the_main_agent"] {
            assert!(names.contains(&kind), "{kind} in {names:?}");
        }
        let meanings = kinds["description"].as_str().unwrap();
        assert!(
            meanings
                .contains("others: The model every agent but the one the user talks to runs on"),
            "{meanings}"
        );
    }
}
