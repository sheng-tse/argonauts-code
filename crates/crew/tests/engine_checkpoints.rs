//! Decision points: answers bound to the evidence hash, and the first decision winning over a
//! slow approver.

mod support;

use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened,
};
use argonauts_core::events::run::{ParkReason, Party, RunMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{CheckpointId, WorkerRef};
use argonauts_core::policy::{PolicyOutcome, Verdict};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::workflow::WorkflowRef;

const TWO_PHASES: &str = "name: two-looks\ndescription: Two looks\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: gate\n  - name: Second look\n    standard: check\n    steps:\n      - kind: gate\n";

/// A crew whose workflow ends its first phase with the lead proposing one more phase, which
/// opens a `change_request` checkpoint.
fn crew_with_a_change(mode: ApprovalMode) -> anyhow::Result<support::Crew> {
    let mut crew = support::Crew::new()?;
    let lead = crew.worker("claude-code/opus", "anthropic");
    crew.settings.default_agent = WorkerRef::new("claude-code", Some("opus".to_owned()));
    crew.settings.workflow = WorkflowRef::new("two-looks");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.approvals.change_request.mode = mode;
    crew.repo
        .write(".argonauts/workflows/two-looks.yaml", TWO_PHASES)?;
    let changed = format!(
        "{TWO_PHASES}  - name: Extra look\n    standard: check\n    steps:\n      - kind: gate\n"
    );
    let change =
        json!({"template": null, "params": [], "composed": changed, "why": "one more look"});
    let unchanged = json!({"template": null, "params": [], "composed": null, "why": "it fits"});
    support::script(
        &lead,
        vec![
            change.clone(),
            unchanged.clone(),
            unchanged.clone(),
            change,
            unchanged.clone(),
            unchanged,
        ],
    );
    Ok(crew)
}

fn opened(events: &[RunEvent]) -> Option<CheckpointOpened> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => Some((**opened).clone()),
        _ => None,
    })
}

fn answer(
    opened: &CheckpointOpened,
    action: AnswerAction,
    sha256: &str,
) -> (EngineCommand, oneshot::Receiver<Result<(), String>>) {
    let (reply, receiver) = oneshot::channel();
    let answer = CheckpointAnswer {
        id: opened.id.clone(),
        action,
        text: None,
        evidence_sha256: sha256.to_owned(),
        cards: Vec::new(),
    };
    (EngineCommand::Answer { answer, reply }, receiver)
}

#[tokio::test]
async fn approval_hash_mismatch_refused() -> anyhow::Result<()> {
    let crew = crew_with_a_change(ApprovalMode::Supervised)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(outcome.exit, EngineExit::Parked(ParkReason::Checkpoint));
    let card = opened(&outcome.events).unwrap_or_else(|| panic!("no checkpoint opened"));
    assert_eq!(card.kind, CheckpointKind::ChangeRequest);
    let mut history = outcome.events;

    // Evidence you didn't see is never approved.
    let (command, reply) = answer(&card, AnswerAction::Approve, "not-the-shown-hash");
    let resumed = crew.resume(history.clone(), vec![command]).await?;
    let refused = reply.await?;
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("evidence changed")),
        "{refused:?}"
    );
    history.extend(resumed.events);

    // Nor is an answer the card didn't offer.
    let (command, reply) = answer(&card, AnswerAction::Merge, &card.evidence_sha256);
    let resumed = crew.resume(history.clone(), vec![command]).await?;
    assert!(reply.await?.is_err());
    history.extend(resumed.events);
    assert!(history.iter().all(|event| !matches!(
        event.kind,
        RunEventKind::Checkpoint(CheckpointEvent::Decided(_))
    )));

    let (command, reply) = answer(&card, AnswerAction::Approve, &card.evidence_sha256);
    let resumed = crew.resume(history.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    history.extend(resumed.events);
    let decided: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => Some(decided.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0].evidence_sha256, card.evidence_sha256);
    assert_eq!(decided[0].by, Party::User);

    let (command, reply) = answer(&card, AnswerAction::Approve, &card.evidence_sha256);
    crew.resume(history.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Err("already decided".to_owned()));

    // You decided it, so there's nothing to spot-check.
    let (command, reply) = spot_check(&card.id, true);
    crew.resume(history, vec![command]).await?;
    assert!(
        reply
            .await?
            .is_err_and(|why| why.contains("isn't waiting for a spot check"))
    );
    assert!(crew.services.policy.spot_checks().is_empty());
    Ok(())
}

fn spot_check(
    id: &CheckpointId,
    correct: bool,
) -> (EngineCommand, oneshot::Receiver<Result<(), String>>) {
    let (reply, receiver) = oneshot::channel();
    let command = EngineCommand::SpotCheck {
        id: id.clone(),
        correct,
        reply,
    };
    (command, receiver)
}

/// Your rules approve the change and queue it for a spot check. Your verdict reaches the
/// policy once, against the checkpoint it approved, even after the run finished.
#[tokio::test]
async fn spot_check_verdict_reaches_the_policy() -> anyhow::Result<()> {
    let crew = crew_with_a_change(ApprovalMode::Auto)?;
    crew.services.policy.decide_next(PolicyOutcome::Decided {
        verdict: Verdict::Approve,
        by: DecideWith::Rules,
        approver: None,
        reasons: vec!["within your rules".to_owned()],
        spot_check: true,
    });
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}",
        outcome.exit
    );
    let queued = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued { id }) => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no spot check was queued"));

    let (unknown, unknown_reply) = spot_check(&CheckpointId::numbered(99), true);
    let (first, first_reply) = spot_check(&queued, false);
    let (again, again_reply) = spot_check(&queued, true);
    crew.resume(outcome.events, vec![unknown, first, again])
        .await?;
    assert!(
        unknown_reply
            .await?
            .is_err_and(|why| why.contains("no checkpoint"))
    );
    assert_eq!(first_reply.await?, Ok(()));
    assert!(
        again_reply
            .await?
            .is_err_and(|why| why.contains("already spot-checked"))
    );

    let checks = crew.services.policy.spot_checks();
    assert_eq!(checks.len(), 1);
    let (point, correct) = &checks[0];
    assert_eq!(point.checkpoint, queued);
    assert_eq!(point.kind, CheckpointKind::ChangeRequest);
    assert!(!correct);
    // A wrong auto-approval counts against the run, once.
    let reversals = reversals_of(&crew)?;
    assert_eq!(reversals.len(), 1, "{reversals:?}");
    assert!(reversals[0].what.contains("spot check"), "{reversals:?}");
    Ok(())
}

/// The reversals the run's outcome log holds.
fn reversals_of(
    crew: &support::Crew,
) -> anyhow::Result<Vec<argonauts_learning::outcomes::Reversal>> {
    use argonauts_crew::retro::outcomes::{self, LogLine};
    let id = argonauts_core::RepoId::from_canonical_path(crew.repo.root());
    let state_dir = crew.repo.home().join("state").join(id.as_str());
    let run = argonauts_core::RunId::new(argonauts_testkit::services::TEST_RUN)?;
    Ok(outcomes::run_lines(&state_dir, &run)?
        .into_iter()
        .filter_map(|line| match line {
            LogLine::Reversal(reversal) => Some(reversal),
            _ => None,
        })
        .collect())
}

/// Turning the crew's work down at a checkpoint is a reversal in the outcome log, against the
/// run; approving it is not.
#[tokio::test]
async fn turning_work_down_at_a_checkpoint_is_a_reversal() -> anyhow::Result<()> {
    let crew = crew_with_a_change(ApprovalMode::Supervised)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    let card = opened(&outcome.events).unwrap_or_else(|| panic!("no checkpoint opened"));
    assert_eq!(card.kind, CheckpointKind::ChangeRequest);
    assert!(reversals_of(&crew)?.is_empty());
    let turned_down = card
        .options
        .iter()
        .map(|option| option.action.clone())
        .find(|action| argonauts_crew::retro::reversals::rejects(card.kind, action))
        .unwrap_or_else(|| panic!("the card offers no way to turn the change down"));
    let (command, reply) = answer(&card, turned_down, &card.evidence_sha256);
    crew.resume(outcome.events, vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    let reversals = reversals_of(&crew)?;
    assert_eq!(reversals.len(), 1, "{reversals:?}");
    assert!(
        reversals[0].what.contains("change request"),
        "{reversals:?}"
    );
    Ok(())
}

/// In auto mode your approver takes a while. You answer first, so your answer wins; the
/// approver's late outcome is kept only as a shadow check, and `user_decided` is called once.
#[tokio::test]
async fn user_answer_beats_slow_approver() -> anyhow::Result<()> {
    let mut crew = crew_with_a_change(ApprovalMode::Supervised)?;
    // Only the run's start, so the lead's first scripted answer, the change, goes to the run
    // below, now in auto.
    let started = vec![crew.run_started(RunMode::Crew, "look twice")?];
    crew.settings.approvals.change_request.mode = ApprovalMode::Auto;
    crew.services.policy.decide_next_after(
        PolicyOutcome::Decided {
            verdict: Verdict::Approve,
            by: DecideWith::Model,
            approver: Some(WorkerRef::new("codex", None)),
            reasons: vec!["looks fine".to_owned()],
            spot_check: false,
        },
        Duration::from_millis(400),
    );
    // Git in the engine reads the test repo's config and home from the environment.
    let _env = crew.repo.env_guard();
    let (context, sink, commands) = crew.context(started)?;
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew);
    let run = tokio::spawn(engine.run(context));

    let card = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(card) = opened(&sink.events()) {
                break card;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(!card.needs_you, "auto starts with the policy deciding");
    let (command, reply) = answer(&card, AnswerAction::SendBack, &card.evidence_sha256);
    commands.send(command).await?;
    assert_eq!(reply.await?, Ok(()));
    let exit = tokio::time::timeout(Duration::from_secs(10), run).await??;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");
    tokio::time::sleep(Duration::from_millis(600)).await;

    let events = sink.events();
    let decided: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
                if decided.id == card.id =>
            {
                Some(decided.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0].by, Party::User);
    assert_eq!(decided[0].action, AnswerAction::SendBack);
    let late = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Checked(check)) if check.id == card.id => {
            Some(check.clone())
        }
        _ => None,
    });
    let late = late.unwrap_or_else(|| panic!("the approver's outcome wasn't kept"));
    assert!(late.shadow);
    assert_eq!(late.verdict, Some(Verdict::Approve));
    let decisions = crew.services.policy.decisions();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].point.checkpoint, card.id);
    assert_eq!(decisions[0].action, AnswerAction::SendBack);
    // Sent back, so no change was recorded.
    assert!(events.iter().all(|event| !matches!(
        event.kind,
        RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Changed(_))
    )));
    Ok(())
}
