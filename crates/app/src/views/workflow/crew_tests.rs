//! What the crew's engine records, drawn by the workflow view: a workflow an agent's own CLI
//! ran sits under that agent by a name no other run or workflow has, and a step tried again
//! is one row with its earlier try beneath, which opens on the try it stands for.

use std::sync::Arc;

use argonauts_core::engine::RepoContext;
use argonauts_core::events::run::{RunLifecycle, RunNamed};
use argonauts_core::events::session::{CliWorkflowRun, EndReason, SessionEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::RunnerEvent;
use argonauts_core::settings::Settings;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::StepName;
use argonauts_core::views::{AgentRow, RowStatus, WorkflowRow};
use argonauts_core::{AgentEvent, ErrorCategory, RepoId, Spend, TaskId};
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::labels::{Asking, session_label};
use argonauts_crew::state::RunState;
use argonauts_testkit::services::{FakeServices, engine_context_in};

use crate::client::test_support::{event, run_id, session_ended, session_started};
use crate::views::sessions::RunFacts;

fn named(seq: u64, name: &str) -> RunEvent {
    event(
        seq,
        RunEventKind::Run(RunLifecycle::Named(RunNamed {
            name: name.to_owned(),
            description: String::new(),
            style: RunStyle::Workflow,
            based_on: None,
            done_when: Vec::new(),
        })),
    )
}

fn row(session: &SessionKey, label: &str, worker: &ModelRef, status: RowStatus) -> AgentRow {
    AgentRow {
        session: session.clone(),
        label: label.to_owned(),
        worker: worker.clone(),
        status,
        tokens: 0,
        spent: Spend::default(),
        elapsed_ms: 0,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    }
}

/// The run is named `add-due-dates`, and so is the workflow Claude Code runs inside its agent,
/// as its script named it. The crew's host records it under the agent's session with a number
/// added, and the view draws it beneath that agent's row.
#[tokio::test]
async fn a_workflow_an_agent_ran_sits_under_it_by_a_name_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repo = RepoContext {
        id: RepoId::from_canonical_path(&root),
        run_dir: dir
            .path()
            .join("state")
            .join("repo")
            .join("runs")
            .join("run"),
        worktrees: dir.path().join("worktrees"),
        base_worktree: dir.path().join("worktrees").join("base"),
        root,
    };
    let agent = SessionKey::numbered(1);
    let history = vec![
        named(1, "add-due-dates"),
        session_started(2, &agent, None, RoleName::Worker, "claude-code"),
    ];
    let services = FakeServices::new();
    let (context, sink, _commands) =
        engine_context_in(&services, Settings::default(), repo, history.clone());
    let shared = Shared::new(&context, RunState::fold(&history)).unwrap();
    let host = Host::new(Arc::new(shared));
    let workflow: CliWorkflowRun = serde_json::from_value(serde_json::json!({
        "id": "wf_1",
        "name": "add-due-dates",
        "description": "",
        "phases": [],
        "agents": [],
        "status": "running",
        "tokens": 0,
        "tool_calls": 0,
        "duration_ms": 0,
    }))
    .unwrap();
    host.record_worker_event(
        "claude-code",
        &agent,
        None,
        RunnerEvent::Agent(AgentEvent::Workflow {
            workflow: Box::new(workflow),
        }),
    )
    .unwrap();

    let log = super::WorkflowLog::of(&sink.events());
    let claude = ModelRef::new("claude-code", None);
    let rows = super::inner::under_agents(
        vec![WorkflowRow::Agent(row(
            &agent,
            "build:add-due-dates",
            &claude,
            RowStatus::Running,
        ))],
        &log,
        None,
    );
    let [WorkflowRow::Agent(_), WorkflowRow::Workflow(nested)] = rows.as_slice() else {
        panic!("the agent's row, then its workflow's: {rows:#?}");
    };
    assert_eq!(nested.parent.as_ref(), Some(&agent));
    assert_eq!(nested.name, "add-due-dates-2", "never the run's own name");
}

/// A reviewer that fails on a task falls back to another agent. The crew labels the second
/// review as the first, and the view folds the failed one beneath it, a step to one row.
#[test]
fn a_review_that_fell_back_is_one_row_with_the_failed_try_beneath() {
    let task = TaskId::new("t1").unwrap();
    let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
    let codex = ModelRef::new("codex", None);
    let claude = ModelRef::new("claude-code", None);
    let asking = |key, worker| Asking {
        key,
        step: None,
        round: 1,
        role: RoleName::Reviewer,
        step_name: Some(StepName::Review),
        task: Some(&task),
        item: Some("due-dates"),
        worker,
        provider: "any",
    };
    let started = |seq, key: &SessionKey, worker: &str, label: &str| {
        let mut started = session_started(seq, key, Some(&task), RoleName::Reviewer, worker);
        if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
            session.step = Some(StepName::Review);
            session.label = Some(label.to_owned());
        }
        started
    };

    let label = session_label(&RunState::fold(&[]), &asking(&first, &codex));
    let mut events = vec![
        started(1, &first, "codex", &label),
        session_ended(
            2,
            &first,
            EndReason::CliError {
                category: ErrorCategory::InvalidRequest,
                message: "Invalid schema for response_format".to_owned(),
            },
            2_000,
        ),
    ];
    let again = session_label(&RunState::fold(&events), &asking(&second, &claude));
    assert_eq!(again, label, "the fallback keeps the step's label");
    events.push(started(3, &second, "claude-code", &again));

    let mut fallback = row(&second, &again, &claude, RowStatus::Done);
    fallback.fallback_for = Some(codex.clone());
    let rows = vec![
        WorkflowRow::Agent(row(&first, &label, &codex, RowStatus::Failed)),
        WorkflowRow::Agent(fallback),
    ];
    let (rows, folded) = super::tries::fold(rows, &RunFacts::of(&events));
    let [WorkflowRow::Agent(step)] = rows.as_slice() else {
        panic!("one row for the step: {rows:#?}");
    };
    assert_eq!(step.session, second);
    assert_eq!(step.tries.len(), 1);
    assert_eq!(step.tries[0].session, first);
    assert_eq!(folded.tries, 1);
}

/// A step with no task goes again on its own session: the crew starts the same key once more,
/// under the label it had, with the same agent or its fallback. Each start is a row
/// the view folds, and each try says why its own start ended.
#[test]
fn a_session_that_went_again_is_one_row_and_each_try_says_why_it_ended() {
    let key = SessionKey::numbered(1);
    let codex = ModelRef::new("codex", None);
    let asking = |worker| Asking {
        key: &key,
        step: None,
        round: 1,
        role: RoleName::TieBreaker,
        step_name: Some(StepName::DesignWorkflow),
        task: None,
        item: None,
        worker,
        provider: "any",
    };
    let label = session_label(&RunState::fold(&[]), &asking(&codex));
    let started = |seq, worker: &str| {
        let mut started = session_started(seq, &key, None, RoleName::TieBreaker, worker);
        if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
            session.step = Some(StepName::DesignWorkflow);
            session.label = Some(label.clone());
        }
        started
    };
    let refused = EndReason::CliError {
        category: ErrorCategory::InvalidRequest,
        message: "Invalid schema for response_format".to_owned(),
    };
    let mut events = vec![
        started(1, "codex"),
        session_ended(2, &key, EndReason::RateLimited, 1_000),
        started(3, "codex"),
        session_ended(4, &key, refused.clone(), 2_000),
    ];
    let claude = ModelRef::new("claude-code", None);
    let again = session_label(&RunState::fold(&events), &asking(&claude));
    assert_eq!(
        again, label,
        "the session keeps its label when it goes again"
    );
    events.push(started(5, "claude-code"));

    let facts = RunFacts::of(&events);
    let rows = facts
        .sessions
        .iter()
        .map(|session| WorkflowRow::Agent(facts.agent_row(session)))
        .collect();
    let (rows, folded) = super::tries::fold(rows, &facts);
    let [WorkflowRow::Agent(step)] = rows.as_slice() else {
        panic!("one row for the step: {rows:#?}");
    };
    assert_eq!(step.worker, claude);
    assert_eq!(step.status, RowStatus::Running);
    let reasons: Vec<Option<&EndReason>> = step
        .tries
        .iter()
        .map(|tried| tried.reason.as_ref())
        .collect();
    assert_eq!(reasons, [Some(&EndReason::RateLimited), Some(&refused)]);
    assert_eq!(folded.tries, 2);
}

/// The step's row stands for the session's newest start, so opening it shows that start: the
/// fallback that is at work, never the try that failed before it.
#[test]
fn a_session_that_went_again_opens_on_its_newest_start() {
    let key = SessionKey::numbered(1);
    let events = vec![
        session_started(1, &key, None, RoleName::TieBreaker, "codex"),
        session_ended(2, &key, EndReason::RateLimited, 1_000),
        session_started(3, &key, None, RoleName::TieBreaker, "claude-code"),
    ];
    let view =
        crate::views::agent::agent_view(&events, &run_id(), &key).expect("the session opens");
    assert_eq!(view.row.worker, ModelRef::new("claude-code", None));
    assert_eq!(view.row.status, RowStatus::Running);
    assert!(
        view.outcome.is_none(),
        "the failed try's end isn't this one's"
    );
}
