//! Every answer format Argonauts Code sends a CLI, held to Codex's strict rules, the strictest any
//! CLI has. A format Codex refuses fails its session in two seconds, every time.

use argonauts_core::answer_format::{codex_problems, strict};
use argonauts_core::steps::StepName;
use argonauts_core::task::report_json_schema;
use argonauts_crew::claude_codes::verdict_schema;
use serde_json::{Value, json};

/// What a workflow script may ask for: a plan's tasks, a verdict, a pick, each with fields
/// it leaves optional.
fn script_formats() -> Vec<(String, Value)> {
    let tasks = json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string"},
            "tasks": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "done_when": {"type": "array", "items": {"type": "string"}},
                },
                "required": ["title"],
            }},
        },
        "required": ["tasks"],
    });
    let verdict = json!({
        "type": "object",
        "properties": {
            "accept": {"type": "boolean"},
            "findings": {"type": "array", "items": {"type": "string"}},
        },
        "required": ["accept"],
    });
    let pick = json!({
        "type": "object",
        "properties": {"keep": {"type": "integer"}, "why": {"type": "string"}},
        "required": ["keep"],
    });
    vec![
        ("script tasks".to_owned(), tasks),
        ("script verdict".to_owned(), verdict),
        ("script pick".to_owned(), pick),
    ]
}

fn every_format() -> Vec<(String, Value)> {
    // Every step, so a new one is held to the rules without a line here.
    let mut formats: Vec<(String, Value)> = StepName::ALL
        .into_iter()
        .filter_map(|name| Some((format!("{name:?}"), name.output_schema()?)))
        .collect();
    formats.push(("the task report".to_owned(), report_json_schema()));
    formats.push(("the reviewers' verdict".to_owned(), verdict_schema()));
    formats.extend(script_formats());
    formats
}

#[test]
fn every_step_but_the_plain_text_ones_has_a_format() {
    let names: Vec<String> = every_format().into_iter().map(|(name, _)| name).collect();
    for step in StepName::ALL {
        let plain_text = matches!(step, StepName::CliCommand | StepName::Direct);
        assert_eq!(
            names.contains(&format!("{step:?}")),
            !plain_text,
            "{step:?}"
        );
    }
}

#[test]
fn a_free_form_field_is_refused_until_it_has_a_type() {
    let free = json!({
        "type": "object",
        "properties": {"data": true, "note": {"description": "anything"}},
        "required": ["data", "note"],
    });
    let problems = codex_problems(&free);
    assert!(
        problems.contains(&"$.data: no type".to_owned()),
        "{problems:#?}"
    );
    assert!(
        problems.contains(&"$.note: no type".to_owned()),
        "{problems:#?}"
    );
    let sent = strict(&free);
    assert!(codex_problems(&sent).is_empty(), "{sent:#}");
}

#[test]
fn every_format_a_cli_gets_meets_codex_strict_rules() {
    for (name, format) in every_format() {
        let sent = strict(&format);
        let problems = codex_problems(&sent);
        assert!(problems.is_empty(), "{name}: {problems:#?}\n{sent:#}");
    }
}

/// The formats Argonauts Code writes itself take only what every CLI accepts, as Claude Code
/// gets them: what Codex would refuse is only a field they leave optional, which its strict
/// shape asks for.
#[test]
fn the_formats_argonauts_writes_itself_leave_codex_only_the_optional_fields() {
    for (name, format) in every_format()
        .into_iter()
        .filter(|(name, _)| !name.starts_with("script"))
    {
        let problems: Vec<String> = codex_problems(&format)
            .into_iter()
            .filter(|problem| !problem.contains("required is missing"))
            .collect();
        assert!(problems.is_empty(), "{name}: {problems:#?}");
    }
}
