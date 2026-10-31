//! A request's tasks under its row, and under a task at work whatever does its work, each
//! counting done of total for what sits under it.

use argonauts_core::base::names::{ApprovalId, ModelRef, SessionKey};
use argonauts_core::look::logos::LogoMark;
use argonauts_core::runs::events::approval::{
    ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, QuestionPacket,
};
use argonauts_core::runs::events::task::{TaskWay, Way};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::{StepRef, WorkflowRef};
use argonauts_core::{RunId, TodoItem};

use super::*;
use crate::frame::state::OpenApproval;

const T0: OffsetDateTime = time::macros::datetime!(2026-10-14 10:41 UTC);

fn task_id(n: u32) -> TaskId {
    TaskId::new(format!("task-{n:03}")).unwrap()
}

fn model(name: &str) -> ModelRef {
    name.parse().unwrap()
}

/// A request at work, twelve minutes in, with its workflow begun so its row shows.
fn request() -> RunState {
    let mut run = RunState::new(RunId::new("run-due").unwrap());
    run.title = "add due dates to the tasks".into();
    run.named = Some(argonauts_core::runs::events::run::RunNamed {
        name: "Add due dates".into(),
        description: String::new(),
        style: argonauts_core::settings::workflows::RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    run.started = Some(T0 - time::Duration::minutes(12));
    run.workflows.push(crate::frame::state::WorkflowState {
        id: argonauts_core::base::names::WorkflowId::numbered(1),
        name: "orchestrator".into(),
        from: Some("orchestrator".into()),
        description: String::new(),
        phases: vec!["Build".into()],
        parent: None,
        steps: Vec::new(),
    });
    run
}

fn task(run: &mut RunState, n: u32, title: &str, way: Way, state: TaskState, worker: &str) {
    let mut row = TaskRow::new(task_id(n), title);
    row.state = Some(state);
    row.way = Some(TaskWay {
        how: way,
        asked_by_you: false,
        steps: None,
    });
    row.worker = Some(model(worker));
    run.tasks.insert(task_id(n), row);
}

/// A session of task `task`, started `minutes` before now.
fn session(
    run: &mut RunState,
    n: u32,
    task: u32,
    (worker, step): (&str, Option<StepName>),
    minutes: i64,
) -> SessionKey {
    let worker = model(worker);
    let agent = worker.agent.clone();
    let key = SessionKey::numbered(n);
    let mut session = SessionState::new(
        key.clone(),
        worker,
        agent,
        RoleName::Worker,
        T0 - time::Duration::minutes(minutes),
    );
    session.task = Some(task_id(task));
    session.step = step;
    session.status = SessionStatus::Working;
    run.session_order.push(key.clone());
    run.sessions.insert(key.clone(), session);
    key
}

fn rows_of(run: RunState) -> Vec<WorkflowRow> {
    let mut state = AppState::default();
    state.runs.insert(run.id.clone(), run);
    run_rows(&state, T0, Instant::now())
}

/// What each row says, as `depth name · way · count`.
fn shape(rows: &[WorkflowRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let count = row
                .count
                .map_or_else(|| "-".to_owned(), |(done, all)| format!("{done}/{all}"));
            let way = row.way.as_deref().unwrap_or("-");
            format!("{} {} · {way} · {count}", row.depth, row.name)
        })
        .collect()
}

/// One task of each way, as a request lists them: its own session counting its to-do list, a
/// workflow's two steps, and three agents.
fn three_ways() -> RunState {
    let mut run = request();
    task(
        &mut run,
        1,
        "Due date field",
        Way::OwnSession,
        TaskState::Merged,
        "claude-code/claude-sonnet-5-5",
    );
    run.tasks.get_mut(&task_id(1)).unwrap().todos = Some((5, 5));
    task(
        &mut run,
        2,
        "Date cases",
        Way::Workflow {
            name: WorkflowRef::new("maker-checker"),
        },
        TaskState::Running,
        "codex/gpt-5.6-luna",
    );
    task(
        &mut run,
        3,
        "Reminder emails",
        Way::Agents { count: 3 },
        TaskState::Running,
        "opencode/qwen3-coder:30b",
    );
    let done = session(
        &mut run,
        1,
        1,
        ("claude-code/claude-sonnet-5-5", Some(StepName::Build)),
        11,
    );
    let first = run.sessions.get_mut(&done).unwrap();
    first.status = SessionStatus::Done;
    first.ended = Some(T0 - time::Duration::minutes(7));
    first.tokens_out = 194_000;
    let build = session(
        &mut run,
        2,
        2,
        ("codex/gpt-5.6-luna", Some(StepName::Build)),
        6,
    );
    let build = run.sessions.get_mut(&build).unwrap();
    build.status = SessionStatus::Done;
    build.ended = Some(T0 - time::Duration::minutes(3));
    build.tokens_out = 380_000;
    let review = session(
        &mut run,
        3,
        2,
        ("claude-code/claude-sonnet-5-5", Some(StepName::Review)),
        3,
    );
    run.sessions.get_mut(&review).unwrap().tokens_out = 232_000;
    for (n, label) in [(4, "build:templates"), (5, "build:scheduler")] {
        let key = session(&mut run, n, 3, ("opencode/qwen3-coder:30b", None), 4);
        run.sessions.get_mut(&key).unwrap().label = Some(label.into());
    }
    run
}

#[test]
fn a_request_lists_its_tasks_and_what_does_their_work() {
    let rows = rows_of(three_ways());
    assert_eq!(
        shape(&rows),
        [
            "0 Add due dates · - · 1/3",
            "1 Due date field · Own session · 5/5",
            "1 Date cases · Maker-checker · 1/2",
            "2 Build · - · -",
            "2 Review · - · -",
            "1 Reminder emails · 3 agents · 0/3",
            "2 Templates · - · -",
            "2 Scheduler · - · -",
        ]
    );
    let kinds: Vec<RunRowKind> = rows.iter().map(|row| row.kind).collect();
    use RunRowKind::{Request, Task, Work};
    assert_eq!(kinds, [Request, Task, Task, Work, Work, Task, Work, Work]);
    // Tasks take the run marks whatever their way, and what does their work the agents'.
    let states: Vec<WorkflowRowState> = rows.iter().map(|row| row.state).collect();
    use WorkflowRowState::{Done, Running};
    assert_eq!(
        states,
        [
            Running, Done, Running, Done, Running, Running, Running, Running
        ]
    );
    // A row that is one session opens it, and a step or agent says what it is doing.
    assert_eq!(
        rows[1].session.as_ref().map(|agent| agent.session.as_str()),
        Some("s-001")
    );
    assert_eq!(rows[2].session, None);
    assert_eq!(
        rows[4].session.as_ref().map(|agent| agent.session.as_str()),
        Some("s-003")
    );
    assert!(rows[4].doing.is_some());
    // Each row shows who serves its model.
    let marks: Vec<Option<LogoMark>> = rows
        .iter()
        .map(|row| row.logo.as_ref().and_then(|logo| logo.mark))
        .collect();
    assert_eq!(marks[0], None);
    assert_eq!(marks[1], Some(LogoMark::ClaudeCode));
    assert_eq!(marks[2], Some(LogoMark::Codex));
    // Every row's tokens are the output of the sessions under it.
    assert_eq!(rows[2].tokens, 612_000);
    assert_eq!(rows[3].tokens, 380_000);
    assert_eq!(rows[0].tokens, 806_000);
    // A task's time runs from its first session's start to its last one's end, or to now.
    assert_eq!(rows[1].elapsed_ms, 4 * 60_000);
    assert_eq!(rows[1].measured_at, None);
    assert_eq!(rows[2].elapsed_ms, 6 * 60_000);
    assert!(rows[2].measured_at.is_some());
}

/// A task in its own session counts its session's to-do list, as the crew counted it or, before
/// it did, as the session reports it. With no list it has no count, and says what it is doing.
#[test]
fn an_own_session_task_counts_its_to_do_list_or_says_what_it_does() {
    let mut run = request();
    task(
        &mut run,
        1,
        "Sample data",
        Way::OwnSession,
        TaskState::Running,
        "antigravity-cli/gemini-3.8-flash",
    );
    let key = session(
        &mut run,
        1,
        1,
        ("antigravity-cli/gemini-3.8-flash", Some(StepName::Build)),
        3,
    );
    let rows = rows_of(run.clone());
    assert_eq!(rows[1].count, None);
    assert!(rows[1].doing.is_some(), "{:?}", rows[1]);

    let todo = |text: &str, status| TodoItem {
        text: text.into(),
        status,
    };
    run.sessions.get_mut(&key).unwrap().todos = vec![
        todo("Write the seed", TodoStatus::Completed),
        todo("Run it", TodoStatus::InProgress),
    ];
    assert_eq!(rows_of(run.clone())[1].count, Some((1, 2)));
    run.tasks.get_mut(&task_id(1)).unwrap().todos = Some((3, 4));
    let rows = rows_of(run);
    assert_eq!(rows[1].count, Some((3, 4)));
    assert_eq!(rows[1].doing, None);
}

fn asked(task: Option<u32>, needs_you: bool) -> OpenApproval {
    OpenApproval {
        opened: Box::new(ApprovalOpened {
            id: ApprovalId::numbered(1),
            kind: ApprovalKind::Question,
            title: "Which routes need a login?".into(),
            task: task.map(task_id),
            packet: ApprovalPacket::Question(QuestionPacket {
                from: model("codex/gpt-5.6-luna"),
                task: task_id(task.unwrap_or(1)),
                text: "Which routes need a login?".into(),
                options: Vec::new(),
            }),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            when_declined: WhenDeclined::Decide,
            options: Vec::new(),
            free_text: FreeText::Answer,
            needs_you,
        }),
        set_aside: false,
        replayed: false,
    }
}

/// Only a question that waits on you marks a row ◆: the request's, and the task it is about,
/// and Enter on either brings it up. One your rules still decide marks nothing, and the task's
/// steps stay folded while it asks.
#[test]
fn only_a_question_that_waits_on_you_marks_a_row() {
    let mut run = three_ways();
    run.waiting = Some(WaitReason::Approval);
    run.approvals.push(asked(Some(2), false));
    let rows = rows_of(run.clone());
    assert_eq!(rows[0].state, WorkflowRowState::Running);
    assert_eq!(rows[2].state, WorkflowRowState::Running);
    assert!(rows.iter().all(|row| row.answers.is_none()), "{rows:#?}");

    run.approvals = vec![asked(Some(2), true)];
    let rows = rows_of(run.clone());
    assert_eq!(rows[0].state, WorkflowRowState::NeedsYou);
    assert_eq!(rows[1].state, WorkflowRowState::Done);
    assert_eq!(rows[2].state, WorkflowRowState::NeedsYou);
    assert_eq!(rows[3].name, "Reminder emails", "Date cases' steps fold");
    let asks = Some(ApprovalId::numbered(1));
    assert_eq!(rows[0].answers, asks);
    assert_eq!(rows[1].answers, None);
    assert_eq!(rows[2].answers, asks);

    // A task in its own session brings up its question in place of opening the session.
    run.approvals = vec![asked(Some(1), true)];
    let rows = rows_of(run);
    assert_eq!(rows[1].state, WorkflowRowState::NeedsYou);
    assert!(rows[1].session.is_some());
    assert_eq!(rows[1].answers, asks);
}

/// A task waiting out an outage, one that hasn't started and one a fallback works on each have
/// their own mark.
#[test]
fn a_tasks_mark_says_where_it_stands() {
    let mut run = request();
    for (n, state) in [
        (1, TaskState::Running),
        (2, TaskState::Waiting),
        (3, TaskState::Running),
    ] {
        task(
            &mut run,
            n,
            &format!("Task {n}"),
            Way::OwnSession,
            state,
            "claude-code/claude-sonnet-5-5",
        );
    }
    run.tasks.get_mut(&task_id(1)).unwrap().waiting_on_agent =
        Some(crate::frame::state::AgentWait {
            agent_id: "claude-code".into(),
            until: None,
        });
    run.tasks.get_mut(&task_id(3)).unwrap().fallback_for =
        Some(model("claude-code/claude-opus-5-5"));
    let states: Vec<WorkflowRowState> = rows_of(run).iter().map(|row| row.state).collect();
    assert_eq!(
        states,
        [
            WorkflowRowState::Running,
            WorkflowRowState::Waiting,
            WorkflowRowState::Queued,
            WorkflowRowState::Fallback,
        ]
    );
}

/// A request that ended is one row, counting its tasks; a cancelled task is left out of both.
#[test]
fn a_request_that_ended_is_one_row() {
    let mut run = three_ways();
    task(
        &mut run,
        4,
        "Dropped",
        Way::OwnSession,
        TaskState::Cancelled,
        "codex/gpt-5.6-luna",
    );
    assert_eq!(rows_of(run.clone())[0].count, Some((1, 3)));
    run.finished = Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    run.ended = Some(T0);
    let rows = rows_of(run);
    assert_eq!(shape(&rows), ["0 Add due dates · - · 1/3"]);
}

/// A task from a record that kept no way was done in its own session.
#[test]
fn a_task_without_a_way_reads_as_its_own_session() {
    let mut run = request();
    task(
        &mut run,
        1,
        "Plan limits",
        Way::OwnSession,
        TaskState::Running,
        "codex/gpt-5.6-luna",
    );
    run.tasks.get_mut(&task_id(1)).unwrap().way = None;
    session(
        &mut run,
        1,
        1,
        ("codex/gpt-5.6-luna", Some(StepName::Build)),
        2,
    );
    let rows = rows_of(run);
    assert_eq!(rows[1].way.as_deref(), Some("Own session"));
    assert!(rows[1].session.is_some());
}

/// A task its CLI's own workflow does counts that workflow's agents, names it in the way
/// column, and lists the agents under it.
#[test]
fn a_task_done_by_its_clis_own_workflow_counts_that_workflows_agents() {
    use argonauts_core::runs::events::session::{CliAgent, CliWorkflowRun, SubagentStatus};

    let mut run = request();
    task(
        &mut run,
        1,
        "Retry the webhook",
        Way::CliWorkflow,
        TaskState::Running,
        "claude-code/claude-sonnet-5-5",
    );
    // A second task, so the workflow is this task's way and not the whole run.
    task(
        &mut run,
        2,
        "Docs",
        Way::OwnSession,
        TaskState::Waiting,
        "codex/gpt-5.6-luna",
    );
    let key = session(
        &mut run,
        1,
        1,
        ("claude-code/claude-sonnet-5-5", Some(StepName::Build)),
        2,
    );
    let agent = |index: u32, label: &str, state| CliAgent {
        index,
        label: label.into(),
        state,
        tokens: 12_000,
        ..CliAgent::default()
    };
    run.sessions
        .get_mut(&key)
        .unwrap()
        .workflows
        .push(CliWorkflowRun {
            id: "wf_retry".into(),
            call: None,
            name: "retry-webhook".into(),
            description: String::new(),
            phases: Vec::new(),
            agents: vec![
                agent(1, "Backoff", CliAgentState::Done),
                agent(2, "Tests", CliAgentState::Running),
                agent(3, "Docs", CliAgentState::Queued),
            ],
            status: SubagentStatus::Running,
            tokens: 36_000,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        });
    let rows = rows_of(run);
    assert_eq!(
        shape(&rows),
        [
            "0 Add due dates · - · 0/2",
            "1 Retry the webhook · Retry-webhook · 1/3",
            "2 Backoff · - · -",
            "2 Tests · - · -",
            "2 Docs · - · -",
            "1 Docs · Own session · -",
        ]
    );
    assert_eq!(rows[2].state, WorkflowRowState::Done);
    assert_eq!(rows[3].state, WorkflowRowState::Running);
    assert_eq!(rows[5].state, WorkflowRowState::Queued);
}

/// While a council talks, its request counts the seats that back every point: none before the
/// first list of points, a seat once it agrees with every open point and doesn't object to
/// yours, and all four when the talk agrees. Once it has tasks, the request counts those.
#[test]
fn a_councils_talk_counts_the_seats_that_back_every_point() {
    use argonauts_core::app::AppEvent;
    use argonauts_core::base::names::{SeatId, WorkflowId};
    use argonauts_core::runs::events::council::{
        CouncilEvent, CouncilEventKind, Point, PointStance, PointState, Seat, SeatRole, Seating,
        Stance,
    };
    use argonauts_core::runs::events::{RunEvent, RunEventKind};
    use argonauts_core::workflows::workflow::StepRef;

    let talk = |phase| StepRef {
        workflow: WorkflowId::numbered(1),
        phase,
        path: vec![0],
    };
    let seats = [
        "claude-code/claude-opus-5-5",
        "codex/gpt-5.6-luna",
        "antigravity-cli/gemini-3.8-flash",
        "opencode/qwen3-coder:30b",
    ];
    let mut state = AppState::default();
    let run = request();
    let id = run.id.clone();
    state.runs.insert(id.clone(), run);
    let mut seq = 0;
    let mut tell = |state: &mut AppState, phase, kind| {
        seq += 1;
        state.apply(&AppEvent::Run {
            event: Box::new(RunEvent {
                seq,
                ts: T0,
                run_id: id.clone(),
                task_id: None,
                session: None,
                source: "argonauts-code".into(),
                kind: RunEventKind::Council(CouncilEvent {
                    talk: talk(phase),
                    kind,
                }),
            }),
        });
    };
    let request_count = |state: &AppState| run_rows(state, T0, Instant::now())[0].count;
    let point = |number, state| Point {
        number,
        words: format!("Point {number}"),
        state,
        sides: Vec::new(),
        raised_by: None,
    };
    let stances = |seat: u32, stances: &[(u32, Stance)]| CouncilEventKind::Stances {
        seat: SeatId::numbered(seat),
        stances: stances
            .iter()
            .map(|&(point, stance)| PointStance { point, stance })
            .collect(),
    };

    tell(
        &mut state,
        0,
        CouncilEventKind::Seated(Seating {
            seats: (1..)
                .zip(seats)
                .map(|(n, model)| Seat {
                    id: SeatId::numbered(n),
                    model: model.parse().unwrap(),
                    role: SeatRole::default(),
                    number: None,
                })
                .collect(),
            notes: Some(SeatId::numbered(1)),
        }),
    );
    assert_eq!(request_count(&state), Some((0, 4)));
    tell(
        &mut state,
        0,
        CouncilEventKind::Points {
            points: vec![point(1, PointState::Open), point(2, PointState::Yours)],
        },
    );
    assert_eq!(
        request_count(&state),
        Some((0, 4)),
        "an open point needs each seat's agree"
    );
    tell(&mut state, 0, stances(1, &[(1, Stance::Agree)]));
    tell(
        &mut state,
        0,
        stances(2, &[(1, Stance::Agree), (2, Stance::Object)]),
    );
    tell(&mut state, 0, stances(3, &[(1, Stance::Question)]));
    assert_eq!(request_count(&state), Some((1, 4)));
    // A later stance replaces the seat's last one on that point and keeps the rest.
    tell(&mut state, 0, stances(2, &[(2, Stance::Agree)]));
    assert_eq!(request_count(&state), Some((2, 4)));
    // A talk that isn't the latest changes nothing.
    tell(&mut state, 1, stances(3, &[(1, Stance::Agree)]));
    assert_eq!(request_count(&state), Some((2, 4)));
    tell(&mut state, 0, CouncilEventKind::Agreed { points: 2 });
    assert_eq!(request_count(&state), Some((4, 4)));

    let run = state.runs.get_mut(&id).unwrap();
    task(
        run,
        1,
        "Due date field",
        Way::OwnSession,
        TaskState::Running,
        "claude-code/claude-opus-5-5",
    );
    assert_eq!(request_count(&state), Some((0, 1)));
}

/// A workflow's row counts as many steps as the workflow declares, from before they start, and
/// a step done again after a send back counts once.
#[test]
fn a_workflow_task_counts_the_steps_its_workflow_declares() {
    use argonauts_core::app::AppEvent;
    use argonauts_core::runs::events::task::{TaskEvent, TaskKind, TaskLimit, TaskSpec};
    use argonauts_core::runs::events::{RunEvent, RunEventKind};

    let mut state = AppState::default();
    let run = request();
    let id = run.id.clone();
    state.runs.insert(id.clone(), run);
    let way = Way::Workflow {
        name: WorkflowRef::new("review-with-tests"),
    };
    state.apply(&AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 1,
            ts: T0,
            run_id: id.clone(),
            task_id: Some(task_id(1)),
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Task(TaskEvent::Created(Box::new(TaskSpec {
                id: task_id(1),
                title: "Date cases".into(),
                task: "Cover the date edge cases".into(),
                kind: TaskKind::Build,
                owned_paths: Vec::new(),
                read_only_paths: Vec::new(),
                forbidden_paths: Vec::new(),
                hidden_paths: Vec::new(),
                depends_on: Vec::new(),
                criteria: Vec::new(),
                worker: Some(model("codex/gpt-5.6-luna")),
                reviewers: vec![model("claude-code/claude-sonnet-5-5")],
                limit: TaskLimit {
                    limit: None,
                    max_minutes: None,
                    max_tool_calls: None,
                    inactivity_min: None,
                },
                predictions: None,
                item: None,
                way: Some(TaskWay {
                    how: way,
                    asked_by_you: false,
                    steps: Some(3),
                }),
            }))),
        }),
    });
    let shown = |state: &AppState| shape(&run_rows(state, T0, Instant::now()));
    assert_eq!(shown(&state)[1], "1 Date cases · Review-with-tests · 0/3");
    let run = state.runs.get_mut(&id).unwrap();
    run.tasks.get_mut(&task_id(1)).unwrap().state = Some(TaskState::Running);
    let worker = ("codex/gpt-5.6-luna", Some(StepName::Build));
    let reviewer = ("claude-code/claude-sonnet-5-5", Some(StepName::Review));
    for (n, (who, step)) in [worker, reviewer, worker].into_iter().enumerate() {
        let run = state.runs.get_mut(&id).unwrap();
        let n = u32::try_from(n).unwrap() + 1;
        let key = session(run, n, 1, (who, step), 4);
        run.sessions.get_mut(&key).unwrap().status = SessionStatus::Done;
        run.sessions.get_mut(&key).unwrap().ended = Some(T0);
    }
    // Built, reviewed and sent back, then built again: two of its three steps are done.
    assert_eq!(shown(&state)[1], "1 Date cases · Review-with-tests · 2/3");
}

/// A workflow's steps count before they start: the build, and a review for each reviewer the
/// plan gave the task, so the task reads 1/2 between its build and its review.
#[test]
fn a_workflow_task_counts_its_review_before_it_starts() {
    use argonauts_core::app::AppEvent;
    use argonauts_core::runs::events::task::{TaskEvent, TaskKind, TaskLimit, TaskSpec};
    use argonauts_core::runs::events::{RunEvent, RunEventKind};

    let mut state = AppState::default();
    let run = request();
    let id = run.id.clone();
    state.runs.insert(id.clone(), run);
    let way = Way::Workflow {
        name: WorkflowRef::new("maker-checker"),
    };
    state.apply(&AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 1,
            ts: T0,
            run_id: id.clone(),
            task_id: Some(task_id(1)),
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Task(TaskEvent::Created(Box::new(TaskSpec {
                id: task_id(1),
                title: "Date cases".into(),
                task: "Cover the date edge cases".into(),
                kind: TaskKind::Build,
                owned_paths: Vec::new(),
                read_only_paths: Vec::new(),
                forbidden_paths: Vec::new(),
                hidden_paths: Vec::new(),
                depends_on: Vec::new(),
                criteria: Vec::new(),
                worker: Some(model("codex/gpt-5.6-luna")),
                reviewers: vec![model("claude-code/claude-sonnet-5-5")],
                limit: TaskLimit {
                    limit: None,
                    max_minutes: None,
                    max_tool_calls: None,
                    inactivity_min: None,
                },
                predictions: None,
                item: None,
                way: Some(TaskWay {
                    how: way,
                    asked_by_you: false,
                    steps: None,
                }),
            }))),
        }),
    });
    let shown = |state: &AppState| shape(&run_rows(state, T0, Instant::now()));
    assert_eq!(shown(&state)[1], "1 Date cases · Maker-checker · 0/2");

    let run = state.runs.get_mut(&id).unwrap();
    run.tasks.get_mut(&task_id(1)).unwrap().state = Some(TaskState::Running);
    let build = session(run, 1, 1, ("codex/gpt-5.6-luna", Some(StepName::Build)), 4);
    assert_eq!(shown(&state)[1], "1 Date cases · Maker-checker · 0/2");

    let run = state.runs.get_mut(&id).unwrap();
    run.sessions.get_mut(&build).unwrap().status = SessionStatus::Done;
    run.sessions.get_mut(&build).unwrap().ended = Some(T0);
    assert_eq!(
        shown(&state)[1..],
        ["1 Date cases · Maker-checker · 1/2", "2 Build · - · -"]
    );
}

/// Enter on a task done by a workflow or by agents opens the run's view on the workflow its
/// sessions work in; a task in its own session opens that session.
#[test]
fn a_task_row_opens_the_workflow_its_sessions_work_in() {
    let mut run = request();
    let maker_checker = Way::Workflow {
        name: WorkflowRef::new("maker-checker"),
    };
    task(
        &mut run,
        1,
        "Date cases",
        maker_checker,
        TaskState::Running,
        "codex/gpt-5.6-luna",
    );
    task(
        &mut run,
        2,
        "Due date field",
        Way::OwnSession,
        TaskState::Running,
        "codex/gpt-5.6",
    );
    let workflow = argonauts_core::base::names::WorkflowId::numbered(1);
    for (n, task) in [(1, 1), (2, 2)] {
        let key = session(
            &mut run,
            n,
            task,
            ("codex/gpt-5.6-luna", Some(StepName::Build)),
            4,
        );
        run.sessions.get_mut(&key).unwrap().workflow_step = Some(StepRef {
            workflow: workflow.clone(),
            phase: 0,
            path: vec![0],
        });
    }
    let rows = rows_of(run);
    let row = |name: &str| rows.iter().find(|row| row.name == name).expect(name);
    assert_eq!(row("Date cases").opens.as_deref(), Some("run-due#wf-001"));
    assert_eq!(row("Due date field").opens, None);
    assert!(row("Due date field").session.is_some());
}
