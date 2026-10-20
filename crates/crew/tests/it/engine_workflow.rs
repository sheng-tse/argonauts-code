//! The workflow runner: the validator's rules, nesting, composed workflows, and changes that go
//! through the `change_request` approval.

use crate::support;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::engine::EngineExit;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind};
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome, WaitReason};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::workflow::{
    StepEnd, StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, WorkflowId};
use argonauts_core::settings::Settings;
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow, WorkflowRef};
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::runner::allowed_workers;
use argonauts_crew::workflow::validate::{Context, Rule, validate};
use argonauts_crew::workflow::yaml;

fn parse(text: &str) -> Workflow {
    yaml::parse(text).unwrap_or_else(|err| panic!("{err}\n{text}"))
}

/// Workflows a `workflow` step may name in these tests, besides the built-ins.
const NESTED: &[(&str, &str)] = &[
    ("review-panel", REVIEW_PANEL),
    ("inner-look", INNER_LOOK),
    ("middle", MIDDLE),
    ("loops", LOOPS),
    ("broken-inside", BROKEN_INSIDE),
];

fn find(name: &str) -> Result<Workflow, String> {
    if let Some((_, text)) = NESTED.iter().find(|(known, _)| *known == name) {
        return yaml::parse(text).map_err(|err| err.to_string());
    }
    argonauts_crew::workflow::builtin::load(name)
}

/// Each worker's provider is the agent in its name, such as `codex` for `codex/gpt-x`.
fn providers(allowed: &[ModelRef]) -> BTreeMap<ModelRef, String> {
    allowed
        .iter()
        .map(|worker| {
            let text = worker.to_string();
            let agent_id = text.split('/').next().unwrap_or_default().to_owned();
            (worker.clone(), agent_id)
        })
        .collect()
}

fn rules(
    workflow: &Workflow,
    settings: &Settings,
    depth: u32,
    limit: Option<f64>,
) -> BTreeSet<String> {
    let allowed = allowed_workers(settings);
    let providers = providers(&allowed);
    let context = Context {
        settings,
        allowed: &allowed,
        providers: &providers,
        lookup: &find,
        depth,
        limit_left_usd: limit,
        as_you_asked: &[],
    };
    match validate(workflow, &context) {
        Ok(()) => BTreeSet::new(),
        Err(errors) => errors
            .iter()
            .map(|error| format!("{:?}", error.rule))
            .collect(),
    }
}

fn phase(steps: &str) -> String {
    format!("name: broken\nphases:\n  - name: Only\n    standard: check\n    steps:\n{steps}")
}

#[test]
fn workflow_validator_rejects_each_rule_break() {
    let settings = Settings::default();
    let cases: Vec<(Rule, String)> = vec![
        (Rule::Empty, "name: empty\nphases: []\n".to_owned()),
        (
            Rule::LoopWithoutMax,
            phase(
                "      - kind: loop_until\n        until: understood\n        steps:\n          - kind: checks\n",
            ),
        ),
        (
            Rule::UnknownCondition,
            phase("      - kind: checks\n        when: the_main_agent_feels_like_it\n"),
        ),
        (
            Rule::UnknownInput,
            phase("      - kind: review\n        role: reviewer\n        input: nowhere\n"),
        ),
        (
            Rule::FieldNotForKind,
            phase("      - kind: checks\n        role: reviewer\n"),
        ),
        (
            Rule::MissingParam,
            phase(
                "      - kind: fan_out\n        role: reviewer\n        count: \"{{reviewers}}\"\n",
            ),
        ),
        (
            Rule::ModelNotAllowed,
            phase(
                "      - kind: review\n        role: reviewer\n        models: [stranger/model-x]\n",
            ),
        ),
        (
            Rule::MissingApproval,
            phase("      - kind: build\n        role: worker\n        input: plan\n"),
        ),
        (
            Rule::BuildNeedsCheckAndReview,
            phase("      - kind: fan_out\n        role: worker\n"),
        ),
        (
            Rule::ReviewSameProvider,
            phase(
                "      - kind: review\n        role: reviewer\n        different_provider_from: nobody\n",
            ),
        ),
    ];
    for (rule, text) in &cases {
        let found = rules(&parse(text), &settings, 0, None);
        assert!(
            found.contains(&format!("{rule:?}")),
            "{rule:?} not found in {found:?}\n{text}"
        );
    }

    // A reviewer never reviews its own work.
    let mut crew = settings.clone();
    crew.crew = vec![ModelRef::new("codex", None)].into();
    let own = phase(
        "      - kind: draft\n        name: answer\n        role: draft\n        models: [codex]\n      - kind: review\n        role: reviewer\n        input: answer\n        models: [codex]\n",
    );
    assert!(rules(&parse(&own), &crew, 0, None).contains("ReviewSameProvider"));

    // Over limit: two task builds at $5 each with $3 left.
    let mut limit = settings.clone();
    limit.limits.spend.per_task_usd = Some(5.0);
    let builds =
        argonauts_crew::workflow::builtin::load("captain").unwrap_or_else(|err| panic!("{err}"));
    assert!(rules(&builds, &limit, 0, Some(3.0)).contains("OverLimit"));
    assert!(rules(&builds, &limit, 0, Some(30.0)).is_empty());

    // Every error comes back at once.
    let many = phase(
        "      - kind: checks\n        role: reviewer\n        when: never\n      - kind: loop_until\n        steps: []\n",
    );
    assert!(rules(&parse(&many), &settings, 0, None).len() >= 4);
}

const NESTS: &str = "name: outer\nphases:\n  - name: Review\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: review-panel\n";

const REVIEW_PANEL: &str = "name: review-panel\nphases:\n  - name: Review\n    standard: check\n    steps:\n      - kind: fan_out\n        role: reviewer\n  - name: Settle\n    standard: decide\n    steps:\n      - kind: judge\n        role: judge\n        when: reviewers_disagree\n";

const INNER_LOOK: &str = "name: inner-look\ndescription: A look one level down\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n";

/// One level of nesting on its own, so a chain through it reaches depth 2.
const MIDDLE: &str = "name: middle\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: inner-look\n";

const LOOPS: &str = "name: loops\nphases:\n  - name: Again\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: loops\n";

const BROKEN_INSIDE: &str = "name: broken-inside\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n        when: the_main_agent_feels_like_it\n";

fn nests(name: &str) -> Workflow {
    parse(&format!(
        "name: outer\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: {name}\n"
    ))
}

#[test]
fn nesting_stops_at_the_levels_the_settings_allow() {
    // One level inside the run's own workflow by default.
    let settings = Settings::default();
    let workflow = parse(NESTS);
    assert!(rules(&workflow, &settings, 0, None).is_empty());
    assert_eq!(
        rules(&workflow, &settings, 1, None),
        BTreeSet::from(["TooDeep".to_owned()])
    );

    // Two levels at most, which a setting can allow but never raise past.
    let mut two = settings.clone();
    two.workflows.max_depth = 2;
    assert!(rules(&workflow, &two, 1, None).is_empty());
    assert_eq!(
        rules(&workflow, &two, 2, None),
        BTreeSet::from(["TooDeep".to_owned()])
    );
    let mut deep = settings.clone();
    deep.workflows.max_depth = 5;
    assert!(rules(&workflow, &deep, 2, None).contains("TooDeep"));
    let plain = parse(
        "name: leaf\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n",
    );
    assert!(rules(&plain, &two, 3, None).contains("TooDeep"));

    // The chain is followed into each nested workflow: outer, middle, inner-look fits from
    // the top with two levels allowed, and one level lower it would reach depth 3. With the
    // one level the default allows, it doesn't fit from the top.
    let chain = nests("middle");
    assert!(rules(&chain, &two, 0, None).is_empty());
    assert_eq!(
        rules(&chain, &two, 1, None),
        BTreeSet::from(["TooDeep".to_owned()])
    );
    assert_eq!(
        rules(&chain, &settings, 0, None),
        BTreeSet::from(["TooDeep".to_owned()])
    );
    let mut flat = settings.clone();
    flat.workflows.max_depth = 0;
    assert_eq!(
        rules(&nests("inner-look"), &flat, 0, None),
        BTreeSet::from(["TooDeep".to_owned()])
    );

    // A nested workflow that can't be found, runs itself, or breaks a rule inside.
    assert_eq!(
        rules(&nests("nowhere"), &settings, 0, None),
        BTreeSet::from(["UnknownWorkflow".to_owned()])
    );
    // A workflow that runs itself is found out at the first level it would repeat.
    assert!(rules(&nests("loops"), &two, 0, None).contains("UnknownWorkflow"));
    assert_eq!(
        rules(&nests("broken-inside"), &settings, 0, None),
        BTreeSet::from(["UnknownCondition".to_owned()])
    );

    // A nested workflow's builds count toward the limit floor.
    let mut limit = settings.clone();
    limit.limits.spend.per_task_usd = Some(5.0);
    let builds_twice = parse(
        "name: twice\nphases:\n  - name: Build\n    standard: do\n    steps:\n      - kind: workflow\n        workflow: captain\n      - kind: workflow\n        workflow: captain\n",
    );
    assert!(rules(&builds_twice, &limit, 0, Some(8.0)).contains("OverLimit"));
    assert!(rules(&builds_twice, &limit, 0, Some(12.0)).is_empty());
}

#[test]
fn validator_rules_for_approvals_loops_and_providers() {
    let settings = Settings::default();

    // A crew build needs Understand and plan approvals that always open.
    let build =
        "      - kind: build\n        role: worker\n        input: plan\n      - kind: merge\n";
    let unconditional = "      - kind: approval\n        approval: understand\n      - kind: approval\n        approval: plan\n";
    assert!(
        rules(
            &parse(&phase(&format!("{unconditional}{build}"))),
            &settings,
            0,
            None
        )
        .is_empty()
    );
    let skippable = "      - kind: approval\n        approval: understand\n      - kind: approval\n        approval: plan\n        when: drafts_agree\n";
    assert!(
        rules(
            &parse(&phase(&format!("{skippable}{build}"))),
            &settings,
            0,
            None
        )
        .contains("MissingApproval")
    );
    let readings = "      - kind: approval\n        approval: understand\n        when: understand_first\n      - kind: approval\n        approval: plan\n";
    assert!(
        rules(
            &parse(&phase(&format!("{readings}{build}"))),
            &settings,
            0,
            None
        )
        .contains("MissingApproval")
    );
    let per_item = "      - kind: for_each\n        input: plan\n        steps:\n          - kind: approval\n            approval: understand\n      - kind: approval\n        approval: plan\n";
    assert!(
        rules(
            &parse(&phase(&format!("{per_item}{build}"))),
            &settings,
            0,
            None
        )
        .contains("MissingApproval")
    );
    // Captain keeps its Understand approval behind `understand_first`, and no other may use it.
    for captain in ["captain", "maker-checker", "best-of-n"] {
        let workflow =
            argonauts_crew::workflow::builtin::load(captain).unwrap_or_else(|err| panic!("{err}"));
        assert!(rules(&workflow, &settings, 0, None).is_empty(), "{captain}");
    }
    let plan_understood =
        phase("      - kind: approval\n        approval: plan\n        when: understand_first\n");
    assert!(rules(&parse(&plan_understood), &settings, 0, None).contains("MissingApproval"));

    // A loop's `max` has a ceiling.
    let endless = phase(
        "      - kind: loop_until\n        until: understood\n        max: 1000\n        steps:\n          - kind: checks\n",
    );
    assert!(rules(&parse(&endless), &settings, 0, None).contains("LoopWithoutMax"));

    // Judges and critics pick their own workers, so they don't take `different_provider_from`.
    for kind in ["judge", "critique"] {
        let text = phase(&format!(
            "      - kind: {kind}\n        role: judge\n        different_provider_from: author\n"
        ));
        assert!(
            rules(&parse(&text), &settings, 0, None).contains("FieldNotForKind"),
            "{kind}"
        );
    }

    // `different_provider_from` compares model providers, not workers.
    let mut crew = settings.clone();
    crew.crew = vec![
        ModelRef::new("claude-code", Some("opus".to_owned())),
        ModelRef::new("claude-code", Some("sonnet".to_owned())),
        ModelRef::new("codex", None),
    ]
    .into();
    let reviewed_by = |reviewer: &str| {
        phase(&format!(
            "      - kind: draft\n        name: answer\n        role: draft\n        models: [claude-code/opus]\n      - kind: review\n        role: reviewer\n        input: answer\n        different_provider_from: answer\n        models: [{reviewer}]\n"
        ))
    };
    assert!(
        rules(&parse(&reviewed_by("claude-code/sonnet")), &crew, 0, None)
            .contains("ReviewSameProvider")
    );
    assert!(rules(&parse(&reviewed_by("codex")), &crew, 0, None).is_empty());
    // With no reviewer named, some reviewer must come from another provider.
    let mut one_provider = crew.clone();
    one_provider.crew = one_provider.crew.members().into_iter().take(2).collect();
    let unnamed = phase(
        "      - kind: draft\n        name: answer\n        role: draft\n        models: [claude-code/opus]\n      - kind: review\n        role: reviewer\n        input: answer\n        different_provider_from: author\n",
    );
    assert!(rules(&parse(&unnamed), &one_provider, 0, None).contains("ReviewSameProvider"));
    assert!(rules(&parse(&unnamed), &crew, 0, None).is_empty());
}

fn event(seq: u64, kind: WorkflowEvent) -> RunEvent {
    RunEvent {
        seq,
        ts: OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::at(OffsetDateTime::UNIX_EPOCH, 1),
        task_id: None,
        session: None,
        source: "argonauts-code".to_owned(),
        kind: RunEventKind::Workflow(kind),
    }
}

fn at(workflow: &WorkflowId, phase: u32, path: &[u32]) -> StepRef {
    StepRef {
        workflow: workflow.clone(),
        phase,
        path: path.to_vec(),
    }
}

fn started(step: StepRef, kind: StepKind, standard: StandardPhase, round: u32) -> WorkflowEvent {
    WorkflowEvent::StepStarted(StepStarted {
        step,
        kind,
        standard,
        round,
    })
}

fn finished(
    step: StepRef,
    kind: StepKind,
    standard: StandardPhase,
    round: u32,
    summary: &str,
) -> WorkflowEvent {
    WorkflowEvent::StepFinished(StepFinished {
        step,
        kind,
        standard,
        round,
        end: StepEnd::Done,
        summary: Some(summary.to_owned()),
    })
}

/// A run's workflow whose `workflow` step runs `review-panel` one level down: the fold counts
/// each workflow's steps apart, knows the nested one's depth and parent, and reports the
/// nested step's standard phase while it runs.
#[test]
fn nested_workflow_counts_and_reports() {
    let outer_id = WorkflowId::numbered(1);
    let inner_id = WorkflowId::numbered(2);
    let outer = parse(NESTS);
    let inner = parse(
        "name: review-panel\nphases:\n  - name: Review\n    standard: check\n    steps:\n      - kind: fan_out\n        role: reviewer\n  - name: Settle\n    standard: decide\n    steps:\n      - kind: judge\n        role: judge\n        when: reviewers_disagree\n",
    );
    let parent = at(&outer_id, 0, &[0]);
    let mut events = vec![
        event(
            1,
            WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: outer_id.clone(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow: outer,
            })),
        ),
        event(
            2,
            started(parent.clone(), StepKind::Workflow, StandardPhase::Check, 1),
        ),
        event(
            3,
            WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: inner_id.clone(),
                from: Some(WorkflowRef::new("review-panel")),
                version: "v2".to_owned(),
                parent: Some(parent.clone()),
                workflow: inner,
            })),
        ),
        event(
            4,
            started(
                at(&inner_id, 0, &[0]),
                StepKind::FanOut,
                StandardPhase::Check,
                1,
            ),
        ),
        event(
            5,
            finished(
                at(&inner_id, 0, &[0]),
                StepKind::FanOut,
                StandardPhase::Check,
                1,
                "reviewer done",
            ),
        ),
        event(
            6,
            started(
                at(&inner_id, 1, &[0]),
                StepKind::Judge,
                StandardPhase::Decide,
                1,
            ),
        ),
    ];
    let state = RunState::fold(&events);
    let workflows = &state.workflows;
    assert_eq!(workflows.root.as_ref(), Some(&outer_id));
    assert_eq!(workflows.depth(&outer_id), 0);
    assert_eq!(workflows.depth(&inner_id), 1);
    assert_eq!(workflows.by_id[&inner_id].parent.as_ref(), Some(&parent));
    assert_eq!(workflows.by_id[&inner_id].steps.len(), 2);
    assert_eq!(workflows.by_id[&outer_id].steps.len(), 1);
    assert_eq!(workflows.by_id[&inner_id].next_phase(), 1);
    assert_eq!(workflows.current_standard(), Some(StandardPhase::Decide));

    events.push(event(
        7,
        finished(
            at(&inner_id, 1, &[0]),
            StepKind::Judge,
            StandardPhase::Decide,
            1,
            "skipped",
        ),
    ));
    events.push(event(
        8,
        finished(
            parent.clone(),
            StepKind::Workflow,
            StandardPhase::Check,
            1,
            "review-panel ran 2 steps",
        ),
    ));
    let state = RunState::fold(&events);
    assert!(state.workflows.by_id[&inner_id].finished);
    assert!(state.workflows.by_id[&outer_id].finished);
    assert_eq!(state.workflows.current_standard(), None);
    assert_eq!(
        state
            .workflows
            .progress(&parent)
            .and_then(|progress| progress.summary.clone())
            .as_deref(),
        Some("review-panel ran 2 steps")
    );
}

/// Only kinds the runner owns, so it runs without agents: a check, a loop whose condition
/// already holds, and a `for_each` over an empty plan.
const COMPOSED: &str = r#"
name: composed-checks
description: A composed workflow of runner steps
phases:
  - name: Look
    standard: check
    steps:
      - kind: checks
        name: first-check
      - kind: loop_until
        name: critique
        until: critique_done
        max: 2
        steps:
          - kind: checks
  - name: Per task
    standard: do
    steps:
      - kind: for_each
        input: plan
        steps:
          - kind: checks
"#;

fn workflow_events(events: &[RunEvent]) -> Vec<&WorkflowEvent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(workflow) => Some(workflow),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn composed_workflow_runs() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.worker("claude-code/opus", "anthropic");
    crew.settings.workflow = "composed-checks".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.critique.rounds = 0;
    crew.repo
        .write(".argonauts/workflows/composed-checks.yaml", COMPOSED)?;
    let outcome = crew.start(RunMode::Crew, "check the repo").await?;
    let events = workflow_events(&outcome.events);
    let Some(WorkflowEvent::Started(started)) = events.first() else {
        panic!("no workflow started: {events:?}");
    };
    assert_eq!(started.from, Some(WorkflowRef::new("composed-checks")));
    // It runs as written, under the run's own name; the file shows only as `based_on`.
    assert_eq!(started.workflow.phases, parse(COMPOSED).phases);
    assert_eq!(started.workflow.name, "check-the-repo");
    assert_eq!(
        started.workflow.based_on.as_deref(),
        Some("composed-checks")
    );
    let finished: Vec<(StepKind, StandardPhase, StepEnd)> = events
        .iter()
        .filter_map(|event| match event {
            WorkflowEvent::StepFinished(finished) if finished.step.path.len() == 1 => {
                Some((finished.kind, finished.standard, finished.end))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        finished,
        vec![
            (StepKind::Checks, StandardPhase::Check, StepEnd::Skipped),
            (StepKind::LoopUntil, StandardPhase::Check, StepEnd::Done),
            (StepKind::ForEach, StandardPhase::Do, StepEnd::Done),
        ]
    );
    Ok(())
}

const TWO_PHASES: &str = "name: two-looks\ndescription: Two looks\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: checks\n  - name: Second look\n    standard: check\n    steps:\n      - kind: checks\n";

/// At the end of the first phase the main agent proposes a third phase. It opens the
/// `change_request` approval and waits for you; once you accept, `Workflow::Changed` names
/// that approval and the new phase runs.
#[tokio::test]
async fn workflow_change_mid_run_goes_through_change_request() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    let main_agent = crew.worker("claude-code/opus", "anthropic");
    crew.settings.crew.main = ModelRef::new("claude-code", Some("opus".to_owned()));
    crew.settings.workflow = "two-looks".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/two-looks.yaml", TWO_PHASES)?;
    let changed = format!(
        "{TWO_PHASES}  - name: Extra look\n    standard: check\n    steps:\n      - kind: checks\n"
    );
    let unchanged = support::brief(None, json!([]), None, "it fits");
    support::script(
        &main_agent,
        vec![
            support::brief(None, json!([]), Some(&changed), "one more look"),
            unchanged.clone(),
            unchanged,
        ],
    );
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(outcome.exit, EngineExit::Waiting(WaitReason::Approval));
    let opened = outcome.events.iter().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(opened.clone()),
        _ => None,
    });
    let opened = opened.unwrap_or_else(|| panic!("no approval opened"));
    assert_eq!(opened.kind, ApprovalKind::ChangeRequest);
    assert!(
        workflow_events(&outcome.events)
            .iter()
            .all(|event| !matches!(event, WorkflowEvent::Changed(_)))
    );

    let (command, reply) = support::answer(
        &outcome.events,
        ApprovalKind::ChangeRequest,
        AnswerAction::Approve,
        None,
    )?;
    let resumed = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    let mut all = outcome.events.clone();
    all.extend(resumed.events.iter().cloned());
    let events = workflow_events(&all);
    let change = events.iter().find_map(|event| match event {
        WorkflowEvent::Changed(changed) => Some(changed.clone()),
        _ => None,
    });
    let change = change.unwrap_or_else(|| panic!("no Workflow::Changed in {events:?}"));
    assert_eq!(change.approval, Some(opened.id));
    assert_eq!(change.workflow.phases.len(), 3);
    assert!(events.iter().any(|event| matches!(
        event,
        WorkflowEvent::StepFinished(finished) if finished.step.phase == 2
    )));
    Ok(())
}

/// A crew whose one fake worker answers nothing, for workflows of runner steps.
fn quiet_crew(workflow: &str) -> anyhow::Result<support::Crew> {
    let mut crew = support::Crew::new()?;
    crew.worker("claude-code/opus", "anthropic");
    crew.settings.workflow = workflow.to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    Ok(crew)
}

const OUTER_LOOKS: &str = "name: outer-looks\ndescription: A look, then a nested one\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n      - kind: workflow\n        workflow: inner-look\n";

#[tokio::test]
async fn nested_workflow_runs_through_the_runner() -> anyhow::Result<()> {
    let crew = quiet_crew("outer-looks")?;
    crew.repo
        .write(".argonauts/workflows/outer-looks.yaml", OUTER_LOOKS)?;
    crew.repo
        .write(".argonauts/workflows/inner-look.yaml", INNER_LOOK)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(outcome.exit, EngineExit::Finished(RunOutcome::BranchLeft));
    let events = workflow_events(&outcome.events);
    let started: Vec<&WorkflowStarted> = events
        .iter()
        .filter_map(|event| match event {
            WorkflowEvent::Started(started) => Some(started.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 2, "{started:?}");
    let (outer, inner) = (started[0], started[1]);
    assert_eq!(inner.from, Some(WorkflowRef::new("inner-look")));
    assert_eq!(inner.parent, Some(at(&outer.id, 0, &[1])));
    assert!(events.iter().any(|event| matches!(
        event,
        WorkflowEvent::StepFinished(finished)
            if finished.step.workflow == inner.id && finished.kind == StepKind::Checks
    )));
    let nested = events.iter().find_map(|event| match event {
        WorkflowEvent::StepFinished(finished) if finished.kind == StepKind::Workflow => {
            Some(finished.clone())
        }
        _ => None,
    });
    let nested = nested.unwrap_or_else(|| panic!("the workflow step never finished"));
    assert_eq!(nested.end, StepEnd::Done);
    assert_eq!(nested.summary.as_deref(), Some("inner-look ran 1 step"));
    Ok(())
}

/// The run's workflow is refused before any step runs, so nothing is spent on it.
fn refused_before_any_step(outcome: &support::Outcome, why: &str) {
    assert_eq!(outcome.exit, EngineExit::Finished(RunOutcome::Failed));
    assert!(
        workflow_events(&outcome.events)
            .iter()
            .all(|event| !matches!(event, WorkflowEvent::StepStarted(_))),
        "a step ran"
    );
    let detail = outcome.events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { detail, .. }) => detail.clone(),
        _ => None,
    });
    let detail = detail.unwrap_or_default();
    assert!(detail.contains(why), "{detail}");
}

#[tokio::test]
async fn nesting_past_depth_two_is_refused_before_the_run() -> anyhow::Result<()> {
    let mut crew = quiet_crew("chain-top")?;
    crew.settings.workflows.max_depth = 2;
    for (name, next) in [("chain-top", "chain-b"), ("chain-b", "middle")] {
        crew.repo.write(
            &format!(".argonauts/workflows/{name}.yaml"),
            &format!(
                "name: {name}\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: {next}\n"
            ),
        )?;
    }
    crew.repo
        .write(".argonauts/workflows/middle.yaml", MIDDLE)?;
    crew.repo
        .write(".argonauts/workflows/inner-look.yaml", INNER_LOOK)?;
    let outcome = crew.start(RunMode::Crew, "look deep").await?;
    refused_before_any_step(&outcome, "at depth 3, and nesting stops at 2");
    Ok(())
}

#[tokio::test]
async fn no_nesting_when_max_depth_is_zero() -> anyhow::Result<()> {
    let mut crew = quiet_crew("outer-looks")?;
    crew.settings.workflows.max_depth = 0;
    crew.repo
        .write(".argonauts/workflows/outer-looks.yaml", OUTER_LOOKS)?;
    crew.repo
        .write(".argonauts/workflows/inner-look.yaml", INNER_LOOK)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    refused_before_any_step(&outcome, "nesting stops at 0");
    Ok(())
}

/// A limit between 0 and the ceiling holds too: one level of nesting runs, and a second is
/// refused before the run spends anything.
#[tokio::test]
async fn max_depth_below_the_ceiling_is_enforced() -> anyhow::Result<()> {
    let mut crew = quiet_crew("outer-looks")?;
    crew.settings.workflows.max_depth = 1;
    crew.repo
        .write(".argonauts/workflows/outer-looks.yaml", OUTER_LOOKS)?;
    crew.repo
        .write(".argonauts/workflows/inner-look.yaml", INNER_LOOK)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(outcome.exit, EngineExit::Finished(RunOutcome::BranchLeft));

    let mut crew = quiet_crew("two-down")?;
    crew.settings.workflows.max_depth = 1;
    crew.repo.write(
        ".argonauts/workflows/two-down.yaml",
        "name: two-down\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: middle\n",
    )?;
    crew.repo
        .write(".argonauts/workflows/middle.yaml", MIDDLE)?;
    crew.repo
        .write(".argonauts/workflows/inner-look.yaml", INNER_LOOK)?;
    let outcome = crew.start(RunMode::Crew, "look deep").await?;
    refused_before_any_step(&outcome, "at depth 2, and nesting stops at 1");
    Ok(())
}

fn captain_report() -> serde_json::Value {
    json!({
        "status": "done",
        "summary": "Fixed the typo",
        // The scripted worker changes no file, so it names none.
        "files_changed": [],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": []
    })
}

/// The log up to and including the `StepFinished` of the top-level step of `kind`: where a
/// pause or crash during the run review leaves it.
fn cut_after(events: &[RunEvent], kind: StepKind) -> Vec<RunEvent> {
    cut_after_at(events, kind, 1)
}

/// The same for the step of `kind` whose path is `depth` long.
fn cut_after_at(events: &[RunEvent], kind: StepKind, depth: usize) -> Vec<RunEvent> {
    let end = events.iter().position(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::StepFinished(finished))
            if finished.kind == kind && finished.step.path.len() == depth)
    });
    let end = end.unwrap_or_else(|| panic!("no {kind:?} step finished"));
    events[..=end].to_vec()
}

fn sessions_started(events: &[RunEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(&event.kind, RunEventKind::Session(SessionEvent::Started(_))))
        .count()
}

async fn decide(
    crew: &support::Crew,
    events: &[RunEvent],
    kind: ApprovalKind,
    action: AnswerAction,
) -> anyhow::Result<Vec<RunEvent>> {
    let (command, reply) = support::answer(events, kind, action, None)?;
    let next = crew.resume(events.to_vec(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    Ok(next.events)
}

/// You abandon at the Understand approval, and the run pauses in its run review. Resumed, it ends
/// as abandoned instead of going on to build what you abandoned.
#[tokio::test]
async fn abandoned_run_resumed_in_its_run_review_stays_abandoned() -> anyhow::Result<()> {
    let mut crew = quiet_crew("captain")?;
    crew.settings.captain.understand_first = true;
    let worker = crew
        .services
        .workers
        .fake("claude-code/opus")
        .unwrap_or_else(|| panic!("no worker"));
    worker.push(argonauts_testkit::runner::FakeSession::structured(json!({
        "summary": "Fix the typo in the README.",
        "success_criteria": ["The README has no typo"],
        "in_scope": ["README.md"],
        "out_of_scope": [],
        "assumptions": [],
        "open_questions": []
    })));
    worker.push(argonauts_testkit::runner::FakeSession::structured(
        captain_report(),
    ));
    let waiting = crew.start(RunMode::Captain, "Fix the README typo").await?;
    assert!(
        matches!(waiting.exit, EngineExit::Waiting(_)),
        "{:?}",
        waiting.exit
    );
    let all = decide(
        &crew,
        &waiting.events,
        ApprovalKind::Understand,
        AnswerAction::Abandon,
    )
    .await?;
    let history = cut_after(&all, StepKind::Approval);
    assert_eq!(sessions_started(&history), 1, "only the reading ran");
    let resumed = crew.resume(history.clone(), Vec::new()).await?;
    assert_eq!(resumed.exit, EngineExit::Finished(RunOutcome::Abandoned));
    let added = &resumed.events[history.len()..];
    assert_eq!(sessions_started(added), 0);
    assert!(
        workflow_events(added)
            .iter()
            .all(|event| !matches!(event, WorkflowEvent::StepStarted(_))),
        "a step ran after you abandoned the run"
    );
    Ok(())
}

/// The run lands, then pauses in its run review. Resumed, it ends as merged, not with the branch
/// left.
#[tokio::test]
async fn merged_run_resumed_in_its_run_review_stays_merged() -> anyhow::Result<()> {
    let crew = quiet_crew("captain")?;
    let worker = crew
        .services
        .workers
        .fake("claude-code/opus")
        .unwrap_or_else(|| panic!("no worker"));
    worker.push(argonauts_testkit::runner::FakeSession::structured(
        captain_report(),
    ));
    let waiting = crew.start(RunMode::Captain, "Fix the README typo").await?;
    assert!(
        matches!(waiting.exit, EngineExit::Waiting(_)),
        "{:?}",
        waiting.exit
    );
    let all = decide(
        &crew,
        &waiting.events,
        ApprovalKind::Merge,
        AnswerAction::Merge,
    )
    .await?;
    let history = cut_after(&all, StepKind::Merge);
    let resumed = crew.resume(history.clone(), Vec::new()).await?;
    assert_eq!(resumed.exit, EngineExit::Finished(RunOutcome::Merged));
    assert_eq!(sessions_started(&resumed.events[history.len()..]), 0);
    Ok(())
}

/// One drafting agent: `map` skips a lone draft, so the drafts agree and the fast path takes it to
/// the plan approval.
#[tokio::test]
async fn council_plans_with_one_drafting_agent() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let outcomes = crew
        .run_to_end(RunMode::Crew, "add a --verbose flag", 1)
        .await?;
    let events: Vec<RunEvent> = outcomes
        .iter()
        .flat_map(|outcome| outcome.events.clone())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, RunEventKind::Plan(PlanEvent::FastPath { .. }))),
        "no fast path"
    );
    let skipped: Vec<(StepKind, StepEnd)> = workflow_events(&events)
        .iter()
        .filter_map(|event| match event {
            WorkflowEvent::StepFinished(finished)
                if matches!(finished.kind, StepKind::Synthesize | StepKind::Judge) =>
            {
                Some((finished.kind, finished.end))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        skipped,
        vec![
            (StepKind::Synthesize, StepEnd::Done),
            (StepKind::Judge, StepEnd::Skipped)
        ]
    );
    let last = outcomes.last().unwrap_or_else(|| panic!("no outcome"));
    assert_eq!(last.exit, EngineExit::Waiting(WaitReason::Approval));
    let opened = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(opened.kind),
        _ => None,
    });
    assert_eq!(opened, Some(ApprovalKind::Plan));
    Ok(())
}

/// A workflow that only sets settings has no phases of its own, so its run starts from the
/// default crew workflow's.
#[tokio::test]
async fn a_workflow_without_phases_starts_from_the_default_crew_workflow() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflow = "ablate-no-critique".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let outcomes = crew
        .run_to_end(RunMode::Crew, "add a --verbose flag", 1)
        .await?;
    let events: Vec<RunEvent> = outcomes
        .iter()
        .flat_map(|outcome| outcome.events.clone())
        .collect();
    let started = workflow_events(&events)
        .iter()
        .find_map(|event| match event {
            WorkflowEvent::Started(started) => Some(started.as_ref().clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no workflow started"));
    assert_eq!(
        started.from.as_ref().map(|from| from.as_str()),
        Some("council")
    );
    Ok(())
}

/// You abandon at the Understand approval inside the Understand loop, and the run crashes before
/// the loop records its end. Resumed, the loop takes the approval's ending from the log instead of
/// starting another round.
#[tokio::test]
async fn abandoned_inside_a_loop_stays_abandoned() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let waiting = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert!(
        matches!(waiting.exit, EngineExit::Waiting(_)),
        "{:?}",
        waiting.exit
    );
    let all = decide(
        &crew,
        &waiting.events,
        ApprovalKind::Understand,
        AnswerAction::Abandon,
    )
    .await?;
    let history = cut_after_at(&all, StepKind::Approval, 2);
    let resumed = crew.resume(history.clone(), Vec::new()).await?;
    assert_eq!(resumed.exit, EngineExit::Finished(RunOutcome::Abandoned));
    let added = &resumed.events[history.len()..];
    assert_eq!(sessions_started(added), 0);
    assert!(
        workflow_events(added)
            .iter()
            .all(|event| !matches!(event, WorkflowEvent::StepStarted(_))),
        "the Understand loop ran another round"
    );
    Ok(())
}
