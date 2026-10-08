//! `argonauts_setup`: the main session sets Argonauts up from what you say. It fills this form,
//! Argonauts checks it the way it checks a settings file, and you see it as one card. Nothing
//! is written until you pick where it applies. Only the main session is offered the tool, so
//! no agent of a crew changes your setup by itself.
//!
//! The settings half of the form is made from the catalog `/config` is drawn from, so the
//! form names every setting and the values each takes, and is never out of date.

use argonauts_core::prompt_parts::{CrewRole, finer_roles};
use serde_json::{Value, json};

pub const NAME: &str = "argonauts_setup";

pub const DESCRIPTION: &str = "Proposes a change to the user's Argonauts setup: the crew and \
    each role's model, the workflow and steps every workflow must contain, a role's \
    instructions, any prompt, goals, limits, fallbacks and mods. Call it only when the user asks for a change, never when they only \
    talk about their setup. Argonauts checks the form: a wrong value comes back as an error \
    for you to fix. A valid one is shown to the user as one card, and nothing is written \
    until they pick where it applies, so say in a line what you would set and that nothing \
    has changed yet. The answer says what they picked. For what a setting does, or for the \
    fields of a workflow, role or mod file, open the guide first.";

/// It writes files once you pick, so a CLI asks before it calls it where it asks at all.
pub const READ_ONLY: bool = false;

/// Where the user said to keep the change, in the same message.
pub const KEEP: [&str; 5] = [
    "ask",
    "request",
    "conversation",
    "repository",
    "all_repositories",
];

/// The JSON object schema the session fills.
pub fn input_schema() -> Value {
    let mut settings = argonauts_registry::settings_form();
    settings["description"] = json!(
        "Settings to change, by dotted key, each with its new value. Only what the user asked \
         to change."
    );
    json!({
        "type": "object",
        "properties": {
            "asked": {
                "type": "string",
                "description": "What the user asked for, in their own words."
            },
            "settings": settings,
            "roles": {
                "type": "array",
                "description": "Instructions for a role, in the role's own file. `add` puts \
                                them after what the role reads anyway, `replace` takes its \
                                place.",
                "items": {
                    "type": "object",
                    "properties": {
                        "role": {"type": "string", "enum": role_names()},
                        "text": {"type": "string"},
                        "mode": {"type": "string", "enum": ["add", "replace"]}
                    },
                    "required": ["role", "text"],
                    "additionalProperties": false
                }
            },
            "prompts": {
                "type": "array",
                "description": "A built-in prompt replaced by the user's own text: its id as \
                                /prompts lists it, such as review/review or frames/reviewers, \
                                and the whole new text.",
                "items": {
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string"},
                        "text": {"type": "string"}
                    },
                    "required": ["prompt", "text"],
                    "additionalProperties": false
                }
            },
            "mods": {
                "type": "array",
                "description": "A mod's file, mods/<name>.toml, as its whole TOML text. Open \
                                the mods guide for its fields.",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "toml": {"type": "string"}
                    },
                    "required": ["name", "toml"],
                    "additionalProperties": false
                }
            },
            "steps": {
                "type": "array",
                "description": "Steps every workflow must contain, whoever writes it, kept in \
                                workflows/always.toml. Only when the user asks for one in \
                                every workflow; a task list for one request needs no setup.",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "role": {"type": "string", "enum": CrewRole::ALL.map(CrewRole::as_str)},
                        "does": {"type": "string"},
                        "when": {
                            "type": "string",
                            "description": "first, last, before <step> or after <step>, such \
                                            as \"before sign-off\". Left out, it goes before \
                                            the sign-off."
                        }
                    },
                    "required": ["name", "role"],
                    "additionalProperties": false
                }
            },
            "keep": {
                "type": "string",
                "enum": KEEP,
                "description": "Where to keep it, only when the user said so in the same \
                                message: \"for this one\" is request, \"from now on\" is \
                                conversation unless they name this repository or all their \
                                repositories, \"save that\" is repository. Otherwise ask, \
                                the default: the card asks them."
            },
            "keep_says": {
                "type": "string",
                "description": "The user's own words that said where, copied exactly."
            },
            "request_too": {
                "type": "boolean",
                "description": "The same message also asks for work, so the change can hold \
                                for that request alone."
            }
        },
        "required": ["asked"],
        "additionalProperties": false
    })
}

/// The names a role's file goes by: the crew's five roles, then each finer role.
fn role_names() -> Vec<&'static str> {
    CrewRole::ALL
        .into_iter()
        .map(CrewRole::as_str)
        .chain(finer_roles().map(|role| role.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_lists_the_settings_and_the_crew_roles() {
        let schema = input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["asked"]));
        let settings = &schema["properties"]["settings"];
        assert!(
            settings["properties"]["workflow"].is_object(),
            "the catalog's keys are on the form"
        );
        assert!(settings["description"].is_string());
        let roles = schema["properties"]["roles"]["items"]["properties"]["role"]["enum"]
            .as_array()
            .expect("role names");
        for crew in ["main", "orchestrator", "workers", "reviewers", "judge"] {
            assert!(roles.iter().any(|role| role == crew), "{crew}");
        }
        assert!(roles.iter().any(|role| role == "critic"), "a finer role");
        let step = &schema["properties"]["steps"]["items"]["properties"];
        assert_eq!(step["role"]["enum"].as_array().map(Vec::len), Some(5));
    }
}
