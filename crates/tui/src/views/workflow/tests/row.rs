//! The row at the bottom takes a workflow's name and its agents, done of all, from what the
//! workflow reports. Like the view, it holds no list of phases: one phase, twelve, phases that
//! repeat, and nothing reported yet. Which phase is going on is the view's to say.

use std::time::{Duration, Instant};

use argonauts_core::app::agent_goal::{AgentGoal, GoalKeeper, GoalState};
use argonauts_core::app::views::conversation_run;
use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::events::session::{
    CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
};
use time::OffsetDateTime;

use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};
use crate::bottom::{AgentPanel, WorkflowPanel, panel_lines};
use crate::bridge::panel::run_rows;
use crate::frame::state::AppState;

fn reported(phases: &[&str], agents: &[(&str, u32, CliAgentState)]) -> CliWorkflowRun {
    CliWorkflowRun {
        id: "wf-1".into(),
        call: None,
        name: "tidy-the-attic".into(),
        description: String::new(),
        phases: phases
            .iter()
            .enumerate()
            .map(|(i, title)| CliPhase {
                index: i as u32 + 1,
                title: (*title).to_owned(),
                detail: String::new(),
            })
            .collect(),
        agents: agents
            .iter()
            .enumerate()
            .map(|(i, (label, phase, state))| CliAgent {
                index: i as u32 + 1,
                label: (*label).to_owned(),
                phase: Some(*phase),
                model: None,
                state: *state,
                tokens: 1_000,
                tool_calls: 0,
                duration_ms: 0,
                started_at_ms: None,
                try_number: 1,
                prompt: None,
                result: None,
                ..CliAgent::default()
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
fn row(workflow: CliWorkflowRun, goals: &[AgentGoal]) -> String {
    let mut state = AppState::default();
    state.conversation.workflows.push(workflow);
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
        checked_by: "Argonauts Code".into(),
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
fn a_workflow_of_one_phase_counts_its_agents() {
    let text = row(
        reported(
            &["Sort the boxes"],
            &[("sort:boxes", 1, CliAgentState::Running)],
        ),
        &[],
    );
    assert!(text.contains("tidy-the-attic"), "{text}");
    assert!(text.contains("  0/1 · "), "{text}");
    assert!(!text.contains("Sort the boxes"), "{text}");
}

#[test]
fn twelve_phases_count_as_one_run_of_agents() {
    let names: Vec<String> = (1..=12).map(|n| format!("Shelf {n}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut agents: Vec<(&str, u32, CliAgentState)> = (1..=11)
        .map(|phase| ("shelve", phase, CliAgentState::Done))
        .collect();
    agents.push(("shelve:last", 12, CliAgentState::Running));
    let text = row(reported(&names, &agents), &[]);
    assert!(text.contains("  11/12 · "), "{text}");
}

#[test]
fn phases_that_repeat_count_their_agents_once() {
    // "Sort" runs twice; the second is the fourth phase, and has its own agent.
    let text = row(
        reported(
            &["Sort", "Label", "Check", "Sort"],
            &[
                ("sort:first", 1, CliAgentState::Done),
                ("label:all", 2, CliAgentState::Done),
                ("check:all", 3, CliAgentState::Done),
                ("sort:again", 4, CliAgentState::Running),
            ],
        ),
        &[],
    );
    assert!(text.contains("  3/4 · "), "{text}");
}

#[test]
fn a_workflow_that_has_reported_nothing_yet_shows_its_name_alone() {
    let text = row(reported(&[], &[]), &[]);
    assert!(text.contains("tidy-the-attic"), "{text}");
    assert!(!text.contains("agents"), "{text}");
    assert!(!text.contains("0/0"), "{text}");
}

/// The row counts its agents and says nothing of the goals they work toward, which their own
/// rows and views show.
#[test]
fn the_row_counts_its_agents_and_says_nothing_of_goals() {
    let workflow = || {
        reported(
            &["Sort"],
            &[
                ("sort:a", 1, CliAgentState::Running),
                ("sort:b", 1, CliAgentState::Running),
                ("sort:c", 1, CliAgentState::Done),
            ],
        )
    };
    let goals = [
        goal("s-001", GoalState::Running),
        goal("s-002", GoalState::Running),
        goal("s-003", GoalState::Met),
    ];
    let text = row(workflow(), &goals);
    assert!(text.contains("  1/3 · "), "{text}");
    assert!(!text.contains("goal"), "{text}");
}

/// A run of the kind the runner makes, its Work step going on, with the agent that wrote the
/// steps done and two builders at work. The run's own phases say only "Work, then Merge".
fn run_with_a_step_workflow() -> (
    AppState,
    argonauts_core::RunId,
    argonauts_core::workflows::workflow::StepRef,
) {
    use argonauts_core::base::names::WorkflowId;
    use argonauts_core::workflows::workflow::{StandardPhase, StepKind, StepRef};

    use crate::frame::state::{SessionStatus, StepState, WorkflowState};

    let mut state = crate::bridge::mentions::fixture::working_state();
    let id = state.attached.clone().unwrap();
    let step = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    };
    let run = state.runs.get_mut(&id).unwrap();
    // Its work isn't divided into tasks, so its row counts its agents.
    run.tasks.clear();
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
    let sessions = [
        (1, "plan:orchestrator", SessionStatus::Done),
        (2, "build:store", SessionStatus::Done),
        (3, "build:cli", SessionStatus::Working),
    ];
    for (n, label, status) in sessions {
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

/// A run whose step runs a workflow takes its agents from what that workflow reports, those
/// still to start counted too.
#[test]
fn a_runs_row_follows_the_workflow_its_step_runs() {
    let (mut state, id, step) = run_with_a_step_workflow();
    // Before the workflow says anything, the run's own agents are all there are.
    let before = run_row_text(&state);
    assert!(before.contains("  2/3 · "), "{before}");

    let workflow = reported(
        &["Build", "Review"],
        &[
            ("build:store", 1, CliAgentState::Done),
            ("build:cli", 1, CliAgentState::Running),
            ("build:docs", 1, CliAgentState::Queued),
            ("review:store", 2, CliAgentState::Queued),
        ],
    );
    state
        .runs
        .get_mut(&id)
        .unwrap()
        .workflow_reported(&step, &workflow);
    // The workflow's four are the row's agents, one of them done. The agent that wrote the
    // steps is none of them, as in the workflow's view.
    let text = run_row_text(&state);
    assert!(text.contains("  1/4 · "), "{text}");

    // A newer report replaces the last one, and the row moves on with it.
    let later = reported(
        &["Build", "Review"],
        &[
            ("build:store", 1, CliAgentState::Done),
            ("build:cli", 1, CliAgentState::Done),
            ("build:docs", 1, CliAgentState::Done),
            ("review:store", 2, CliAgentState::Running),
        ],
    );
    let run = state.runs.get_mut(&id).unwrap();
    run.workflow_reported(&step, &later);
    let text = run_row_text(&state);
    assert!(text.contains("  3/4 · "), "{text}");

    // A report that names nothing leaves the run's own agents.
    let nothing = reported(&[], &[]);
    let run = state.runs.get_mut(&id).unwrap();
    run.workflow_reported(&step, &nothing);
    let text = run_row_text(&state);
    assert!(text.contains("  2/3 · "), "{text}");
}

/// The runner's own event is all the row needs: as it arrives with the run's other events,
/// the row takes the agents still to come from it.
#[test]
fn the_runners_report_event_reaches_the_runs_row() {
    use argonauts_core::app::AppEvent;
    use argonauts_core::runs::events::workflow::{WorkflowEvent, WorkflowReport};
    use argonauts_core::runs::events::{RunEvent, RunEventKind};

    let (mut state, id, step) = run_with_a_step_workflow();
    let workflow = reported(
        &["Build", "Review"],
        &[
            ("build:store", 1, CliAgentState::Done),
            ("build:cli", 1, CliAgentState::Running),
            ("review:store", 2, CliAgentState::Queued),
        ],
    );
    state.apply(&AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 40,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: id,
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Workflow(WorkflowEvent::Reported(Box::new(WorkflowReport {
                step,
                workflow,
                sessions: Vec::new(),
                size_asked: None,
            }))),
        }),
    });
    // The workflow's three, one of them done: the agent that wrote the steps is none of them.
    let text = run_row_text(&state);
    assert!(text.contains("  1/3 · "), "{text}");
}

/// A run the screen holds, named as the pictures name theirs, that ended `ago` before `wall`
/// after `secs` of work: `total` agents, `done` of them done, and `tokens` written.
fn ended_run(
    outcome: argonauts_core::runs::events::run::RunOutcome,
    (done, total): (u32, u32),
    secs: i64,
    tokens: u64,
    (wall, ago): (OffsetDateTime, Duration),
) -> AppState {
    use argonauts_core::base::names::{ModelRef, RoleName, WorkflowId};
    use argonauts_core::runs::events::run::RunNamed;
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::workflows::workflow::StepRef;

    use crate::frame::state::{RunState, SessionState, SessionStatus};

    let id = argonauts_core::RunId::new("run-20261010-091503-3fa2").unwrap();
    let mut run = RunState::new(id.clone());
    run.named = Some(RunNamed {
        name: "Add due dates".to_owned(),
        description: String::new(),
        style: RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    let ended = wall - ago;
    run.started = Some(ended - time::Duration::seconds(secs));
    run.ended = Some(ended);
    run.finished = Some(outcome);
    run.merged_onto = Some("main".to_owned());
    run.spent.output_tokens = tokens;
    let worker: ModelRef = "claude-code/claude-sonnet-5-5".parse().unwrap();
    for n in 1..=total {
        let key = SessionKey::numbered(n);
        let mut session = SessionState::new(
            key.clone(),
            worker.clone(),
            "claude-code",
            RoleName::Worker,
            ended,
        );
        session.workflow_step = Some(StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        });
        session.status = if n <= done {
            SessionStatus::Done
        } else {
            SessionStatus::Stopped
        };
        run.session_order.push(key.clone());
        run.sessions.insert(key, session);
    }
    let mut state = AppState::default();
    state.attached = Some(id.clone());
    state.runs.insert(id, run);
    state
}

/// A run that ended keeps its row, its mark saying how it ended as the pictures' rows do:
/// merged, and stopped. The row goes when its time is up, the same time Claude Code keeps a
/// finished workflow listed, and a run of an earlier session has none.
#[test]
fn a_run_that_ended_keeps_its_row_for_a_while() {
    use argonauts_core::runs::events::run::RunOutcome;

    use crate::bridge::panel::KEPT_AFTER_END;
    use crate::testing::pictures::workflow_pictures::dark;

    let theme = dark();
    let now = Instant::now();
    let ctx = RenderCtx::new(124, &theme, Clock::still(now));
    let wall = OffsetDateTime::UNIX_EPOCH + time::Duration::days(400);
    let ago = Duration::from_secs(5);
    let cases = [
        (RunOutcome::Merged, (6, 6), 380, 177_000, "  ✔  ", None),
        (RunOutcome::Stopped, (3, 6), 302, 140_000, "❯ ×  ", Some(0)),
    ];
    for (outcome, (done, all), secs, tokens, mark, selected) in cases {
        let state = ended_run(outcome, (done, all), secs, tokens, (wall, ago));
        let rows = run_rows(&state, wall, now);
        assert_eq!(rows.len(), 1, "{outcome:?}");
        assert_eq!(rows[0].leaves_at, Some(now + KEPT_AFTER_END - ago));
        let mut panel = WorkflowPanel {
            rows,
            max_rows: 5,
            selected,
            ..WorkflowPanel::default()
        };
        let agents = AgentPanel::default();
        let text = plain_text(&panel_lines(&panel, &agents, &ctx));
        assert!(text.starts_with(mark), "{outcome:?}: {text}");
        assert!(text.contains(&format!("  {done}/{all} · ")), "{text}");

        // The screen wakes when the row's time is up, and lets it go then, not before.
        let leaves = now + KEPT_AFTER_END - ago;
        assert_eq!(panel.next_change(&agents, &ctx.clock, &theme), Some(leaves));
        assert!(!panel.let_go(&agents, leaves - Duration::from_millis(1)));
        assert!(panel.let_go(&agents, leaves));
        assert!(panel.rows.is_empty());
        assert_eq!(panel.selected, None);

        // Rows built again after that hold none of it.
        let later = wall + time::Duration::seconds(26);
        assert!(run_rows(&state, later, now).is_empty(), "{outcome:?}");
    }

    // A run that ended before this screen started, as a resumed conversation's did.
    let old = Duration::from_secs(3 * 24 * 3600);
    let state = ended_run(RunOutcome::Merged, (6, 6), 380, 177_000, (wall, old));
    assert!(run_rows(&state, wall, now).is_empty());
}
