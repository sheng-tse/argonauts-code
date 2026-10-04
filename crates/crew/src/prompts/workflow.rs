//! The lead's workflow design, the one prompt for both a new task and a change at a phase
//! boundary.

pub const DESIGN_WORKFLOW: &str = include_str!("workflow/design_workflow.md");

#[cfg(test)]
mod tests {
    use argonauts_core::steps::design::{DesignInput, DesignOutput, DesignWorkflow};
    use argonauts_core::steps::output_schema;

    use super::*;

    /// The prompt names the input and output fields it explains, so a renamed field shows here
    /// before an agent reads instructions about a field that's gone.
    #[test]
    fn the_prompt_names_the_fields_it_explains() {
        let input = serde_json::to_value(DesignInput {
            request: String::new(),
            goal: Vec::new(),
            examples: Vec::new(),
            step_kinds: Vec::new(),
            allow_composed: true,
            errors: Vec::new(),
            current: Some(String::new()),
            progress: vec![String::new()],
            style: Some(argonauts_core::settings::workflows::RunStyle::Goal),
        })
        .unwrap();
        for field in ["current", "progress", "errors", "style"] {
            assert!(input.get(field).is_some(), "{field} left DesignInput");
            assert!(
                DESIGN_WORKFLOW.contains(&format!("`{field}`")),
                "{field} missing from the prompt"
            );
        }
        let output = serde_json::to_value(DesignOutput {
            name: String::new(),
            description: String::new(),
            based_on: None,
            why: String::new(),
            params: Vec::new(),
            workflow: None,
            done_when: Vec::new(),
            plan: Vec::new(),
            questions: Vec::new(),
            first_attempt: None,
        })
        .unwrap();
        for field in [
            "name",
            "description",
            "based_on",
            "why",
            "workflow",
            "params",
            "done_when",
            "plan",
            "questions",
            "first_attempt",
        ] {
            assert!(output.get(field).is_some(), "{field} left DesignOutput");
            assert!(
                DESIGN_WORKFLOW.contains(&format!("`{field}`")),
                "{field} missing from the prompt"
            );
        }
        assert!(DESIGN_WORKFLOW.contains("read-only"));
    }

    /// The schema the step enforces meets the strict rules every vendor accepts: closed
    /// objects, every property required, and optional values written as nullable.
    #[test]
    fn the_design_schema_is_strict() {
        let schema = output_schema::<DesignWorkflow>();
        let param = &schema["properties"]["params"]["items"];
        assert_eq!(schema["additionalProperties"], false, "{schema:#}");
        assert_eq!(param["additionalProperties"], false, "{schema:#}");
        let required = |object: &serde_json::Value| -> Vec<String> {
            object["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        };
        assert_eq!(
            required(&schema),
            [
                "based_on",
                "description",
                "done_when",
                "first_attempt",
                "name",
                "params",
                "plan",
                "questions",
                "why",
                "workflow"
            ],
            "{schema:#}"
        );
        assert_eq!(required(param), ["name", "value"], "{schema:#}");
        for nullable in ["based_on", "workflow"] {
            assert_eq!(
                schema["properties"][nullable]["type"],
                serde_json::json!(["string", "null"]),
                "{schema:#}"
            );
        }
    }
}
