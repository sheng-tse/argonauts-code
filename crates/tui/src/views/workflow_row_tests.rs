//! The row at the bottom takes a workflow's name, its phase going on, and its agents from what
//! the workflow reports, and its goals from the goals the screen knows of. Like the view, it
//! holds no list of phases: one phase, twelve, phases that repeat, and nothing reported yet.

use std::time::{Duration, Instant};

use argonauts_core::agent_goal::{AgentGoal, GoalKeeper, GoalState};
use argonauts_core::events::session::{
    SubagentStatus, VendorAgent, VendorAgentState, VendorPhase, VendorWorkflow,
};
use argonauts_core::names::SessionKey;
use argonauts_core::views::conversation_run;
use time::OffsetDateTime;

use crate::bottom::{AgentPanel, WorkflowPanel, panel_lines};
use crate::bridge::panel::run_rows;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

fn reported(phases: &[&str], agents: &[(&str, u32, VendorAgentState)]) -> VendorWorkflow {
    VendorWorkflow {
        id: "wf-1".into(),
        call: None,
        name: "tidy-the-attic".into(),
        description: String::new(),
        phases: phases
            .iter()
            .enumerate()
            .map(|(i, title)| VendorPhase {
                index: i as u32 + 1,
                title: (*title).to_owned(),
                detail: String::new(),
            })
            .collect(),
        agents: agents
            .iter()
            .enumerate()
            .map(|(i, (label, phase, state))| VendorAgent {
                index: i as u32 + 1,
                label: (*label).to_owned(),
                phase: Some(*phase),
                model: None,
                state: *state,
                tokens: 1_000,
                tool_calls: 0,
                duration_ms: 0,
                started_at_ms: None,
                attempt: 1,
                prompt: None,
                result: None,
            })
            .collect(),
        status: SubagentStatus::Running,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        summary: None,
        started_at_ms: None,
    }
}

/// The one row the panel draws for the conversation's workflow, as text.
fn row(workflow: VendorWorkflow, goals: &[AgentGoal]) -> String {
    let mut state = AppState::default();
    state.chat.workflows.push(workflow);
    state.goals.remember(&conversation_run(), goals);
    let now = Instant::now();
    let rows = run_rows(&state, OffsetDateTime::now_utc(), now);
    assert_eq!(rows.len(), 1);
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(120, &theme, Clock::new(now, now + Duration::from_secs(1)));
    let panel = WorkflowPanel {
        rows,
        max_rows: 5,
        ..WorkflowPanel::default()
    };
    plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx))
}

fn goal(session: &str, state: GoalState) -> AgentGoal {
    AgentGoal {
        session: SessionKey::parse(session).unwrap(),
        label: "sort:boxes".into(),
        role: None,
        task: None,
        kept_by: GoalKeeper::Argonauts,
        kept_note: None,
        checked_by: "Argonauts".into(),
        state,
        stopped_because: None,
        set_ms_ago: None,
        lines: Vec::new(),
        tries: Vec::new(),
        counting: String::new(),
        max_tries: None,
        work_ms: 0,
        tokens: 0,
        checks: Vec::new(),
        checking: false,
        cli_says: None,
    }
}

#[test]
fn a_workflow_of_one_phase_names_that_phase() {
    let text = row(
        reported(
            &["Sort the boxes"],
            &[("sort:boxes", 1, VendorAgentState::Running)],
        ),
        &[],
    );
    assert!(text.contains("tidy-the-attic"), "{text}");
    assert!(text.contains("0/1 agents · Sort the boxes ·"), "{text}");
}

#[test]
fn the_twelfth_of_twelve_phases_is_named_when_it_is_the_one_going_on() {
    let names: Vec<String> = (1..=12).map(|n| format!("Shelf {n}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut agents: Vec<(&str, u32, VendorAgentState)> = (1..=11)
        .map(|phase| ("shelve", phase, VendorAgentState::Done))
        .collect();
    agents.push(("shelve:last", 12, VendorAgentState::Running));
    let text = row(reported(&names, &agents), &[]);
    assert!(text.contains("11/12 agents · Shelf 12 ·"), "{text}");
}

#[test]
fn of_two_phases_with_one_name_the_row_follows_the_one_going_on() {
    // "Sort" runs twice; the second is the fourth phase, and has its own agent.
    let text = row(
        reported(
            &["Sort", "Label", "Check", "Sort"],
            &[
                ("sort:first", 1, VendorAgentState::Done),
                ("label:all", 2, VendorAgentState::Done),
                ("check:all", 3, VendorAgentState::Done),
                ("sort:again", 4, VendorAgentState::Running),
            ],
        ),
        &[],
    );
    assert!(text.contains("3/4 agents · Sort ·"), "{text}");
    assert!(!text.contains("Label"), "{text}");
}

#[test]
fn a_workflow_that_has_reported_nothing_yet_shows_its_name_alone() {
    let text = row(reported(&[], &[]), &[]);
    assert!(text.contains("tidy-the-attic"), "{text}");
    assert!(!text.contains("agents"), "{text}");
    assert!(!text.contains("0/0"), "{text}");
}

#[test]
fn the_row_counts_the_goals_still_running() {
    let workflow = || {
        reported(
            &["Sort"],
            &[
                ("sort:a", 1, VendorAgentState::Running),
                ("sort:b", 1, VendorAgentState::Running),
                ("sort:c", 1, VendorAgentState::Done),
            ],
        )
    };
    let goals = [
        goal("s-001", GoalState::Running),
        goal("s-002", GoalState::Running),
        goal("s-003", GoalState::Met),
    ];
    let text = row(workflow(), &goals);
    assert!(
        text.contains("1/3 agents · Sort · 2 goals running ·"),
        "{text}"
    );
    let text = row(workflow(), &goals[1..]);
    assert!(text.contains("· Sort · 1 goal running ·"), "{text}");
    let text = row(workflow(), &goals[2..]);
    assert!(!text.contains("running"), "{text}");
}

/// A run of the kind the runner makes, its Work step going on, with the agent that wrote the
/// steps done and two builders seated. The run's own phases say only "Work, then Merge".
fn run_with_a_step_workflow() -> (
    AppState,
    argonauts_core::RunId,
    argonauts_core::workflow::StepRef,
) {
    use argonauts_core::names::WorkflowId;
    use argonauts_core::workflow::{StandardPhase, StepKind, StepRef};

    use crate::state::{SessionStatus, StepState, WorkflowState};

    let mut state = crate::mentions::fixture::working_state();
    let id = state.attached.clone().unwrap();
    let step = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    };
    let run = state.runs.get_mut(&id).unwrap();
    run.workflows.push(WorkflowState {
        id: WorkflowId::numbered(1),
        name: "script".into(),
        from: None,
        description: String::new(),
        phases: vec!["Work".into(), "Merge".into()],
        parent: None,
        steps: vec![StepState {
            step: step.clone(),
            kind: StepKind::Build,
            standard: StandardPhase::Do,
            round: 1,
            end: None,
            summary: None,
        }],
    });
    run.last_step = Some(step.clone());
    let seats = [
        (1, "plan:orchestrator", SessionStatus::Done),
        (2, "build:store", SessionStatus::Done),
        (3, "build:cli", SessionStatus::Working),
    ];
    for (n, label, status) in seats {
        let session = run.sessions.get_mut(&SessionKey::numbered(n)).unwrap();
        session.label = Some(label.into());
        session.workflow_step = Some(step.clone());
        session.status = status;
    }
    (state, id, step)
}

/// The run's own row, as text.
fn run_row_text(state: &AppState) -> String {
    let now = Instant::now();
    let rows = run_rows(state, OffsetDateTime::UNIX_EPOCH, now);
    assert_eq!(rows.len(), 1);
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(120, &theme, Clock::new(now, now + Duration::from_secs(1)));
    let panel = WorkflowPanel {
        rows,
        max_rows: 5,
        ..WorkflowPanel::default()
    };
    plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx))
}

/// A run whose step runs a workflow takes its phase going on and its agents from what that
/// workflow reports: the phase is the workflow's own, and agents still to start are counted.
#[test]
fn a_runs_row_follows_the_workflow_its_step_runs() {
    let (mut state, id, step) = run_with_a_step_workflow();
    // Before the workflow says anything, the run's own phase is all there is.
    let before = run_row_text(&state);
    assert!(before.contains("2/3 steps · Work ·"), "{before}");

    let workflow = reported(
        &["Build", "Review"],
        &[
            ("build:store", 1, VendorAgentState::Done),
            ("build:cli", 1, VendorAgentState::Running),
            ("build:docs", 1, VendorAgentState::Queued),
            ("review:store", 2, VendorAgentState::Queued),
        ],
    );
    state
        .runs
        .get_mut(&id)
        .unwrap()
        .workflow_reported(&step, &workflow);
    // The agent that wrote the steps, and the workflow's four: two done of five.
    let text = run_row_text(&state);
    assert!(text.contains("2/5 steps · Build ·"), "{text}");

    // A newer report replaces the last one, and the row moves on with it.
    let later = reported(
        &["Build", "Review"],
        &[
            ("build:store", 1, VendorAgentState::Done),
            ("build:cli", 1, VendorAgentState::Done),
            ("build:docs", 1, VendorAgentState::Done),
            ("review:store", 2, VendorAgentState::Running),
        ],
    );
    let run = state.runs.get_mut(&id).unwrap();
    run.workflow_reported(&step, &later);
    let text = run_row_text(&state);
    assert!(text.contains("4/5 steps · Review ·"), "{text}");
}

/// Twelve phases, two of one name, or none reported yet: the row names the one going on, and
/// before any agent has started it keeps the run's own phase.
#[test]
fn a_runs_row_names_whatever_phase_its_steps_workflow_is_in() {
    let (mut state, id, step) = run_with_a_step_workflow();
    let names: Vec<String> = (1..=12).map(|n| format!("Shelf {n}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let twelve = reported(&names, &[("shelve:last", 12, VendorAgentState::Running)]);
    state
        .runs
        .get_mut(&id)
        .unwrap()
        .workflow_reported(&step, &twelve);
    assert!(run_row_text(&state).contains("· Shelf 12 ·"));

    let twice = reported(
        &["Sort", "Label", "Sort"],
        &[
            ("sort:first", 1, VendorAgentState::Done),
            ("label:all", 2, VendorAgentState::Done),
            ("sort:again", 3, VendorAgentState::Queued),
        ],
    );
    let run = state.runs.get_mut(&id).unwrap();
    run.workflow_reported(&step, &twice);
    // Between two agents, the phase of the last one that started.
    let text = run_row_text(&state);
    assert!(text.contains("steps · Label ·"), "{text}");

    let nothing = reported(&[], &[]);
    let run = state.runs.get_mut(&id).unwrap();
    run.workflow_reported(&step, &nothing);
    let text = run_row_text(&state);
    assert!(text.contains("2/3 steps · Work ·"), "{text}");
}

/// The runner's own event is all the row needs: as it arrives with the run's other events,
/// the row takes the phase going on and the agents still to come from it.
#[test]
fn the_runners_report_event_reaches_the_runs_row() {
    use argonauts_core::app::AppEvent;
    use argonauts_core::events::workflow::{WorkflowEvent, WorkflowReport};
    use argonauts_core::events::{RunEvent, RunEventKind};

    let (mut state, id, step) = run_with_a_step_workflow();
    let workflow = reported(
        &["Build", "Review"],
        &[
            ("build:store", 1, VendorAgentState::Done),
            ("build:cli", 1, VendorAgentState::Running),
            ("review:store", 2, VendorAgentState::Queued),
        ],
    );
    state.apply(&AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 40,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: id,
            card_id: None,
            session: None,
            source: "conductor".into(),
            kind: RunEventKind::Workflow(WorkflowEvent::Reported(Box::new(WorkflowReport {
                step,
                workflow,
                sessions: Vec::new(),
                size_asked: None,
            }))),
        }),
    });
    let text = run_row_text(&state);
    assert!(text.contains("2/4 steps · Build ·"), "{text}");
}
