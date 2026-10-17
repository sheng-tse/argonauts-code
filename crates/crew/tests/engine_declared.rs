//! What a run declares: its name and description, its workflow's phases, and
//! each phase's start, end, or skip by the declared name, so the views draw only what runs.
//! And every agent's label.

mod support;

use argonauts_core::events::run::{RunLifecycle, RunMode, RunNamed, handed_over};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflow::WorkflowRef;

fn workflow_events(events: &[RunEvent]) -> Vec<&WorkflowEvent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(workflow) => Some(workflow),
            _ => None,
        })
        .collect()
}

fn named(events: &[RunEvent]) -> Vec<&RunNamed> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::Named(named)) => Some(named),
            _ => None,
        })
        .collect()
}

/// `(index, title)` of every phase mark of one kind, in order.
fn marks(events: &[RunEvent], kind: &str) -> Vec<(u32, String)> {
    workflow_events(events)
        .into_iter()
        .filter_map(|event| match (kind, event) {
            ("started", WorkflowEvent::PhaseStarted(mark)) => Some((mark.index, mark.name.clone())),
            ("ended", WorkflowEvent::PhaseEnded { phase, end }) if *end == StepEnd::Done => {
                Some((phase.index, phase.name.clone()))
            }
            ("skipped", WorkflowEvent::PhaseSkipped { phase, .. }) => {
                Some((phase.index, phase.name.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A request the main session handed over may say in its first words how to run it, or only
/// agree to it. The run is named and described from the session's account of the work, which
/// follows them.
#[tokio::test]
async fn a_handed_over_run_is_named_for_the_work() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    let request = handed_over(
        "Use a workflow for this, with Codex as the reviewer. Fix the money rounding.",
        "Fix the money rounding so the three failing tests pass.",
    );
    let outcome = crew.start(RunMode::Captain, &request).await?;
    let names = named(&outcome.events);
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(names[0].name, "fix-the-money-rounding");
    assert_eq!(
        names[0].description,
        "Fix the money rounding so the three failing tests pass."
    );
    Ok(())
}

#[tokio::test]
async fn a_captain_run_is_named_and_declares_only_what_it_runs() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    let outcome = crew
        .start(RunMode::Captain, "fix the typo in the readme")
        .await?;
    let events = &outcome.events;

    // Named before anything runs, from the request when intake never ran, and never on a
    // small word.
    let names = named(events);
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(names[0].name, "fix-the-typo");
    assert_eq!(names[0].description, "fix the typo in the readme");
    assert_eq!(names[0].style, RunStyle::Workflow);

    let workflow = workflow_events(events);
    let Some(WorkflowEvent::Started(started)) = workflow.first() else {
        panic!("no workflow started: {workflow:?}");
    };
    assert_eq!(started.from, Some(WorkflowRef::new("captain")));
    assert_eq!(started.workflow.name, "fix-the-typo");
    assert_eq!(started.workflow.based_on.as_deref(), Some("captain"));
    let Some(WorkflowEvent::Declared(declared)) = workflow.get(1) else {
        panic!("the workflow isn't declared first: {workflow:?}");
    };
    assert_eq!(declared.name, "fix-the-typo");
    let phases: Vec<&str> = declared.phases.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(phases, ["Understand", "Build", "Merge"]);
    assert!(declared.phases.iter().all(|phase| !phase.detail.is_empty()));

    // Your prompt is the goal, so Understand never runs, and says why.
    assert_eq!(marks(events, "skipped"), [(0, "Understand".to_owned())]);
    let why = workflow.iter().find_map(|event| match event {
        WorkflowEvent::PhaseSkipped { why, .. } => Some(why.as_str()),
        _ => None,
    });
    assert_eq!(why, Some("Your prompt is the goal"));
    let started_phases = marks(events, "started");
    assert_eq!(started_phases[0], (0, "Understand".to_owned()));
    assert!(
        started_phases.contains(&(1, "Build".to_owned())),
        "{started_phases:?}"
    );
    assert!(marks(events, "ended").contains(&(1, "Build".to_owned())));

    // Every agent has a label; the task's worker goes by its build label, not its task id.
    let labels: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => started.label.clone(),
            _ => None,
        })
        .collect();
    assert!(!labels.is_empty());
    assert!(
        labels.iter().any(|label| label.starts_with("build:")),
        "{labels:?}"
    );
    assert!(
        labels.iter().all(|label| !label.contains("task-")),
        "{labels:?}"
    );
    Ok(())
}

const LOOK_THEN_MAYBE: &str = "name: look-then-maybe\ndescription: A look, then one more if the \
    reviewers disagree\nphases:\n  - name: Look\n    detail: the checks\n    standard: check\n    \
    steps:\n      - kind: checks\n        label: \"look:checks\"\n  - name: Maybe\n    detail: only \
    when reviewers disagree\n    standard: check\n    steps:\n      - kind: checks\n        when: \
    reviewers_disagree\n";

#[tokio::test]
async fn a_phase_whose_steps_all_skip_is_skipped_with_its_reason() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.worker("claude-code/opus", "anthropic");
    crew.settings.workflow = "look-then-maybe".to_owned();
    crew.settings.workflows.orchestrator_designs = false.into();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.repo
        .write(".argonauts/workflows/look-then-maybe.yaml", LOOK_THEN_MAYBE)?;
    let outcome = crew.start(RunMode::Crew, "look at the repo").await?;
    let events = &outcome.events;
    // With no task to check, Look has nothing to do; Maybe's condition doesn't hold. Each says
    // why, and neither counts as started unless a step in it did.
    assert_eq!(
        marks(events, "skipped"),
        [(0, "Look".to_owned()), (1, "Maybe".to_owned())]
    );
    assert!(marks(events, "ended").is_empty());
    assert!(
        !marks(events, "started")
            .iter()
            .any(|(index, _)| *index == 1)
    );
    let reasons: Vec<String> = workflow_events(events)
        .into_iter()
        .filter_map(|event| match event {
            WorkflowEvent::PhaseSkipped { why, .. } => Some(why.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        [
            "no task to run the checks on",
            "reviewers_disagree doesn't hold"
        ]
    );
    Ok(())
}
