//! A run that waits on a worker's question and is woken by something other than its answer, as
//! Shift+Tab, guidance or a crash wake it: the new engine still knows the question. Replayed from
//! a Captain run's record, its paths replaced, which waited on such a question when Shift+Tab
//! turned autopilot on and then ended "blocked" with nothing merged.

mod support;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serde_json::json;
use tokio::sync::oneshot;

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{ApprovalEvent, ApprovalKind};
use argonauts_core::events::run::{ResumeCause, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind};
use argonauts_core::events::task::{TaskEvent, TaskState};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::policy::EvidencePacket;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::steps::StepName;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_crew::state::RunState;
use argonauts_testkit::runner::{FakeRunner, FakeSession};
use argonauts_testkit::services::TEST_RUN;

use support::{Crew, Outcome};

const RECORD: &str = include_str!("fixtures/runs/captain-review-loop.jsonl");
/// What the question's approval was opened over, as the run kept it.
const EVIDENCE: &str = include_str!("fixtures/runs/captain-review-loop.evidence.json");
const TASK: &str = "task-fix-the-money-rounding";

/// Claude Code works and is the main agent, Codex reviews and Antigravity CLI judges, as in the
/// recorded run. The main agent answers a question in a sentence; reviews accept.
fn crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let answers = |spec: &argonauts_core::session_runner::SessionSpec| match spec.step {
        Some(StepName::ModelJudge) => FakeSession::structured(json!({
            "verdict": "approve",
            "reasons": ["the evidence meets the rules"],
            "rules_cited": ["R7"]
        })),
        None if spec.json_schema.is_none() => {
            FakeSession::completed("Block the merge: the sign-off already decides.")
        }
        _ => support::smallest_answer(spec),
    };
    crew.services
        .workers
        .add(FakeRunner::cli("claude-code", "anthropic").respond_with(answers));
    crew.services
        .workers
        .add(FakeRunner::cli("codex", "openai").respond_with(answers));
    crew.services
        .workers
        .add(FakeRunner::cli("antigravity-cli", "google").respond_with(answers));
    crew.settings.crew = CrewSettings {
        main: "claude-code".parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec!["claude-code".parse()?],
        reviewer: vec!["codex".parse()?],
        judge: vec!["codex".parse()?, "antigravity-cli".parse()?],
        worker_workflows: false,
        merger: Vec::new(),
    };
    crew.settings.workflow = "orchestrator".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    Ok(crew)
}

/// The record up to the wait on the question, its paths those of this test's repo, and the
/// task's branch made where the record had it.
fn history(crew: &Crew) -> anyhow::Result<Vec<RunEvent>> {
    let repo = crew.repo_context()?;
    let base = crew.base_commit()?;
    let root = crew.repo.root().display().to_string();
    let run_dir = repo.run_dir.display().to_string();
    let worktrees = repo.worktrees.display().to_string();
    let mut history = support::recorded_run(
        RECORD,
        &[
            ("{run_dir}", &run_dir),
            ("{worktrees}", &worktrees),
            ("{run}", TEST_RUN),
            ("{repo}", &root),
            ("{base_commit}", base.trim()),
        ],
    );
    // The evidence names the run, so this run's copy has a hash of its own.
    let evidence: EvidencePacket = serde_json::from_str(&EVIDENCE.replace("{run}", TEST_RUN))?;
    let folder = repo.run_dir.join("evidence");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(
        folder.join("approval-001.json"),
        serde_json::to_vec(&evidence)?,
    )?;
    for event in &mut history {
        if let RunEventKind::Approval(ApprovalEvent::Opened(opened)) = &mut event.kind {
            opened.evidence_sha256 = evidence.sha256();
        }
    }
    for branch in ["merged", TASK] {
        crew.repo.git(&[
            "branch",
            &format!("argonauts/{TEST_RUN}/{branch}"),
            base.trim(),
        ])?;
    }
    Ok(history)
}

/// Relaunches the waiting run with `commands` queued, as the run host does with the requests
/// it saved, with the real approval policy, until it waits or finishes again.
async fn relaunch(
    crew: &Crew,
    history: Vec<RunEvent>,
    commands: Vec<EngineCommand>,
) -> anyhow::Result<Outcome> {
    let (mut context, sink, sender) = crew.context(history)?;
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
    for command in commands {
        sender.send(command).await.ok().context("the inbox")?;
    }
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Captain).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

/// The events this engine added to the record.
fn added(outcome: &Outcome, history: &[RunEvent]) -> Vec<RunEvent> {
    let last = history.last().map_or(0, |event| event.seq);
    outcome
        .events
        .iter()
        .filter(|event| event.seq > last)
        .cloned()
        .collect()
}

fn resumed_cause(events: &[RunEvent]) -> Option<ResumeCause> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Resumed(resumed)) => Some(resumed.cause),
        _ => None,
    })
}

fn stopped_blocked(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::StepFinished(finished))
            if finished.summary.as_deref().is_some_and(|summary| summary.contains("it is blocked")))
    })
}

/// The record itself: the task blocked on m-001, approval-001 open, and the run waiting on it.
#[tokio::test]
async fn the_record_waits_on_the_question() -> anyhow::Result<()> {
    let crew = crew()?;
    let history = history(&crew)?;
    let state = RunState::fold(&history);
    let task = argonauts_core::TaskId::new(TASK)?;
    assert_eq!(state.tasks.by_id[&task].state, TaskState::Blocked);
    let open: Vec<_> = state.approvals.open().map(|r| r.opened.kind).collect();
    assert_eq!(open, [ApprovalKind::Question]);
    assert!(state.run.waiting.is_some());
    Ok(())
}

/// Shift+Tab on the waiting run: the relaunched engine rebuilds the question from the record and
/// autopilot answers it at once, so the task goes on instead of ending blocked. The wake is
/// recorded as yours, since nothing answered the question.
#[tokio::test]
async fn shift_tab_on_the_waiting_run_answers_its_question_at_once() -> anyhow::Result<()> {
    let crew = crew()?;
    let history = history(&crew)?;
    let _env = crew.repo.env_guard();
    let commands = vec![EngineCommand::ApprovalMode {
        mode: ApprovalMode::Autopilot,
    }];
    let outcome = relaunch(&crew, history.clone(), commands).await?;
    let events = added(&outcome, &history);

    assert_eq!(resumed_cause(&events), Some(ResumeCause::User));
    let decided = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(decided.clone()),
        _ => None,
    });
    let decided = decided.expect("approval-001 was decided");
    assert_eq!(decided.id.to_string(), "approval-001");
    let answered = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Blackboard(BlackboardEvent::Posted(message))
            if message.kind == BlackboardKind::Answer
                && message.in_reply_to.as_ref().is_some_and(|to| to.to_string() == "m-001"))
    });
    assert!(answered, "the question got its answer");
    // The transcript's item says the question was answered in your place, and by what.
    let said = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)) => Some(answered),
        _ => None,
    });
    let said = said.expect("the answer given in your place is recorded");
    assert_eq!(said.question.to_string(), "m-001");
    assert!(said.asker.starts_with("build:"), "{said:?}");
    assert!(!said.answer.is_empty(), "{said:?}");
    assert!(!stopped_blocked(&events), "the step didn't end blocked");
    let left_blocked = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Task(TaskEvent::State { from, to, .. })
            if *from == Some(TaskState::Blocked) && *to == TaskState::Running)
    });
    assert!(left_blocked, "the task went back to work");
    Ok(())
}

/// Guidance on the waiting run relaunches it too: the task waits on its question again, and the
/// run waits on the same approval rather than ending with nothing merged.
#[tokio::test]
async fn guidance_on_the_waiting_run_waits_on_its_question_again() -> anyhow::Result<()> {
    let crew = crew()?;
    let history = history(&crew)?;
    let _env = crew.repo.env_guard();
    let (reply, _answer) = oneshot::channel();
    let commands = vec![EngineCommand::Guidance {
        text: "Keep the tests as they are.".into(),
        reply,
    }];
    let outcome = relaunch(&crew, history.clone(), commands).await?;
    let events = added(&outcome, &history);

    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_ne!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Failed),
        "nothing ended it"
    );
    assert_eq!(resumed_cause(&events), Some(ResumeCause::User));
    assert!(!stopped_blocked(&events));
    let state = RunState::fold(&outcome.events);
    let open: Vec<String> = state
        .approvals
        .open()
        .map(|record| record.opened.id.to_string())
        .collect();
    assert_eq!(open, ["approval-001"], "the same question still waits");
    Ok(())
}
