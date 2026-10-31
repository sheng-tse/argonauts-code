//! Autopilot never waits for the person. What a run would have asked is settled by the settings,
//! the rules, the judge and the main agent, each in one line, with the real approval policy
//! over fake workers.

use crate::support;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::ErrorCategory;
use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::base::names::{AgentId, RoleName};
use argonauts_core::models::limits::{Candidate, CostEstimate, HealthState, RecoveryAction};
use argonauts_core::models::meter::LimitDimension;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::agent_status::FallbackPlan;
use argonauts_core::runs::events::approval::{
    ApprovalEvent, ApprovalKind, ApprovalPacket, MergeQuestion,
};
use argonauts_core::runs::events::run::{Party, RunLifecycle, RunMode};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::shared::{AnsweredForYou, BlackboardEvent};
use argonauts_core::runs::events::task::{MergeEvent, TaskEvent, TaskState};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::workflows::steps::StepName;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::engine::services::{MemoryHealthBoard, TableLimits};
use argonauts_testkit::{FakeRunner, FakeSession};

use support::{Crew, Outcome, names};

const FILE: &str = "verbose.txt";
const QUESTION: &str = "Should --verbose also write to a log file?";

/// What a build session does on each of its turns, by how many came before.
type Turn = Arc<dyn Fn(usize, &SessionSpec) -> Value + Send + Sync>;

/// A task's report as its worker writes it.
fn report(status: &str, questions: &[&str], concerns: &[&str]) -> Value {
    json!({
        "status": status,
        "summary": "Added the --verbose flag.",
        "files_changed": [FILE],
        "tests_run": [],
        "concerns": concerns,
        "questions": questions,
        "change_requests": [],
        "memory_proposals": [],
    })
}

/// Two providers' agents, either of which may get the one task: whoever builds it does as
/// `turn` says, and answers a plain question in a sentence. A third provider's CLI is installed
/// to judge. Checks pass.
fn crew(turn: Turn) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.settings.divide.contracts_first = false;
    let passing = |name: &str| CheckCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
    let turns = Arc::new(AtomicUsize::new(0));
    for (worker, provider) in [("claude-code/opus", "anthropic"), ("codex/gpt-x", "openai")] {
        let (turn, turns) = (turn.clone(), turns.clone());
        crew.enlist(
            FakeRunner::cli(worker, provider).respond_with(move |spec| match spec.step {
                Some(StepName::Build) => {
                    std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                        .expect("the build session writes its file");
                    let answer = turn(turns.fetch_add(1, Ordering::SeqCst), spec);
                    FakeSession::structured(answer).with_event(support::wrote(FILE))
                }
                Some(StepName::PlanToTasks) => FakeSession::structured(json!({
                    "tasks": [{
                        "key": "flag",
                        "title": "Add the --verbose flag",
                        "task": "Add a --verbose flag.",
                        "kind": "build",
                        "owned_paths": [FILE],
                        "read_only_paths": [],
                        "depends_on": [],
                        "criteria": [],
                    }]
                })),
                None => FakeSession::completed("No: log to stderr only."),
                _ => judged(spec),
            }),
        );
    }
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(judged),
    );
    Ok(crew)
}

/// An approval citing the default rules at the judge's step, else the smallest valid answer.
fn judged(spec: &SessionSpec) -> FakeSession {
    if spec.step == Some(StepName::ModelJudge) {
        return FakeSession::structured(json!({
            "verdict": "approve",
            "reasons": ["the evidence meets the rules"],
            "rules_cited": ["R7"]
        }));
    }
    support::smallest_answer(spec)
}

/// Everything a session was given to read, as one text.
fn prompt_text(spec: &SessionSpec) -> String {
    let parts: Vec<&str> = spec.prompt.iter().map(|part| part.text.as_str()).collect();
    parts.join("\n")
}

/// Runs the request on autopilot, with the real policy, until the run waits or finishes.
async fn on_autopilot(crew: &Crew) -> anyhow::Result<Outcome> {
    in_mode(crew, ApprovalMode::Autopilot).await
}

/// Runs the request in `mode`, with the real policy, until the run waits or finishes.
async fn in_mode(crew: &Crew, mode: ApprovalMode) -> anyhow::Result<Outcome> {
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let autopilot = RunEvent {
        seq: 1,
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode,
            by: Party::User,
        }),
        ..started.clone()
    };
    let (mut context, sink, _commands) = crew.context(vec![started, autopilot])?;
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    context.services.policy = argonauts_policy::approval_policy_with_models(
        context.settings.clone(),
        Arc::new(|| None),
        state,
        crew.services.workers.clone(),
        crew.services.registry.clone(),
        crew.services.steps.clone(),
    );
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

fn notes(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Every question answered in your place.
fn answered_for_you(events: &[RunEvent]) -> Vec<&AnsweredForYou> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)) => Some(answered),
            _ => None,
        })
        .collect()
}

/// Every approval that opened, and who decided each.
fn approvals(events: &[RunEvent]) -> Vec<(ApprovalKind, Option<Party>)> {
    let mut opened = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(open)) => {
                opened.push((open.id.clone(), open.kind, None));
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                if let Some(open) = opened.iter_mut().find(|(id, ..)| *id == decided.id) {
                    open.2 = Some(decided.by.clone());
                }
            }
            _ => {}
        }
    }
    opened.into_iter().map(|(_, kind, by)| (kind, by)).collect()
}

fn task_states(events: &[RunEvent]) -> Vec<TaskState> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::State { to, .. }) => Some(*to),
            _ => None,
        })
        .collect()
}

fn merged(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    })
}

/// Nothing in the run was left for the person: it finished, no approval was decided by them or
/// left open, and no task sat waiting on an answer at its end.
fn assert_nothing_waited(outcome: &Outcome) {
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: {:#?}",
        outcome.exit,
        notes(events)
    );
    for (kind, by) in approvals(events) {
        assert!(
            by.as_ref().is_some_and(|by| *by != Party::User),
            "a {kind:?} approval was left to the person: {by:?}"
        );
    }
    assert_ne!(task_states(events).last(), Some(&TaskState::Blocked));
}

/// A worker that stops on a question gets the main agent's answer in its own session and
/// carries on: the task merges, with no approval for the question and one line that says who
/// answered. Before, the answer reached the board and the task stayed blocked for good.
#[tokio::test]
async fn a_question_that_blocks_is_answered_and_its_task_carries_on() -> anyhow::Result<()> {
    let given = Arc::new(Mutex::new(Vec::new()));
    let kept = given.clone();
    let crew = crew(Arc::new(move |turn, spec| {
        kept.lock().unwrap().push(prompt_text(spec));
        match turn {
            0 => report("blocked", &[QUESTION], &[]),
            _ => report("done", &[], &["Assumed: stderr only, as the answer says."]),
        }
    }))?;
    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    assert_nothing_waited(&outcome);
    assert!(merged(events), "{:?}", names(events));
    assert!(crew.repo.root().join(FILE).exists());
    assert!(
        !approvals(events)
            .iter()
            .any(|(kind, _)| *kind == ApprovalKind::Question),
        "{:?}",
        approvals(events)
    );
    assert!(
        !task_states(events).contains(&TaskState::Blocked),
        "{:?}",
        task_states(events)
    );
    let answered = answered_for_you(events);
    assert!(
        answered.iter().any(|answered| {
            answered.asker == "build:flag"
                && answered.answer == "No: log to stderr only."
                && answered.by.is_some()
                && !answered.finished
        }),
        "{answered:#?}"
    );
    // The builder's second turn was given the answer.
    let given = given.lock().unwrap();
    assert_eq!(given.len(), 2, "{given:#?}");
    assert!(given[1].contains("No: log to stderr only."), "{given:#?}");
    // The merge question shows what was answered in your place, and by whom.
    let carried = merge_questions(events);
    assert_eq!(carried.len(), 1, "{carried:#?}");
    assert_eq!(carried[0].text, QUESTION);
    assert_eq!(
        carried[0].answer.as_deref(),
        Some("No: log to stderr only.")
    );
    assert!(carried[0].answered_by.is_some(), "{carried:#?}");
    Ok(())
}

/// A question in a report that finished its work asks nobody and starts no model: it stands as
/// its worker's call, and the task merges.
#[tokio::test]
async fn a_finished_reports_question_becomes_its_workers_call() -> anyhow::Result<()> {
    let crew = crew(Arc::new(|_, _| report("done", &[QUESTION], &[])))?;
    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    assert_nothing_waited(&outcome);
    assert!(merged(events), "{:?}", names(events));
    assert!(
        !approvals(events)
            .iter()
            .any(|(kind, _)| *kind == ApprovalKind::Question),
        "{:?}",
        approvals(events)
    );
    let said = notes(events);
    assert!(
        said.iter().any(|line| {
            line.starts_with("Decided on autopilot · build:flag (")
                && line
                    .ends_with(") kept its own call on: Should --verbose also write to a log file?")
        }),
        "{said:#?}"
    );
    assert_eq!(
        merge_questions(events),
        vec![MergeQuestion {
            text: QUESTION.to_owned(),
            answer: Some("its agent's own call".to_owned()),
            answered_by: None,
        }]
    );
    Ok(())
}

/// In auto, a question in a report that needs a decision but finished its work waits for
/// nobody: the task goes on to its reviews with one line, no question opens, and the question
/// stays open on the board for the merge question to show.
#[tokio::test]
async fn in_auto_a_finished_reports_question_doesnt_block_its_task() -> anyhow::Result<()> {
    let crew = crew(Arc::new(|_, _| report("needs_decision", &[QUESTION], &[])))?;
    let outcome = in_mode(&crew, ApprovalMode::Auto).await?;
    let events = &outcome.events;
    assert!(
        !approvals(events)
            .iter()
            .any(|(kind, _)| *kind == ApprovalKind::Question),
        "{:?}",
        approvals(events)
    );
    assert!(
        !task_states(events).contains(&TaskState::Blocked),
        "{:?}",
        task_states(events)
    );
    assert!(
        task_states(events).contains(&TaskState::InReview),
        "{:?}",
        task_states(events)
    );
    let said = notes(events);
    assert!(
        said.iter()
            .any(|line| line.starts_with("build:flag asked, and goes on to its reviews: ")),
        "{said:#?}"
    );
    // The merge question carries the question, still open, beside its task.
    assert_eq!(
        merge_questions(events),
        vec![MergeQuestion {
            text: QUESTION.to_owned(),
            answer: None,
            answered_by: None,
        }]
    );
    Ok(())
}

/// What the merge question shows of the questions its tasks' agents asked you.
fn merge_questions(events: &[RunEvent]) -> Vec<MergeQuestion> {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => match &opened.packet {
                ApprovalPacket::Merge(packet) => Some(packet.clone()),
                _ => None,
            },
            _ => None,
        })
        .map(|packet| {
            packet
                .rows
                .into_iter()
                .flat_map(|row| row.questions)
                .collect()
        })
        .unwrap_or_default()
}

/// On autopilot a worker's instructions never offer a question for the person: the report's
/// lines say to decide and write down what was assumed.
#[tokio::test]
async fn a_workers_instructions_offer_no_question_for_the_person() -> anyhow::Result<()> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    let crew = crew(Arc::new(move |_, spec| {
        kept.lock().unwrap().push(prompt_text(spec));
        report("done", &[], &[])
    }))?;
    let outcome = on_autopilot(&crew).await?;
    assert_nothing_waited(&outcome);
    let seen = seen.lock().unwrap();
    let prompt = seen.first().context("the task was built")?;
    assert!(!prompt.contains("for the user"), "{prompt}");
    assert!(
        !prompt.contains("when you need an answer first"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Nobody else is asked on autopilot"),
        "{prompt}"
    );
    Ok(())
}

/// A check's result as the fake runner gives it.
fn check(passed: bool, summary: &str, tail: &str) -> argonauts_core::runs::events::CheckOutcome {
    argonauts_core::runs::events::CheckOutcome {
        name: "tests".to_owned(),
        kind: argonauts_core::runs::events::task::CheckKind::Command,
        passed,
        command: Some(vec!["npm".to_owned(), "test".to_owned()]),
        exit_code: Some(i32::from(!passed)),
        duration_ms: 1,
        summary: summary.to_owned(),
        output_tail: tail.to_owned(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    }
}

/// The starting code already fails one test. The checks run on it before planning, so the
/// planner, the worker and the reviewer are told, and the task, which breaks nothing more, isn't
/// failed for it: its checks read "as at the start", and it merges. Before, the planner never
/// knew and every task was judged on the whole suite.
#[tokio::test]
async fn a_test_that_already_failed_is_not_the_tasks_to_answer_for() -> anyhow::Result<()> {
    let built = Arc::new(Mutex::new(Vec::new()));
    let kept = built.clone();
    let crew = crew(Arc::new(move |_, spec| {
        kept.lock().unwrap().push(prompt_text(spec));
        report("done", &[], &[])
    }))?;
    let failing = "not ok 3 - the bucket never holds more than its capacity";
    crew.services
        .checks
        .push_at_start(check(false, "15 passed, 1 failed", failing));
    // The task's own check fails the same way, with one more test passing.
    crew.services
        .checks
        .push(check(false, "16 passed, 1 failed", failing));

    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    assert_nothing_waited(&outcome);
    assert!(merged(events), "{:#?}", notes(events));
    assert_eq!(crew.services.checks.start_requests().len(), 1);

    let said = notes(events);
    assert!(
        said.contains(
            &"Already failing before the run changed anything: tests (npm test): 15 passed, 1 \
              failed. Each task answers only for what it breaks, and the final check still \
              runs everything."
        ),
        "{said:#?}"
    );
    let task_checks: Vec<(bool, &str)> = events
        .iter()
        .filter(|event| {
            event
                .task_id
                .as_ref()
                .is_some_and(|task| task.as_str() != "start")
        })
        .filter_map(|event| match &event.kind {
            RunEventKind::Check(check) if check.name == "tests" => {
                Some((check.passed, check.summary.as_str()))
            }
            _ => None,
        })
        .collect();
    assert!(
        task_checks.contains(&(true, "16 passed, 1 failed, as at the start")),
        "{task_checks:?}"
    );

    // The worker was told what already fails, with the end of the output that names the test.
    let built = built.lock().unwrap();
    let prompt = built.first().context("the task was built")?;
    assert!(
        prompt.contains("Already failing on the code this run started from"),
        "{prompt}"
    );
    assert!(prompt.contains(failing), "{prompt}");

    // So were the planner and the reviewer.
    let heard = |step: StepName| -> Vec<String> {
        ["claude-code/opus", "codex/gpt-x"]
            .into_iter()
            .filter_map(|name| crew.services.workers.fake(name))
            .flat_map(|fake| fake.specs())
            .filter(|spec| spec.step == Some(step))
            .map(|spec| prompt_text(&spec))
            .collect()
    };
    // Each agent that drafts a plan of its own; the step that merges the drafts adds nothing.
    let drafts: Vec<String> = heard(StepName::Draft)
        .into_iter()
        .filter(|draft| draft.contains("You draft alone"))
        .collect();
    assert!(!drafts.is_empty(), "no plan was drafted");
    assert!(
        drafts
            .iter()
            .all(|draft| draft.contains("the plan gives that fix to one task")),
        "{drafts:#?}"
    );
    let reviews = heard(StepName::Review);
    assert!(
        reviews
            .iter()
            .all(|review| review.contains("at the start, before this run: tests (npm test)")),
        "{reviews:#?}"
    );
    // And whoever turns the plan into tasks, so no task is told to leave the fix alone.
    let divided = heard(StepName::PlanToTasks);
    assert!(!divided.is_empty(), "the plan was never divided");
    assert!(
        divided
            .iter()
            .all(|asked| asked.contains("Already failing on the code this run started from")),
        "{divided:#?}"
    );
    Ok(())
}

/// A task that breaks a test that passed at the start still fails its checks, and the worker is
/// told how the start compares.
#[tokio::test]
async fn a_task_that_breaks_more_than_the_start_still_fails_its_checks() -> anyhow::Result<()> {
    let crew = crew(Arc::new(|_, _| report("done", &[], &[])))?;
    crew.services
        .checks
        .push_at_start(check(false, "15 passed, 1 failed", ""));
    crew.services.checks.push(check(
        false,
        "14 passed, 2 failed",
        "not ok 7 - parses a flag",
    ));
    let outcome = on_autopilot(&crew).await?;
    let failed: Vec<&str> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Check(check) if !check.passed && check.name == "tests" => {
                Some(check.summary.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(
        failed.contains(&"14 passed, 2 failed (at the start: 15 passed, 1 failed)"),
        "{failed:?}"
    );
    Ok(())
}

const NOTES: &str = "notes.txt";
const OUTAGE: &str = "outage.txt";
const BUILDERS: [&str; 2] = ["claude-code/opus", "codex/gpt-x"];

/// What one run meets in [`nothing_waits_on_autopilot`], by task. `task-001`'s builder stops on
/// a question, then its session crashes on a try and its retry, so the task needs a decision,
/// and on the try that decision grants it finishes with a question in its report. `task-002`'s
/// first build hits its spend limit. `task-003` starts once both are in, and an outage marks
/// its builder's CLI down under it.
struct Trouble {
    /// Who built `task-001` first, and how many turns it has had.
    first_builder: Mutex<Option<(String, usize)>>,
    limit_hit: std::sync::atomic::AtomicBool,
    /// The CLI the outage took down under `task-003`, once it has.
    went_down: Mutex<Option<AgentId>>,
    limits: Arc<TableLimits>,
    health: Arc<MemoryHealthBoard>,
}

impl Trouble {
    fn new(crew: &Crew) -> Self {
        Self {
            first_builder: Mutex::default(),
            limit_hit: false.into(),
            went_down: Mutex::default(),
            limits: crew.services.limits.clone(),
            health: crew.services.health.clone(),
        }
    }

    /// `task-003`'s first builder meets an outage that has its CLI marked down, on its try and
    /// the retry, and the other builder may take the task. The CLI stays down to the run's end.
    fn outage(&self, spec: &SessionSpec) -> FakeSession {
        let overloaded = || {
            FakeSession::ended(EndReason::CliError {
                category: ErrorCategory::Overloaded,
                message: "overloaded".to_owned(),
            })
        };
        let me = spec.model.agent_id();
        let mut went_down = self.went_down.lock().unwrap();
        match went_down.as_ref() {
            Some(down) if *down == me => return overloaded(),
            Some(down) => {
                self.health.set(down, HealthState::Healthy, None);
                std::fs::write(spec.workdir.join(OUTAGE), "back\n").expect("the file is written");
                return FakeSession::structured(report("done", &[], &[]))
                    .with_event(support::wrote(OUTAGE));
            }
            None => {}
        }
        *went_down = Some(me.clone());
        self.health.set(&me, HealthState::Down, Some("overloaded"));
        let other = BUILDERS
            .into_iter()
            .find(|name| !name.starts_with(&spec.model.agent))
            .expect("another builder");
        self.limits
            .set_next(RecoveryAction::Ask(Box::new(FallbackPlan {
                task: spec.task.clone().expect("a build session has its task"),
                title: format!("Write {OUTAGE}"),
                stopped: spec.model.clone(),
                reason: "overloaded".to_owned(),
                resets_at: None,
                work: Default::default(),
                checks_ran: false,
                candidates: vec![Candidate {
                    worker: other.parse().expect("a model"),
                    plan_usage_left: Some(0.5),
                    plan_usage_text: "50%".to_owned(),
                    track_record: None,
                    cost: CostEstimate::PlanUsage(0.1),
                    needs_confirm: false,
                    may_run_out: false,
                }],
                mode: FallbackMode::Ask,
                strategy: Default::default(),
                race_count: 1,
                also_moving: Vec::new(),
            })));
        overloaded()
    }

    fn build(&self, spec: &SessionSpec) -> FakeSession {
        let task = spec.task.as_ref().map(|task| task.as_str().to_owned());
        if task.as_deref() == Some("task-003") {
            return self.outage(spec);
        }
        if task.as_deref() == Some("task-002") {
            if !self.limit_hit.swap(true, Ordering::SeqCst) {
                return FakeSession::ended(EndReason::LimitHit {
                    dimension: LimitDimension::Spend,
                });
            }
            std::fs::write(spec.workdir.join(NOTES), "notes\n").expect("the notes are written");
            return FakeSession::structured(report("done", &[], &[]))
                .with_event(support::wrote(NOTES));
        }
        let me = spec.model.to_string();
        let turn = {
            let mut first = self.first_builder.lock().unwrap();
            match first.as_mut() {
                None => {
                    *first = Some((me, 1));
                    Some(0)
                }
                Some((builder, turns)) if *builder == me => {
                    *turns += 1;
                    Some(*turns - 1)
                }
                Some(_) => None,
            }
        };
        match turn {
            Some(0) => FakeSession::structured(report("blocked", &[QUESTION], &[])),
            Some(1 | 2) => FakeSession::ended(EndReason::Crashed {
                message: "the CLI is down".to_owned(),
            }),
            _ => {
                std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                    .expect("the build session writes its file");
                let finished = report(
                    "done",
                    &["Should the flag have a short form too?"],
                    &["Assumed: --verbose takes no value."],
                );
                FakeSession::structured(finished).with_event(support::wrote(FILE))
            }
        }
    }
}

/// One run on autopilot meets what a run stops for: a worker's question that blocks it, a
/// session that keeps crashing, the task needing a decision, a question in a finished report,
/// a spend limit, an outage that marks a CLI down, the merge, and the closing answer. Each is
/// decided by the settings, the rules, the judge or the main agent and said in one line, and
/// nothing waits.
#[tokio::test]
async fn nothing_waits_on_autopilot() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.divide.contracts_first = false;
    let passing = |name: &str| CheckCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
    // Limits of your own, with room left for one more try, since this run reaches one.
    crew.settings.limits.spend.spend_per_run = Some(100.0);
    crew.settings.limits.spend.per_task_usd = Some(3.0);
    let trouble = Arc::new(Trouble::new(&crew));
    let task = |key: &str, file: &str, after: &[&str]| {
        json!({
            "key": key, "title": format!("Write {file}"), "task": format!("Write {file}."),
            "kind": "build", "owned_paths": [file], "read_only_paths": [], "depends_on": after,
            "criteria": [],
        })
    };
    let plan = json!({"tasks": [
        task("flag", FILE, &[]),
        task("notes", NOTES, &[]),
        task("outage", OUTAGE, &["flag", "notes"]),
    ]});
    for (worker, provider) in BUILDERS.into_iter().zip(["anthropic", "openai"]) {
        let (trouble, plan) = (trouble.clone(), plan.clone());
        crew.enlist(
            FakeRunner::cli(worker, provider).respond_with(move |spec| match spec.step {
                Some(StepName::Build) => trouble.build(spec),
                Some(StepName::PlanToTasks) => FakeSession::structured(plan.clone()),
                None => FakeSession::completed("No: log to stderr only."),
                _ => judged(spec),
            }),
        );
    }
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(judged),
    );
    // A third provider reviews, so a task has a reviewer whichever builder ends up with it.
    crew.settings
        .roles
        .entry(RoleName::Reviewer)
        .or_default()
        .models = vec!["antigravity-cli".parse()?];

    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    let said = notes(events);
    assert_nothing_waited(&outcome);

    // Every kind of stop was met, and none was left to the person.
    let met: Vec<ApprovalKind> = approvals(events)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    for kind in [
        ApprovalKind::Limit,
        ApprovalKind::Stuck,
        ApprovalKind::Fallback,
        ApprovalKind::Merge,
    ] {
        assert!(met.contains(&kind), "no {kind:?} approval in {met:?}");
    }
    assert!(!met.contains(&ApprovalKind::Question), "{met:?}");
    // How to choose among a down CLI's tasks is a question for a person, so it never opened:
    // the task went to the agent recommended for it, in one line.
    assert!(!met.contains(&ApprovalKind::AgentDown), "{met:?}");
    assert!(trouble.went_down.lock().unwrap().is_some());
    assert!(
        said.iter()
            .any(|line| line.contains("stopped · Write outage.txt")
                && line.contains("instead of asking you")),
        "{said:#?}"
    );
    assert!(crew.repo.root().join(OUTAGE).exists(), "{said:#?}");

    // The question that blocked was answered by a model, and its task carried on.
    let answered = answered_for_you(&outcome.events);
    assert!(
        answered
            .iter()
            .any(|answered| answered.asker.starts_with("build:") && answered.by.is_some()),
        "{answered:#?}"
    );
    // The task whose CLI went down wasn't put off: autopilot gave it another try, with a note.
    assert!(
        said.iter().any(|line| line.starts_with(
            "Autopilot answered \"Write verbose.txt needs you\" with \"Give it another try\" \
             instead of asking you"
        )),
        "{said:#?}"
    );
    // No line the run wrote names a task by its id.
    assert!(
        !said.iter().any(|line| line.contains("task-001")),
        "{said:#?}"
    );
    let told: Vec<String> = BUILDERS
        .into_iter()
        .filter_map(|name| crew.services.workers.fake(name))
        .flat_map(|fake| fake.specs())
        .filter(|spec| spec.step == Some(StepName::Build))
        .map(|spec| prompt_text(&spec))
        .filter(|prompt| prompt.contains("the run is on autopilot and gave this task another try"))
        .collect();
    assert!(!told.is_empty(), "no builder heard why it got another try");
    // The question in the finished report stands as its worker's call.
    assert!(
        said.iter()
            .any(|line| line.starts_with("Decided on autopilot · ")
                && line.ends_with("kept its own call on: Should the flag have a short form too?")),
        "{said:#?}"
    );
    // The work that could merge did.
    assert!(merged(events), "{said:#?}");
    assert!(crew.repo.root().join(FILE).exists());

    // The closing answer lists what was decided, and the main agent was told so.
    let closing = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) => Some(answer.as_ref()),
            _ => None,
        })
        .context("the run closed with an answer")?;
    let decided: Vec<&str> = closing.decided.iter().map(|d| d.what.as_str()).collect();
    assert!(
        decided.contains(&"--verbose takes no value"),
        "{decided:#?}"
    );
    assert!(
        decided.contains(&"Should the flag have a short form too?"),
        "{decided:#?}"
    );
    assert!(
        closing
            .decided
            .iter()
            .any(|d| d.whose == "autopilot's choice"),
        "{:#?}",
        closing.decided
    );
    assert!(
        closing
            .left
            .iter()
            .all(|line| !line.starts_with("Assumed:")),
        "{:#?}",
        closing.left
    );
    let closing_asked: Vec<String> = BUILDERS
        .into_iter()
        .filter_map(|name| crew.services.workers.fake(name))
        .flat_map(|fake| fake.specs())
        .filter(|spec| spec.step == Some(StepName::CloseRun))
        .map(|spec| prompt_text(&spec))
        .collect();
    assert!(
        closing_asked
            .iter()
            .any(|asked| asked.contains("decided_on_autopilot")
                && asked.contains("never ask the person to confirm")),
        "{closing_asked:#?}"
    );
    Ok(())
}

/// Whether any auto-approval joined the queue of calls for the person to check. Autopilot
/// draws none, whatever share the settings sample.
fn a_spot_check_is_queued(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { .. })
        )
    })
}

/// A checkout on no branch has nothing the run could merge into until someone names a branch,
/// which only the person can. On autopilot nobody is asked: the run keeps its own branch, the
/// line gives that reason, and nothing is queued for the person to check about a merge that
/// never happened.
#[tokio::test]
async fn on_autopilot_a_checkout_on_no_branch_keeps_the_runs_branch() -> anyhow::Result<()> {
    let mut crew = crew(Arc::new(|_, _| report("done", &[], &[])))?;
    // Auto would draw every approval for a spot check with this, the merge's among them.
    crew.settings.approvals.spot_check_sample = 1.0;
    crew.repo.git(&["checkout", "--quiet", "--detach"])?;
    let outcome = on_autopilot(&crew).await?;
    assert_nothing_waited(&outcome);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft)
    );
    let said = notes(&outcome.events);
    assert!(
        said.iter().any(|note| note.contains(
            "with \"Keep the run on its branch\" instead of asking you, since only you can \
             name the branch"
        )),
        "{said:#?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("a hard limit")),
        "{said:#?}"
    );
    assert!(!a_spot_check_is_queued(&outcome.events));
    Ok(())
}

/// In a folder without git, the person and the run changed the same line. Only the person can
/// pick a version, so on autopilot the run pauses with nothing written, says so in one line,
/// and no question is left open.
#[tokio::test]
async fn on_autopilot_a_file_both_changed_waits_to_be_written_in() -> anyhow::Result<()> {
    let folder = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let yours = folder.clone();
    let mut crew = crew(Arc::new(move |_, _| {
        // Saved while the run works, as an edit of the person's own would be.
        if let Some(folder) = yours.lock().unwrap().as_ref() {
            std::fs::write(folder.join(FILE), "--quiet\n").expect("the person's edit is saved");
        }
        report("done", &[], &[])
    }))?;
    crew.settings.approvals.spot_check_sample = 1.0;
    crew.repo.write(FILE, "--silent\n")?;
    crew.repo.git(&["add", FILE])?;
    crew.repo.git(&["commit", "--quiet", "-m", "a flag"])?;
    let root = crew.repo.root().to_path_buf();
    let hidden = argonauts_core::runs::hidden_git::dir(crew.repo_context()?.state_dir());
    std::fs::create_dir_all(hidden.parent().context("a state folder")?)?;
    std::fs::rename(root.join(".git"), &hidden)?;
    *folder.lock().unwrap() = Some(root.clone());

    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    let said = notes(events);
    for (kind, by) in approvals(events) {
        assert!(
            by.as_ref().is_some_and(|by| *by != Party::User),
            "a {kind:?} approval was left to the person: {by:?}\n{said:#?}"
        );
    }
    assert!(
        said.iter()
            .any(|note| note.contains("both changed verbose.txt")
                && note.contains(
                    "with \"Merge later\" instead of asking you, since only you can pick between \
                 your version and the run's"
                )),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|note| note.starts_with("The run waits to be written into your folder")),
        "{said:#?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("a hard limit")),
        "{said:#?}"
    );
    // Nothing was written, and the person's edit is as they saved it.
    assert_eq!(std::fs::read_to_string(root.join(FILE))?, "--quiet\n");
    assert!(!a_spot_check_is_queued(events));
    // Paused, which /resume takes up, and not waiting on an answer.
    assert_eq!(
        outcome.exit,
        EngineExit::Waiting(argonauts_core::runs::events::run::WaitReason::Paused)
    );
    Ok(())
}
