//! `argonauts_lookup`: the main session answers questions about your own crew from
//! Argonauts Code's records, never from a guess. It reads and changes nothing else, and where there
//! is no record its answer says so.

use serde_json::{Value, json};

pub const NAME: &str = "argonauts_lookup";

pub const DESCRIPTION: &str = "Looks up Argonauts Code's own records for this repository: each \
    model's reviews with what it sent back and why, tasks, tokens, spend, fallbacks, or the \
    setup in use. Use it for any question about the user's crew, models or setup, and answer \
    from its figures, never from a guess. Where it finds no record, say so. It changes \
    nothing.";

pub const READ_ONLY: bool = true;

/// What can be looked up, each with what it gives.
pub const WHAT: [(&str, &str); 6] = [
    (
        "reviews",
        "each reviewer model's reviews, and what it sent back and why",
    ),
    (
        "tasks",
        "the tasks of the runs, how each ended and who built it",
    ),
    ("tokens", "tokens by model"),
    ("spend", "spend by model"),
    (
        "fallbacks",
        "each time a model stopped and fell back to another, and why",
    ),
    ("setup", "the setup in use and what the user set"),
];

/// How far back a lookup reads when the call doesn't say.
pub const DEFAULT_DAYS: u32 = 7;

pub fn input_schema() -> Value {
    let names: Vec<&str> = WHAT.iter().map(|(name, _)| *name).collect();
    let about: Vec<String> = WHAT
        .iter()
        .map(|(name, gives)| format!("{name}: {gives}"))
        .collect();
    json!({
        "type": "object",
        "properties": {
            "what": {
                "type": "string",
                "enum": names,
                "description": about.join(". ")
            },
            "days": {
                "type": "integer",
                "minimum": 1,
                "maximum": 365,
                "description": format!("How many days back, {DEFAULT_DAYS} when left out.")
            },
            "model": {
                "type": "string",
                "description": "Only this model or CLI, by any part of its name, such as \
                                astra or codex."
            },
            "everywhere": {
                "type": "boolean",
                "description": "For tokens and spend: every repository, not only this one."
            }
        },
        "required": ["what"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_names_everything_there_is_to_look_up() {
        let schema = input_schema();
        let listed = schema["properties"]["what"]["enum"]
            .as_array()
            .expect("what");
        assert_eq!(listed.len(), WHAT.len());
        assert_eq!(schema["required"], json!(["what"]));
    }
}
