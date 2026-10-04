//! What a run declares: its name and description, its workflow's phases, and
//! each phase's start, end, or skip by the declared title, so the views draw only what runs.
//! And every agent's label.

mod support;

use argonauts_core::events::run::{RunLifecycle, RunMode, RunNamed};
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
            ("started", WorkflowEvent::PhaseStarted(mark)) => {
                Some((mark.index, mark.title.clone()))
            }
            ("ended", WorkflowEvent::PhaseEnded { phase, end }) if *end == StepEnd::Done => {
                Some((phase.index, phase.title.clone()))
            }
            ("skipped", WorkflowEvent::PhaseSkipped { phase, .. }) => {
                Some((phase.index, phase.title.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_solo_run_is_named_and_declares_only_what_it_runs() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    let outcome = crew
        .start(RunMode::Solo, "fix the typo in the readme")
        .await?;
    let events = &outcome.events;

    // Named before anything runs, from the request when intake never ran.
    let names = named(events);
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(names[0].name, "fix-the-typo-in");
    assert_eq!(names[0].description, "fix the typo in the readme");
    assert_eq!(names[0].style, RunStyle::Workflow);

    let workflow = workflow_events(events);
    let Some(WorkflowEvent::Started(started)) = workflow.first() else {
        panic!("no workflow started: {workflow:?}");
    };
    assert_eq!(started.from, Some(WorkflowRef::new("solo")));
    assert_eq!(started.workflow.name, "fix-the-typo-in");
    assert_eq!(started.workflow.based_on.as_deref(), Some("solo"));
    let Some(WorkflowEvent::Declared(declared)) = workflow.get(1) else {
        panic!("the workflow isn't declared first: {workflow:?}");
    };
    assert_eq!(declared.name, "fix-the-typo-in");
    let titles: Vec<&str> = declared.phases.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Understand", "Build", "Merge"]);
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

    // Every agent has a label; the card's owner goes by its build label, not its card id.
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
        labels.iter().all(|label| !label.contains("card-")),
        "{labels:?}"
    );
    Ok(())
}

const LOOK_THEN_MAYBE: &str = "name: look-then-maybe\ndescription: A look, then one more if the \
    reviewers disagree\nphases:\n  - title: Look\n    detail: the gates\n    standard: check\n    \
    steps:\n      - kind: gate\n        label: \"look:gates\"\n  - title: Maybe\n    detail: only \
    when reviewers disagree\n    standard: check\n    steps:\n      - kind: gate\n        when: \
    reviewers_disagree\n";

#[tokio::test]
async fn a_phase_whose_steps_all_skip_is_skipped_with_its_reason() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    crew.worker("claude-code/opus", "anthropic");
    crew.settings.workflow = WorkflowRef::new("look-then-maybe");
    crew.settings.workflows.lead_designs = false;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.repo
        .write(".argonauts/workflows/look-then-maybe.yaml", LOOK_THEN_MAYBE)?;
    let outcome = crew.start(RunMode::Crew, "look at the repo").await?;
    let events = &outcome.events;
    // With no card to gate, Look has nothing to do; Maybe's condition doesn't hold. Each says
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
            "no card to run the gates on",
            "reviewers_disagree doesn't hold"
        ]
    );
    Ok(())
}
