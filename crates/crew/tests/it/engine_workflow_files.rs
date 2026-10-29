//! Workflows as phases plus settings: the built-in workflow files, what each workflow changes
//! in the flow, every step kind in one composed run, and the council end to end.

use crate::support;
use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, FreeText,
};
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::run::{
    CaptainOrCrewDecision, Party, RunLifecycle, RunMode, RunOutcome,
};
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskSpec};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::policy::{PolicyOutcome, Verdict};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::protocol::{CaptainKind, CheckCommand, JudgingMethod};
use argonauts_core::steps::StepName;
use argonauts_core::workflow::{Count, StandardPhase, Step, StepKind, Workflow, WorkflowRef};
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::runner::captain_workflow;
use argonauts_crew::workflow::{StepAt, builtin, conditions};
use argonauts_testkit::runner::{FakeRunner, FakeSession};

fn load(name: &str) -> Workflow {
    builtin::load(name).unwrap_or_else(|err| panic!("{err}"))
}

fn all_steps(workflow: &Workflow) -> Vec<&Step> {
    fn walk<'a>(steps: &'a [Step], out: &mut Vec<&'a Step>) {
        for step in steps {
            out.push(step);
            walk(&step.steps, out);
        }
    }
    let mut out = Vec::new();
    for phase in &workflow.phases {
        walk(&phase.steps, &mut out);
    }
    out
}

fn kinds(workflow: &Workflow) -> BTreeSet<StepKind> {
    all_steps(workflow)
        .into_iter()
        .map(|step| step.kind)
        .collect()
}

#[test]
fn builtin_protocols_run_as_workflow_files() {
    let names: Vec<&str> = builtin::names().collect();
    assert_eq!(
        names,
        [
            "council",
            "orchestrator",
            "captain",
            "maker-checker",
            "best-of-n",
            "draft-and-vote"
        ]
    );
    for name in names {
        let workflow = load(name);
        assert_eq!(workflow.name, name);
        assert!(kinds(&workflow).contains(&StepKind::Build), "{name}");
        assert_eq!(
            workflow.phases.last().map(|phase| phase.standard),
            Some(StandardPhase::Decide)
        );
    }
    let standards = |workflow: &Workflow| -> Vec<StandardPhase> {
        workflow.phases.iter().map(|phase| phase.standard).collect()
    };
    let four = [
        StandardPhase::Understand,
        StandardPhase::Plan,
        StandardPhase::Do,
        StandardPhase::Decide,
    ];
    // The crew's fixed phases, which a workflow without its own runs, keep every planning step.
    let base = load("base");
    assert_eq!(standards(&base), four);
    for kind in [
        StepKind::Read,
        StepKind::Compare,
        StepKind::Approval,
        StepKind::LoopUntil,
        StepKind::Draft,
        StepKind::Map,
        StepKind::Critique,
        StepKind::Judge,
        StepKind::Synthesize,
        StepKind::Build,
        StepKind::Merge,
    ] {
        assert!(kinds(&base).contains(&kind), "{kind:?}");
    }
    // Council plans by its talk, with no drafts to judge.
    let council = load("council");
    assert_eq!(standards(&council), four);
    assert!(kinds(&council).contains(&StepKind::Talk));
    for kind in [
        StepKind::Draft,
        StepKind::Map,
        StepKind::Critique,
        StepKind::Judge,
    ] {
        assert!(!kinds(&council).contains(&kind), "{kind:?}");
    }
}

fn holds(settings: &Settings, condition: &str, round: u32) -> bool {
    let at = StepAt {
        step: argonauts_core::workflow::StepRef {
            workflow: argonauts_core::names::WorkflowId::numbered(1),
            phase: 1,
            path: vec![0, 2],
        },
        round,
    };
    conditions::holds(&RunState::default(), settings, condition, &at)
        .unwrap_or_else(|err| panic!("{err}"))
}

#[test]
fn workflow_behaviors() {
    // Captain picks its workflow from the Captain settings.
    let mut captain = Settings::default();
    assert_eq!(captain_workflow(&captain), "captain");
    captain.captain.review = true;
    assert_eq!(captain_workflow(&captain), "maker-checker");
    captain.captain.tries = 3;
    assert_eq!(captain_workflow(&captain), "best-of-n");
    assert!(!holds(&captain, "understand_first", 1));
    captain.captain.understand_first = true;
    assert!(holds(&captain, "understand_first", 1));

    // The fixed phases: blind drafts, a critique loop capped by `critique.rounds`, judging only
    // where drafts disagree.
    let base = load("base");
    let steps = all_steps(&base);
    assert!(
        steps
            .iter()
            .any(|step| step.kind == StepKind::Draft && step.blind)
    );
    let critique = steps
        .iter()
        .find(|step| step.until.as_deref() == Some("critique_done"))
        .unwrap_or_else(|| panic!("no critique loop"));
    assert!(
        critique
            .steps
            .iter()
            .any(|step| step.kind == StepKind::Critique)
    );
    let judge = steps.iter().find(|step| step.kind == StepKind::Judge);
    assert_eq!(
        judge.and_then(|step| step.when.as_deref()),
        Some("drafts_disagree")
    );
    let mut ablated = Settings::default();
    ablated.critique.rounds = 0;
    assert!(
        holds(&ablated, "critique_done", 1),
        "no critique round when rounds = 0"
    );
    ablated.critique.rounds = 1;
    assert!(!holds(&ablated, "critique_done", 1));
    assert!(holds(&ablated, "critique_done", 2));

    // orchestrator: one orchestrator plans, with no blind drafts or map.
    let orchestrated = load("orchestrator");
    let drafts: Vec<_> = all_steps(&orchestrated)
        .into_iter()
        .filter(|step| step.kind == StepKind::Draft)
        .collect();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].role, Some(RoleName::Orchestrator));
    assert!(!kinds(&orchestrated).contains(&StepKind::Map));

    // draft-and-vote: blind drafts, then a vote, with no critique.
    let vote = load("draft-and-vote");
    assert!(kinds(&vote).contains(&StepKind::Judge));
    assert!(!kinds(&vote).contains(&StepKind::Critique));

    // ...and its judge names the vote, so it votes whatever `judging.method` says.
    let judge = all_steps(&vote)
        .into_iter()
        .find(|step| step.kind == StepKind::Judge)
        .cloned();
    assert_eq!(
        judge.as_ref().map(argonauts_crew::plan::judge::method),
        Some(Ok(Some(JudgingMethod::Vote)))
    );

    // The Captain workflows build from the goal and merge; each file says how.
    let build = |name: &str| {
        let workflow = load(name);
        assert!(!kinds(&workflow).contains(&StepKind::Draft), "{name}");
        let build = all_steps(&workflow)
            .into_iter()
            .find(|step| step.kind == StepKind::Build)
            .cloned();
        let build = build.unwrap_or_else(|| panic!("{name} has no build"));
        assert_eq!(build.input.as_deref(), Some("goal"), "{name}");
        (workflow, build)
    };
    let (_, captain) = build("captain");
    assert_eq!(
        (captain.count, captain.different_provider_from),
        (None, None)
    );
    let (_, reviewed) = build("maker-checker");
    assert_eq!(reviewed.different_provider_from.as_deref(), Some("author"));
    let (best, tries) = build("best-of-n");
    assert_eq!(tries.count, Some(Count::Param("{{tries}}".to_owned())));
    assert_eq!(best.params.0.get("tries"), Some(&json!(3)));
}

/// The smallest answer a strict JSON schema accepts: every required field at its simplest.
fn minimal(schema: &Value, root: &Value) -> Value {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or_default();
        let definitions = root.get("$defs").or_else(|| root.get("definitions"));
        return definitions
            .and_then(|definitions| definitions.get(name))
            .map_or(Value::Null, |definition| minimal(definition, root));
    }
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return first.clone();
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(first) = schema
            .get(key)
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            return minimal(first, root);
        }
    }
    let kind = match schema.get("type") {
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind == "null")
            .or_else(|| types.iter().filter_map(Value::as_str).next()),
        Some(Value::String(kind)) => Some(kind.as_str()),
        _ => None,
    };
    match kind {
        Some("null") | None => Value::Null,
        Some("string") => Value::String("ok".to_owned()),
        Some("integer") | Some("number") => {
            json!(schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0) as i64)
        }
        Some("boolean") => Value::Bool(false),
        Some("array") => {
            let count = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            let item = schema
                .get("items")
                .map_or(Value::Null, |items| minimal(items, root));
            Value::Array(vec![item; count as usize])
        }
        Some(_) => {
            let mut object = Map::new();
            let properties = schema.get("properties").and_then(Value::as_object);
            for name in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = name.as_str() else { continue };
                let property = properties.and_then(|properties| properties.get(name));
                object.insert(
                    name.to_owned(),
                    property.map_or(Value::Null, |property| minimal(property, root)),
                );
            }
            Value::Object(object)
        }
    }
}

/// A worker that answers every session with the smallest valid answer, and makes a change in
/// its worktree when it builds.
fn scripted_worker(name: &str, provider: &str) -> FakeRunner {
    FakeRunner::cli(name, provider).respond_with(|spec| {
        let built = (spec.step == Some(StepName::Build)).then(|| format!("{}.txt", spec.key));
        if let Some(file) = &built {
            let _ = std::fs::write(spec.workdir.join(file), "work\n");
        }
        let schema = spec.json_schema.clone().unwrap_or(Value::Null);
        let mut answer = minimal(&schema, &schema);
        // A plan needs a task to be approved, and the orchestrator's needs its worker and
        // reviewers too to pass the rules.
        if matches!(
            spec.step,
            Some(StepName::Orchestrate | StepName::PlanToTasks)
        ) {
            answer["tasks"] = json!([{
                "key": "limits",
                "title": "Per-key rate limits",
                "task": "Add per-key rate limiting to the public API",
                "kind": "build",
                "owned_paths": ["src/limits"],
                "read_only_paths": [],
                "depends_on": [],
                "criteria": []
            }]);
        }
        if spec.step == Some(StepName::CloseRun) {
            answer = json!({ "answer": "Rate limiting is in, and the checks pass." });
        }
        if spec.step == Some(StepName::Orchestrate) {
            answer["assignments"] = json!([{
                "task": "limits",
                "worker": "codex/gpt-5",
                "reviewers": ["claude-code/opus", "agy/gemini-3"]
            }]);
        }
        // Every judge ranks plans 1, 2, 3 in that order, and only the scores for plans it was
        // shown count, so a vote has a majority: plan 1's judge prefers 2, the others prefer 1.
        if spec.step == Some(StepName::Judge) {
            let criteria = argonauts_crew::plan::judging::rubric().len();
            let score = |plan: u32, value: f64| {
                json!({
                    "plan": plan,
                    "criteria": vec![value; criteria],
                    "rationale": "ranked by label",
                    "addressed_to_judges": false
                })
            };
            answer = json!({ "scores": [score(1, 9.0), score(2, 6.0), score(3, 3.0)] });
        }
        let session = FakeSession::structured(answer);
        // The change is made with the agent's edit tool, as a CLI reports it.
        match built {
            Some(file) => session.with_event(support::wrote(&file)),
            None => session,
        }
    })
}

fn crew_of_three(workflow: &str) -> anyhow::Result<support::Crew> {
    let mut crew = support::Crew::new()?;
    let workers = [
        ("claude-code/opus", "anthropic"),
        ("codex/gpt-5", "openai"),
        ("agy/gemini-3", "google"),
    ];
    crew.settings.crew.clear();
    for (name, provider) in workers {
        let worker: Arc<FakeRunner> = crew.services.workers.add(scripted_worker(name, provider));
        let _ = worker;
        crew.settings.crew.add(name.parse::<ModelRef>()?);
    }
    crew.settings.crew.main = crew.settings.crew.members()[0].clone();
    crew.settings.workflow = workflow.to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.workflows.orchestrator_designs = false.into();
    // One build task is plan enough here; contracts first would ask for an interface task and
    // acceptance tests besides.
    crew.settings.divide.contracts_first = false;
    Ok(crew)
}

type Pick = dyn Fn(&ApprovalOpened) -> Option<AnswerAction>;

/// Your answer to the newest open question: the option `pick` names when the question offers it,
/// else its first, with a line typed when it asks for your own words.
fn answer_with(
    events: &[RunEvent],
    pick: &Pick,
) -> Option<(EngineCommand, oneshot::Receiver<Result<(), String>>)> {
    let mut open: Vec<ApprovalOpened> = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => open.push((**opened).clone()),
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|question| question.id != decided.id)
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.retain(|question| question.id != *id)
            }
            _ => {}
        }
    }
    let question = open.pop()?;
    let picked = pick(&question).and_then(|action| {
        question
            .options
            .iter()
            .find(|option| option.action == action)
    });
    let option = picked.or_else(|| {
        question
            .options
            .iter()
            .find(|option| option.action != AnswerAction::ShowDiff)
    })?;
    let text = (option.action == AnswerAction::Answer || question.free_text == FreeText::Answer)
        .then(|| "yes".to_owned());
    let (reply, receiver) = oneshot::channel();
    let answer = ApprovalAnswer {
        id: question.id.clone(),
        action: option.action.clone(),
        text,
        evidence_sha256: question.evidence_sha256.clone(),
        tasks: Vec::new(),
    };
    Some((EngineCommand::Answer { answer, reply }, receiver))
}

/// Starts the run and answers every question it waits on with its first option, until it ends.
async fn drive(crew: &support::Crew, request: &str) -> anyhow::Result<(EngineExit, Vec<RunEvent>)> {
    drive_as(crew, RunMode::Crew, request, &|_| None).await
}

/// Starts the run in `mode` and answers every question it waits on as `pick` says, until it ends.
async fn drive_as(
    crew: &support::Crew,
    mode: RunMode,
    request: &str,
    pick: &Pick,
) -> anyhow::Result<(EngineExit, Vec<RunEvent>)> {
    let outcome = crew.start(mode, request).await?;
    let mut exit = outcome.exit;
    let mut history = outcome.events;
    for _ in 0..30 {
        if !matches!(exit, EngineExit::Waiting(_)) {
            break;
        }
        let Some((command, reply)) = answer_with(&history, pick) else {
            break;
        };
        let resumed = crew.resume(history.clone(), vec![command]).await?;
        assert_eq!(reply.await?, Ok(()));
        exit = resumed.exit;
        history.extend(resumed.events);
    }
    Ok((exit, history))
}

fn started_kinds(events: &[RunEvent]) -> Vec<(StepKind, StandardPhase)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::StepStarted(started)) => {
                Some((started.kind, started.standard))
            }
            _ => None,
        })
        .collect()
}

const EVERY_KIND: &str = r#"
name: every-kind
description: Every step kind once
phases:
  - name: Understand
    standard: understand
    steps:
      - kind: read
        name: readings
        role: reader
      - kind: compare
        role: comparer
        input: readings
      - kind: approval
        approval: understand
  - name: Plan
    standard: plan
    steps:
      - kind: draft
        name: drafts
        role: draft
        blind: true
      - kind: map
        name: map
        role: map
        input: drafts
      - kind: critique
        role: critic
        input: map
      - kind: synthesize
        role: draft
        input: drafts
      - kind: judge
        role: judge
        input: drafts
      - kind: approval
        approval: plan
  - name: Build
    standard: do
    steps:
      - kind: build
        role: worker
        input: plan
      - kind: for_each
        input: plan
        steps:
          - kind: checks
  - name: Check
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        input: diff
      - kind: review
        role: reviewer
        input: diff
      - kind: loop_until
        until: understood
        max: 1
        steps:
          - kind: checks
      - kind: workflow
        workflow: inner-look
  - name: Merge
    standard: decide
    steps:
      - kind: merge
"#;

const INNER_LOOK: &str = "name: inner-look\ndescription: A look one level down\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n";

#[tokio::test]
async fn workflow_every_step_kind() -> anyhow::Result<()> {
    let crew = crew_of_three("every-kind")?;
    crew.repo
        .write(".argonauts/workflows/every-kind.yaml", EVERY_KIND)?;
    crew.repo
        .write(".argonauts/workflows/inner-look.yaml", INNER_LOOK)?;
    let (_, events) = drive(&crew, "add per-key rate limiting to the public API").await?;
    let workflow = argonauts_crew::workflow::yaml::parse(EVERY_KIND).map_err(anyhow::Error::msg)?;
    let started = started_kinds(&events);
    let seen: BTreeSet<StepKind> = started.iter().map(|(kind, _)| *kind).collect();
    for step in all_steps(&workflow) {
        assert!(
            seen.contains(&step.kind),
            "{:?} never started: {started:?}",
            step.kind
        );
    }
    // Each step carries its phase's standard phase.
    for (kind, standard) in &started {
        let expected = match kind {
            StepKind::Read | StepKind::Compare => StandardPhase::Understand,
            StepKind::Draft | StepKind::Map | StepKind::Critique | StepKind::Synthesize => {
                StandardPhase::Plan
            }
            StepKind::Build | StepKind::ForEach => StandardPhase::Do,
            StepKind::Merge => StandardPhase::Decide,
            _ => continue,
        };
        assert_eq!(standard, &expected, "{kind:?}");
    }
    let nested = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Workflow(WorkflowEvent::Started(started)) if started.parent.is_some()
        )
    });
    assert!(nested, "the `workflow` step started no nested workflow");
    Ok(())
}

/// WORKFLOW's end-to-end example with scripted agents: goal, blind drafts, the map, judging,
/// the plan approval, tasks, and the merge, first supervised and then in auto.
#[tokio::test]
async fn council_full_run() -> anyhow::Result<()> {
    let crew = crew_of_three("council")?;
    let (exit, events) = drive(&crew, "add per-key rate limiting to the public API").await?;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Merged));
    // The main agent closes the run in its own words, beside what changed.
    let closing = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) => Some(answer.clone()),
            _ => None,
        })
        .expect("the run closes with an answer");
    assert_eq!(closing.text, "Rate limiting is in, and the checks pass.");
    assert_eq!(closing.by, Some(crew.settings.crew.main.clone()));
    assert!(
        closing.files.iter().any(|file| file.path.ends_with(".txt"))
            && closing
                .files
                .iter()
                .all(|file| !file.path.starts_with(".argonauts/")),
        "{closing:?}"
    );
    let standards: BTreeSet<StandardPhase> = started_kinds(&events)
        .into_iter()
        .map(|(_, standard)| standard)
        .collect();
    for standard in [
        StandardPhase::Understand,
        StandardPhase::Plan,
        StandardPhase::Do,
        StandardPhase::Decide,
    ] {
        assert!(standards.contains(&standard), "{standard:?}");
    }
    let root = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Workflow(WorkflowEvent::Started(started)) => Some(started.from.clone()),
        _ => None,
    });
    assert_eq!(root, Some(Some(WorkflowRef::new("council"))));
    let by_you = events.iter().filter(|event| {
        matches!(
            &event.kind,
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) if decided.by == Party::User
        )
    });
    let by_you = by_you.count();
    assert!(
        by_you >= 2,
        "supervised: you decide the goal, the plan, and the merge"
    );
    // The main agent is told who reviewed the work, what each said, and how often you were asked.
    let closer = crew
        .services
        .workers
        .fake(crew.settings.crew.main.to_string().as_str())
        .expect("the main agent");
    let told = closer
        .specs()
        .into_iter()
        .find(|spec| spec.step == Some(StepName::CloseRun))
        .expect("the main agent closes the run");
    let input: Value = told
        .prompt
        .iter()
        .find(|part| part.kind == argonauts_core::events::session::InputPartKind::StepInput)
        .map(|part| serde_json::from_str(&part.text))
        .expect("the closing input")?;
    // A resumed run's events repeat what came before, so the decisions are counted by id.
    let decided_by_you: BTreeSet<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.by == Party::User =>
            {
                Some(decided.id.to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        input["needed_you"],
        json!(decided_by_you.len()),
        "{input:#}"
    );
    let asked = input["asked"].as_array().expect("who was asked");
    assert!(
        !asked.is_empty()
            && asked.iter().all(|said| said
                .as_str()
                .is_some_and(|said| said.ends_with(": accepted"))),
        "{input:#}"
    );

    let mut auto = crew_of_three("council")?;
    auto.settings.approvals.understand.mode = ApprovalMode::Auto;
    auto.settings.approvals.plan.mode = ApprovalMode::Auto;
    auto.settings.approvals.merge.mode = ApprovalMode::Auto;
    for _ in 0..10 {
        auto.services.policy.decide_next(PolicyOutcome::Decided {
            verdict: Verdict::Approve,
            by: DecideWith::Rules,
            judge: None,
            reasons: vec!["your rules passed".to_owned()],
            spot_check: false,
        });
    }
    let (exit, events) = drive(&auto, "add per-key rate limiting to the public API").await?;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Merged));
    let by_rules = events.iter().filter(|event| {
        matches!(
            &event.kind,
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) if decided.by == Party::Rules
        )
    });
    assert!(
        by_rules.count() >= 2,
        "auto: your rules decide the plan and the merge"
    );
    Ok(())
}

/// The workflows the run followed, in the order they started, by name. A resumed run's
/// history repeats what came before, so each workflow counts once.
fn followed(events: &[RunEvent]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Started(started))
                if started.parent.is_none() && seen.insert(started.id.clone()) =>
            {
                Some(
                    started
                        .from
                        .as_ref()
                        .map_or_else(String::new, ToString::to_string),
                )
            }
            _ => None,
        })
        .collect()
}

fn created_tasks(events: &[RunEvent]) -> Vec<TaskSpec> {
    let mut tasks: Vec<TaskSpec> = Vec::new();
    for event in events {
        if let RunEventKind::Task(TaskEvent::Created(spec)) = &event.kind
            && !tasks.iter().any(|task| task.id == spec.id)
        {
            tasks.push((**spec).clone());
        }
    }
    tasks
}

fn passing_check() -> CheckCommand {
    CheckCommand {
        name: "check".to_owned(),
        command: vec!["true".to_owned()],
    }
}

/// "Run it as Captain" at the plan approval builds the Captain task from the goal and merges it,
/// although the run started as a crew run and its plan was never approved.
#[tokio::test]
async fn plan_approval_switch_to_captain_finishes() -> anyhow::Result<()> {
    let crew = crew_of_three("council")?;
    let captain_at_plan = |question: &ApprovalOpened| {
        (question.kind == ApprovalKind::Plan).then_some(AnswerAction::SwitchToCaptain)
    };
    let (exit, events) = drive_as(
        &crew,
        RunMode::Crew,
        "add per-key rate limiting to the public API",
        &captain_at_plan,
    )
    .await?;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Merged));
    assert_eq!(followed(&events), ["council", "captain"]);
    let tasks = created_tasks(&events);
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].kind, TaskKind::Captain);
    Ok(())
}

/// A Captain run the rules started as Captain first, then crew, goes to Orchestrator once its
/// task needs you, since that is the crew workflow setup recommends.
#[tokio::test]
async fn a_captain_task_that_needs_you_goes_to_orchestrator() -> anyhow::Result<()> {
    let mut crew = crew_of_three("captain")?;
    crew.settings.checks.run = vec![passing_check()];
    // The checks fail for as many rounds of fixes as the task gets.
    for _ in 0..10 {
        crew.services.checks.fail_next("1 failed");
    }
    let mut started = crew.run_started(
        RunMode::Captain,
        "add per-key rate limiting to the public API",
    )?;
    let RunEventKind::Run(RunLifecycle::Started(run)) = &mut started.kind else {
        anyhow::bail!("the first event starts the run");
    };
    run.captain_or_crew = Some(CaptainOrCrewDecision {
        mode: RunMode::Captain,
        move_to_crew: true,
        rule: "captain_max_files".to_owned(),
        reason: "Captain first, then crew if the task needs you".to_owned(),
        captain: Some(CaptainKind::Checked),
    });
    let mut outcome = crew.resume(vec![started], Vec::new()).await?;
    for _ in 0..30 {
        if followed(&outcome.events).len() > 1 || !matches!(outcome.exit, EngineExit::Waiting(_)) {
            break;
        }
        let Some((command, _reply)) = answer_with(&outcome.events, &|_| None) else {
            break;
        };
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
    }
    assert_eq!(
        followed(&outcome.events),
        ["captain", "orchestrator"],
        "{:?}",
        outcome.exit
    );
    Ok(())
}

/// Each built-in runs to a merge with scripted agents, and does what its file says whatever
/// the settings say.
#[tokio::test]
async fn each_builtin_runs_end_to_end() -> anyhow::Result<()> {
    let request = "add per-key rate limiting to the public API";
    for (name, mode) in [
        ("council", RunMode::Crew),
        ("orchestrator", RunMode::Crew),
        ("draft-and-vote", RunMode::Crew),
        ("captain", RunMode::Captain),
        ("maker-checker", RunMode::Captain),
        ("best-of-n", RunMode::Captain),
    ] {
        let mut crew = crew_of_three(name)?;
        crew.settings.checks.run = vec![passing_check()];
        let (exit, events) = drive_as(&crew, mode, request, &|_| None).await?;
        assert_eq!(exit, EngineExit::Finished(RunOutcome::Merged), "{name}");
        assert_eq!(followed(&events), [name], "{name}");
        let tasks = created_tasks(&events);
        match name {
            "captain" => {
                assert_eq!(tasks.len(), 1, "{name}");
                assert!(tasks[0].reviewers.is_empty(), "{name}");
            }
            "maker-checker" => {
                assert_eq!(tasks.len(), 1, "{name}");
                assert_eq!(tasks[0].reviewers.len(), 1, "{name}");
                assert_ne!(tasks[0].reviewers[0], crew.settings.crew.main, "{name}");
            }
            "best-of-n" => {
                assert_eq!(tasks.len(), 3, "{name}: {tasks:?}");
                assert!(
                    tasks.iter().all(|task| task.kind == TaskKind::Captain),
                    "{name}"
                );
            }
            "draft-and-vote" => {
                let method = events.iter().find_map(|event| match &event.kind {
                    RunEventKind::Plan(PlanEvent::Judged(result)) => Some(result.method),
                    _ => None,
                });
                assert_eq!(method, Some(JudgingMethod::Vote), "{name}");
            }
            _ => {}
        }
        let built = started_kinds(&events)
            .iter()
            .any(|(kind, _)| *kind == StepKind::Build);
        assert!(built, "{name} never reached its build");
    }
    Ok(())
}
