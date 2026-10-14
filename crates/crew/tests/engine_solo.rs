//! Solo scenarios: one card for the default agent, your prompt as the goal unless
//! `solo.restate_first`, review by another family under `solo.review`, then the merge
//! checkpoint.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::card::{AttemptCause, CardEvent, CardKind, CardSpec};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointPacket, MergePacket,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::run::{
    ParkReason, Party, PhaseChange, ResumeCause, RunLifecycle, RunMode, RunOutcome, RunPhase,
};
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::log::{Note, NoteLevel, RETRO_FAILED};
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{Access, SessionSpec};
use argonauts_core::{PhaseStatus, RunId};
use argonauts_crew::goal::file;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

use support::{Crew, Outcome, answer, names};

const REQUEST: &str = "Fix the typo in the README's install section";
const OWNER: &str = "claude-code/opus";

fn report() -> Value {
    json!({
        "status": "done",
        "summary": "Fixed the typo",
        // The scripted owner changes no file, so it names none.
        "files_changed": [],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
        "answer": "I fixed the misspelled word in the README's install section."
    })
}

fn restatement() -> Value {
    json!({
        "summary": "Fix the misspelled word in the README's install section.",
        "success_criteria": ["The install section has no typo"],
        "in_scope": ["README.md"],
        "out_of_scope": ["every other file"],
        "assumptions": [],
        "open_questions": []
    })
}

/// A solo crew whose default agent is a fake Claude Code.
fn solo_crew() -> anyhow::Result<(Crew, Arc<FakeWorker>)> {
    let mut crew = Crew::new()?;
    crew.settings.main_agent = OWNER.parse()?;
    // Each phase end would ask the default agent for a workflow change, and the retro would ask
    // for a process change. These scenarios script neither session.
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let owner = crew.worker(OWNER, "claude");
    Ok((crew, owner))
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

fn created(events: &[RunEvent]) -> Vec<(usize, CardSpec)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match &event.kind {
            RunEventKind::Card(CardEvent::Created(spec)) => Some((at, spec.as_ref().clone())),
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent], kind: CheckpointKind) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == kind)
        })
        .count()
}

fn merge_packet(events: &[RunEvent]) -> MergePacket {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => match &opened.packet {
                CheckpointPacket::Merge(packet) => Some(packet.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("a merge approval opened")
}

fn builds(worker: &FakeWorker) -> Vec<SessionSpec> {
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
                phase: RunPhase::Goal,
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
    kind: CheckpointKind,
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
async fn solo_lifecycle() -> anyhow::Result<()> {
    let (crew, owner) = solo_crew()?;
    owner.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Solo, REQUEST).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );

    // Your prompt is the goal, approved by the setting before the card exists.
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

    // One solo card for the default agent, built with the goal's hash.
    let cards = created(&outcome.events);
    assert_eq!(cards.len(), 1);
    let (created_at, card) = &cards[0];
    assert!(*created_at > approved_at);
    assert_eq!(card.kind, CardKind::Solo);
    assert_eq!(card.owner, Some(OWNER.parse::<WorkerRef>()?));
    assert_eq!(card.task, REQUEST);
    let build = builds(&owner);
    assert_eq!(build.len(), 1);
    assert_eq!(build[0].goal_sha256.as_deref(), Some(goal.sha256.as_str()));
    // The run's only card has no other owners to speak of, and its parts never run together.
    let sent: String = build[0]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(sent.starts_with("You are a worker."), "{sent}");
    assert!(sent.contains("You make this change on your own."), "{sent}");
    assert!(!sent.contains("Other workers build"), "{sent}");
    assert!(sent.contains("Kind: the whole change\n\n"), "{sent}");

    // Then the merge checkpoint, and merging ends the run.
    assert_eq!(opened(&outcome.events, CheckpointKind::Merge), 1);
    assert_eq!(opened(&outcome.events, CheckpointKind::Goal), 0);
    let merged = decide(&crew, &outcome, CheckpointKind::Merge, AnswerAction::Merge).await?;
    assert_eq!(merged.exit, EngineExit::Finished(RunOutcome::Merged));

    // The owner's own answer closes the run, just before it finishes, with nobody asked again.
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
    assert_eq!(answer.by, Some(OWNER.parse::<WorkerRef>()?));
    assert!(
        answer
            .files
            .iter()
            .all(|file| !file.path.starts_with(".argonauts/")),
        "{answer:?}"
    );
    assert!(
        builds(&owner).len() == 1
            && !owner
                .specs()
                .iter()
                .any(|spec| spec.step == Some(StepName::CloseRun)),
        "a solo run asks nobody for its answer"
    );
    let closings = kinds
        .iter()
        .filter(|kind| matches!(kind, RunEventKind::Run(RunLifecycle::ClosingAnswer(_))))
        .count();
    assert_eq!(closings, 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_retro_leaves_the_merge_standing() -> anyhow::Result<()> {
    let (mut crew, owner) = solo_crew()?;
    // The retro asks the default agent for a process change, and it has nothing scripted left.
    crew.settings.evolution.propose_every = 1;
    owner.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Solo, REQUEST).await?;
    let merged = decide(&crew, &outcome, CheckpointKind::Merge, AnswerAction::Merge).await?;
    assert_eq!(merged.exit, EngineExit::Finished(RunOutcome::Merged));

    let retro: Vec<(PhaseStatus, bool)> = merged
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Phase(PhaseChange {
                phase: RunPhase::Retro,
                status,
                summary,
            }) => Some((*status, summary.is_some())),
            _ => None,
        })
        .collect();
    // Why it failed rides on the phase, and the note says it in plain words.
    assert_eq!(
        retro,
        [(PhaseStatus::Started, false), (PhaseStatus::Failed, true)]
    );
    assert!(merged.events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Note(Note { level: NoteLevel::Warn, text }) if text == RETRO_FAILED
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
async fn a_pause_cancelled_before_it_settles_resumes_and_parks_once() -> anyhow::Result<()> {
    let (crew, owner) = solo_crew()?;
    // The build runs until the pause stops it; resumed, it reports.
    owner.push(FakeSession::until_stopped(Vec::new()));
    owner.push(FakeSession::structured(report()));
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Solo, REQUEST)?;
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
    assert_eq!(exit, EngineExit::Parked(ParkReason::Checkpoint));

    let events = run.sink.events();
    let kinds = names(&events);
    let count = |name: &str| kinds.iter().filter(|kind| *kind == name).count();
    assert_eq!(count("run/pause_requested"), 1, "{kinds:?}");
    assert_eq!(count("run/pause_cancelled"), 1, "{kinds:?}");
    assert_eq!(count("run/paused"), 0, "{kinds:?}");
    assert_eq!(count("run/parked"), 1, "{kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("run/parked"));
    assert_eq!(opened(&events, CheckpointKind::Merge), 1);
    // The stopped build ended paused, and a resumed attempt started only after that end.
    let causes: Vec<AttemptCause> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Card(CardEvent::Attempt { cause, .. }) => Some(*cause),
            _ => None,
        })
        .collect();
    assert_eq!(causes, [AttemptCause::First, AttemptCause::Resumed]);
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
    assert_eq!(builds(&owner).len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_stopped_run_reopens_and_resumes_its_build() -> anyhow::Result<()> {
    let (crew, owner) = solo_crew()?;
    // The build runs until the stop; reopened, it resumes and reports.
    owner.push(FakeSession::until_stopped(Vec::new()));
    owner.push(FakeSession::structured(report()));
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Solo, REQUEST)?;
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
    assert_eq!(reopened.exit, EngineExit::Parked(ParkReason::Checkpoint));
    let after = &reopened.events[stopped.len()..];
    let RunEventKind::Run(RunLifecycle::Resumed(resumed)) = &after[0].kind else {
        panic!("{:?}", names(after));
    };
    assert_eq!(resumed.cause, ResumeCause::User);
    let [session] = resumed.sessions.as_slice() else {
        panic!("{:?}", resumed.sessions);
    };
    assert!(
        session.resumed && session.vendor_session.is_some(),
        "{session:?}"
    );
    let builds = builds(&owner);
    assert_eq!(builds.len(), 2);
    assert_eq!(
        builds[1].key, builds[0].key,
        "the same session, not a new one"
    );
    assert_eq!(builds[1].resume, session.vendor_session);
    assert_eq!(opened(&reopened.events, CheckpointKind::Merge), 1);
    Ok(())
}

#[tokio::test]
async fn solo_restate_first_waits() -> anyhow::Result<()> {
    let (mut crew, owner) = solo_crew()?;
    crew.settings.solo.restate_first = true;
    owner.push(FakeSession::structured(restatement()));
    owner.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Solo, REQUEST).await?;

    // The default agent restated the goal, and nothing is built until you approve it.
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(opened(&outcome.events, CheckpointKind::Goal), 1);
    let restated: Vec<_> = goal_events(&outcome.events)
        .into_iter()
        .filter_map(|(_, event)| match event {
            GoalEvent::Restated { label, worker, .. } => Some((label.clone(), worker.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(restated, [("A".to_owned(), OWNER.parse::<WorkerRef>()?)]);
    assert!(approved(&outcome.events).is_none());
    assert!(created(&outcome.events).is_empty());
    assert!(builds(&owner).is_empty());
    let restate = owner
        .specs()
        .into_iter()
        .find(|spec| spec.step == Some(StepName::Restate))
        .expect("a restate session");
    assert_eq!(restate.access, Access::ReadOnly);

    let next = decide(&crew, &outcome, CheckpointKind::Goal, AnswerAction::Approve).await?;
    let (_, goal, by) = approved(&next.events).expect("you approved the goal");
    assert_eq!(by, Party::User);
    assert_eq!(goal.source, GoalSource::Restatements);
    assert_eq!(goal.criteria[0].text, "The install section has no typo");
    assert_eq!(created(&next.events).len(), 1);
    let build = builds(&owner);
    assert_eq!(build.len(), 1);
    assert_eq!(build[0].goal_sha256.as_deref(), Some(goal.sha256.as_str()));
    Ok(())
}

#[tokio::test]
async fn solo_reviewed_other_provider() -> anyhow::Result<()> {
    let (mut crew, owner) = solo_crew()?;
    crew.settings.solo.review = true;
    // Families come from the registry, which knows this one by its model's name.
    let reviewer = crew.worker("codex/gpt-x", "gpt");
    owner.push(FakeSession::structured(report()));
    reviewer.push(FakeSession::structured(json!({
        "verdict": "approve",
        "summary": "The typo is fixed and nothing else changed",
        "comments": [],
        "tamper_suspected": false
    })));
    let outcome = crew.start(RunMode::Solo, REQUEST).await?;

    let cards = created(&outcome.events);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].1.reviewers, ["codex/gpt-x".parse::<WorkerRef>()?]);
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
        owner
            .specs()
            .iter()
            .all(|spec| spec.step != Some(StepName::Review)),
        "the worker never reviews its own task"
    );
    assert_eq!(opened(&outcome.events, CheckpointKind::Merge), 1);
    Ok(())
}

#[tokio::test]
async fn solo_goal_file_not_a_violation() -> anyhow::Result<()> {
    let (crew, owner) = solo_crew()?;
    owner.push(FakeSession::structured(report()));
    let outcome = crew.start(RunMode::Solo, REQUEST).await?;
    assert_eq!(opened(&outcome.events, CheckpointKind::Merge), 1);

    let goal_path = file::relative_path(&run_of(&outcome.events))
        .to_string_lossy()
        .into_owned();
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated
        .iter()
        .rev()
        .find(|(point, _)| point.kind == CheckpointKind::Merge)
        .expect("the policy saw the merge approval");
    // The goal file is kept under Argonauts' home, so it's never part of the change.
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
    // The card's rows add up to its totals, with no row of Argonauts' own.
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
            RunEventKind::Gate(gate) if !gate.violations.is_empty() => Some(gate.clone()),
            _ => None,
        })
        .collect();
    assert!(violations.is_empty(), "{violations:?}");
    Ok(())
}

/// Best of n keeps one attempt; the run closes with that attempt's own answer, never the
/// answer of one it left on its branch.
#[tokio::test]
async fn best_of_n_closes_with_the_kept_attempts_answer() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.main_agent = OWNER.parse()?;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    crew.settings.solo.attempts = 2;
    crew.services
        .workers
        .add(
            FakeWorker::harness(OWNER, "claude").respond_with(|spec: &SessionSpec| {
                if spec.step != Some(StepName::Build) {
                    return match &spec.json_schema {
                        Some(schema) => FakeSession::structured(support::fill(schema)),
                        None => FakeSession::completed("done"),
                    };
                }
                let card = spec
                    .card
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let mut said = report();
                said["answer"] = json!(format!("The answer from {card}."));
                FakeSession::structured(said)
            }),
        );
    crew.settings.crew.add(OWNER.parse()?);
    let outcome = crew.start(RunMode::Solo, REQUEST).await?;
    let packet = merge_packet(&outcome.events);
    let kept = packet.rows.first().map(|row| row.card.to_string());
    let kept = kept.ok_or_else(|| anyhow::anyhow!("the merge approval names no task"))?;
    assert!(kept.contains("-attempt-"), "{kept}");
    let merged = decide(&crew, &outcome, CheckpointKind::Merge, AnswerAction::Merge).await?;
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
