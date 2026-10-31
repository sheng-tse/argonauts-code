//! A run that Claude Code's own workflow runs, built from the events such a run records: the
//! session that plans it, the workflow it reports, and the reviewer and sign-off it asks
//! through Argonauts Code. The view it gives is the one the terminal's test draws, in the same
//! view every workflow has.

use argonauts_core::TaskId;
use argonauts_core::app::views::{
    AgentRow, InnerAgent, RowStatus, StatusKind, WorkflowRow, WorkflowView,
};
use argonauts_core::base::names::{RoleName, SessionKey, WorkflowId};
use argonauts_core::runs::events::run::{RunLifecycle, RunNamed};
use argonauts_core::runs::events::session::{
    CliAgent, CliAgentState, CliPhase, CliWorkflowRun, EndReason, SessionEvent, SubagentStatus,
};
use argonauts_core::runs::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflows::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, Spend};
use serde_json::json;

use crate::client::test_support::{event, run_id, session_ended, session_started, usage};
use crate::views::{inner_agent_view, workflow_view};

/// The view the terminal's golden test draws, kept beside its golden file.
const DRAWN: &str = include_str!("../../../../../tui/tests/golden/own_workflow.view.json");

/// A session the run starts at `seq` for its one step, under `label`.
fn asked(seq: u64, session: &SessionKey, role: RoleName, worker: &str, label: &str) -> RunEvent {
    let mut started = session_started(seq, session, None, role, worker);
    if let RunEventKind::Session(SessionEvent::Started(start)) = &mut started.kind {
        start.worker = worker.parse().unwrap();
        start.workflow_step = Some(script_step());
        start.label = Some(label.to_owned());
    }
    started
}

fn script_step() -> StepRef {
    StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    }
}

fn cli_agent(index: u32, label: &str, phase: u32, state: CliAgentState) -> CliAgent {
    CliAgent {
        index,
        label: label.to_owned(),
        phase: Some(phase),
        model: Some("claude-haiku-4-5".to_owned()),
        state,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        started_at_ms: None,
        try_number: 1,
        prompt: None,
        result: None,
        ..CliAgent::default()
    }
}

/// The run: the build done in 1m 9s on 210k tokens, the reviewer asked for changes,
/// and the sign-off reading the change for the last 15s of the run's 1m 36s.
fn add_due_dates() -> Vec<RunEvent> {
    let host = SessionKey::numbered(1);
    let reviewer = SessionKey::numbered(2);
    let judge = SessionKey::numbered(3);
    let task = TaskId::new("task-add-due-dates").unwrap();
    let workflow: Workflow = serde_json::from_value(json!({
        "name": "add-due-dates",
        "phases": [{"name": "Work", "standard": "do", "steps": [{"kind": "script"}]}]
    }))
    .unwrap();
    let mut build = cli_agent(1, "build:add-due-dates", 1, CliAgentState::Done);
    (build.tokens, build.duration_ms) = (210_000, 69_000);
    (build.tool_calls, build.try_number) = (14, 2);
    build.prompt = Some("Add a due date to each task, and show it in the list.".to_owned());
    build.result = Some("Tasks take --due, and the list shows the date.".to_owned());
    let phases = [
        ("Build", "each agent on the model its role names"),
        ("Review", ""),
        ("Final", ""),
    ];
    let reported = CliWorkflowRun {
        id: "wf_due_dates".to_owned(),
        call: None,
        name: "add-due-dates".to_owned(),
        description: "Add due dates".to_owned(),
        phases: phases
            .iter()
            .zip(1..)
            .map(|((title, detail), index)| CliPhase {
                index,
                title: (*title).to_owned(),
                detail: (*detail).to_owned(),
            })
            .collect(),
        agents: vec![
            build,
            cli_agent(2, "review:due-dates", 2, CliAgentState::Done),
            cli_agent(3, "final:due-dates", 2, CliAgentState::Running),
        ],
        status: SubagentStatus::Running,
        tokens: 210_000,
        tool_calls: 0,
        duration_ms: 90_000,
        summary: None,
        started_at_ms: None,
    };
    let started = WorkflowStarted {
        id: WorkflowId::numbered(1),
        from: None,
        version: "v1".to_owned(),
        parent: None,
        workflow,
    };
    let step = StepStarted {
        step: script_step(),
        kind: StepKind::Script,
        standard: StandardPhase::Do,
        round: 1,
    };
    let mut planner = asked(
        2,
        &host,
        RoleName::Worker,
        "claude-code",
        "build:add-due-dates",
    );
    planner.task_id = Some(task);
    let mut review_ended = session_ended(80, &reviewer, EndReason::Completed, 21_000);
    if let RunEventKind::Session(SessionEvent::Ended(end)) = &mut review_ended.kind {
        end.structured = Some(json!({"accept": false, "summary": "Due dates can be in the past"}));
        end.spend = Spend {
            input_tokens: 16_000,
            output_tokens: 1_700,
            ..Spend::default()
        };
    }
    let mut workflow_reported = event(
        6,
        RunEventKind::Agent(AgentEvent::Workflow {
            workflow: Box::new(reported),
        }),
    );
    workflow_reported.session = Some(host.clone());
    vec![
        event(
            0,
            RunEventKind::Run(RunLifecycle::Named(RunNamed {
                name: "Add due dates".to_owned(),
                description: "Tasks get a due date, shown in the list".to_owned(),
                style: RunStyle::Workflow,
                based_on: Some("orchestrator".to_owned()),
                done_when: Vec::new(),
            })),
        ),
        event(
            0,
            RunEventKind::Workflow(WorkflowEvent::Started(Box::new(started))),
        ),
        event(1, RunEventKind::Workflow(WorkflowEvent::StepStarted(step))),
        planner,
        usage(5, &host, 200_000, 10_000, 0.05),
        workflow_reported,
        asked(
            59,
            &reviewer,
            RoleName::Reviewer,
            "codex/gpt-5.6-luna",
            "review:due-dates",
        ),
        usage(79, &reviewer, 16_000, 1_700, 0.01),
        review_ended,
        asked(
            81,
            &judge,
            RoleName::Judge,
            "codex/gpt-5.6-luna",
            "final:due-dates",
        ),
        usage(96, &judge, 35_000, 1_400, 0.02),
    ]
}

#[test]
fn the_runs_view_is_the_one_the_terminal_draws() {
    let events = add_due_dates();
    let view = workflow_view(&events, &run_id(), None).expect("the run's view");
    let shown = serde_json::to_string_pretty(&view).unwrap();
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP") {
        std::fs::write(
            std::path::Path::new(&dir).join("own_workflow.view.json"),
            &shown,
        )
        .unwrap();
    }
    let drawn: WorkflowView = serde_json::from_str(DRAWN).unwrap();
    assert_eq!(view, drawn, "{shown}");
}

/// Until Claude Code's workflow reports its phases the run's shape isn't known, so there is no
/// view of it: not Argonauts Code's own Work and Merge, and not the session writing it.
#[test]
fn nothing_is_drawn_until_claude_codes_workflow_names_its_phases() {
    let events = add_due_dates();
    let reported = events
        .iter()
        .position(|event| matches!(event.kind, RunEventKind::Agent(AgentEvent::Workflow { .. })))
        .expect("the workflow's report");
    let writing = &events[..reported];
    assert!(workflow_view(writing, &run_id(), None).is_none());
    assert!(crate::views::being_written(writing));
    let view = workflow_view(&events, &run_id(), None).expect("the run's view");
    assert!(!crate::views::being_written(&events));
    let phases: Vec<&str> = view
        .phases
        .iter()
        .map(|phase| phase.name.as_str())
        .collect();
    assert!(!phases.contains(&"Work"), "{phases:?}");
}

fn agents(view: &WorkflowView) -> Vec<&AgentRow> {
    view.phases
        .iter()
        .flat_map(|phase| &phase.rows)
        .filter_map(|row| match row {
            WorkflowRow::Agent(agent) => Some(agent),
            _ => None,
        })
        .collect()
}

/// Claude Code's own agent has no session of ours, so its row says which of the workflow's
/// agents it is, and that opens what Claude Code reports of it. The reviewer and the judge
/// it asked through Argonauts Code are sessions, and open as such.
#[test]
fn claude_codes_own_agent_opens_as_itself() {
    let events = add_due_dates();
    let view = workflow_view(&events, &run_id(), None).expect("the run's view");
    let rows = agents(&view);
    let labels: Vec<(&str, bool)> = rows
        .iter()
        .map(|row| (row.label.as_str(), row.inner.is_some()))
        .collect();
    assert_eq!(
        labels,
        [
            ("build:add-due-dates", true),
            ("review:due-dates", false),
            ("final:due-dates", false),
        ]
    );

    let build = rows[0];
    let inner = build.inner.clone().expect("which agent it is");
    assert_eq!(inner.index, 1);
    let opened = inner_agent_view(&events, &run_id(), &inner).expect("its own view");
    assert!(opened.row.is(build), "{:?}", opened.row);
    assert_eq!(opened.row.session, SessionKey::numbered(1));
    assert_eq!(opened.row.status, RowStatus::Done);
    assert_eq!(opened.row.worker.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(
        opened.prompt,
        "Add a due date to each task, and show it in the list."
    );
    assert_eq!(
        opened.outcome.as_deref(),
        Some("Tasks take --due, and the list shows the date.")
    );
    assert_eq!(opened.tool_calls, 14);
    assert_eq!(opened.facts, ["try 2"]);
    assert!(opened.activity.is_empty());

    // An agent the workflow never had, and a workflow the run never ran.
    let gone = InnerAgent {
        index: 9,
        ..inner.clone()
    };
    assert!(inner_agent_view(&events, &run_id(), &gone).is_none());
    let other = InnerAgent {
        workflow: WorkflowId::numbered(7),
        index: 1,
    };
    assert!(inner_agent_view(&events, &run_id(), &other).is_none());
}

/// Once the run merged, its end line tells the whole story: who built it with Claude Code's
/// own workflow, who reviewed it and who signed it off.
#[test]
fn a_merged_runs_end_line_names_who_did_its_work() {
    use argonauts_core::app::views::StatusKind;
    use argonauts_core::runs::events::run::RunOutcome;

    let mut events = add_due_dates();
    events.push(event(
        120,
        RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        }),
    ));
    let view = workflow_view(&events, &run_id(), None).expect("the run's view");
    let ended = view
        .status_lines
        .iter()
        .find(|line| line.kind == StatusKind::Ended)
        .expect("the end line");
    assert_eq!(
        ended.text,
        "Merged · Haiku 4.5 built it, GPT 5.6 Luna reviewed, GPT 5.6 Luna signed off"
    );
}

/// Claude Code's workflow asks for its review a second time under the same label, which the
/// crew labels by its round: that ask fills the second row of the label, and its own row is no
/// extra row beside them.
#[test]
fn a_review_asked_again_fills_the_row_of_its_round() {
    let mut events = add_due_dates();
    let again = SessionKey::numbered(4);
    for event in &mut events {
        if let RunEventKind::Agent(AgentEvent::Workflow { workflow }) = &mut event.kind {
            workflow
                .agents
                .push(cli_agent(4, "review:due-dates", 2, CliAgentState::Running));
        }
    }
    events.push(asked(
        97,
        &again,
        RoleName::Reviewer,
        "codex/gpt-5.6-luna",
        "review:due-dates · round 2",
    ));
    let view = workflow_view(&events, &run_id(), None).unwrap();
    let rows: Vec<(&str, &SessionKey)> = agents(&view)
        .iter()
        .map(|agent| (agent.label.as_str(), &agent.session))
        .collect();
    let reviews: Vec<&SessionKey> = rows
        .iter()
        .filter(|(label, _)| label.starts_with("review:"))
        .map(|(_, session)| *session)
        .collect();
    assert_eq!(
        reviews,
        [&SessionKey::numbered(2), &again],
        "each review row is its own round's: {rows:?}"
    );
}

/// The one count rule holds through the recorded Captain run, where Claude Code's own workflow
/// asks the crew's reviewers, some of them under labels none of its agents has: at every event
/// the header, the sum of the phases and the run's row in the bottom list say the same.
#[test]
fn the_header_the_phases_and_the_bottom_row_count_alike_through_a_recorded_run() {
    const RECORDED: &str =
        include_str!("../../../../../crew/tests/fixtures/runs/captain-review-loop.jsonl");
    let text = [
        ("{run}", "run-20261014-054425-0001"),
        ("{repo}", "/repo"),
        ("{run_dir}", "/state/run"),
        ("{worktrees}", "/worktrees"),
        ("{base_commit}", "eecdf983f2dfa422118f1a999438c399f3d446f8"),
        ("{branch}", "argonauts/run"),
        ("{item}", "money"),
    ]
    .iter()
    .fold(RECORDED.to_owned(), |text, (from, to)| {
        text.replace(from, to)
    });
    let events: Vec<RunEvent> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect();
    let run = events[0].run_id.clone();
    let wall = events.last().expect("events").ts;
    let mut state = argonauts_tui::frame::state::AppState::default();
    let mut unlike = Vec::new();
    let mut compared = 0;
    for at in 0..events.len() {
        state.apply(&argonauts_core::app::AppEvent::Run {
            event: Box::new(events[at].clone()),
        });
        let Some(view) = workflow_view(&events[..=at], &run, None) else {
            continue;
        };
        let rows = argonauts_tui::bridge::panel::run_rows(&state, wall, std::time::Instant::now());
        let Some(row) = rows.iter().find(|row| row.key == run.as_str()) else {
            continue;
        };
        let phases = view
            .phases
            .iter()
            .filter_map(|phase| phase.count.as_ref())
            .fold((0, 0), |(done, total), count| {
                (done + count.done, total + count.total)
            });
        let header = (view.agents_done, view.agents_total);
        // A row with no agent yet has nothing to count.
        let bottom = row.count.unwrap_or_default();
        compared += 1;
        if header != phases || header != bottom {
            unlike.push((events[at].seq, header, phases, bottom));
        }
    }
    assert!(compared > 20, "{compared} moments compared");
    assert!(
        unlike.is_empty(),
        "(seq, header, phases, bottom row): {unlike:?}"
    );
}

/// A council's script names the run in its first report: the view's header goes by that name,
/// drawn as words, and by what the script says it does, after the mode, never by a slug of your
/// words.
#[test]
fn a_council_goes_by_the_name_its_script_reports() {
    const RECORDED: &str =
        include_str!("../../../../../tui/tests/golden/transcript/resumed_council/events.jsonl");
    let events: Vec<RunEvent> = RECORDED
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect();
    let run = events[0].run_id.clone();
    let view = (0..events.len())
        .find_map(|at| workflow_view(&events[..=at], &run, None))
        .expect("the council's shape is known");
    assert_eq!(view.name, "Per key rate limit");
    assert_eq!(view.notes, ["Council"]);
    assert_eq!(
        view.description,
        "Per-API-key token buckets, refill capped at capacity, integer Retry-After on 429"
    );
}

/// The recorded council tried again and again, each try's proposer on agy finding
/// its model gone with nothing to fall back to. What a try noted, such as the first try's fallback
/// to opencode-acp, says nothing once the run goes on again, and the run that stopped says why
/// under its header, as its item in the conversation does.
#[test]
fn a_stopped_council_says_why_it_stopped_and_no_earlier_tries_note() {
    use argonauts_core::app::views::{StatusKind, Tone};

    const RECORDED: &str =
        include_str!("../../../../../tui/tests/golden/transcript/resumed_council/events.jsonl");
    let events: Vec<RunEvent> = RECORDED
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect();
    let run = events[0].run_id.clone();
    let said = |upto: usize| {
        let view = workflow_view(&events[..=upto], &run, None).expect("the council's view");
        view.status_lines
            .iter()
            .map(|line| (line.kind, line.tone, line.text.clone()))
            .collect::<Vec<_>>()
    };
    let resumed: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event.kind, RunEventKind::Run(RunLifecycle::Resumed(_))))
        .map(|(at, _)| at)
        .collect();
    assert!(resumed.len() >= 2, "the record holds later tries");
    for at in resumed {
        assert!(said(at).is_empty(), "{:?}", said(at));
    }
    assert_eq!(
        said(events.len() - 1),
        [(
            StatusKind::Ended,
            Tone::Bad,
            "Stopped · nobody to fall back to as the task's worker".to_owned()
        )]
    );
}

/// The Captain run where Claude Code's own workflow named its agents by round and asked the
/// crew under the bare labels, as it recorded itself, personal details taken out.
const CAPTAIN: &str =
    include_str!("../../../../../crew/tests/fixtures/runs/captain-own-phases.jsonl");

fn captain_events() -> Vec<RunEvent> {
    CAPTAIN
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect()
}

/// The run's events up to `at`, a moment of it, as `12:19:23.5`.
fn until(events: &[RunEvent], at: &str) -> Vec<RunEvent> {
    events
        .iter()
        .filter(|event| event.ts.time().to_string().as_str() <= at)
        .cloned()
        .collect()
}

/// The views the terminal's golden test of that run draws, kept beside its picture: the
/// workflow 57 s in while Gemini judges, the judge's view then, and the workflow once merged.
const CAPTAIN_DRAWN: [(&str, &str); 3] = [
    (
        "captain_after.judging.json",
        include_str!("../../../../../tui/tests/golden/captain_after.judging.json"),
    ),
    (
        "captain_after.judge.json",
        include_str!("../../../../../tui/tests/golden/captain_after.judge.json"),
    ),
    (
        "captain_after.done.json",
        include_str!("../../../../../tui/tests/golden/captain_after.done.json"),
    ),
];

/// Each of those views from the run's own events, as JSON, in the order of [`CAPTAIN_DRAWN`].
fn captain_views() -> [String; 3] {
    let events = captain_events();
    let run = events[0].run_id.clone();
    let judging = until(&events, "12:19:23.5");
    let view = workflow_view(&judging, &run, None).expect("the run's view while it judges");
    let judge = crate::views::agent_view(&judging, &run, &SessionKey::numbered(3))
        .expect("the judge's view");
    let done = workflow_view(&events, &run, None).expect("the run's view once merged");
    [
        serde_json::to_string_pretty(&view).unwrap(),
        serde_json::to_string_pretty(&judge).unwrap(),
        serde_json::to_string_pretty(&done).unwrap(),
    ]
}

/// The recorded Captain run's views are the ones the terminal's picture test draws.
#[test]
fn the_captain_runs_views_are_the_ones_its_picture_draws() {
    let shown = captain_views();
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP") {
        for ((name, _), json) in CAPTAIN_DRAWN.iter().zip(&shown) {
            std::fs::write(std::path::Path::new(&dir).join(name), json).unwrap();
        }
    }
    for ((name, drawn), json) in CAPTAIN_DRAWN.iter().zip(&shown) {
        let drawn: serde_json::Value = serde_json::from_str(drawn).unwrap();
        let json: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(json, drawn, "{name}");
    }
}

/// In that run the phases are exactly Claude Code's: Verify holds only its own agent, and
/// Review and Judge are the rows of the agents that asked, filled by who answered, Codex and
/// agy. Nothing of the run's own, no Merge, is a phase.
#[test]
fn a_captain_runs_asks_fill_the_rows_of_the_agents_that_asked() {
    let events = captain_events();
    let run = events[0].run_id.clone();
    let view = workflow_view(&events, &run, None).expect("the run's view");
    let names: Vec<&str> = view
        .phases
        .iter()
        .map(|phase| phase.name.as_str())
        .collect();
    assert_eq!(names, ["Build", "Review", "Judge", "Verify"]);
    let agents = |phase: usize| -> Vec<(String, String)> {
        view.phases[phase]
            .rows
            .iter()
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => {
                    Some((agent.label.clone(), agent.worker.agent.clone()))
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        agents(1),
        [("review:fix-the-money".to_owned(), "codex".to_owned())]
    );
    assert_eq!(
        agents(2),
        [(
            "sign-off:fix-the-money".to_owned(),
            "antigravity-cli".to_owned()
        )]
    );
    assert_eq!(
        agents(3),
        [("verify:tests".to_owned(), "claude-code".to_owned())]
    );
    assert_eq!((view.agents_done, view.agents_total), (4, 4));
    // While Gemini judged, Verify hadn't started and held nothing.
    let judging = workflow_view(&until(&events, "12:19:23.5"), &run, None).expect("the view");
    assert!(judging.phases[3].rows.is_empty(), "{:?}", judging.phases[3]);
    assert_eq!((judging.agents_done, judging.agents_total), (2, 3));
}

/// The line under the header follows the asks: the run's own note while Claude's agents build,
/// then, while the review is asked, who reviews whose change, and while the judge signs off,
/// the review's verdict and who signs off.
#[test]
fn a_captain_runs_line_follows_its_asks() {
    let events = captain_events();
    let run = events[0].run_id.clone();
    let line = |at: &str| {
        let view = workflow_view(&until(&events, at), &run, None).expect("the run's view");
        let notes: Vec<String> = view
            .status_lines
            .iter()
            .filter(|line| line.kind == StatusKind::Note)
            .map(|line| line.text.clone())
            .collect();
        notes.join("\n")
    };
    assert_eq!(
        line("12:18:50"),
        "fix-the-money-rounding · Claude Code's own workflow · reviews by GPT 6 Luna on Codex"
    );
    assert_eq!(
        line("12:19:00"),
        "review: GPT 6 Luna on Codex reviews Haiku 5.5's change"
    );
    assert_eq!(
        line("12:19:23.5"),
        "judge: GPT 6 Luna on Codex approved Haiku 5.5's change · Gemini 3.8 Flash Low on agy \
         signs off"
    );
}

/// The judge weighs the change with the review before it, here one that approved and found
/// nothing, so its view names that review all the same, with what it said. Its prompt's own
/// words start past the judge's frame, and the preview names what the rest holds.
#[test]
fn a_captain_runs_judge_is_given_the_review_that_approved() {
    let events = captain_events();
    let run = events[0].run_id.clone();
    let judging = until(&events, "12:19:23.5");
    let judge = crate::views::agent_view(&judging, &run, &SessionKey::numbered(3))
        .expect("the judge's view");
    let earlier = judge.earlier.expect("the review it was given");
    assert_eq!(
        (earlier.from.as_str(), earlier.round),
        ("review:fix-the-money", 1)
    );
    assert!(earlier.items.is_empty());
    let verdict = earlier.verdict.expect("what that review said");
    assert_eq!(
        (verdict.said.as_str(), verdict.by.to_string()),
        ("approved", "codex/gpt-6-luna".to_owned())
    );
    assert_eq!(
        verdict.summary.as_deref(),
        Some("The money parser and formatter changes address all three specified failures.")
    );
    let about = judge.prompt_about.expect("where its own words start");
    assert_eq!(
        judge
            .prompt
            .lines()
            .nth(about.first)
            .map(|line| &line[..15]),
        Some("Decide whether ")
    );
    assert_eq!(
        about.holds,
        [
            "your request",
            "the earlier review",
            "the test run",
            "the change"
        ]
    );
    let reviewer = crate::views::agent_view(&judging, &run, &SessionKey::numbered(2))
        .expect("the reviewer's view");
    assert_eq!(reviewer.earlier, None, "a first review has none before it");
}
