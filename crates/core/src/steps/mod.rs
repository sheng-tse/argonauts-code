//! LLM steps: every judgment call is a schema-checked step whose output feeds a fixed rule.
//! Code, never the model, decides what happens next.

pub mod card;
pub mod design;
mod design_shorthand;
pub mod goal;
pub mod handoff;
pub mod intake;
pub mod judgment;
pub mod narrate;
pub mod plan;
pub mod pursue;
mod runner;

pub use runner::{SchemaStepRunner, last_json_object};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::events::session::{EndReason, SessionEnd, loaded_words};
use crate::future::BoxFuture;
use crate::worker::{SessionControl, SessionSpec, Worker, WorkerError, WorkerEvent};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepName {
    TriageEstimate,
    ChatAnswer,
    Restate,
    Differ,
    /// The lead answers the goal's open questions for you.
    AnswerQuestions,
    Draft,
    Map,
    Critique,
    Revise,
    ProposeSpike,
    SpikeReport,
    Judge,
    PlanToCards,
    Predict,
    Build,
    AcceptanceTests,
    Review,
    Adjudicate,
    ResolveConflict,
    Coordinate,
    Retro,
    /// The closing answer of a crew run.
    CloseRun,
    ModelApprover,
    Narrate,
    DesignWorkflow,
    HandoffSummary,
    /// The lead writes a goal's next attempt.
    PlanAttempt,
    /// A check from another company judges a goal's criteria.
    CheckGoal,
    /// One of the main agent CLI's own commands, such as `/init`.
    VendorCommand,
    /// A direct solo turn: your message to the main session, as its CLI takes it.
    Direct,
}

pub trait Step {
    const NAME: StepName;
    /// What the prompt template renders from.
    type Input: Serialize + JsonSchema;
    /// What the worker must return, checked against [`output_schema`].
    type Output: Serialize + DeserializeOwned + JsonSchema;

    /// The answer with the near-misses that can only mean one thing read as the schema says,
    /// before the schema checks it. Most steps take their answers as they come.
    fn normalize(output: Value) -> Value {
        output
    }
}

pub struct StepRequest {
    pub worker: Arc<dyn Worker>,
    /// Its prompt already holds the rendered input, and its `json_schema` is the output schema.
    pub spec: SessionSpec,
    /// Retries in the same session when the output doesn't match. Default 1.
    pub retries_on_mismatch: u8,
    /// Steering commands for the running session, passed on as they arrive.
    pub control: Option<mpsc::Receiver<SessionControl>>,
    /// Cancelling stops the session at once, and the step ends with the session's end.
    pub cancel: CancellationToken,
    /// The step's [`Step::normalize`], applied to each answer before the schema checks it.
    pub normalize: fn(Value) -> Value,
}

impl StepRequest {
    /// A request with no steering and its own cancel token.
    pub fn new(worker: Arc<dyn Worker>, spec: SessionSpec) -> Self {
        Self {
            worker,
            spec,
            retries_on_mismatch: 1,
            control: None,
            cancel: CancellationToken::new(),
            normalize: std::convert::identity,
        }
    }
}

/// Called with every event of the step's session. It runs on the runner's task and must not
/// block, so forward into an unbounded channel or streamed text is lost.
pub type StepObserver = Box<dyn FnMut(&WorkerEvent) + Send>;

#[derive(Debug, Clone, PartialEq)]
pub struct StepOutcome {
    /// The output, valid against the schema.
    pub value: Value,
    /// How the last session ended.
    pub end: SessionEnd,
    /// Sessions or turns used, 1 when the first answer matched.
    pub attempts: u8,
}

/// Why a step produced no usable output. Each step has a fixed fallback for these.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StepError {
    #[error("worker: {0}")]
    Worker(#[from] WorkerError),
    #[error("{}", ended_early(.0))]
    Session(Box<SessionEnd>),
    #[error("output didn't match the schema: {}", errors.join("; "))]
    SchemaMismatch {
        raw: Option<String>,
        errors: Vec<String>,
    },
}

/// Why a step's session gave no answer, in words. The session's record is for the raw view.
fn ended_early(end: &SessionEnd) -> String {
    let why = match &end.reason {
        EndReason::Completed => return "the session ended without an answer".to_owned(),
        EndReason::VendorError { message, .. }
        | EndReason::Auth { message }
        | EndReason::Billing { message }
        | EndReason::StartFailed { message }
        | EndReason::Crashed { message } => first_line(message),
        EndReason::AdapterBroken { detail } => first_line(detail),
        EndReason::IsolationBroken { plugins, skills } => {
            format!("it loaded {}", loaded_words(plugins, skills))
        }
        EndReason::ModelUnavailable { model } => format!("{model} isn't available"),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value.get("why").and_then(Value::as_str).map(str::to_owned))
            .map_or_else(|| "it stopped".to_owned(), |tag| tag.replace('_', " ")),
    };
    format!("the session ended early: {why}")
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no reason given")
        .to_owned()
}

pub trait StepRunner: Send + Sync {
    fn run(
        &self,
        request: StepRequest,
        observer: StepObserver,
    ) -> BoxFuture<'_, Result<StepOutcome, StepError>>;
}

/// The strict JSON Schema for `S::Output`, accepted by every vendor's structured output mode.
pub fn output_schema<S: Step>() -> Value {
    strict_schema(schemars::schema_for!(S::Output).to_value())
}

/// Rewrites a schema into the subset every vendor's strict mode accepts.
///
/// Every object lists all its properties as required and gets `additionalProperties: false`
/// (Codex strict mode needs both). A field with a serde default doesn't get `null` added,
/// because serde fills in defaults only for missing fields and would reject a `null`. `oneOf`
/// becomes `anyOf`, which means the same for the tagged enums schemars writes. A type that
/// refers to itself keeps its `$ref` from the second level down.
pub fn strict_schema(schema: Value) -> Value {
    let mut root = match schema {
        Value::Object(map) => map,
        other => return other,
    };
    let mut defs = Map::new();
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(found)) = root.remove(key) {
            defs.extend(found);
        }
    }
    let mut strict = Strict {
        defs,
        expanding: Vec::new(),
        recursive: Map::new(),
    };
    let mut out = strict.node(&Value::Object(root));
    if !strict.recursive.is_empty()
        && let Value::Object(map) = &mut out
    {
        map.insert("$defs".to_owned(), Value::Object(strict.recursive));
    }
    out
}

/// Keywords every vendor's structured output mode accepts, kept as they are.
const KEPT_KEYWORDS: &[&str] = &["type", "enum", "description"];

struct Strict {
    defs: Map<String, Value>,
    /// Definitions being inlined right now, to spot a type that contains itself.
    expanding: Vec<String>,
    /// Definitions of self-referring types, which stay as `$ref`.
    recursive: Map<String, Value>,
}

impl Strict {
    fn node(&mut self, node: &Value) -> Value {
        let Value::Object(map) = node else {
            // `true` means any value, which strict modes can't express; an empty schema is the
            // closest they accept. `false` never validates and never appears in step outputs.
            return match node {
                Value::Bool(true) => json!({}),
                other => other.clone(),
            };
        };
        if let Some(Value::String(reference)) = map.get("$ref") {
            return self.inline(reference, map);
        }
        let mut out = Map::new();
        for (key, value) in map {
            match key.as_str() {
                key if KEPT_KEYWORDS.contains(&key) => {
                    out.insert(key.to_owned(), value.clone());
                }
                "const" => {
                    out.insert("enum".to_owned(), json!([value]));
                }
                "properties" => {
                    let properties = value
                        .as_object()
                        .map(|props| {
                            props
                                .iter()
                                .map(|(name, prop)| (name.clone(), self.node(prop)))
                                .collect()
                        })
                        .unwrap_or_default();
                    out.insert("properties".to_owned(), Value::Object(properties));
                }
                "items" => {
                    out.insert("items".to_owned(), self.node(value));
                }
                "prefixItems" => {
                    // Tuples: strict modes know only one item schema, so allow any of them.
                    let items: Vec<Value> = value
                        .as_array()
                        .map(|items| items.iter().map(|item| self.node(item)).collect())
                        .unwrap_or_default();
                    out.insert("items".to_owned(), json!({ "anyOf": items }));
                }
                "anyOf" | "oneOf" => {
                    let options: Vec<Value> = value
                        .as_array()
                        .map(|options| options.iter().map(|o| self.node(o)).collect())
                        .unwrap_or_default();
                    out.insert("anyOf".to_owned(), Value::Array(options));
                }
                "allOf" => {
                    for part in value.as_array().into_iter().flatten() {
                        if let Value::Object(part) = self.node(part) {
                            merge_into(&mut out, part);
                        }
                    }
                }
                _ => {}
            }
        }
        if is_object_schema(&out) {
            let mut names: Vec<String> = out
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| props.keys().cloned().collect())
                .unwrap_or_default();
            // Sorted, so the schema comes out the same when a dependency turns on serde_json's
            // `preserve_order`.
            names.sort();
            out.entry("properties".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            let names = names.into_iter().map(Value::String).collect();
            out.insert("required".to_owned(), Value::Array(names));
            out.insert("additionalProperties".to_owned(), Value::Bool(false));
        }
        Value::Object(out)
    }

    /// Replaces a `$ref` with its definition, keeping sibling keywords such as `description`.
    fn inline(&mut self, reference: &str, map: &Map<String, Value>) -> Value {
        let name = reference.rsplit('/').next().unwrap_or(reference).to_owned();
        let Some(target) = self.defs.get(&name).cloned() else {
            // A reference to nothing we know can't be inlined; drop it rather than send it.
            return json!({});
        };
        if self.expanding.contains(&name) {
            if !self.recursive.contains_key(&name) {
                self.recursive.insert(name.clone(), Value::Null);
                self.expanding.push(name.clone());
                let expanded = self.node(&target);
                self.expanding.pop();
                self.recursive.insert(name.clone(), expanded);
            }
            return json!({ "$ref": format!("#/$defs/{name}") });
        }
        let mut merged = target.as_object().cloned().unwrap_or_default();
        for (key, value) in map {
            if key != "$ref" {
                merged.insert(key.clone(), value.clone());
            }
        }
        self.expanding.push(name);
        let out = self.node(&Value::Object(merged));
        self.expanding.pop();
        out
    }
}

fn is_object_schema(map: &Map<String, Value>) -> bool {
    let typed_object = match map.get("type") {
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => kinds.iter().any(|k| k == "object"),
        _ => false,
    };
    typed_object || map.contains_key("properties")
}

/// Merges an `allOf` part. Properties add up; other keys keep the first value seen.
fn merge_into(out: &mut Map<String, Value>, part: Map<String, Value>) {
    for (key, value) in part {
        match (out.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(more)) if key == "properties" => {
                existing.extend(more);
            }
            (None, value) => {
                out.insert(key, value);
            }
            _ => {}
        }
    }
}

pub fn decode<S: Step>(outcome: &StepOutcome) -> Result<S::Output, StepError> {
    serde_json::from_value(outcome.value.clone()).map_err(|err| StepError::SchemaMismatch {
        raw: Some(outcome.value.to_string()),
        errors: vec![err.to_string()],
    })
}

/// The line a person reads for a step's output: its `summary`, else the first line of a chat
/// answer. The JSON itself is for code and the Ctrl+O detail.
pub fn summary_line(output: &Value) -> Option<String> {
    ["summary", "answer_markdown"]
        .iter()
        .filter_map(|key| output.get(key)?.as_str())
        .find_map(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(str::to_owned)
}

/// Argonauts' own tools: the report, however the vendor sends it (`StructuredOutput`, agy's
/// `finish`, or the `argonauts_report` MCP tool under any server prefix), and
/// `argonauts_status`. They're how a session talks to Argonauts rather than work it does, so
/// the screen leaves them out of an agent's activity.
pub fn is_own_tool(name: &str) -> bool {
    let normal: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(normal.as_str(), "structuredoutput" | "finish")
        || normal.ends_with("argonautsreport")
        || normal.ends_with("argonautsstatus")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::{card, goal, intake, judgment, narrate, plan};

    /// A step's error is read in the main view's details, so it's words, never a record dump.
    #[test]
    fn a_step_error_says_why_in_words() {
        let ended = |reason: EndReason| {
            StepError::Session(Box::new(SessionEnd {
                vendor_session: None,
                exit_code: Some(0),
                signal: None,
                duration_ms: 745,
                reason,
                spend: crate::Spend::default(),
                tool_calls: 0,
                result_text: None,
                structured: None,
                last_message: None,
                raw_log: Some("/tmp/raw.jsonl".into()),
            }))
            .to_string()
        };
        let broken = ended(EndReason::AdapterBroken {
            detail: "Internal error: OpenCode service failure\nat line 2".into(),
        });
        assert_eq!(
            broken,
            "the session ended early: Internal error: OpenCode service failure"
        );
        assert_eq!(
            ended(EndReason::Completed),
            "the session ended without an answer"
        );
        assert_eq!(
            ended(EndReason::ContextFull),
            "the session ended early: context full"
        );
        let mismatch = StepError::SchemaMismatch {
            raw: None,
            errors: vec![
                "Additional properties are not allowed".into(),
                "x is missing".into(),
            ],
        };
        assert_eq!(
            mismatch.to_string(),
            "output didn't match the schema: Additional properties are not allowed; x is missing"
        );
    }

    #[test]
    fn own_tools_are_the_report_and_the_status() {
        for name in [
            "StructuredOutput",
            "structured_output",
            "finish",
            "mcp__argonauts__argonauts_report",
            "argonauts_argonauts_report",
            "mcp__argonauts__argonauts_status",
        ] {
            assert!(is_own_tool(name), "{name}");
        }
        for name in ["Bash", "finish_line", "report", "status", "Read"] {
            assert!(!is_own_tool(name), "{name}");
        }
    }

    #[test]
    fn an_outputs_summary_line_is_its_summary_else_its_answer() {
        let report = json!({"status": "done", "summary": "\nCreated hello.txt\nand more"});
        assert_eq!(summary_line(&report).as_deref(), Some("Created hello.txt"));
        let answer = json!({"status": "done", "answer_markdown": "It's **two** files.\n\n- a"});
        assert_eq!(
            summary_line(&answer).as_deref(),
            Some("It's **two** files.")
        );
        assert_eq!(
            summary_line(&json!({"status": "done", "summary": " "})),
            None
        );
        assert_eq!(summary_line(&json!(["summary"])), None);
    }

    /// Keywords Codex's strict mode (and so every vendor) rejects or ignores.
    const FORBIDDEN: &[&str] = &[
        "$schema",
        "$ref",
        "$defs",
        "definitions",
        "oneOf",
        "allOf",
        "not",
        "const",
        "format",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
        "minItems",
        "maxItems",
        "uniqueItems",
        "title",
        "default",
        "examples",
        "patternProperties",
        "propertyNames",
        "minProperties",
        "maxProperties",
    ];

    fn strict_problems(schema: &Value, path: &str, problems: &mut Vec<String>) {
        let Value::Object(map) = schema else {
            problems.push(format!("{path}: not an object schema"));
            return;
        };
        for key in map.keys() {
            if FORBIDDEN.contains(&key.as_str()) {
                problems.push(format!("{path}: keyword {key}"));
            }
        }
        if is_object_schema(map) {
            if map.get("additionalProperties") != Some(&Value::Bool(false)) {
                problems.push(format!("{path}: additionalProperties isn't false"));
            }
            let properties = map.get("properties").and_then(Value::as_object);
            let names: Vec<&String> = properties.map(|p| p.keys().collect()).unwrap_or_default();
            let required: Vec<&str> = map
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if names.len() != required.len()
                || names.iter().any(|n| !required.contains(&n.as_str()))
            {
                problems.push(format!("{path}: not every property is required"));
            }
            for (name, prop) in properties.into_iter().flatten() {
                strict_problems(prop, &format!("{path}.{name}"), problems);
            }
        }
        if let Some(items) = map.get("items") {
            strict_problems(items, &format!("{path}[]"), problems);
        }
        if let Some(Value::Array(options)) = map.get("anyOf") {
            for (i, option) in options.iter().enumerate() {
                strict_problems(option, &format!("{path}|{i}"), problems);
            }
        }
    }

    fn assert_strict<S: Step>() {
        let schema = output_schema::<S>();
        assert_eq!(
            schema["type"],
            "object",
            "{:?}: the root must be an object",
            S::NAME
        );
        let mut problems = Vec::new();
        strict_problems(&schema, "$", &mut problems);
        assert!(
            problems.is_empty(),
            "{:?}: {problems:#?}\n{schema:#}",
            S::NAME
        );
        assert!(jsonschema::validator_for(&schema).is_ok(), "{:?}", S::NAME);
    }

    #[test]
    fn every_step_output_schema_meets_codex_strict_rules() {
        assert_strict::<intake::TriageEstimate>();
        assert_strict::<intake::ChatAnswer>();
        assert_strict::<goal::Restate>();
        assert_strict::<goal::Differ>();
        assert_strict::<plan::Draft>();
        assert_strict::<plan::Map>();
        assert_strict::<plan::Critique>();
        assert_strict::<plan::Revise>();
        assert_strict::<plan::ProposeSpike>();
        assert_strict::<plan::SpikeReportStep>();
        assert_strict::<plan::Judge>();
        assert_strict::<plan::PlanToCards>();
        assert_strict::<plan::Coordinate>();
        assert_strict::<card::Predict>();
        assert_strict::<card::Build>();
        assert_strict::<card::ResolveConflict>();
        assert_strict::<card::AcceptanceTests>();
        assert_strict::<card::Retro>();
        assert_strict::<judgment::Review>();
        assert_strict::<judgment::Adjudicate>();
        assert_strict::<judgment::ModelApprover>();
        assert_strict::<narrate::Narrate>();
    }

    /// Finds maps (objects whose `additionalProperties` is a schema), which `strict_schema`
    /// would turn into objects that must stay empty.
    fn maps_in(node: &Value, path: &str, found: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                if map
                    .get("additionalProperties")
                    .is_some_and(Value::is_object)
                {
                    found.push(path.to_owned());
                }
                for (key, child) in map {
                    maps_in(child, &format!("{path}.{key}"), found);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    maps_in(child, &format!("{path}[{i}]"), found);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn no_step_output_holds_a_map() {
        fn raw<S: Step>() -> (StepName, Value) {
            (S::NAME, schemars::schema_for!(S::Output).to_value())
        }
        for (name, schema) in [
            raw::<intake::TriageEstimate>(),
            raw::<intake::ChatAnswer>(),
            raw::<goal::Restate>(),
            raw::<goal::Differ>(),
            raw::<plan::Draft>(),
            raw::<plan::Map>(),
            raw::<plan::Critique>(),
            raw::<plan::Revise>(),
            raw::<plan::ProposeSpike>(),
            raw::<plan::SpikeReportStep>(),
            raw::<plan::Judge>(),
            raw::<plan::PlanToCards>(),
            raw::<plan::Coordinate>(),
            raw::<card::Predict>(),
            raw::<card::Build>(),
            raw::<card::ResolveConflict>(),
            raw::<card::AcceptanceTests>(),
            raw::<card::Retro>(),
            raw::<judgment::Review>(),
            raw::<judgment::Adjudicate>(),
            raw::<judgment::ModelApprover>(),
            raw::<narrate::Narrate>(),
        ] {
            let mut found = Vec::new();
            maps_in(&schema, "$", &mut found);
            assert!(
                found.is_empty(),
                "{name:?} has a map, which strict mode can't express: {found:?}"
            );
        }
    }

    #[test]
    fn every_step_input_schema_generates() {
        fn input<S: Step>() -> Value {
            schemars::schema_for!(S::Input).to_value()
        }
        for schema in [
            input::<intake::TriageEstimate>(),
            input::<goal::Restate>(),
            input::<plan::Judge>(),
            input::<card::Build>(),
            input::<judgment::ModelApprover>(),
            input::<narrate::Narrate>(),
        ] {
            assert!(jsonschema::validator_for(&schema).is_ok());
        }
    }

    #[test]
    fn strict_schema_rewrites_the_usual_schemars_shapes() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Out",
            "type": "object",
            "properties": {
                "count": {"type": "integer", "format": "uint32", "minimum": 0},
                "note": {"type": ["string", "null"]},
                "kind": {"$ref": "#/$defs/Kind", "description": "what kind"},
                "child": {"anyOf": [{"$ref": "#/$defs/Child"}, {"type": "null"}]},
                "tags": {"type": "array", "items": {"type": "string"}, "default": []}
            },
            "required": ["count"],
            "$defs": {
                "Kind": {"oneOf": [
                    {"type": "string", "const": "a", "description": "A"},
                    {"type": "string", "const": "b"}
                ]},
                "Child": {"type": "object", "properties": {"x": {"type": "number", "format": "double"}},
                          "required": ["x"]}
            }
        });
        let strict = strict_schema(schema);
        assert_eq!(
            strict,
            json!({
                "type": "object",
                "properties": {
                    "count": {"type": "integer"},
                    "note": {"type": ["string", "null"]},
                    "kind": {"description": "what kind", "anyOf": [
                        {"type": "string", "enum": ["a"], "description": "A"},
                        {"type": "string", "enum": ["b"]}
                    ]},
                    "child": {"anyOf": [
                        {"type": "object", "properties": {"x": {"type": "number"}},
                         "required": ["x"], "additionalProperties": false},
                        {"type": "null"}
                    ]},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                // Sorted, whatever order serde_json keeps the properties in.
                "required": ["child", "count", "kind", "note", "tags"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn strict_schema_keeps_a_self_referring_type_as_one_definition() {
        let schema = json!({
            "$ref": "#/$defs/Node",
            "$defs": {"Node": {"type": "object", "properties": {
                "children": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
            }}}
        });
        let strict = strict_schema(schema);
        assert_eq!(
            strict["properties"]["children"]["items"],
            json!({"$ref": "#/$defs/Node"})
        );
        assert_eq!(
            strict["$defs"]["Node"]["additionalProperties"],
            json!(false)
        );
        assert!(jsonschema::validator_for(&strict).is_ok());
    }

    #[test]
    fn valid_outputs_still_validate_after_the_rewrite() {
        let schema = output_schema::<narrate::Narrate>();
        let output = narrate::NarrateOutput {
            lines: vec!["Card 2 passed its gates.".into()],
        };
        let value = serde_json::to_value(&output).unwrap();
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));

        let schema = output_schema::<intake::TriageEstimate>();
        let reply = json!({"kind": "unclear", "answer_markdown": "Could be either.",
            "estimate": null, "how_to_run": []});
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&reply));
        let wrong =
            json!({"kind": "unclear", "answer_markdown": "x", "estimate": null, "extra": 1});
        assert!(!jsonschema::validator_for(&schema).unwrap().is_valid(&wrong));
    }

    #[test]
    fn a_report_without_the_newer_lists_still_parses_and_the_schema_asks_for_them() {
        let old = json!({
            "status": "done",
            "summary": "built it",
            "files_changed": ["hello.txt"],
            "tests_run": [],
            "concerns": []
        });
        let report: card::CardReport = serde_json::from_value(old).unwrap();
        assert!(report.questions.is_empty() && report.memory_proposals.is_empty());
        let schema = output_schema::<card::Build>();
        let required = schema["required"].as_array().unwrap();
        for field in ["questions", "change_requests", "memory_proposals"] {
            assert!(required.iter().any(|name| name == field), "{field}");
        }
    }
}
