//! `argonauts_guide`: the main session opens one page of Argonauts Code's guide when a setup needs
//! more than the form says, so the detail isn't carried in every message. A page is the README
//! `/setup` writes for that folder, with the topic's settings as the catalog lists them.

use serde_json::{Value, json};

pub const NAME: &str = "argonauts_guide";

pub const DESCRIPTION: &str = "Opens one page of Argonauts Code's guide: how its files work and \
    every setting of one topic, with the values each takes. Open the page a setup needs before \
    you fill argonauts_setup, such as the fields of a workflow file or how a role's \
    instructions reach it. It changes nothing.";

pub const READ_ONLY: bool = true;

/// The pages there are, each with what it covers.
pub const TOPICS: [(&str, &str); 8] = [
    (
        "overview",
        "what each file in .argonauts/ is for, and which one wins",
    ),
    (
        "workflows",
        "workflow files, their steps, and the steps every workflow must contain",
    ),
    ("roles", "the crew's roles and a role's own instructions"),
    (
        "prompts",
        "what an agent reads, who writes it, and the prompt files",
    ),
    ("goals", "a condition the work keeps going until it holds"),
    ("limits", "spend and plan-usage limits"),
    ("fallbacks", "which model goes on when another can't"),
    ("mods", "mod files, their bodies and data"),
];

pub fn input_schema() -> Value {
    let names: Vec<&str> = TOPICS.iter().map(|(name, _)| *name).collect();
    let about: Vec<String> = TOPICS
        .iter()
        .map(|(name, covers)| format!("{name}: {covers}"))
        .collect();
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "enum": names,
                "description": about.join(". ")
            }
        },
        "required": ["topic"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_names_every_page() {
        let schema = input_schema();
        let listed = schema["properties"]["topic"]["enum"]
            .as_array()
            .expect("topics");
        assert_eq!(listed.len(), TOPICS.len());
        for (name, _) in TOPICS {
            assert!(listed.iter().any(|topic| topic == name), "{name}");
        }
    }
}
