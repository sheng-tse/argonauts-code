//! LLM steps: every judgment call is a schema-checked step whose output feeds a fixed rule.
//! Code, never the model, decides what happens next.

pub mod asking;
pub mod design;
mod design_shorthand;
pub mod fallback;
pub mod instructions;
pub mod intake;
pub mod judgment;
pub mod narrate;
pub mod plan;
pub mod pursue;
mod runner;
pub mod talk;
pub mod task;
pub mod understand;

pub use runner::{SchemaStepRunner, last_json_object};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::agents::session_runner::{
    RunnerError, RunnerEvent, SessionControl, SessionRunner, SessionSpec,
};
use crate::base::future::BoxFuture;
use crate::runs::events::session::{EndReason, SessionEnd, loaded_words};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepName {
    Estimate,
    ConversationAnswer,
    Read,
    Compare,
    /// The main agent answers the goal's open questions for you.
    AnswerQuestions,
    Draft,
    Map,
    Critique,
    Revise,
    ProposeExperiment,
    ExperimentReport,
    Judge,
    PlanToTasks,
    Predict,
    Build,
    AcceptanceTests,
    Review,
    Settle,
    ResolveConflict,
    /// The merger writes the message a run's change lands with.
    CommitMessage,
    Orchestrate,
    RunReview,
    /// The closing answer of a crew run.
    CloseRun,
    ModelJudge,
    Narrate,
    DesignWorkflow,
    FallbackSummary,
    /// The judge picks among fallbacks that did the same task at once.
    PickTry,
    /// The main agent writes a goal's next try.
    PlanTry,
    /// A model from another provider judges a goal's criteria.
    CheckGoal,
    /// A command of the main agent's CLI, such as `/init`.
    CliCommand,
    /// A direct Captain turn: your message to the main session, as its CLI takes it.
    Direct,
    /// A model you named writes another agent's instructions.
    WriteInstructions,
    /// A seat's turn in a council's talk: its first answer, a message, a tapback, the notes,
    /// or its answer to you in private.
    Talk,
}

pub trait Step {
    const NAME: StepName;
    /// What the prompt template renders from.
    type Input: Serialize + JsonSchema;
    /// What the agent must return, checked against [`output_schema`].
    type Output: Serialize + DeserializeOwned + JsonSchema;

    /// The answer with the near-misses that can only mean one thing read as the schema says,
    /// before the schema checks it. Most steps take their answers as they come.
    fn normalize(output: Value) -> Value {
        output
    }
}

pub struct StepRequest {
    pub worker: Arc<dyn SessionRunner>,
    /// Its prompt already holds the rendered input, and its `json_schema` is the output schema.
    pub spec: SessionSpec,
    /// Retries in the same session when the output doesn't match. Default 1.
    pub retries_on_mismatch: u8,
    /// How many times a session that ends its turn with a question, instead of its answer, is
    /// told to decide and carry on (`approvals.ask_limit`). Once more after that it is told to
    /// take the option it recommended. `None` replies to no question, so it fails like any
    /// answer that doesn't match. Default `None`.
    pub ask_limit: Option<u8>,
    /// Steering commands for the running session, passed on as they arrive.
    pub control: Option<mpsc::Receiver<SessionControl>>,
    /// Cancelling stops the session at once, and the step ends with the session's end.
    pub cancel: CancellationToken,
    /// The step's [`Step::normalize`], applied to each answer before the schema checks it.
    pub normalize: fn(Value) -> Value,
}

impl StepRequest {
    /// A request with no steering and its own cancel token.
    pub fn new(worker: Arc<dyn SessionRunner>, spec: SessionSpec) -> Self {
        Self {
            worker,
            spec,
            retries_on_mismatch: 1,
            ask_limit: None,
            control: None,
            cancel: CancellationToken::new(),
            normalize: std::convert::identity,
        }
    }
}

/// Called with every event of the step's session. It runs on the runner's task and must not
/// block, so forward into an unbounded channel or streamed text is lost.
pub type StepObserver = Box<dyn FnMut(&RunnerEvent) + Send>;

#[derive(Debug, Clone, PartialEq)]
pub struct StepOutcome {
    /// The output, valid against the schema.
    pub value: Value,
    /// How the last session ended.
    pub end: SessionEnd,
    /// Sessions or turns used, 1 when the first answer matched.
    pub tries: u8,
}

/// Why a step produced no usable output. Each step has a fixed fallback for these.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StepError {
    #[error("session runner: {0}")]
    Runner(#[from] RunnerError),
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
        // A CLI's error reads as the one plain cause every other line gives for it.
        EndReason::CliError { .. } => {
            if let Some(line) = end.reason.plain_line("its CLI") {
                return line;
            }
            "it gave an error".to_owned()
        }
        EndReason::Auth { message }
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

/// The JSON Schema for `S::Output` as the type writes it, in the shape every agent's structured
/// output mode takes. A CLI whose strict mode wants every field required gets
/// [`crate::workflows::answer_format::strict`] of it.
pub fn output_schema<S: Step>() -> Value {
    written_schema(schemars::schema_for!(S::Output).to_value())
}

impl StepName {
    /// Every step, so a check can hold each one's answer format to the same rules.
    pub const ALL: [StepName; 34] = [
        Self::Estimate,
        Self::ConversationAnswer,
        Self::Read,
        Self::Compare,
        Self::AnswerQuestions,
        Self::Draft,
        Self::Map,
        Self::Critique,
        Self::Revise,
        Self::ProposeExperiment,
        Self::ExperimentReport,
        Self::Judge,
        Self::PlanToTasks,
        Self::Predict,
        Self::Build,
        Self::AcceptanceTests,
        Self::Review,
        Self::Settle,
        Self::ResolveConflict,
        Self::CommitMessage,
        Self::Orchestrate,
        Self::RunReview,
        Self::CloseRun,
        Self::ModelJudge,
        Self::Narrate,
        Self::DesignWorkflow,
        Self::FallbackSummary,
        Self::PickTry,
        Self::PlanTry,
        Self::CheckGoal,
        Self::CliCommand,
        Self::Direct,
        Self::WriteInstructions,
        Self::Talk,
    ];

    /// The answer format a CLI is asked for in this step, or `None` where the answer is plain
    /// text: a CLI's own command and a direct turn. No arm is a catch-all, so a new step can't
    /// be left out.
    pub fn output_schema(self) -> Option<Value> {
        let schema = match self {
            Self::Estimate => output_schema::<intake::Estimate>(),
            Self::ConversationAnswer => output_schema::<intake::ConversationAnswer>(),
            Self::Read => output_schema::<understand::Read>(),
            Self::Compare => output_schema::<understand::Compare>(),
            Self::AnswerQuestions => output_schema::<understand::AnswerQuestions>(),
            Self::Draft => output_schema::<plan::Draft>(),
            Self::Map => output_schema::<plan::Map>(),
            Self::Critique => output_schema::<plan::Critique>(),
            Self::Revise => output_schema::<plan::Revise>(),
            Self::ProposeExperiment => output_schema::<plan::ProposeExperiment>(),
            Self::ExperimentReport => output_schema::<plan::ExperimentReportStep>(),
            Self::Judge => output_schema::<plan::Judge>(),
            Self::PlanToTasks => output_schema::<plan::PlanToTasks>(),
            Self::Predict => output_schema::<task::Predict>(),
            Self::Build => output_schema::<task::Build>(),
            Self::AcceptanceTests => output_schema::<task::AcceptanceTests>(),
            Self::Review => output_schema::<judgment::Review>(),
            Self::Settle => output_schema::<judgment::Settle>(),
            Self::ResolveConflict => output_schema::<task::ResolveConflict>(),
            // Its prompt is the change itself, so it renders no input of its own.
            Self::CommitMessage => {
                written_schema(schemars::schema_for!(task::CommitMessage).to_value())
            }
            Self::Orchestrate => output_schema::<plan::Orchestrate>(),
            Self::RunReview => output_schema::<task::RunReview>(),
            Self::CloseRun => output_schema::<task::CloseRun>(),
            Self::ModelJudge => output_schema::<judgment::ModelJudge>(),
            Self::Narrate => output_schema::<narrate::Narrate>(),
            // The first brief and its fix answer in the same shape.
            Self::DesignWorkflow => output_schema::<design::DesignWorkflow>(),
            Self::FallbackSummary => output_schema::<fallback::FallbackSummary>(),
            Self::PickTry => output_schema::<fallback::PickTry>(),
            Self::PlanTry => output_schema::<pursue::PlanTry>(),
            Self::CheckGoal => output_schema::<pursue::CheckGoal>(),
            Self::WriteInstructions => output_schema::<instructions::WriteInstructions>(),
            Self::Talk => output_schema::<talk::Talk>(),
            Self::CliCommand | Self::Direct => return None,
        };
        Some(schema)
    }
}

/// Rewrites a schema into the subset every agent's strict mode accepts.
///
/// Every object lists all its properties as required and gets `additionalProperties: false`
/// (Codex strict mode needs both). A field with a serde default doesn't get `null` added,
/// because serde fills in defaults only for missing fields and would reject a `null`. `oneOf`
/// becomes `anyOf`, which means the same for the tagged enums schemars writes. A type that
/// refers to itself keeps its `$ref` from the second level down.
pub fn strict_schema(schema: Value) -> Value {
    rewrite(schema, Required::Every)
}

/// [`strict_schema`]'s subset with each object requiring only what the schema itself requires,
/// so a field with a default may be left out. Claude Code checks an answer against this shape.
pub fn written_schema(schema: Value) -> Value {
    rewrite(schema, Required::AsWritten)
}

/// Which properties an object lists as required.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Required {
    Every,
    AsWritten,
}

fn rewrite(schema: Value, required: Required) -> Value {
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
        required,
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

/// Keywords every agent's structured output mode accepts, kept as they are.
const KEPT_KEYWORDS: &[&str] = &["type", "enum", "description"];

struct Strict {
    required: Required,
    defs: Map<String, Value>,
    /// Definitions being inlined right now, to spot a type that contains itself.
    expanding: Vec<String>,
    /// Definitions of self-referring types, which stay as `$ref`.
    recursive: Map<String, Value>,
}

impl Strict {
    fn node(&mut self, node: &Value) -> Value {
        let Value::Object(map) = node else {
            // `true` means any value, which strict modes can't express. `false` never
            // validates and never appears in step outputs.
            return match node {
                Value::Bool(true) => any_value(None),
                other => other.clone(),
            };
        };
        if let Some(Value::String(reference)) = map.get("$ref") {
            return self.inline(reference, map);
        }
        let mut out = Map::new();
        let mut written = names_of(map.get("required"));
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
                            written.extend(names_of(part.get("required")));
                            merge_into(&mut out, part);
                        }
                    }
                }
                _ => {}
            }
        }
        if is_object_schema(&out) {
            // In the order the properties are declared, never sorted: a model writes its fields
            // in this order, and one that writes a report's long `answer` first runs it on into
            // the fields after it.
            let names = out
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| {
                    props
                        .keys()
                        .filter(|name| self.required == Required::Every || written.contains(name))
                        .cloned()
                        .map(Value::String)
                        .collect()
                })
                .unwrap_or_default();
            out.entry("properties".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            out.insert("required".to_owned(), Value::Array(names));
            out.insert("additionalProperties".to_owned(), Value::Bool(false));
        }
        if !["type", "enum", "anyOf", "$ref"]
            .iter()
            .any(|key| out.contains_key(*key))
        {
            return any_value(out.get("description").and_then(Value::as_str));
        }
        Value::Object(out)
    }

    /// Replaces a `$ref` with its definition, keeping sibling keywords such as `description`.
    fn inline(&mut self, reference: &str, map: &Map<String, Value>) -> Value {
        let name = reference.rsplit('/').next().unwrap_or(reference).to_owned();
        let Some(target) = self.defs.get(&name).cloned() else {
            // A reference to nothing we know can't be inlined, so it takes any value.
            return any_value(map.get("description").and_then(Value::as_str));
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

/// The names a `required` list holds.
fn names_of(required: Option<&Value>) -> Vec<String> {
    required
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A field that takes any value, as strict modes accept one: the value written as JSON text,
/// which `answer_format::as_written` reads back.
fn any_value(description: Option<&str>) -> Value {
    let description = match description {
        Some(description) => format!("{description} Written as JSON text."),
        None => "JSON text".to_owned(),
    };
    json!({"type": "string", "description": description})
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

/// The line a person reads for a step's output: its `summary`, else the first line of a
/// conversation answer. The JSON itself is for code and the Ctrl+O detail.
pub fn summary_line(output: &Value) -> Option<String> {
    ["summary", "answer_markdown"]
        .iter()
        .filter_map(|key| output.get(key)?.as_str())
        .find_map(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(str::to_owned)
}

/// Argonauts Code's own tools: the report, however the agent sends it (`StructuredOutput`, agy's
/// `finish`, or the `argonauts_report` MCP tool under any server prefix), and
/// `argonauts_status`. They're how a session talks to Argonauts Code rather than work it does, so
/// the screen leaves them out of an agent's activity.
pub fn is_own_tool(name: &str) -> bool {
    is_report_tool(name) || normal_tool_name(name).ends_with("argonautsstatus")
}

/// The tool a session sends its report with, under any of [`is_own_tool`]'s names.
pub fn is_report_tool(name: &str) -> bool {
    let normal = normal_tool_name(name);
    matches!(normal.as_str(), "structuredoutput" | "finish") || normal.ends_with("argonautsreport")
}

fn normal_tool_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::steps::{intake, judgment, narrate, plan, task, understand};

    /// A step's error is read in the main view's details, so it's words, never a record dump.
    #[test]
    fn a_step_error_says_why_in_words() {
        let ended = |reason: EndReason| {
            StepError::Session(Box::new(SessionEnd {
                cli_session: None,
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
        // A CLI's error gives the one plain cause every other line gives.
        assert_eq!(
            ended(EndReason::CliError {
                category: crate::ErrorCategory::InvalidRequest,
                message: "Invalid schema for response_format: 'summary' is not in required".into(),
            }),
            "its CLI refused the answer format Argonauts Code asked for"
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

    /// Keywords Codex's strict mode (and so every agent) rejects or ignores.
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

    /// The format Codex gets for `S`.
    fn assert_strict<S: Step>() {
        let schema = crate::workflows::answer_format::strict(&output_schema::<S>());
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
        assert_strict::<intake::Estimate>();
        assert_strict::<intake::ConversationAnswer>();
        assert_strict::<understand::Read>();
        assert_strict::<understand::Compare>();
        assert_strict::<plan::Draft>();
        assert_strict::<plan::Map>();
        assert_strict::<plan::Critique>();
        assert_strict::<plan::Revise>();
        assert_strict::<plan::ProposeExperiment>();
        assert_strict::<plan::ExperimentReportStep>();
        assert_strict::<plan::Judge>();
        assert_strict::<plan::PlanToTasks>();
        assert_strict::<plan::Orchestrate>();
        assert_strict::<task::Predict>();
        assert_strict::<task::Build>();
        assert_strict::<task::ResolveConflict>();
        assert_strict::<task::AcceptanceTests>();
        assert_strict::<task::RunReview>();
        assert_strict::<judgment::Review>();
        assert_strict::<judgment::Settle>();
        assert_strict::<judgment::ModelJudge>();
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
            raw::<intake::Estimate>(),
            raw::<intake::ConversationAnswer>(),
            raw::<understand::Read>(),
            raw::<understand::Compare>(),
            raw::<plan::Draft>(),
            raw::<plan::Map>(),
            raw::<plan::Critique>(),
            raw::<plan::Revise>(),
            raw::<plan::ProposeExperiment>(),
            raw::<plan::ExperimentReportStep>(),
            raw::<plan::Judge>(),
            raw::<plan::PlanToTasks>(),
            raw::<plan::Orchestrate>(),
            raw::<task::Predict>(),
            raw::<task::Build>(),
            raw::<task::ResolveConflict>(),
            raw::<task::AcceptanceTests>(),
            raw::<task::RunReview>(),
            raw::<judgment::Review>(),
            raw::<judgment::Settle>(),
            raw::<judgment::ModelJudge>(),
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
            input::<intake::Estimate>(),
            input::<understand::Read>(),
            input::<plan::Judge>(),
            input::<task::Build>(),
            input::<judgment::ModelJudge>(),
            input::<narrate::Narrate>(),
        ] {
            assert!(jsonschema::validator_for(&schema).is_ok());
        }
    }

    /// The names serde writes for every step, read from the enum's own schema.
    fn names_in(node: &Value, names: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    match (key.as_str(), value) {
                        ("const", Value::String(name)) => names.push(name.clone()),
                        ("enum", Value::Array(values)) => {
                            names.extend(values.iter().filter_map(Value::as_str).map(str::to_owned))
                        }
                        _ => names_in(value, names),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| names_in(item, names)),
            _ => {}
        }
    }

    #[test]
    fn every_step_is_listed_once() {
        let mut names = Vec::new();
        names_in(&schemars::schema_for!(StepName).to_value(), &mut names);
        names.sort();
        let mut listed: Vec<String> = StepName::ALL
            .iter()
            .map(|name| {
                serde_json::to_value(name)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        listed.sort();
        assert_eq!(listed, names);
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
        let strict = strict_schema(schema.clone());
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
                // In the order the properties are declared.
                "required": ["count", "note", "kind", "child", "tags"],
                "additionalProperties": false
            })
        );
        let written = written_schema(schema);
        assert_eq!(written["required"], json!(["count"]), "{written:#}");
        assert_eq!(
            written["properties"]["child"]["anyOf"][0]["required"],
            json!(["x"])
        );
    }

    /// The format as written requires only what the type does, in the order it declares it.
    #[test]
    fn the_written_schema_requires_what_the_type_requires() {
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "notes": {"type": "array", "items": {"type": "string"}, "default": []},
                "status": {"type": "string"},
                "part": {"allOf": [{"$ref": "#/$defs/Part"}]}
            },
            "required": ["status", "summary"],
            "$defs": {
                "Part": {"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
                         "required": ["b"]}
            }
        });
        let written = written_schema(schema);
        assert_eq!(written["required"], json!(["summary", "status"]));
        assert_eq!(written["additionalProperties"], json!(false));
        assert_eq!(written["properties"]["part"]["required"], json!(["b"]));
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
            lines: vec!["Task 2 passed its checks.".into()],
        };
        let value = serde_json::to_value(&output).unwrap();
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));

        let schema = output_schema::<intake::Estimate>();
        let reply = json!({"kind": "unclear", "answer_markdown": "Could be either.",
            "estimate": null, "how_to_run": []});
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&reply));
        let wrong =
            json!({"kind": "unclear", "answer_markdown": "x", "estimate": null, "extra": 1});
        assert!(!jsonschema::validator_for(&schema).unwrap().is_valid(&wrong));
    }

    /// Claude Code may leave out the lists with a default, and the long answer, and Codex is
    /// asked for each of them, `answer` last.
    #[test]
    fn a_report_without_the_newer_lists_still_parses_and_only_codex_is_asked_for_them() {
        let old = json!({
            "status": "done",
            "summary": "built it",
            "files_changed": ["hello.txt"],
            "tests_run": [],
            "concerns": []
        });
        let report: task::TaskReport = serde_json::from_value(old).unwrap();
        assert!(report.questions.is_empty() && report.memory_proposals.is_empty());
        let schema = output_schema::<task::Build>();
        assert_eq!(
            schema["required"],
            json!([
                "status",
                "summary",
                "files_changed",
                "tests_run",
                "concerns"
            ])
        );
        let strict = crate::workflows::answer_format::strict(&schema);
        assert_eq!(
            strict["required"],
            json!([
                "status",
                "summary",
                "files_changed",
                "tests_run",
                "concerns",
                "questions",
                "change_requests",
                "memory_proposals",
                "answer"
            ])
        );
    }
}
