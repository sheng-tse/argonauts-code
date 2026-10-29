//! Decision points: answers bound to the evidence hash, and the first decision winning over a
//! slow judge.

use crate::support;
use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened};
use argonauts_core::events::run::{Party, RunMode, WaitReason};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ApprovalId, ModelRef};
use argonauts_core::policy::{PolicyOutcome, Verdict};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_testkit::deadline;

const TWO_PHASES: &str = "name: two-looks\ndescription: Two looks\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: checks\n  - name: Second look\n    standard: check\n    steps:\n      - kind: checks\n";

/// A crew whose workflow ends its first phase with the main agent proposing one more phase, which
/// opens a `change_request` approval.
fn crew_with_a_change(mode: ApprovalMode) -> anyhow::Result<support::Crew> {
    let mut crew = support::Crew::new()?;
    let main_agent = crew.worker("claude-code/opus", "anthropic");
    crew.settings.crew.main = ModelRef::new("claude-code", Some("opus".to_owned()));
    crew.settings.workflow = "two-looks".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.approvals.change_request.mode = mode;
    crew.repo
        .write(".argonauts/workflows/two-looks.yaml", TWO_PHASES)?;
    let changed = format!(
        "{TWO_PHASES}  - name: Extra look\n    standard: check\n    steps:\n      - kind: checks\n"
    );
    let change = support::brief(None, json!([]), Some(&changed), "one more look");
    let unchanged = support::brief(None, json!([]), None, "it fits");
    support::script(
        &main_agent,
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

fn opened(events: &[RunEvent]) -> Option<ApprovalOpened> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some((**opened).clone()),
        _ => None,
    })
}

fn answer(
    opened: &ApprovalOpened,
    action: AnswerAction,
    sha256: &str,
) -> (EngineCommand, oneshot::Receiver<Result<(), String>>) {
    let (reply, receiver) = oneshot::channel();
    let answer = ApprovalAnswer {
        id: opened.id.clone(),
        action,
        text: None,
        evidence_sha256: sha256.to_owned(),
        tasks: Vec::new(),
    };
    (EngineCommand::Answer { answer, reply }, receiver)
}

#[tokio::test]
async fn approval_hash_mismatch_refused() -> anyhow::Result<()> {
    let crew = crew_with_a_change(ApprovalMode::Supervised)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(outcome.exit, EngineExit::Waiting(WaitReason::Approval));
    let question = opened(&outcome.events).unwrap_or_else(|| panic!("no approval opened"));
    assert_eq!(question.kind, ApprovalKind::ChangeRequest);
    let mut history = outcome.events;

    // Evidence you didn't see is never approved.
    let (command, reply) = answer(&question, AnswerAction::Approve, "not-the-shown-hash");
    let resumed = crew.resume(history.clone(), vec![command]).await?;
    let refused = reply.await?;
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("evidence changed")),
        "{refused:?}"
    );
    history.extend(resumed.events);

    // Nor is an answer the question didn't offer.
    let (command, reply) = answer(&question, AnswerAction::Merge, &question.evidence_sha256);
    let resumed = crew.resume(history.clone(), vec![command]).await?;
    assert!(reply.await?.is_err());
    history.extend(resumed.events);
    assert!(history.iter().all(|event| !matches!(
        event.kind,
        RunEventKind::Approval(ApprovalEvent::Decided(_))
    )));

    let (command, reply) = answer(&question, AnswerAction::Approve, &question.evidence_sha256);
    let resumed = crew.resume(history.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    history.extend(resumed.events);
    let decided: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(decided.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0].evidence_sha256, question.evidence_sha256);
    assert_eq!(decided[0].by, Party::User);

    let (command, reply) = answer(&question, AnswerAction::Approve, &question.evidence_sha256);
    crew.resume(history.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Err("already decided".to_owned()));

    // You decided it, so there's nothing to spot-check.
    let (command, reply) = spot_check(&question.id, true);
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
    id: &ApprovalId,
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
/// policy once, against the approval it approved, even after the run finished.
#[tokio::test]
async fn spot_check_verdict_reaches_the_policy() -> anyhow::Result<()> {
    let crew = crew_with_a_change(ApprovalMode::Auto)?;
    crew.services.policy.decide_next(PolicyOutcome::Decided {
        verdict: Verdict::Approve,
        by: DecideWith::Rules,
        judge: None,
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
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { id }) => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no spot check was queued"));

    let (unknown, unknown_reply) = spot_check(&ApprovalId::numbered(99), true);
    let (first, first_reply) = spot_check(&queued, false);
    let (again, again_reply) = spot_check(&queued, true);
    crew.resume(outcome.events, vec![unknown, first, again])
        .await?;
    assert!(
        unknown_reply
            .await?
            .is_err_and(|why| why.contains("no approval"))
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
    assert_eq!(point.approval, queued);
    assert_eq!(point.kind, ApprovalKind::ChangeRequest);
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
    use argonauts_crew::run_review::outcomes::{self, LogLine};
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

/// Turning the crew's work down at an approval is a reversal in the outcome log, against the
/// run; approving it is not.
#[tokio::test]
async fn turning_work_down_at_an_approval_is_a_reversal() -> anyhow::Result<()> {
    let crew = crew_with_a_change(ApprovalMode::Supervised)?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    let question = opened(&outcome.events).unwrap_or_else(|| panic!("no approval opened"));
    assert_eq!(question.kind, ApprovalKind::ChangeRequest);
    assert!(reversals_of(&crew)?.is_empty());
    let turned_down = question
        .options
        .iter()
        .map(|option| option.action.clone())
        .find(|action| argonauts_crew::run_review::reversals::rejects(question.kind, action))
        .unwrap_or_else(|| panic!("the question offers no way to turn the change down"));
    let (command, reply) = answer(&question, turned_down, &question.evidence_sha256);
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

/// In auto mode your judge takes a while. You answer first, so your answer wins; the
/// judge's late outcome is kept only as an auto preview check, and `user_decided` is called once.
#[tokio::test]
async fn user_answer_beats_slow_judge() -> anyhow::Result<()> {
    let mut crew = crew_with_a_change(ApprovalMode::Supervised)?;
    // Only the run's start, so the main agent's first scripted answer, the change, goes to the run
    // below, now in auto.
    let started = vec![crew.run_started(RunMode::Crew, "look twice")?];
    crew.settings.approvals.change_request.mode = ApprovalMode::Auto;
    crew.services.policy.decide_next_after(
        PolicyOutcome::Decided {
            verdict: Verdict::Approve,
            by: DecideWith::Model,
            judge: Some(ModelRef::new("codex", None)),
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

    let question = tokio::time::timeout(deadline::scaled(Duration::from_secs(10)), async {
        loop {
            if let Some(question) = opened(&sink.events()) {
                break question;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(!question.needs_you, "auto starts with the policy deciding");
    let (command, reply) = answer(&question, AnswerAction::SendBack, &question.evidence_sha256);
    commands.send(command).await?;
    assert_eq!(reply.await?, Ok(()));
    let exit = tokio::time::timeout(deadline::scaled(Duration::from_secs(10)), run).await??;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");
    tokio::time::sleep(Duration::from_millis(600)).await;

    let events = sink.events();
    let decided: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.id == question.id =>
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
        RunEventKind::Approval(ApprovalEvent::Checked(check)) if check.id == question.id => {
            Some(check.clone())
        }
        _ => None,
    });
    let late = late.unwrap_or_else(|| panic!("the judge's outcome wasn't kept"));
    assert!(late.auto_preview);
    assert_eq!(late.verdict, Some(Verdict::Approve));
    let decisions = crew.services.policy.decisions();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].point.approval, question.id);
    assert_eq!(decisions[0].action, AnswerAction::SendBack);
    // Sent back, so no change was recorded.
    assert!(events.iter().all(|event| !matches!(
        event.kind,
        RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Changed(_))
    )));
    Ok(())
}

/// In autopilot nothing waits for you, so `approvals.ask_you_about` doesn't keep a workflow
/// change yours: your rules and the judge weigh it as autopilot weighs any change.
#[tokio::test]
async fn ask_you_about_leaves_a_workflow_change_to_autopilot() -> anyhow::Result<()> {
    let mut crew = crew_with_a_change(ApprovalMode::Autopilot)?;
    crew.settings.approvals.ask_you_about = vec!["Workflow change".to_owned()];
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    let changes: Vec<ApprovalOpened> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::ChangeRequest =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .collect();
    assert!(!changes.is_empty(), "{:?}", outcome.exit);
    for change in &changes {
        assert_eq!(change.mode, ApprovalMode::Autopilot, "{change:?}");
        assert!(!change.needs_you, "{change:?}");
    }
    Ok(())
}

/// `approvals.ask_you_about` brings a workflow change to you even in auto, where the judge
/// would otherwise decide it.
#[tokio::test]
async fn ask_you_about_keeps_a_workflow_change_yours_in_auto() -> anyhow::Result<()> {
    let mut crew = crew_with_a_change(ApprovalMode::Auto)?;
    crew.settings.approvals.ask_you_about = vec!["Workflow change".to_owned()];
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(outcome.exit, EngineExit::Waiting(WaitReason::Approval));
    let question = opened(&outcome.events).unwrap_or_else(|| panic!("no approval opened"));
    assert_eq!(question.kind, ApprovalKind::ChangeRequest);
    assert_eq!(question.mode, ApprovalMode::Supervised);
    assert!(question.needs_you);
    Ok(())
}
