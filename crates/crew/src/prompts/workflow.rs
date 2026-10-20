//! The main agent's workflow design, the one prompt for both a new task and a change at a phase
//! boundary.

pub const DESIGN_WORKFLOW: &str = argonauts_core::template!("prompts/workflow/design.md");
/// The same main agent session, told what code refused in its brief.
pub const FIX_DESIGN: &str = argonauts_core::template!("prompts/workflow/fix-design.md");

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
            as_you_asked: vec![String::new()],
        })
        .unwrap();
        for field in ["current", "progress", "errors", "style", "as_you_asked"] {
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
            first_try: None,
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
            "first_try",
        ] {
            assert!(output.get(field).is_some(), "{field} left DesignOutput");
            assert!(
                DESIGN_WORKFLOW.contains(&format!("`{field}`")),
                "{field} missing from the prompt"
            );
        }
        assert!(DESIGN_WORKFLOW.contains("read-only"));
        // What a resumed main agent is told names every field it may have to fix.
        for field in [
            "errors",
            "name",
            "description",
            "based_on",
            "workflow",
            "done_when",
        ] {
            assert!(
                FIX_DESIGN.contains(&format!("`{field}`")),
                "{field} missing from the fix prompt"
            );
        }
    }

    /// The schema the step enforces closes every object and asks for what runs, `based_on` or
    /// `workflow`, either of them null; the strict shape Codex gets asks for every field.
    #[test]
    fn the_design_schema_asks_for_what_runs() {
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
            ["name", "description", "based_on", "why", "workflow"],
            "{schema:#}"
        );
        let strict = argonauts_core::answer_format::strict(&schema);
        assert_eq!(
            required(&strict),
            [
                "name",
                "description",
                "based_on",
                "why",
                "params",
                "workflow",
                "done_when",
                "plan",
                "questions",
                "first_try"
            ],
            "{strict:#}"
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
