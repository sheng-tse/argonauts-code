//! A run that Claude Code's own workflow runs, built from the events such a run records: the
//! session that plans it, the workflow it reports, and the reviewer and sign-off it asks
//! through Argonauts Code. The view it gives is the one the terminal's test draws, in the same
//! view every workflow has.

use argonauts_core::TaskId;
use argonauts_core::events::run::{RunLifecycle, RunNamed};
use argonauts_core::events::session::{
    CliAgent, CliAgentState, CliPhase, CliWorkflowRun, EndReason, SessionEvent, SubagentStatus,
};
use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{RoleName, SessionKey, WorkflowId};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::views::{AgentRow, InnerAgent, RowStatus, WorkflowRow, WorkflowView};
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, Spend};
use serde_json::json;

use crate::client::test_support::{event, run_id, session_ended, session_started, usage};
use crate::views::{inner_agent_view, workflow_view};

/// The view the terminal's golden test draws, kept beside its golden file.
const DRAWN: &str = include_str!("../../../../tui/tests/golden/own_workflow.view.json");

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
