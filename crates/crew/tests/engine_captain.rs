//! Captain scenarios: one task for the main agent, your prompt as the goal unless
//! `captain.understand_first`, review by another provider under `captain.review`, then the merge
//! approval.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalPacket, MergePacket,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::run::{
    Party, PhaseChange, ResumeCause, RunLifecycle, RunMode, RunOutcome, RunPhase, WaitReason,
};
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskSpec, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::log::{Note, NoteLevel, RUN_REVIEW_FAILED};
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::{Access, SessionSpec};
use argonauts_core::steps::StepName;
use argonauts_core::{PhaseStatus, RunId};
use argonauts_crew::understand::file;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, Outcome, answer, names};

const REQUEST: &str = "Fix the typo in the README's install section";
const WORKER: &str = "claude-code/opus";

fn report() -> Value {
    json!({
        "status": "done",
        "summary": "Fixed the typo",
        // The scripted worker changes no file, so it names none.
        "files_changed": [],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
        "answer": "I fixed the misspelled word in the README's install section."
    })
}

fn reading() -> Value {
    json!({
        "summary": "Fix the misspelled word in the README's install section.",
        "success_criteria": ["The install section has no typo"],
        "in_scope": ["README.md"],
        "out_of_scope": ["every other file"],
        "assumptions": [],
        "open_questions": []
    })
}

/// A Captain crew whose main agent is a fake Claude Code.
fn captain_crew() -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.crew.main = WORKER.parse()?;
    // Each phase end would ask the main agent for a workflow change, and the run review would ask
    // for a process change. These scenarios script neither session.
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let worker = crew.worker(WORKER, "anthropic");
    Ok((crew, worker))
}

fn goal_events(events: &[RunEvent]) -> Vec<(usize, &GoalEvent)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match &event.kind {
            RunEventKind::Goal(goal) => Some((at, goal)),
            _ => None,
        })
        .collect()
}

fn approved(events: &[RunEvent]) -> Option<(usize, GoalFile, Party)> {
    goal_events(events)
        .into_iter()
        .find_map(|(at, event)| match event {
            GoalEvent::Approved { file, by } => Some((at, file.clone(), by.clone())),
            _ => None,
        })
}

fn created(events: &[RunEvent]) -> Vec<(usize, TaskSpec)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => Some((at, spec.as_ref().clone())),
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent], kind: ApprovalKind) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == kind)
        })
        .count()
}

fn merge_packet(events: &[RunEvent]) -> MergePacket {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => match &opened.packet {
                ApprovalPacket::Merge(packet) => Some(packet.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("a merge approval opened")
}

fn builds(worker: &FakeRunner) -> Vec<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .collect()
}

fn goal_phase(events: &[RunEvent]) -> Vec<(PhaseStatus, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Phase(PhaseChange {
                phase: RunPhase::Understand,
                status,
                summary,
            }) => Some((*status, summary.clone())),
            _ => None,
        })
        .collect()
}

async fn decide(
    crew: &Crew,
    outcome: &Outcome,
    kind: ApprovalKind,
    action: AnswerAction,
) -> anyhow::Result<Outcome> {
    let (command, reply) = answer(&outcome.events, kind, action, None)?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    Ok(next)
}

fn run_of(events: &[RunEvent]) -> RunId {
    events[0].run_id.clone()
}

#[tokio::test]
async fn captain_lifecycle() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    worker.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );

    // Your prompt is the goal, approved by the setting before the task exists.
    let drafted = goal_events(&outcome.events)
        .into_iter()
        .find_map(|(_, event)| match event {
            GoalEvent::Drafted(file) => Some(file.clone()),
            _ => None,
        })
        .expect("the prompt was recorded as the goal");
    assert_eq!(drafted.source, GoalSource::Prompt);
    assert_eq!(drafted.summary, REQUEST);
    let (approved_at, goal, by) = approved(&outcome.events).expect("the goal was approved");
    assert_eq!(by, Party::Config);
    assert_eq!(goal.sha256, drafted.sha256);
    assert_eq!(goal.sha256, file::sha256(&goal));
    assert_eq!(
        goal_phase(&outcome.events),
        [
            (PhaseStatus::Started, None),
            (
                PhaseStatus::Finished,
                Some("Your prompt is the goal".to_owned())
            ),
        ]
    );

    // One Captain task for the main agent, built with the goal's hash.
    let tasks = created(&outcome.events);
    assert_eq!(tasks.len(), 1);
    let (created_at, task) = &tasks[0];
    assert!(*created_at > approved_at);
    assert_eq!(task.kind, TaskKind::Captain);
    assert_eq!(task.worker, Some(WORKER.parse::<ModelRef>()?));
    assert_eq!(task.task, REQUEST);
    let build = builds(&worker);
    assert_eq!(build.len(), 1);
    assert_eq!(build[0].goal_sha256.as_deref(), Some(goal.sha256.as_str()));
    // The run's only task has no other workers to speak of, and its parts never run together.
    let sent: String = build[0]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(sent.starts_with("You are a worker."), "{sent}");
    assert!(sent.contains("You make this change on your own."), "{sent}");
    assert!(!sent.contains("Other workers build"), "{sent}");
    assert!(sent.contains("Kind: the whole change\n\n"), "{sent}");

    // Then the merge approval, and merging ends the run.
    assert_eq!(opened(&outcome.events, ApprovalKind::Merge), 1);
    assert_eq!(opened(&outcome.events, ApprovalKind::Understand), 0);
    let merged = decide(&crew, &outcome, ApprovalKind::Merge, AnswerAction::Merge).await?;
    assert_eq!(merged.exit, EngineExit::Finished(RunOutcome::Merged));

    // The worker's own answer closes the run, just before it finishes, with nobody asked again.
    let kinds: Vec<&RunEventKind> = merged.events.iter().map(|event| &event.kind).collect();
    let closing = kinds
        .iter()
        .rposition(|kind| matches!(kind, RunEventKind::Run(RunLifecycle::ClosingAnswer(_))))
        .expect("the run has a closing answer");
    let finished = kinds
        .iter()
        .rposition(|kind| matches!(kind, RunEventKind::Run(RunLifecycle::Finished { .. })))
        .expect("the run finished");
    assert!(closing < finished);
    let RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) = kinds[closing] else {
        unreachable!();
    };
    assert_eq!(
        answer.text,
        "I fixed the misspelled word in the README's install section."
    );
    assert_eq!(answer.by, Some(WORKER.parse::<ModelRef>()?));
    assert!(
        answer
            .files
            .iter()
            .all(|file| !file.path.starts_with(".argonauts/")),
        "{answer:?}"
    );
    assert!(
        builds(&worker).len() == 1
            && !worker
                .specs()
                .iter()
                .any(|spec| spec.step == Some(StepName::CloseRun)),
        "a Captain run asks nobody for its answer"
    );
    let closings = kinds
        .iter()
        .filter(|kind| matches!(kind, RunEventKind::Run(RunLifecycle::ClosingAnswer(_))))
        .count();
    assert_eq!(closings, 1);
    Ok(())
}

/// A Captain run has no planning step, so its one task is the first to meet the code it
/// started from. The checks run there before the worker's first prompt is written, which
/// tells it what already fails, as a planner is told in a crew run.
#[tokio::test]
async fn the_captains_worker_is_told_what_already_fails() -> anyhow::Result<()> {
    use argonauts_core::events::CheckOutcome;
    use argonauts_core::events::task::CheckKind;
    use argonauts_core::settings::protocol::CheckCommand;

    let (mut crew, worker) = captain_crew()?;
    crew.settings.checks.run = vec![CheckCommand {
        name: "tests".to_owned(),
        command: vec!["npm".to_owned(), "test".to_owned()],
    }];
    let failing = "not ok 3 - the bucket never holds more than its capacity";
    crew.services.checks.push_at_start(CheckOutcome {
        name: "tests".to_owned(),
        kind: CheckKind::Command,
        passed: false,
        command: Some(vec!["npm".to_owned(), "test".to_owned()]),
        exit_code: Some(1),
        duration_ms: 1,
        summary: "15 passed, 1 failed".to_owned(),
        output_tail: failing.to_owned(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    });
    worker.push(FakeSession::structured(report()));
    crew.start(RunMode::Captain, REQUEST).await?;

    assert_eq!(crew.services.checks.start_requests().len(), 1);
    let built = builds(&worker);
    let first = built.first().expect("the task was built");
    let prompt: Vec<&str> = first.prompt.iter().map(|part| part.text.as_str()).collect();
    let prompt = prompt.join("\n");
    assert!(
        prompt.contains("Already failing on the code this run started from"),
        "{prompt}"
    );
    assert!(
        prompt.contains("tests (npm test): 15 passed, 1 failed"),
        "{prompt}"
    );
    assert!(prompt.contains(failing), "{prompt}");
    Ok(())
}

#[tokio::test]
async fn a_failed_run_review_leaves_the_merge_standing() -> anyhow::Result<()> {
    let (mut crew, worker) = captain_crew()?;
    // The run review asks the main agent for a process change, and it has nothing scripted left.
    crew.settings.evolution.propose_every = 1;
    worker.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    let merged = decide(&crew, &outcome, ApprovalKind::Merge, AnswerAction::Merge).await?;
    assert_eq!(merged.exit, EngineExit::Finished(RunOutcome::Merged));

    let run_review: Vec<(PhaseStatus, bool)> = merged
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Phase(PhaseChange {
                phase: RunPhase::RunReview,
                status,
                summary,
            }) => Some((*status, summary.is_some())),
            _ => None,
        })
        .collect();
    // Why it failed rides on the phase, and the note says it in plain words.
    assert_eq!(
        run_review,
        [(PhaseStatus::Started, false), (PhaseStatus::Failed, true)]
    );
    assert!(merged.events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Note(Note { level: NoteLevel::Warn, text }) if text == RUN_REVIEW_FAILED
    )));
    let finished: Vec<RunOutcome> = merged
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some(*outcome),
            _ => None,
        })
        .collect();
    assert_eq!(finished, [RunOutcome::Merged]);
    Ok(())
}

#[tokio::test]
async fn a_pause_cancelled_before_it_settles_resumes_and_waits_once() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    // The build runs until the pause stops it; resumed, it reports.
    worker.push(FakeSession::until_stopped(Vec::new()));
    worker.push(FakeSession::structured(report()));
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Captain, REQUEST)?;
    let mut building = false;
    for _ in 0..500 {
        building = names(&run.sink.events()).contains(&"session/started".to_owned());
        if building {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(building, "{:?}", names(&run.sink.events()));

    // The cancel lands while the stopped build is still ending.
    run.commands
        .send(EngineCommand::Pause { now: true })
        .await?;
    run.commands.send(EngineCommand::CancelPause).await?;
    let exit = tokio::time::timeout(Duration::from_secs(30), run.exit).await??;
    assert_eq!(exit, EngineExit::Waiting(WaitReason::Approval));

    let events = run.sink.events();
    let kinds = names(&events);
    let count = |name: &str| kinds.iter().filter(|kind| *kind == name).count();
    assert_eq!(count("run/pause_requested"), 1, "{kinds:?}");
    assert_eq!(count("run/pause_cancelled"), 1, "{kinds:?}");
    assert_eq!(count("run/paused"), 0, "{kinds:?}");
    assert_eq!(count("run/waiting"), 1, "{kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("run/waiting"));
    assert_eq!(opened(&events, ApprovalKind::Merge), 1);
    // The stopped build ended paused, and a resumed try started only after that end.
    let causes: Vec<TryCause> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Try { cause, .. }) => Some(*cause),
            _ => None,
        })
        .collect();
    assert_eq!(causes, [TryCause::First, TryCause::Resumed]);
    let sessions: Vec<(&str, EndReason)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(_)) => {
                Some(("started", EndReason::Completed))
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => Some(("ended", end.reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        sessions,
        [
            ("started", EndReason::Completed),
            ("ended", EndReason::Paused),
            ("started", EndReason::Completed),
            ("ended", EndReason::Completed),
        ]
    );
    assert_eq!(builds(&worker).len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_stopped_run_reopens_and_resumes_its_build() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    // The build runs until the stop; reopened, it resumes and reports.
    worker.push(FakeSession::until_stopped(Vec::new()));
    worker.push(FakeSession::structured(report()));
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Captain, REQUEST)?;
    let mut building = false;
    for _ in 0..500 {
        building = names(&run.sink.events()).contains(&"session/started".to_owned());
        if building {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(building, "{:?}", names(&run.sink.events()));
    run.commands.send(EngineCommand::Stop).await?;
    let exit = tokio::time::timeout(Duration::from_secs(30), run.exit).await??;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Stopped));
    let stopped = run.sink.events();

    // Without your asking, a stopped run stays stopped.
    let again = crew.resume(stopped.clone(), Vec::new()).await?;
    assert_eq!(again.exit, EngineExit::Finished(RunOutcome::Stopped));
    assert_eq!(again.events.len(), stopped.len(), "nothing was recorded");

    let reopened = crew.reopen(stopped.clone()).await?;
    assert_eq!(reopened.exit, EngineExit::Waiting(WaitReason::Approval));
    let after = &reopened.events[stopped.len()..];
    let RunEventKind::Run(RunLifecycle::Resumed(resumed)) = &after[0].kind else {
        panic!("{:?}", names(after));
    };
    assert_eq!(resumed.cause, ResumeCause::User);
    let [session] = resumed.sessions.as_slice() else {
        panic!("{:?}", resumed.sessions);
    };
    assert!(
        session.resumed && session.cli_session.is_some(),
        "{session:?}"
    );
    let builds = builds(&worker);
    assert_eq!(builds.len(), 2);
    assert_eq!(
        builds[1].key, builds[0].key,
        "the same session, not a new one"
    );
    assert_eq!(builds[1].resume, session.cli_session);
    assert_eq!(opened(&reopened.events, ApprovalKind::Merge), 1);
    Ok(())
}

#[tokio::test]
async fn captain_understand_first_waits() -> anyhow::Result<()> {
    let (mut crew, worker) = captain_crew()?;
    crew.settings.captain.understand_first = true;
    worker.push(FakeSession::structured(reading()));
    worker.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;

    // The main agent read the request back, and nothing is built until you approve it.
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(opened(&outcome.events, ApprovalKind::Understand), 1);
    let readings: Vec<_> = goal_events(&outcome.events)
        .into_iter()
        .filter_map(|(_, event)| match event {
            GoalEvent::Read {
                label,
                worker: model,
                ..
            } => Some((label.clone(), model.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(readings, [("A".to_owned(), WORKER.parse::<ModelRef>()?)]);
    assert!(approved(&outcome.events).is_none());
    assert!(created(&outcome.events).is_empty());
    assert!(builds(&worker).is_empty());
    let read_session = worker
        .specs()
        .into_iter()
        .find(|spec| spec.step == Some(StepName::Read))
        .expect("a read session");
    assert_eq!(read_session.access, Access::ReadOnly);

    let next = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
        AnswerAction::Approve,
    )
    .await?;
    let (_, goal, by) = approved(&next.events).expect("you approved the goal");
    assert_eq!(by, Party::User);
    assert_eq!(goal.source, GoalSource::Readings);
    assert_eq!(goal.criteria[0].text, "The install section has no typo");
    assert_eq!(created(&next.events).len(), 1);
    let build = builds(&worker);
    assert_eq!(build.len(), 1);
    assert_eq!(build[0].goal_sha256.as_deref(), Some(goal.sha256.as_str()));
    Ok(())
}

#[tokio::test]
async fn captain_reviewed_other_provider() -> anyhow::Result<()> {
    let (mut crew, worker) = captain_crew()?;
    crew.settings.captain.review = true;
    // Providers come from the registry, which knows this one by its model's name.
    let reviewer = crew.worker("codex/gpt-x", "openai");
    worker.push(FakeSession::structured(report()));
    reviewer.push(FakeSession::structured(json!({
        "verdict": "approve",
        "summary": "The typo is fixed and nothing else changed",
        "comments": [],
        "tamper_suspected": false
    })));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;

    let tasks = created(&outcome.events);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].1.reviewers, ["codex/gpt-x".parse::<ModelRef>()?]);
    let reviews: Vec<SessionSpec> = reviewer
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::Review))
        .collect();
    assert_eq!(reviews.len(), 1);
    assert!(matches!(
        reviews[0].access,
        Access::ReadOnly | Access::NoTools
    ));
    assert!(
        worker
            .specs()
            .iter()
            .all(|spec| spec.step != Some(StepName::Review)),
        "the worker never reviews its own task"
    );
    assert_eq!(opened(&outcome.events, ApprovalKind::Merge), 1);
    Ok(())
}

#[tokio::test]
async fn captain_goal_file_not_a_violation() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    worker.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    assert_eq!(opened(&outcome.events, ApprovalKind::Merge), 1);

    let goal_path = file::relative_path(&run_of(&outcome.events))
        .to_string_lossy()
        .into_owned();
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated
        .iter()
        .rev()
        .find(|(point, _)| point.kind == ApprovalKind::Merge)
        .expect("the policy saw the merge approval");
    // The goal file is kept under Argonauts Code's home, so it's never part of the change.
    assert!(
        evidence.facts.conductor_paths.is_empty(),
        "{:?}",
        evidence.facts.conductor_paths
    );
    assert!(!evidence.facts.agent_paths.contains(&goal_path));
    assert!(
        !evidence
            .facts
            .agent_paths
            .iter()
            .any(|path| path.starts_with(".argonauts/"))
    );
    assert!(evidence.facts.protected_paths.is_empty());
    // The task's rows add up to its totals, with no row of Argonauts Code's own.
    let packet = merge_packet(&outcome.events);
    let rows: u32 = packet.rows.iter().map(|row| row.stat.files_changed).sum();
    assert_eq!(packet.conductor.files_changed, 0, "{packet:?}");
    assert_eq!(
        rows + packet.conductor.files_changed,
        packet.combined.files_changed,
        "{packet:?}"
    );
    assert_eq!(
        packet
            .rows
            .iter()
            .map(|row| row.stat.insertions)
            .sum::<u32>()
            + packet.conductor.insertions,
        packet.combined.insertions
    );
    let violations: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Check(check) if !check.violations.is_empty() => Some(check.clone()),
            _ => None,
        })
        .collect();
    assert!(violations.is_empty(), "{violations:?}");
    Ok(())
}

/// Best of n keeps one try; the run closes with that try's own answer, never the
/// answer of one it left on its branch.
#[tokio::test]
async fn best_of_n_closes_with_the_kept_tries_answer() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.crew.main = WORKER.parse()?;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    crew.settings.captain.tries = 2;
    crew.services
        .workers
        .add(
            FakeRunner::cli(WORKER, "anthropic").respond_with(|spec: &SessionSpec| {
                if spec.step != Some(StepName::Build) {
                    return match &spec.json_schema {
                        Some(schema) => FakeSession::structured(support::fill(schema)),
                        None => FakeSession::completed("done"),
                    };
                }
                let task = spec
                    .task
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let mut said = report();
                said["answer"] = json!(format!("The answer from {task}."));
                FakeSession::structured(said)
            }),
        );
    crew.settings.crew.add(WORKER.parse()?);
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    let packet = merge_packet(&outcome.events);
    let kept = packet.rows.first().map(|row| row.task.to_string());
    let kept = kept.ok_or_else(|| anyhow::anyhow!("the merge approval names no task"))?;
    assert!(kept.contains("-try-"), "{kept}");
    let merged = decide(&crew, &outcome, ApprovalKind::Merge, AnswerAction::Merge).await?;
    assert_eq!(merged.exit, EngineExit::Finished(RunOutcome::Merged));
    let answer = merged
        .events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) => Some(answer.clone()),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("the run has no closing answer"))?;
    assert_eq!(answer.text, format!("The answer from {kept}."));
    assert!(
        answer.left.iter().any(|line| line.contains("not kept")),
        "{answer:?}"
    );
    Ok(())
}
