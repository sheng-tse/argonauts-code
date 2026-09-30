//! The workflow contracts: workflows as data, their events, the workflow, agent, and context
//! views, the lead's design step, and `/workflow` and `/context`.

use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::engine::EngineCommand;
use argonauts_core::events::RunEvent;
use argonauts_core::events::session::SessionStarted;
use argonauts_core::input::{
    Handler, ParsedInput, SLASH_COMMANDS, SlashCommand, WorkflowAction, parse_input,
};
use argonauts_core::layout::{project_files, user_files};
use argonauts_core::names::{CheckpointId, WorkflowId};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::ui::{Glyphs, VendorInternals};
use argonauts_core::steps::design::{DesignInput, DesignWorkflow};
use argonauts_core::steps::{Step, StepName};
use argonauts_core::workflow::{
    Count, StandardPhase, StepKind, StepWorkers, Workflow, WorkflowRef,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// Parses `value` as `T`, writes it back, and checks nothing was lost or added.
fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug + PartialEq>(value: Value) -> T {
    let parsed: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("{err}\n{value:#}"));
    let written = serde_json::to_value(&parsed).unwrap();
    assert_eq!(written, value, "the JSON changed on the way through");
    let again: T = serde_json::from_value(written).unwrap();
    assert_eq!(again, parsed);
    parsed
}

fn schema_errors(schema: Value, value: &Value) -> Vec<String> {
    let validator = jsonschema::validator_for(&schema).unwrap();
    validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect()
}

fn envelope(kind: Value) -> Value {
    json!({
        "seq": 7,
        "ts": "2026-09-29T12:00:00Z",
        "run_id": "run-01",
        "source": "conductor",
        "kind": kind,
    })
}

fn spend() -> Value {
    json!({"input_tokens": 1200, "output_tokens": 300, "cached_tokens": 800, "cost_usd": 0.25,
           "cost_partial": false, "cost_estimated": true})
}

fn review_panel() -> Value {
    json!({
        "name": "review-panel",
        "description": "Two reviewers from other families; an adjudicator only if they disagree",
        "params": {"target": "diff", "reviewers": 2},
        "phases": [
            {"name": "Review", "standard": "check", "steps": [
                {"kind": "fan_out", "role": "reviewer", "count": "{{reviewers}}",
                 "blind": true, "different_family_from": "author"}]},
            {"name": "Adjudicate", "standard": "decide", "steps": [
                {"kind": "judge", "role": "adjudicator", "when": "reviewers_disagree"}]}
        ]
    })
}

/// Uses every step field at least once.
fn composed() -> Value {
    json!({
        "name": "draft-build-review",
        "description": "",
        "phases": [
            {"name": "Plan", "standard": "plan", "steps": [
                {"kind": "draft", "name": "drafts", "count": 3, "blind": true,
                 "task": "Draft a plan for {{target}}"},
                {"kind": "checkpoint", "checkpoint": "plan"}]},
            {"name": "Build", "standard": "do", "steps": [
                {"kind": "for_each", "input": "plan", "steps": [
                    {"kind": "loop_until", "until": "gates_pass", "max": 3, "steps": [
                        {"kind": "build", "role": "owner"}, {"kind": "gate"}]}]}]},
            {"name": "Check", "standard": "check", "steps": [
                {"kind": "workflow", "workflow": "review-panel", "params": {"reviewers": 3}},
                {"kind": "review", "workers": ["claude-code/opus", "codex"]},
                {"kind": "fan_out", "roles": ["reviewer", "critic"], "workers": "{{panel}}"},
                {"kind": "merge"}]}
        ]
    })
}

#[test]
fn the_review_panel_example_parses_validates_and_round_trips() {
    let schema = schemars::schema_for!(Workflow).to_value();
    for value in [review_panel(), composed()] {
        let errors = schema_errors(schema.clone(), &value);
        assert!(errors.is_empty(), "{errors:#?}");
        round_trip::<Workflow>(value);
    }
    let panel: Workflow = round_trip(review_panel());
    assert_eq!(panel.params.0["reviewers"], json!(2));
    let [review, adjudicate] = &panel.phases[..] else {
        panic!("expected two phases");
    };
    assert_eq!(review.standard, StandardPhase::Check);
    assert_eq!(review.steps[0].kind, StepKind::FanOut);
    assert_eq!(
        review.steps[0].count,
        Some(Count::Param("{{reviewers}}".into()))
    );
    assert!(review.steps[0].blind);
    assert_eq!(adjudicate.standard, StandardPhase::Decide);
    assert_eq!(
        adjudicate.steps[0].when.as_deref(),
        Some("reviewers_disagree")
    );
}

#[test]
fn a_fan_out_takes_several_roles_and_named_or_placeholder_workers() {
    let workflow: Workflow = round_trip(json!({"name": "second-look", "phases": [
        {"name": "Look", "standard": "check", "steps": [
            {"kind": "fan_out", "roles": ["reviewer", "critic"], "task": "Check {{target}}"},
            {"kind": "fan_out", "role": "reviewer", "workers": "{{reviewers}}"},
            {"kind": "review", "workers": ["claude-code/opus", "codex/gpt-x"]}]}],
        "description": ""}));
    let [by_role, by_param, by_name] = &workflow.phases[0].steps[..] else {
        panic!("expected three steps");
    };
    assert_eq!(by_role.roles, [RoleName::Reviewer, RoleName::Critic]);
    assert!(by_role.workers.is_empty() && by_role.role.is_none());
    assert_eq!(by_param.workers, StepWorkers::Param("{{reviewers}}".into()));
    assert!(by_param.roles.is_empty());
    let named: Vec<WorkerRef> = ["claude-code/opus", "codex/gpt-x"]
        .iter()
        .map(|text| text.parse().unwrap())
        .collect();
    assert_eq!(by_name.workers, StepWorkers::Named(named));
    assert_eq!(StepWorkers::default(), StepWorkers::Named(Vec::new()));
}

#[test]
fn a_misspelled_workflow_key_is_refused() {
    let misspelled = [
        json!({"name": "x", "phases": [], "paramz": {}}),
        json!({"name": "x", "phases": [{"name": "Do", "standard": "do", "step": []}]}),
        json!({"name": "x", "phases": [{"name": "Do", "standard": "do", "steps": [
            {"kind": "review", "blnd": true}]}]}),
        json!({"name": "x", "phases": [{"name": "Do", "standard": "do", "steps": [
            {"kind": "review", "different_family": "author"}]}]}),
    ];
    for value in misspelled {
        let err = serde_json::from_value::<Workflow>(value.clone()).unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "{err}\n{value:#}"
        );
    }
}

#[test]
fn a_bare_workflow_fills_in_its_description_and_params() {
    let bare: Workflow = serde_json::from_value(json!({"name": "solo", "phases": [
        {"name": "Do", "standard": "do", "steps": [{"kind": "build"}]}]}))
    .unwrap();
    assert_eq!(bare.description, "");
    assert!(bare.params.is_empty());
    assert!(!bare.phases[0].steps[0].blind);
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({"name": "solo", "description": "", "phases": [
            {"name": "Do", "standard": "do", "steps": [{"kind": "build"}]}]})
    );
}

#[test]
fn step_kinds_and_standard_phases_use_snake_case() {
    for name in [
        "restate",
        "differ",
        "draft",
        "map",
        "fan_out",
        "for_each",
        "review",
        "judge",
        "refute",
        "synthesize",
        "build",
        "gate",
        "loop_until",
        "checkpoint",
        "merge",
        "workflow",
    ] {
        round_trip::<StepKind>(json!(name));
    }
    for name in ["understand", "plan", "do", "check", "decide"] {
        round_trip::<StandardPhase>(json!(name));
    }
}

#[test]
fn counts_are_a_number_or_a_placeholder() {
    assert_eq!(round_trip::<Count>(json!(2)), Count::Fixed(2));
    assert_eq!(
        round_trip::<Count>(json!("{{reviewers}}")),
        Count::Param("{{reviewers}}".into())
    );
    assert!(serde_json::from_value::<Count>(json!(-1)).is_err());
    assert_eq!(
        round_trip::<WorkflowRef>(json!("peer-crew")).as_str(),
        "peer-crew"
    );
    assert_eq!(WorkflowId::numbered(1).as_str(), "wf-001");
}

#[test]
fn every_workflow_event_keeps_its_wire_format() {
    let workflow = |data: Value| envelope(json!({"type": "workflow", "data": data}));
    let step = json!({"workflow": "wf-001", "phase": 0, "path": [0]});
    let nested_step = json!({"workflow": "wf-002", "phase": 1, "path": [2, 0]});
    let mut events = vec![
        workflow(
            json!({"event": "started", "id": "wf-001", "from": "review-panel",
            "version": "ab12", "parent": null, "workflow": review_panel()}),
        ),
        workflow(json!({"event": "started", "id": "wf-002", "from": null,
            "version": "cd34", "parent": step.clone(), "workflow": composed()})),
        workflow(
            json!({"event": "step_started", "step": nested_step, "kind": "loop_until",
            "standard": "do", "round": 2}),
        ),
        workflow(
            json!({"event": "changed", "id": "wf-001", "version": "ef56",
            "workflow": composed(), "checkpoint": "cp-004",
            "reason": "add a security review before merge"}),
        ),
    ];
    for end in ["done", "failed", "skipped", "stopped"] {
        events.push(workflow(
            json!({"event": "step_finished", "step": step.clone(), "kind": "fan_out",
            "standard": "check", "round": 1, "end": end, "summary": "2 reviewers agreed"}),
        ));
    }
    for event in events {
        round_trip::<RunEvent>(event);
    }
}

fn session_started() -> Value {
    json!({"worker": "codex", "vendor": "codex", "family": "gpt", "kind": "harness",
        "role": "reviewer", "step": "review", "access": "read_only", "lifetime": "card",
        "resumed_from": null, "follow_key": 2})
}

#[test]
fn sessions_outside_a_workflow_keep_their_older_json() {
    let started: SessionStarted = round_trip(session_started());
    assert_eq!(started.workflow_step, None);
    let mut placed = session_started();
    placed["workflow_step"] = json!({"workflow": "wf-001", "phase": 0, "path": [0]});
    let started: SessionStarted = round_trip(placed);
    assert_eq!(started.workflow_step.unwrap().path, [0]);
}

fn agent_row() -> Value {
    json!({"session": "s-003", "label": "reviewer · card-002", "worker": "codex/gpt-x",
        "status": "running", "tokens": 38000, "spent": spend(), "elapsed_ms": 372000,
        "subagents": 3, "context": {"used": 120000, "cap": 200000},
        "standing_in_for": "claude-code/opus"})
}

#[test]
fn the_workflow_agent_and_context_views_keep_their_wire_format() {
    let view = |data: Value| json!({"type": "view", "data": data});
    let workflow = view(json!({"data": "workflow", "view": {
        "run": "run-01", "id": "wf-001", "name": "review-panel",
        "description": "Two reviewers", "breadcrumb": [], "agents_done": 1,
        "agents_total": 3, "elapsed_ms": 72000, "spent": spend(),
        "phases": [{"name": "Review", "standard": "check", "status": "running", "done": 1,
            "total": 3, "rows": [
                {"row": "agent", "session": "s-003", "label": "reviewer · card-002",
                 "worker": "codex/gpt-x", "status": "done", "tokens": 38000,
                 "spent": spend(), "elapsed_ms": 372000, "subagents": 0, "context": null,
                 "standing_in_for": null},
                {"row": "workflow", "id": "wf-002", "name": "security-review",
                 "status": "waiting", "agents_done": 0, "agents_total": 2,
                 "workers": ["antigravity-cli", "claude-code/opus"], "elapsed_ms": 0}]},
            {"name": "Adjudicate", "standard": "decide", "status": "needs_you", "done": 0,
             "total": 1, "rows": []}]}}));
    let agent = view(json!({"data": "agent", "view": {
        "run": "run-01", "row": agent_row(), "prompt": "Review card-002's diff.",
        "activity": [{"tool": "Bash", "summary": "npm test", "ok": true},
                     {"tool": "Read", "summary": "src/limits/store.ts", "ok": false},
                     {"tool": "Edit", "summary": "src/limits/store.ts", "ok": null}],
        "tool_calls": 195, "diff": {"files_changed": 2, "insertions": 18, "deletions": 4},
        "subagents": {"title": "Threads", "note": "steps not streamed", "children": [
            {"id": "th-1", "parent": null, "kind": "thread", "label": "explorer",
             "prompt": null, "status": "running", "last_message": null, "log_uri": null,
             "steps_visible": false}]},
        "outcome": null}}));
    let context = view(json!({"data": "context", "view": {"rows": [
        {"run": null, "session": "s-001", "label": "lead", "worker": "claude-code/opus",
         "used": 42000, "window": 200000, "cap": 180000, "tool_calls": 12, "spent": spend(),
         "rotations": 0},
        {"run": "run-01", "session": "s-003", "label": "owner · card-002",
         "worker": "openrouter/z-ai/glm-5", "used": 91000, "window": null, "cap": null,
         "tool_calls": 40, "spent": spend(), "rotations": 1}]}}));
    for event in [workflow, agent, context] {
        let AppEvent::View { data } = round_trip::<AppEvent>(event) else {
            panic!("not a view");
        };
        assert!(matches!(
            *data,
            ViewData::Workflow { .. } | ViewData::Agent { .. } | ViewData::Context { .. }
        ));
    }
}

#[test]
fn view_requests_for_workflows_agents_and_context_keep_their_wire_format() {
    let request = |view: Value| json!({"type": "request", "view": view});
    for view in [
        json!({"view": "workflow", "run": "run-01", "workflow": null}),
        json!({"view": "workflow", "run": "run-01", "workflow": "wf-002"}),
        json!({"view": "agent", "run": "run-01", "session": "s-003"}),
        json!({"view": "context"}),
    ] {
        round_trip::<ViewRequest>(view.clone());
        round_trip::<UiCommand>(request(view));
    }
}

fn parsed(text: &str) -> Value {
    serde_json::to_value(parse_input(text).unwrap_or_else(|err| panic!("{text}: {err:?}"))).unwrap()
}

#[test]
fn context_and_workflow_commands_parse_to_their_wire_format() {
    let context = json!({"input": "slash", "command": {"command": "context"}});
    assert_eq!(parsed("/context"), context);
    round_trip::<ParsedInput>(context);

    let show = json!({"input": "slash",
        "command": {"command": "workflow", "name": "review-panel", "action": "show"}});
    assert_eq!(parsed("/workflow show review-panel"), show);
    round_trip::<ParsedInput>(show);
    assert_eq!(
        parsed("/workflow"),
        json!({"input": "slash", "command": {"command": "workflow", "name": null,
               "action": "use"}})
    );
    for action in ["use", "show", "copy", "copy_to_project", "diff"] {
        round_trip::<WorkflowAction>(json!(action));
    }
    // A command written before `action` existed reads as picking the workflow.
    assert_eq!(
        serde_json::from_value::<SlashCommand>(
            json!({"command": "workflow", "name": "review-panel"})
        )
        .unwrap(),
        SlashCommand::Workflow {
            name: Some("review-panel".into()),
            action: WorkflowAction::Use
        }
    );

    for name in ["context", "workflow"] {
        let spec = SLASH_COMMANDS.iter().find(|spec| spec.name == name);
        assert_eq!(spec.map(|spec| spec.handler), Some(Handler::App), "{name}");
    }
}

/// A sample answer for a step: it must pass the strict schema the worker gets, and decode.
fn check_step_output<S: Step>(value: Value)
where
    S::Output: std::fmt::Debug + PartialEq,
{
    let schema = argonauts_core::steps::output_schema::<S>();
    let errors = schema_errors(schema, &value);
    assert!(errors.is_empty(), "{:?}: {errors:#?}", S::NAME);
    round_trip::<S::Output>(value);
}

#[test]
fn design_answers_pass_the_strict_schema_and_decode() {
    assert_eq!(DesignWorkflow::NAME, StepName::DesignWorkflow);
    round_trip::<StepName>(json!("design_workflow"));
    check_step_output::<DesignWorkflow>(json!({"template": "review-panel",
        "params": [{"name": "reviewers", "value": "3"}, {"name": "target", "value": "diff"}],
        "composed": null, "why": "A small diff; three reviewers are enough."}));
    check_step_output::<DesignWorkflow>(json!({"template": null, "params": [],
        "composed": "name: tight-fix\nphases:\n  - name: Fix\n    standard: do\n    steps:\n      - kind: build\n",
        "why": "No template fits a one-line fix."}));
    round_trip::<DesignInput>(json!({"request": "add per-key rate limiting",
        "goal": ["429 with Retry-After"],
        "examples": [{"name": "review-panel", "description": "Two reviewers",
                      "yaml": "name: review-panel\n", "runs": 12, "passed": 10}],
        "step_kinds": ["fan_out: the same task to several workers"],
        "allow_composed": true, "errors": []}));
    let adapting = round_trip::<DesignInput>(json!({"request": "add per-key rate limiting",
        "goal": [], "examples": [], "step_kinds": [], "allow_composed": true, "errors": [],
        "current": "name: review-panel\nphases:\n  - name: Review  # finished\n",
        "progress": ["Review: the two reviewers disagreed on the retry header"]}));
    assert_eq!(adapting.progress.len(), 1);
    assert!(adapting.current.is_some());
}

#[test]
fn the_new_workflow_defaults_hold() {
    let settings = Settings::default();
    assert_eq!(settings.workflow, WorkflowRef::new("peer-crew"));
    let workflows = &settings.workflows;
    assert!(workflows.lead_designs && workflows.allow_composed && workflows.adapt_at_phase_ends);
    assert_eq!(workflows.max_depth, 2);
    let ui = &settings.ui;
    assert_eq!(ui.panel.vendor_internals, VendorInternals::Discovered);
    assert!(!ui.panel.agents);
    assert_eq!(ui.panel.max_rows, 5);
    assert_eq!(ui.glyphs, Glyphs::Unicode);
    assert_eq!(ui.ambiguous_width, 1);
}

#[test]
fn settings_with_the_workflow_keys_set_pass_the_schema() {
    let settings: Settings = toml::from_str(
        r#"
workflow = "review-panel"
[workflows]
lead_designs = false
allow_composed = false
max_depth = 1
adapt_at_phase_ends = false
[ui]
glyphs = "ascii"
ambiguous_width = 2
[ui.panel]
vendor_internals = "hidden"
agents = true
max_rows = 8
"#,
    )
    .unwrap();
    assert_eq!(settings.workflow.as_str(), "review-panel");
    assert_eq!(settings.workflows.max_depth, 1);
    assert_eq!(settings.ui.panel.vendor_internals, VendorInternals::Hidden);
    assert_eq!(settings.ui.glyphs, Glyphs::Ascii);
    let value = serde_json::to_value(&settings).unwrap();
    let errors = schema_errors(schemars::schema_for!(Settings).to_value(), &value);
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(serde_json::from_value::<Settings>(value).unwrap(), settings);
    round_trip::<VendorInternals>(json!("summary"));
}

#[test]
fn workflow_folders_have_fixed_names() {
    assert_eq!(user_files::WORKFLOWS, "workflows");
    assert_eq!(project_files::WORKFLOWS, "workflows");
}

#[tokio::test]
async fn a_workflow_change_answers_with_its_checkpoint() {
    let workflow: Workflow = serde_json::from_value(review_panel()).unwrap();
    let (reply, answer) = oneshot::channel();
    let command = EngineCommand::ProposeWorkflowChange {
        target: Some(WorkflowId::numbered(2)),
        workflow: Box::new(workflow.clone()),
        reason: "add a security review before merge".into(),
        reply,
    };
    let EngineCommand::ProposeWorkflowChange {
        target,
        workflow: proposed,
        reply,
        ..
    } = command
    else {
        panic!("not a workflow change");
    };
    assert_eq!(target, Some(WorkflowId::numbered(2)));
    assert_eq!(*proposed, workflow);
    reply.send(Ok(CheckpointId::numbered(4))).unwrap();
    assert_eq!(answer.await.unwrap(), Ok(CheckpointId::numbered(4)));
}
