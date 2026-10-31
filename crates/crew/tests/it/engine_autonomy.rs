//! The autonomous defaults end to end, with the real approval policy over fake workers: a run
//! goes from goal to merge without asking you, the main agent answering the crew's questions, and
//! each case that must still ask does, saying why.

use crate::support;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;

use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::meter::LimitDimension;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalPacket, PolicyCheck,
};
use argonauts_core::runs::events::goal::{GoalEvent, GoalFile};
use argonauts_core::runs::events::run::{Party, RunLifecycle, RunMode};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::shared::{AnsweredForYou, BlackboardEvent};
use argonauts_core::runs::events::task::MergeEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::approvals::{
    Answerer, ApprovalMode, AtHardLimit, DecideWith, Otherwise,
};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::FallbackEntry;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::settings::workflows::OrchestratorDesigns;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::WorkflowRef;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::pty::deadline;
use argonauts_testkit::{FakeRunner, FakeSession};

use support::{Crew, Gh, Outcome, names};

const FILE: &str = "verbose.txt";
const NOTES: &str = "notes.txt";

/// The judge picked for every approval: the cheapest model of a third provider.
const JUDGE: &str = "antigravity-cli/gemini-3.8-flash-low";

/// A crew of two providers' agents, each building `FILE`, with passing checks. A third
/// provider's CLI is installed but not in the crew, so it can judge what both wrote.
fn crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    enlist(&mut crew, "claude-code/opus", "anthropic");
    enlist(&mut crew, "codex/gpt-x", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    Ok(crew)
}

/// A crew member that plans one task for `FILE`, writes it when it builds, and otherwise gives
/// the smallest valid answer.
fn enlist(crew: &mut Crew, worker: &str, provider: &str) {
    enlist_asking(crew, worker, provider, None);
}

/// [`enlist`], finding the readings disagree on a detail, which `question` asks you about.
fn enlist_asking(crew: &mut Crew, worker: &str, provider: &str, question: Option<&'static str>) {
    enlist_with(crew, worker, provider, question, "detail", Vec::new());
}

/// What a crew member does at a step in place of the defaults.
#[derive(Debug, Clone)]
enum Own {
    Answer(serde_json::Value),
    Crash,
    /// The first such session on `task` across the crew hits its limit; the rest go on.
    LimitHitOnce {
        task: &'static str,
        hit: Arc<AtomicBool>,
    },
}

/// [`enlist_asking`], the difference tagged `tag`, doing `own` at the steps they name, such as
/// answering the goal's questions when it is the main agent.
fn enlist_with(
    crew: &mut Crew,
    worker: &str,
    provider: &str,
    question: Option<&'static str>,
    tag: &'static str,
    own: Vec<(StepName, Own)>,
) {
    let fake = FakeRunner::cli(worker, provider).respond_with(move |spec: &SessionSpec| {
        match own.iter().find(|(step, _)| spec.step == Some(*step)) {
            Some((_, Own::Answer(own))) => return FakeSession::structured(own.clone()),
            Some((_, Own::Crash)) => {
                return FakeSession::ended(EndReason::Crashed {
                    message: "segfault".to_owned(),
                });
            }
            Some((_, Own::LimitHitOnce { task, hit }))
                if spec.task.as_ref().is_some_and(|on| on.as_str() == *task)
                    && !hit.swap(true, Ordering::SeqCst) =>
            {
                return FakeSession::ended(EndReason::LimitHit {
                    dimension: LimitDimension::Spend,
                });
            }
            Some((_, Own::LimitHitOnce { .. })) | None => {}
        }
        match (spec.step, question) {
            (Some(StepName::Compare), Some(question)) => {
                return FakeSession::structured(serde_json::json!({
                    "agreements": [],
                    "differences": [{
                        "topic": "the log level",
                        "readings": [
                            {"label": "A", "reading": "debug"},
                            {"label": "B", "reading": "trace"}
                        ],
                        "tag": tag,
                        "question": {
                            "text": question,
                            "options": ["debug", "trace"],
                            "recommended": 0,
                            "basis": "A reads it as debug"
                        }
                    }]
                }));
            }
            (Some(StepName::Build), _) => {
                // A plan's second task, when it has one, writes a file of its own.
                let file = match spec.task.as_ref().map(|task| task.as_str()) {
                    Some("task-002") => NOTES,
                    _ => FILE,
                };
                std::fs::write(spec.workdir.join(file), "--verbose\n")
                    .expect("the build session writes its file");
                // Written with the agent's edit tool, as a CLI reports it.
                return answer(spec).with_event(support::wrote(file));
            }
            (Some(StepName::PlanToTasks), _) => {
                return FakeSession::structured(serde_json::json!({
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
                }));
            }
            _ => {}
        }
        answer(spec)
    });
    crew.services.workers.add(fake);
    let reference: ModelRef = worker.parse().expect("a worker reference");
    if crew.settings.crew.is_empty() {
        crew.settings.crew.main = reference.clone();
    }
    crew.settings.crew.add(reference);
}

/// An approval citing the default rules as the judge step, else the smallest valid answer
/// for the step's schema, or a plain end when it has none.
fn answer(spec: &SessionSpec) -> FakeSession {
    if spec.step == Some(StepName::ModelJudge) {
        return FakeSession::structured(serde_json::json!({
            "verdict": "approve",
            "reasons": ["the evidence meets the rules"],
            "rules_cited": ["R7"]
        }));
    }
    match &spec.json_schema {
        Some(schema) => FakeSession::structured(support::fill(schema)),
        None => FakeSession::completed("done"),
    }
}

/// A test command each task runs and a suite the final check runs, both passing. The plan is
/// one build task, so it goes without an interface task first.
fn with_checks(crew: &mut Crew) {
    crew.settings.divide.contracts_first = false;
    let passing = |name: &str| CheckCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
}

/// Starts the run with the real policy in place of the fakes' asking one, and drives it until
/// it waits or finishes. The policy keeps its records in the repo's state folder, as it does
/// under `~/.argonauts/state`.
async fn run(crew: &Crew) -> anyhow::Result<Outcome> {
    run_in(crew, None).await
}

/// [`run`], with Shift+Tab's `mode` chosen before the run's first decision.
async fn run_in(crew: &Crew, mode: Option<ApprovalMode>) -> anyhow::Result<Outcome> {
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let mut history = vec![started.clone()];
    if let Some(mode) = mode {
        history.push(RunEvent {
            seq: 1,
            kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
                mode,
                by: Party::User,
            }),
            ..started
        });
    }
    drive(crew, history).await
}

/// Drives the engine over `history`, a new run or a waiting one, with the real policy in place
/// of the fakes' asking one, until it waits or finishes. Its events include `history`.
async fn drive(crew: &Crew, history: Vec<RunEvent>) -> anyhow::Result<Outcome> {
    let (mut context, sink, _commands) = crew.context(history)?;
    context.services.policy = real_policy(crew, &context.settings)?;
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

/// The real policy, keeping its records in the repo's state folder, as it does under
/// `~/.argonauts/state`.
fn real_policy(
    crew: &Crew,
    settings: &Arc<argonauts_core::settings::Settings>,
) -> anyhow::Result<Arc<dyn argonauts_core::safety::policy::ApprovalPolicy>> {
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    let services = &crew.services;
    Ok(argonauts_policy::approval_policy_with_models(
        settings.clone(),
        Arc::new(|| None),
        state,
        services.workers.clone(),
        services.registry.clone(),
        services.steps.clone(),
    ))
}

fn checks(events: &[RunEvent]) -> Vec<&PolicyCheck> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Checked(check)) => Some(check),
            _ => None,
        })
        .collect()
}

/// The kinds of approval still open, in the order they opened.
fn open_kinds(events: &[RunEvent]) -> Vec<ApprovalKind> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                open.push((opened.id.clone(), opened.kind));
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|(id, _)| *id != decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.retain(|(open, _)| open != id);
            }
            _ => {}
        }
    }
    open.into_iter().map(|(_, kind)| kind).collect()
}

/// Every reason the policy gave on approvals of `kind`, auto preview records included.
fn reasons(events: &[RunEvent], kind: ApprovalKind) -> Vec<String> {
    let ids: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.id.clone())
            }
            _ => None,
        })
        .collect();
    checks(events)
        .into_iter()
        .filter(|check| ids.contains(&check.id))
        .flat_map(|check| {
            check
                .reasons
                .iter()
                .cloned()
                .chain(check.violations.iter().map(|v| v.detail.clone()))
        })
        .collect()
}

/// Each decided approval's kind and who decided it, in order.
fn decided(events: &[RunEvent]) -> Vec<(ApprovalKind, Party)> {
    let mut kinds = Vec::new();
    let mut found = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                kinds.push((opened.id.clone(), opened.kind));
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                if let Some((_, kind)) = kinds.iter().find(|(id, _)| *id == decided.id) {
                    found.push((*kind, decided.by.clone()));
                }
            }
            _ => {}
        }
    }
    found
}

fn judge_sessions(specs: &[SessionSpec]) -> usize {
    specs
        .iter()
        .filter(|spec| spec.step == Some(StepName::ModelJudge))
        .count()
}

#[tokio::test]
async fn a_run_goes_from_goal_to_merge_without_asking() -> anyhow::Result<()> {
    let crew = crew()?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    let run_merged = events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    });
    assert!(run_merged, "{:?}", names(events));

    // The goal and the plan were decided by your rules and the cheapest model of a provider
    // that wrote none of them, and the merge by the merge rules alone. You were never asked.
    let judge: ModelRef = JUDGE.parse()?;
    let by_judge = Party::Judge {
        model: judge.clone(),
    };
    let expected = [
        (ApprovalKind::Understand, by_judge.clone()),
        (ApprovalKind::Plan, by_judge),
        (ApprovalKind::Merge, Party::Rules),
    ];
    assert_eq!(decided(events), expected);
    let agy = crew.services.workers.fake("antigravity-cli").expect("agy");
    assert_eq!(judge_sessions(&agy.specs()), 2);
    assert!(agy.specs().iter().all(|spec| spec.model == judge));
    Ok(())
}

#[tokio::test]
async fn with_no_agent_of_another_provider_the_goal_asks_with_a_warning() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    enlist(&mut crew, "claude-code/opus", "anthropic");
    enlist(&mut crew, "claude-code/sonnet", "anthropic");
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Understand]);
    let why = reasons(&outcome.events, ApprovalKind::Understand).join("\n");
    assert!(why.contains("another provider than Anthropic"), "{why}");
    assert!(why.contains("pick a judge in /approvals"), "{why}");
    Ok(())
}

#[tokio::test]
async fn a_merge_past_the_daily_cap_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = Some(0);
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    let why = reasons(&outcome.events, ApprovalKind::Merge).join("\n");
    assert!(why.contains("the cap is 0"), "{why}");
    Ok(())
}

/// In autopilot a hard limit does what the approval's `at_hard_limit` says: the merge past
/// the daily cap pushes the run's branch and opens a pull request, and set to wait it waits
/// for you with one line why.
#[tokio::test]
async fn in_autopilot_a_hard_limit_does_what_you_set() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = Some(0);
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::OpenPullRequest;
    let remote = crew.with_remote()?;
    crew.with_tools(Some(Gh::Opens("https://example.invalid/you/repo/pull/3")))?;
    let outcome = run(&crew).await?;
    assert!(
        matches!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::PullRequest)
        ),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(&outcome.events)
    );
    assert!(decided(&outcome.events).contains(&(ApprovalKind::Merge, Party::Autopilot)));
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note.contains("with \"Open a pull request\" instead of asking you")),
        "{said:?}"
    );
    // The setting sends the branch off this computer, and the pull request is open.
    let remote = remote.to_string_lossy().into_owned();
    let pushed = crew
        .repo
        .git(&["--git-dir", &remote, "branch", "--list", "argonauts/*"])?;
    assert!(!pushed.is_empty(), "the run's branch is on the remote");
    let opened = outcome.events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Merge(MergeEvent::PullRequest { url: Some(url), .. })
                if url == "https://example.invalid/you/repo/pull/3"
        )
    });
    assert!(opened, "{:?}", names(&outcome.events));

    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::Wait;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note.contains("to you, as your settings say for a hard limit")),
        "{said:?}"
    );
    Ok(())
}

/// How many merge approvals the run opened.
fn merge_questions(events: &[RunEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Approval(ApprovalEvent::Opened(opened))
                    if opened.kind == ApprovalKind::Merge
            )
        })
        .count()
}

/// A pull request the setting asks for can't open where there is no remote. Nobody is asked
/// again in autopilot, so the run keeps its branch, and one line says why.
#[tokio::test]
async fn in_autopilot_a_pull_request_that_cant_open_keeps_the_branch() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = Some(0);
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::OpenPullRequest;
    crew.with_tools(Some(Gh::Opens("https://example.invalid/you/repo/pull/3")))?;
    let outcome = tokio::time::timeout(
        deadline::scaled(std::time::Duration::from_secs(60)),
        run(&crew),
    )
    .await
    .context("the run asked for a pull request again and again")??;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
        "{:?}",
        names(&outcome.events)
    );
    assert_eq!(merge_questions(&outcome.events), 1);
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note
                .starts_with("No pull request was opened: this repository has no remote")),
        "{said:#?}"
    );
    assert!(
        said.iter().any(
            |note| note.starts_with("Autopilot kept the run's work on argonauts/")
                && note
                    .contains("instead of asking you again, since this repository has no remote")
        ),
        "{said:#?}"
    );
    Ok(())
}

/// A checkout with no branch at all, as a CI checkout is, has nothing a run could merge into
/// and nothing to offer you. Whoever approves the merge, the run ends with its work on its own
/// branch after that one approval, and one line says why.
#[tokio::test]
async fn a_checkout_with_no_branch_at_all_keeps_the_runs_branch() -> anyhow::Result<()> {
    for autopilot in [false, true] {
        let mut crew = crew()?;
        crew.settings.approvals.autopilot = autopilot;
        crew.repo.git(&["checkout", "--quiet", "--detach"])?;
        crew.repo.git(&["branch", "--quiet", "-D", "main"])?;
        let outcome = tokio::time::timeout(
            deadline::scaled(std::time::Duration::from_secs(60)),
            run(&crew),
        )
        .await
        .context("the run asked to merge again and again")??;
        assert_eq!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
            "autopilot {autopilot}: {:?}",
            names(&outcome.events)
        );
        assert_eq!(merge_questions(&outcome.events), 1, "autopilot {autopilot}");
        let said = notes(&outcome.events);
        assert!(
            said.iter().any(|note| note
                .starts_with("This checkout has no branch, so the run's work stays on argonauts/")),
            "autopilot {autopilot}: {said:#?}"
        );
        let kept = crew.repo.git(&["branch", "--list", "argonauts/*"])?;
        assert!(!kept.is_empty(), "the run's branch is kept");
    }
    Ok(())
}

/// What your rules chose in your place and couldn't be done isn't put to the same rules
/// again: the merge question opens as yours, with the reason on it.
#[tokio::test]
async fn in_auto_a_landing_that_cant_be_done_comes_to_you() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.merge.decide_with = DecideWith::Rules;
    crew.settings.approvals.merge.otherwise = Otherwise::OpenPullRequest;
    crew.settings.approvals.rules.merge.max_changed_lines = 0;
    crew.with_tools(Some(Gh::Opens("https://example.invalid/you/repo/pull/3")))?;
    let outcome = tokio::time::timeout(
        deadline::scaled(std::time::Duration::from_secs(60)),
        run(&crew),
    )
    .await
    .context("the run asked for a pull request again and again")??;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}: {:?}",
        outcome.exit,
        names(&outcome.events)
    );
    assert!(decided(&outcome.events).contains(&(ApprovalKind::Merge, Party::Rules)));
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    assert_eq!(merge_questions(&outcome.events), 2);
    let again = outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(opened),
            _ => None,
        });
    let again = again.context("the merge question opened again")?;
    assert!(again.needs_you, "it is yours to answer");
    let ApprovalPacket::Merge(packet) = &again.packet else {
        anyhow::bail!("a merge question");
    };
    let why = packet.again.as_deref().unwrap_or_default();
    assert!(why.starts_with("this repository has no remote"), "{why}");
    Ok(())
}

/// Another round can't change the daily merge cap, so a merge set to send back at a hard limit
/// stops there instead: the run ends with its branch kept, and the merge approval opens once.
#[tokio::test]
async fn in_autopilot_a_cap_another_round_cant_change_stops_instead_of_sending_back()
-> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = Some(0);
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::SendBack;
    let outcome = tokio::time::timeout(
        deadline::scaled(std::time::Duration::from_secs(60)),
        run(&crew),
    )
    .await
    .context("the run never ended")??;
    assert!(
        matches!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft)
        ),
        "{:?}",
        outcome.exit
    );
    let merges: Vec<_> = decided(&outcome.events)
        .into_iter()
        .filter(|(kind, _)| *kind == ApprovalKind::Merge)
        .collect();
    assert_eq!(merges, [(ApprovalKind::Merge, Party::Autopilot)]);
    Ok(())
}

/// A plan set to send back at a hard limit another round could clear goes back once, telling
/// the main agent which limit stopped it. When the next plan stops at a hard limit too, autopilot
/// runs the work as a checked Captain run instead of asking. The Captain task can't
/// land the protected path either, so it's deferred with its work kept, and nothing waits.
#[tokio::test]
async fn in_autopilot_a_hard_limit_sends_back_once_and_says_why() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.protected_paths = vec![FILE.to_owned()];
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.plan.at_hard_limit = AtHardLimit::SendBack;
    let outcome = tokio::time::timeout(
        deadline::scaled(std::time::Duration::from_secs(60)),
        run(&crew),
    )
    .await
    .context("the run never ended")??;
    let sent_back: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.by == Party::Autopilot && decided.action == AnswerAction::SendBack =>
            {
                decided.text.clone()
            }
            _ => None,
        })
        .collect();
    assert_eq!(sent_back.len(), 1, "{sent_back:?}");
    assert!(
        sent_back[0].starts_with("Sent back at a hard limit: ")
            && sent_back[0].contains("protected"),
        "{sent_back:?}"
    );
    let plans = outcome
        .events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Plan)
        })
        .count();
    assert_eq!(plans, 2);
    let captain = outcome.events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Decided(decided))
            if decided.by == Party::Autopilot && decided.action == AnswerAction::SwitchToCaptain)
    });
    assert!(captain, "{:#?}", notes(&outcome.events));
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(&outcome.events)
    );
    assert!(waited_for_you(&outcome.events).is_empty());
    assert!(!crew.repo.root().join(FILE).exists());
    Ok(())
}

#[tokio::test]
async fn a_merge_the_judge_rejects_asks() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    crew.settings.approvals.merge.decide_with =
        argonauts_core::settings::approvals::DecideWith::RulesAndModel;
    enlist(&mut crew, "claude-code/opus", "anthropic");
    enlist(&mut crew, "codex/gpt-x", "openai");
    let rejecting = FakeRunner::cli("antigravity-cli", "google")
        .with_cheapest_model("gemini-3.8-flash-low")
        .respond_with(|spec: &SessionSpec| {
            if spec.step == Some(StepName::ModelJudge) && is_merge(spec) {
                return FakeSession::structured(serde_json::json!({
                    "verdict": "reject",
                    "reasons": ["the change isn't what the goal asked for"],
                    "rules_cited": []
                }));
            }
            answer(spec)
        });
    crew.services.workers.add(rejecting);
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    let why = reasons(&outcome.events, ApprovalKind::Merge).join("\n");
    assert!(why.contains("isn't what the goal asked for"), "{why}");

    // In autopilot the same merge keeps the branch instead, and says why.
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    assert!(
        matches!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft)
        ),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(&outcome.events)
    );
    assert!(decided(&outcome.events).contains(&(ApprovalKind::Merge, Party::Autopilot)));
    let said = notes(&outcome.events);
    assert!(
        said.iter().any(
            |note| note.contains("with \"Keep the branch\" instead of asking you")
                && note.contains("isn't what the goal asked for")
        ),
        "{said:?}"
    );
    Ok(())
}

fn is_merge(spec: &SessionSpec) -> bool {
    spec.prompt
        .iter()
        .any(|part| part.text.contains("combined diff"))
}

#[tokio::test]
async fn a_merge_you_keep_supervised_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.merge.mode = ApprovalMode::Supervised;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    Ok(())
}

const QUESTION: Option<&str> = Some("Which log level does --verbose turn on?");

/// The crew with both members raising [`QUESTION`] about a detail, the main agent answering it with
/// `main_agent`.
fn asking_crew(main_agent: Option<serde_json::Value>) -> anyhow::Result<Crew> {
    asking_crew_about("detail", main_agent.map(Own::Answer))
}

/// [`asking_crew`], the question tagged `tag`, the main agent doing `main_agent` when it's asked.
fn asking_crew_about(tag: &'static str, main_agent: Option<Own>) -> anyhow::Result<Crew> {
    let judge = FakeRunner::cli("antigravity-cli", "google")
        .with_cheapest_model("gemini-3.8-flash-low")
        .respond_with(answer);
    crew_asking(tag, main_agent, judge)
}

fn asking_crew_judged_by(
    main_agent: Option<serde_json::Value>,
    judge: FakeRunner,
) -> anyhow::Result<Crew> {
    crew_asking("detail", main_agent.map(Own::Answer), judge)
}

fn crew_asking(
    tag: &'static str,
    main_agent: Option<Own>,
    judge: FakeRunner,
) -> anyhow::Result<Crew> {
    let mut crew = crew()?;
    crew.services.workers = Default::default();
    crew.settings.crew.clear();
    let own: Vec<(StepName, Own)> = main_agent
        .iter()
        .map(|main_agent| (StepName::AnswerQuestions, main_agent.clone()))
        .collect();
    enlist_with(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        QUESTION,
        tag,
        own.clone(),
    );
    enlist_with(&mut crew, "codex/gpt-x", "openai", QUESTION, tag, own);
    crew.services.workers.add(judge);
    Ok(crew)
}

/// The tie-breaker's role falls back only to the crew's other agent, which fails the same way, so
/// nobody answers in the main agent's place.
fn the_tie_breaker_falls_back_only_to_the_crew(crew: &mut Crew) -> anyhow::Result<()> {
    let codex: ModelRef = "codex/gpt-x".parse()?;
    let role = CrewRole::fallback_of(RoleName::TieBreaker);
    let fallbacks = crew.settings.fallback.roles.entry(role).or_default();
    fallbacks.order = vec![FallbackEntry::Model(codex)];
    Ok(())
}

fn main_agent_picks_trace() -> serde_json::Value {
    serde_json::json!({"answers": [{
        "n": 1,
        "option": 1,
        "why": "the README calls trace the developer level",
        "needs_user": null
    }]})
}

fn approved_goal(events: &[RunEvent]) -> Option<&GoalFile> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Goal(GoalEvent::Approved { file, .. }) => Some(file),
        _ => None,
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

/// Why each question still open came to you, as it says.
fn why_asked(events: &[RunEvent], kind: ApprovalKind) -> Vec<String> {
    let open: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.id.clone())
            }
            _ => None,
        })
        .collect();
    checks(events)
        .into_iter()
        .filter(|check| !check.auto_preview && check.verdict.is_none() && open.contains(&check.id))
        .flat_map(|check| check.reasons.clone())
        .collect()
}

#[tokio::test]
async fn in_auto_the_main_agent_answers_the_goals_questions_and_nothing_asks() -> anyhow::Result<()>
{
    let crew = asking_crew(Some(main_agent_picks_trace()))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );

    // The difference never became a question for you: the main agent's answer is an assumption
    // the judge checked.
    let questions = events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.title.starts_with("What should the goal say"))
        })
        .count();
    assert_eq!(questions, 0, "{:?}", names(events));
    let goal = approved_goal(events).expect("the goal was approved");
    let assumed: Vec<_> = goal
        .assumptions
        .iter()
        .filter(|assumption| assumption.by_main_agent())
        .collect();
    assert_eq!(assumed.len(), 1, "{:?}", goal.assumptions);
    assert!(assumed[0].text.ends_with(": trace"), "{:?}", assumed[0]);
    assert!(assumed[0].basis.contains("the README calls trace"));
    assert!(
        notes(events)
            .iter()
            .any(|note| note.starts_with("The main agent answered 1 of the goal's questions")),
        "{:?}",
        notes(events)
    );

    let judge: ModelRef = JUDGE.parse()?;
    let by_judge = Party::Judge { model: judge };
    let expected = [
        (ApprovalKind::Understand, by_judge.clone()),
        (ApprovalKind::Plan, by_judge),
        (ApprovalKind::Merge, Party::Rules),
    ];
    assert_eq!(decided(events), expected);

    // The main agent is Understand's tie-breaker, so it answered, once.
    let claude = crew
        .services
        .workers
        .fake("claude-code/opus")
        .expect("claude");
    let asked: Vec<_> = claude
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::AnswerQuestions))
        .collect();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].prompt.iter().any(|part| {
        part.text
            .contains("Which log level does --verbose turn on?")
    }));
    Ok(())
}

#[tokio::test]
async fn questions_you_answer_ask_even_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew(Some(main_agent_picks_trace()))?;
    crew.settings.approvals.answers = Some(Answerer::You);
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Understand]);
    assert!(decided(&outcome.events).is_empty());
    let why = why_asked(&outcome.events, ApprovalKind::Understand).join("\n");
    assert!(why.contains("yours to answer"), "{why}");
    Ok(())
}

#[tokio::test]
async fn a_question_that_turns_on_your_fact_asks_and_says_so() -> anyhow::Result<()> {
    let needs_you = serde_json::json!({"answers": [{
        "n": 1,
        "option": null,
        "why": "nothing in the repo says",
        "needs_user": "which level your log collector keeps"
    }]});
    let crew = asking_crew(Some(needs_you))?;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Understand]);
    let why = why_asked(&outcome.events, ApprovalKind::Understand);
    assert_eq!(
        why,
        ["the main agent needs a fact only you have, which level your log collector keeps"]
    );
    Ok(())
}

/// How many goal questions were put to you.
fn questions_asked(events: &[RunEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Understand
                    && matches!(&opened.packet, ApprovalPacket::Understand(goal) if !goal.questions.is_empty()))
        })
        .count()
}

fn main_agent_asked(crew: &Crew) -> usize {
    crew.services
        .workers
        .fake("claude-code/opus")
        .expect("claude")
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::AnswerQuestions))
        .count()
}

/// The review's probe: a security question, the main agent's session crashing, and your goal rules
/// deciding alone. In auto `approvals.ask_you_about` keeps it yours, so it never reaches the
/// main agent, nothing passes for your answer, and the run waits for you.
#[tokio::test]
async fn a_topic_ask_you_about_names_stays_yours_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Crash))?;
    crew.settings.approvals.understand.decide_with = DecideWith::Rules;
    // `understand.always_ask` only makes it a question; this keeps it yours.
    crew.settings.approvals.ask_you_about = vec!["Security".to_owned()];
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(events), [ApprovalKind::Understand]);
    assert_eq!(questions_asked(events), 1);
    assert!(decided(events).is_empty());
    assert!(approved_goal(events).is_none());
    assert_eq!(main_agent_asked(&crew), 0, "the main agent never saw it");
    assert!(
        !notes(events)
            .iter()
            .any(|note| note.starts_with("The main agent answered")),
        "{:?}",
        notes(events)
    );
    let why = why_asked(events, ApprovalKind::Understand);
    assert_eq!(
        why,
        ["your /approvals settings keep security questions yours"]
    );
    Ok(())
}

/// A model picked to answer answers in autopilot only. In auto a question
/// `approvals.ask_you_about` names stays yours all the same, and the picked model is never
/// asked.
#[tokio::test]
async fn a_picked_model_leaves_ask_you_about_questions_yours_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Crash))?;
    crew.settings.approvals.understand.decide_with = DecideWith::Rules;
    crew.settings.approvals.ask_you_about = vec!["security".to_owned()];
    crew.settings.approvals.answers = Some(Answerer::Model("opencode/glm-5".parse()?));
    crew.services
        .workers
        .add(FakeRunner::cli("opencode", "zhipu").respond_with(answer));
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(questions_asked(events), 1);
    let why = why_asked(events, ApprovalKind::Understand);
    assert_eq!(
        why,
        ["your /approvals settings keep security questions yours"]
    );
    let picked = crew
        .services
        .workers
        .fake("opencode")
        .expect("the picked model");
    assert!(
        picked
            .specs()
            .iter()
            .all(|spec| spec.step != Some(StepName::AnswerQuestions)),
        "the picked model answered outside autopilot"
    );
    Ok(())
}

/// When the main agent's session fails, its questions come to you: the crew's recommended answer
/// never passes for it, and nothing calls it the main agent's.
#[tokio::test]
async fn when_the_main_agent_fails_its_questions_come_to_you() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("detail", Some(Own::Crash))?;
    the_tie_breaker_falls_back_only_to_the_crew(&mut crew)?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(questions_asked(events), 1);
    assert!(approved_goal(events).is_none());
    // A crash is an agent failure, so the main agent is asked once more before it counts as failed.
    assert_eq!(main_agent_asked(&crew), 2);
    let said = notes(events);
    assert!(
        said.iter().any(|note| note.starts_with(
            "The main agent couldn't answer the goal's questions, so they come to you"
        )),
        "{said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|note| note.starts_with("The main agent answered")),
        "{said:?}"
    );
    assert_eq!(
        why_asked(events, ApprovalKind::Understand),
        ["the main agent couldn't answer it"]
    );
    Ok(())
}

/// Autopilot as /approvals sets it up: the one switch on, a model you picked answering the crew's
/// questions in the main agent's place, the topics `approvals.ask_you_about` names too, and a judge
/// you picked checking its answers. The main agent is never asked.
#[tokio::test]
async fn a_picked_model_answers_in_the_main_agents_place_and_a_picked_judge_checks()
-> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", None)?;
    let picked: ModelRef = "opencode/glm-5".parse()?;
    crew.services
        .workers
        .add(
            FakeRunner::cli("opencode", "zhipu").respond_with(|spec: &SessionSpec| {
                if spec.step == Some(StepName::AnswerQuestions) {
                    return FakeSession::structured(main_agent_picks_trace());
                }
                answer(spec)
            }),
        );
    let judge: ModelRef = "antigravity-cli/gemini-3.6-pro".parse()?;
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.answers = Some(Answerer::Model(picked.clone()));
    crew.settings.approvals.decides = judge.clone().into();
    crew.settings.approvals.ask_you_about = vec!["security".to_owned()];

    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    assert_eq!(questions_asked(events), 0);
    assert_eq!(main_agent_asked(&crew), 0, "the main agent was never asked");
    let answerer = crew
        .services
        .workers
        .fake("opencode")
        .expect("the picked model");
    let asked: Vec<SessionSpec> = answerer
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::AnswerQuestions))
        .collect();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].model, picked);

    let goal = approved_goal(events).expect("the goal was approved");
    let assumed: Vec<_> = goal
        .assumptions
        .iter()
        .filter(|assumption| assumption.by_main_agent())
        .collect();
    assert_eq!(assumed.len(), 1, "{:?}", goal.assumptions);
    assert!(assumed[0].text.ends_with(": trace"), "{:?}", assumed[0]);
    // Named as people read it, never by its id.
    let named = argonauts_core::look::display_names::DisplayNames::default().worker(&picked);
    assert_ne!(named, picked.to_string());
    assert!(
        notes(events)
            .iter()
            .any(|note| note.starts_with(&format!("{named} answered 1 of the goal's questions"))),
        "{:?}",
        notes(events)
    );

    // The switch put every approval in autopilot, and the judge you picked checked.
    let modes: Vec<ApprovalMode> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(opened.mode),
            _ => None,
        })
        .collect();
    assert!(!modes.is_empty());
    assert!(
        modes.iter().all(|mode| *mode == ApprovalMode::Autopilot),
        "{modes:?}"
    );
    let by_judge = Party::Judge {
        model: judge.clone(),
    };
    assert!(
        decided(events).contains(&(ApprovalKind::Understand, by_judge)),
        "{:?}",
        decided(events)
    );
    let checked: Vec<SessionSpec> = crew
        .services
        .workers
        .fake("antigravity-cli")
        .expect("the judge")
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::ModelJudge))
        .collect();
    assert!(!checked.is_empty());
    assert!(checked.iter().all(|spec| spec.model == judge));
    Ok(())
}

/// Chosen yourself, `answers = "main"` lets the main agent answer every question, the ones
/// `understand.always_ask` makes questions too, so the run goes on without you.
#[tokio::test]
async fn with_the_main_agent_chosen_it_answers_always_ask_questions_too() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Answer(main_agent_picks_trace())))?;
    crew.settings.approvals.answers = Some(Answerer::Main);
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    assert_eq!(questions_asked(events), 0);
    let goal = approved_goal(events).expect("the goal was approved");
    assert!(
        goal.assumptions
            .iter()
            .any(|assumption| assumption.by_main_agent())
    );
    Ok(())
}

/// A model picked to answer that can't, such as one no agent provides, leaves the goal's
/// questions to the main agent in autopilot, after one line that says so, and the run goes on.
#[tokio::test]
async fn a_picked_model_that_cant_answer_the_goal_leaves_it_to_the_main_agent() -> anyhow::Result<()>
{
    let mut crew = asking_crew_about("security", Some(Own::Answer(main_agent_picks_trace())))?;
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.answers = Some(Answerer::Model("nobody/x".parse()?));
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    assert_eq!(
        main_agent_asked(&crew),
        1,
        "the main agent answered in its place"
    );
    let said = notes(events).join("\n");
    assert!(
        said.contains("couldn't answer the goal's questions, so the main agent answers them"),
        "{said}"
    );
    let goal = approved_goal(events).expect("the goal was approved");
    assert!(
        goal.assumptions
            .iter()
            .any(|assumption| assumption.by_main_agent() && assumption.text.ends_with(": trace")),
        "{:?}",
        goal.assumptions
    );
    Ok(())
}

/// A must-ask difference held back over the question limit counts as an open question, so no
/// judge settles the goal in your place, though the main agent answers in auto.
#[tokio::test]
async fn a_held_back_difference_ask_you_about_names_still_waits_for_you() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("scope", Some(Own::Answer(main_agent_picks_trace())))?;
    crew.settings.understand.max_questions = 0;
    crew.settings.approvals.ask_you_about = vec!["scope".to_owned()];
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(events), [ApprovalKind::Understand]);
    assert_eq!(questions_asked(events), 0, "held back, not asked");
    assert!(approved_goal(events).is_none());
    let why = why_asked(events, ApprovalKind::Understand).join("\n");
    assert!(
        why.contains("1 question is open, and it's yours to answer"),
        "{why}"
    );
    Ok(())
}

#[tokio::test]
async fn in_supervised_mode_the_questions_come_to_you() -> anyhow::Result<()> {
    let crew = asking_crew(Some(main_agent_picks_trace()))?;
    let outcome = run_in(&crew, Some(ApprovalMode::Supervised)).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Understand]);
    let claude = crew
        .services
        .workers
        .fake("claude-code/opus")
        .expect("claude");
    assert!(
        claude
            .specs()
            .iter()
            .all(|spec| spec.step != Some(StepName::AnswerQuestions))
    );
    // Supervised waits for you anyway, so the question has no auto reason to give.
    assert!(why_asked(&outcome.events, ApprovalKind::Understand).is_empty());
    Ok(())
}

#[tokio::test]
async fn the_main_agents_answers_the_judge_rejects_ask_with_its_reason() -> anyhow::Result<()> {
    let unsure = FakeRunner::cli("antigravity-cli", "google")
        .with_cheapest_model("gemini-3.8-flash-low")
        .respond_with(|spec: &SessionSpec| {
            if spec.step == Some(StepName::ModelJudge) {
                return FakeSession::structured(serde_json::json!({
                    "verdict": "ask_you",
                    "reasons": ["the request doesn't settle the log level"],
                    "rules_cited": []
                }));
            }
            answer(spec)
        });
    let crew = asking_crew_judged_by(Some(main_agent_picks_trace()), unsure)?;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Understand]);
    let why = why_asked(&outcome.events, ApprovalKind::Understand).join("\n");
    assert!(why.contains("doesn't settle the log level"), "{why}");
    Ok(())
}

/// The sample run's case: Ultracode, the main agent picks a workflow that drafts no plan, which
/// `orchestrator_designs = "any"` allows, and in auto mode its plan approval used to ask, since its
/// evidence named no commit and no facts.
#[tokio::test]
async fn a_workflow_the_main_agent_picks_goes_to_merge_without_asking() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    let design = support::brief(
        Some("maker-checker"),
        serde_json::json!([]),
        None,
        "One small change, reviewed by another provider",
    );
    let own = vec![(StepName::DesignWorkflow, Own::Answer(design))];
    enlist_with(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        None,
        "detail",
        own,
    );
    enlist(&mut crew, "codex/gpt-x", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    crew.settings.ultracode = true;
    crew.settings.workflows.orchestrator_designs = OrchestratorDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = false;

    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    let picked = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(argonauts_core::runs::events::workflow::WorkflowEvent::Started(started))
            if started.from == Some(WorkflowRef::new("maker-checker")))
    });
    assert!(picked, "{:?}", names(events));
    // One question before anything writes: Understand's, which the judge settles in auto. A
    // Captain build needs no plan, so no plan approval follows it.
    let judge: ModelRef = JUDGE.parse()?;
    let starts: Vec<_> = decided(events)
        .into_iter()
        .filter(|(kind, _)| matches!(kind, ApprovalKind::Understand | ApprovalKind::Plan))
        .collect();
    assert_eq!(
        starts,
        [(ApprovalKind::Understand, Party::Judge { model: judge })]
    );
    Ok(())
}

#[tokio::test]
async fn a_plan_that_owns_a_protected_path_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.protected_paths = vec![FILE.to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Plan]);
    let why = reasons(&outcome.events, ApprovalKind::Plan).join("\n");
    assert!(why.contains("protected paths"), "{why}");
    Ok(())
}

/// With review across providers off, `gpt-oss` may review `gpt`'s task, but one provider made
/// both, so their review doesn't let auto merge.
#[tokio::test]
async fn a_merge_reviewed_only_within_one_provider_asks() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    crew.settings.review.other_provider = false;
    crew.services
        .registry
        .add(serde_json::from_value(serde_json::json!({
            "id": "opencode/gpt-oss-120b",
            "provider": "openai",
            "kind": "cli",
            "access": "local",
            "cost": {"unit": "none"},
        }))?);
    enlist(&mut crew, "codex/gpt-x", "openai");
    enlist(&mut crew, "opencode/gpt-oss-120b", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    let why = reasons(&outcome.events, ApprovalKind::Merge).join("\n");
    assert!(
        why.contains(", picked from its builder's own provider, reviewed "),
        "{why}"
    );
    Ok(())
}

/// The same merge on autopilot waits for nobody: one provider made and reviewed the task, so
/// the run keeps its work on its branch, unmerged, and one line says why.
#[tokio::test]
async fn on_autopilot_a_merge_reviewed_only_within_one_provider_keeps_the_branch()
-> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    crew.settings.review.other_provider = false;
    crew.services
        .registry
        .add(serde_json::from_value(serde_json::json!({
            "id": "opencode/gpt-oss-120b",
            "provider": "openai",
            "kind": "cli",
            "access": "local",
            "cost": {"unit": "none"},
        }))?);
    enlist(&mut crew, "codex/gpt-x", "openai");
    enlist(&mut crew, "opencode/gpt-oss-120b", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
        "{said:#?}"
    );
    assert!(!run_merged(&outcome.events));
    assert!(decided(&outcome.events).contains(&(ApprovalKind::Merge, Party::Autopilot)));
    // The line names who reviewed the task, picked from its builder's own provider.
    assert!(
        said.iter()
            .any(|note| note.starts_with("Autopilot answered ")
                && note.contains("instead of asking you, since ")
                && note.contains(", picked from its builder's own provider, reviewed ")),
        "{said:#?}"
    );
    Ok(())
}

/// A reviewer you chose lets the run merge on autopilot, though one provider made and reviewed
/// the task: your choice is the rule's, as the merge's reason says.
#[tokio::test]
async fn on_autopilot_a_reviewer_you_chose_from_one_provider_lets_it_merge() -> anyhow::Result<()> {
    use argonauts_core::base::names::RoleName;
    use argonauts_core::settings::roles::RoleSettings;

    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    crew.settings.review.other_provider = false;
    crew.services
        .registry
        .add(serde_json::from_value(serde_json::json!({
            "id": "opencode/gpt-oss-120b",
            "provider": "openai",
            "kind": "cli",
            "access": "local",
            "cost": {"unit": "none"},
        }))?);
    enlist(&mut crew, "codex/gpt-x", "openai");
    enlist(&mut crew, "opencode/gpt-oss-120b", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    crew.settings.roles.insert(
        RoleName::Reviewer,
        RoleSettings {
            models: vec!["codex/gpt-x".parse()?, "opencode/gpt-oss-120b".parse()?],
            ..RoleSettings::default()
        },
    );
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::Merged),
        "{said:#?}"
    );
    assert!(run_merged(&outcome.events));
    Ok(())
}

/// A crew of one provider can't keep review across providers, so the run eases that rule and
/// says so, and the review follows it: the other member reviews, never the builder. On
/// autopilot the merge rule still wants an approval from another provider or one you chose,
/// so the run keeps its work on its branch, and the line names who reviewed which task.
#[tokio::test]
async fn on_autopilot_a_crew_of_one_provider_reviews_within_it_and_keeps_the_branch()
-> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    crew.services
        .registry
        .add(serde_json::from_value(serde_json::json!({
            "id": "opencode/gpt-oss-120b",
            "provider": "openai",
            "kind": "cli",
            "access": "local",
            "cost": {"unit": "none"},
        }))?);
    enlist(&mut crew, "codex/gpt-x", "openai");
    enlist(&mut crew, "opencode/gpt-oss-120b", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
        "{said:#?}"
    );
    assert!(!run_merged(&outcome.events));
    assert!(
        said.iter()
            .any(|note| note.contains("a task may be reviewed by its worker's provider")),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|note| note.starts_with("Autopilot answered ")
                && note.contains(", picked from its builder's own provider, reviewed ")),
        "{said:#?}"
    );
    let reviewed_by = |cli: &str| -> anyhow::Result<usize> {
        let fake = crew.services.workers.fake(cli).context("a crew member")?;
        Ok(fake
            .specs()
            .iter()
            .filter(|spec| spec.role == RoleName::Reviewer)
            .count())
    };
    assert_eq!(
        reviewed_by("codex/gpt-x")? + reviewed_by("opencode/gpt-oss-120b")?,
        1
    );
    Ok(())
}

/// "Use the crew" names nobody to review, so on autopilot the reviewer code picked from the
/// builder's own provider still keeps the work on its branch.
#[tokio::test]
async fn on_autopilot_use_the_crew_names_no_reviewer_and_keeps_the_branch() -> anyhow::Result<()> {
    use argonauts_core::runs::events::run::{CheckedInstruction, InstructionOutcome, RunLifecycle};
    use argonauts_core::workflows::steps::intake::InstructionKind;

    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    crew.services
        .registry
        .add(serde_json::from_value(serde_json::json!({
            "id": "opencode/gpt-oss-120b",
            "provider": "openai",
            "kind": "cli",
            "access": "local",
            "cost": {"unit": "none"},
        }))?);
    enlist(&mut crew, "codex/gpt-x", "openai");
    enlist(&mut crew, "opencode/gpt-oss-120b", "openai");
    let mut started = crew.run_started(RunMode::Crew, "use the crew to add a --verbose flag")?;
    if let RunEventKind::Run(RunLifecycle::Started(run)) = &mut started.kind {
        run.instructions.push(CheckedInstruction {
            says: "use the crew".to_owned(),
            kind: InstructionKind::Crew,
            outcome: InstructionOutcome::Followed {
                said: "the crew".to_owned(),
                note: None,
            },
        });
    }
    let autopilot = RunEvent {
        seq: 1,
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
        ..started.clone()
    };
    let outcome = drive(&crew, vec![started, autopilot]).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
        "{:#?}",
        notes(&outcome.events)
    );
    assert!(!run_merged(&outcome.events));
    Ok(())
}

/// A crew of one has nobody but the builder to review its task. The builder never reviews its
/// own work, and another try by it would end the same way, so the task goes to the merge
/// unreviewed. On autopilot the merge rule keeps the work on its branch, naming the task.
#[tokio::test]
async fn on_autopilot_a_task_nobody_but_its_builder_could_review_keeps_the_branch()
-> anyhow::Result<()> {
    use argonauts_core::runs::events::task::{TaskEvent, TaskState};

    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    enlist(&mut crew, "codex/gpt-x", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
        "{said:#?}"
    );
    assert!(!run_merged(&outcome.events));
    assert!(
        said.iter().any(|note| note.starts_with(
            "Autopilot answered \"Merge this run into main?\" with \"Keep the branch\" instead \
             of asking you, since nobody but its builder is left to review "
        )),
        "{said:#?}"
    );
    let moves: Vec<(TaskState, &str)> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::State { to, reason, .. }) => Some((*to, reason.as_str())),
            _ => None,
        })
        .collect();
    assert!(
        moves.iter().any(|(to, why)| *to == TaskState::Merging
            && why.starts_with("nobody but its builder is left to review ")
            && why.ends_with(", so it goes to the merge unreviewed")),
        "{moves:#?}"
    );
    assert!(
        moves.iter().all(|(to, _)| *to != TaskState::NeedsYou),
        "{moves:#?}"
    );
    let codex = crew
        .services
        .workers
        .fake("codex/gpt-x")
        .context("the crew")?;
    let specs = codex.specs();
    assert!(specs.iter().all(|spec| spec.role != RoleName::Reviewer));
    let builds = specs
        .iter()
        .filter(|spec| spec.step == Some(StepName::Build))
        .count();
    assert_eq!(builds, 1, "one build, with no second try");
    Ok(())
}

/// Outside autopilot the same merge asks you, and its reason names the task nobody reviewed.
#[tokio::test]
async fn a_merge_with_a_task_nobody_could_review_asks_naming_it() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    enlist(&mut crew, "codex/gpt-x", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Merge]);
    let why = reasons(&outcome.events, ApprovalKind::Merge).join("\n");
    assert!(
        why.contains("Nobody but its builder is left to review "),
        "{why}"
    );
    assert!(!why.contains("a reviewer didn't approve"), "{why}");
    Ok(())
}

/// With no fallback list set, a stopped task's candidates are the other models the installed
/// CLIs offer that can take work: never the stopped model, no endpoint, none that's down, best
/// record on this kind of task first, then the most plan usage left.
#[tokio::test]
async fn a_stopped_task_goes_to_the_other_installed_clis() -> anyhow::Result<()> {
    use argonauts_core::base::clock::Clock;
    use argonauts_core::models::limits::{
        HealthState, PlanUsageBoard, PlanUsageReading, PlanUsageSource, PlanUsageStatus,
        TrackRecord,
    };
    use argonauts_core::runs::events::task::{TaskKind, TaskSpec};
    use argonauts_core::settings::fallback::FallbackEntry;
    use argonauts_crew::fallback::defaults;

    let mut crew = Crew::new()?;
    enlist(&mut crew, "claude-code/opus", "anthropic");
    let services = &crew.services;
    for (worker, provider) in [
        ("claude-code/sonnet", "anthropic"),
        ("codex/gpt-x", "openai"),
        ("antigravity-cli", "gemini"),
        ("opencode/glm-5", "zhipu"),
        ("hermes", "qwen"),
    ] {
        services.workers.add(FakeRunner::cli(worker, provider));
    }
    services
        .workers
        .add(FakeRunner::endpoint("openrouter/deepseek", "deepseek"));
    let r = |text: &str| -> ModelRef { text.parse().expect("a worker reference") };
    services.health.set(
        &r("hermes").agent_id(),
        HealthState::Down,
        Some("not logged in"),
    );
    let record = |passed| TrackRecord { passed, of: 5 };
    services
        .track_records
        .set(r("codex/gpt-x"), "build", record(4));
    services
        .track_records
        .set(r("antigravity-cli"), "build", record(4));
    services
        .track_records
        .set(r("opencode/glm-5"), "build", record(1));
    for (agent_id, used) in [("codex", 0.7), ("antigravity-cli", 0.2)] {
        services.plan_usage.observe(PlanUsageReading {
            agent_id: r(agent_id).agent_id(),
            status: PlanUsageStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: None,
            source: PlanUsageSource::UsageProbe,
            observed_at: services.clock.now(),
            detail: None,
            wrap_up: false,
        });
    }

    let (host, _) = crew.host(RunMode::Crew)?;
    let stopped = r("claude-code/opus");
    let task = TaskSpec {
        id: "task-001".parse()?,
        title: "Add the --verbose flag".to_owned(),
        task: "Add a --verbose flag.".to_owned(),
        kind: TaskKind::Build,
        owned_paths: vec![FILE.to_owned()],
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(stopped.clone()),
        reviewers: Vec::new(),
        limit: Default::default(),
        predictions: None,
        item: None,
        way: None,
    };
    // The stopped CLI's other model is listed too, last for want of a record: the recovery
    // rules keep it when only the stopped model failed and drop it when its CLI did.
    let models = [
        "antigravity-cli",
        "codex/gpt-x",
        "opencode/glm-5",
        "claude-code/sonnet",
    ];
    let expected: Vec<FallbackEntry> = models
        .into_iter()
        .map(|worker| FallbackEntry::Model(r(worker)))
        .collect();
    assert!(defaults::apply(&host, &task, &stopped));
    assert_eq!(defaults::candidates(&host, &task, &stopped), expected);
    Ok(())
}

/// Autopilot, from request to merge with no question. The comparers disagree on a scope question,
/// which `approvals.ask_you_about` keeps yours in auto, and the task's first build hits its limit
/// with no reserve to release. The main agent answers the question as an assumption the judge
/// checks, the limit hit ends that try safely and says so, and the run merges.
#[tokio::test]
async fn autopilot_goes_from_request_to_merge_with_no_question() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.services.workers = Default::default();
    crew.settings.crew.clear();
    // Limits of your own, since this run is about one being reached.
    crew.settings.limits.spend.spend_per_run = Some(20.0);
    crew.settings.limits.spend.per_task_usd = Some(3.0);
    let hit = Arc::new(AtomicBool::new(false));
    let task = |key: &str, file: &str| {
        serde_json::json!({
            "key": key,
            "title": format!("Write {file}"),
            "task": format!("Write {file}."),
            "kind": "build",
            "owned_paths": [file],
            "read_only_paths": [],
            "depends_on": [],
            "criteria": [],
        })
    };
    let plan = serde_json::json!({"tasks": [task("flag", FILE), task("notes", NOTES)]});
    for (worker, provider) in [("claude-code/opus", "anthropic"), ("codex/gpt-x", "openai")] {
        let own = vec![
            (
                StepName::AnswerQuestions,
                Own::Answer(main_agent_picks_trace()),
            ),
            (StepName::PlanToTasks, Own::Answer(plan.clone())),
            (
                StepName::Build,
                Own::LimitHitOnce {
                    task: "task-001",
                    hit: hit.clone(),
                },
            ),
        ];
        enlist_with(&mut crew, worker, provider, QUESTION, "scope", own);
    }
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    crew.settings.approvals.ask_you_about = vec!["scope".to_owned()];

    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    let run_merged = events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    });
    assert!(run_merged, "{:?}", names(events));
    assert!(hit.load(Ordering::SeqCst), "no build hit its limit");
    // The task past its limit waits, deferred with its work kept, and the other one merged.
    assert!(crew.repo.root().join(NOTES).exists());
    assert!(!crew.repo.root().join(FILE).exists());

    // Nothing waited for you: no question asked, and none was answered by you.
    let asked = waited_for_you(events);
    assert!(asked.is_empty(), "{asked:#?}");
    assert!(
        decided(events).iter().all(|(_, by)| *by != Party::User),
        "{:?}",
        decided(events)
    );
    assert_eq!(questions_asked(events), 0);

    // The scope question is the main agent's assumption, which the judge checked with the goal.
    let goal = approved_goal(events).expect("the goal was approved");
    assert!(
        goal.assumptions
            .iter()
            .any(|assumption| assumption.by_main_agent() && assumption.text.ends_with(": trace")),
        "{:?}",
        goal.assumptions
    );
    let judge: ModelRef = JUDGE.parse()?;
    assert!(decided(events).contains(&(ApprovalKind::Understand, Party::Judge { model: judge })));

    // The limit past its reserve ended that task's part safely instead of asking, and the
    // transcript says what autopilot chose and why.
    assert!(decided(events).contains(&(ApprovalKind::Limit, Party::Autopilot)));
    let said = notes(events);
    let ended = |what: &str, why: &str| {
        said.iter().any(|note| {
            note.starts_with("Autopilot answered \"Write verbose.txt ")
                && note.contains(&format!(
                    "with \"{what}\" instead of asking you, since {why}"
                ))
        })
    };
    assert!(
        ended("Stop and ask me", "the remaining tasks need"),
        "{said:?}"
    );
    assert!(ended("Defer it", "no fallback"), "{said:?}");

    // What the policy found is kept for review, and the closing answer can list each ending
    // as the transcript said it.
    let state = argonauts_crew::state::RunState::fold(events);
    let endings = argonauts_crew::approvals::autopilot::endings(&state);
    assert_eq!(endings.len(), 2, "{endings:#?}");
    for ending in &endings {
        assert!(
            said.contains(&ending.as_str()),
            "{ending} isn't in {said:#?}"
        );
    }
    Ok(())
}

/// The approvals that waited for you: the policy left them to you, and autopilot didn't
/// settle them in your place.
fn waited_for_you(events: &[RunEvent]) -> Vec<&PolicyCheck> {
    let settled: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.by == Party::Autopilot =>
            {
                Some(decided.id.clone())
            }
            _ => None,
        })
        .collect();
    checks(events)
        .into_iter()
        .filter(|check| {
            !check.auto_preview && check.verdict.is_none() && !settled.contains(&check.id)
        })
        .collect()
}

/// Shift+Tab to autopilot while a goal question waits for you leaves it to the main agent, as if
/// autopilot had been on: the main agent's answer becomes the goal's assumption, which the judge
/// checks with the goal. Nothing in the transcript names another answer.
#[tokio::test]
async fn autopilot_leaves_the_waiting_goal_question_to_the_main_agent() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Answer(main_agent_picks_trace())))?;
    crew.settings.approvals.ask_you_about = vec!["security".to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(
        why_asked(&outcome.events, ApprovalKind::Understand),
        ["your /approvals settings keep security questions yours"]
    );

    let (host, sink) = crew.host_over(outcome.events.clone())?;
    argonauts_crew::engine::commands::approval_mode(&host, ApprovalMode::Autopilot)?;
    let mut events = outcome.events.clone();
    events.extend(sink.events());
    assert!(open_kinds(&events).is_empty(), "{:?}", open_kinds(&events));
    let withdrawn: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Withdrawn { reason, .. }) => {
                Some(reason.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        withdrawn,
        [argonauts_crew::approvals::autopilot::LEFT_TO_THE_MAIN_AGENT]
    );

    let resumed = drive(&crew, events).await?;
    let events = &resumed.events;
    assert!(
        matches!(resumed.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        resumed.exit,
        open_kinds(events)
    );
    let goal = approved_goal(events).expect("the goal was approved");
    let assumed = goal
        .assumptions
        .iter()
        .find(|assumption| assumption.by_main_agent())
        .expect("the main agent's answer is the goal's assumption");
    assert!(assumed.text.ends_with(": trace"), "{assumed:?}");
    let judge: ModelRef = JUDGE.parse()?;
    assert!(decided(events).contains(&(ApprovalKind::Understand, Party::Judge { model: judge })));
    let said = notes(events);
    assert!(
        !said
            .iter()
            .any(|note| note.starts_with("Autopilot answered \"What should the goal say?")),
        "{said:?}"
    );
    Ok(())
}

/// An Ultracode crew whose main agent answers the workflow design with `design`.
fn designing_crew(design: serde_json::Value) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    let own = vec![(StepName::DesignWorkflow, Own::Answer(design))];
    enlist_with(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        None,
        "detail",
        own,
    );
    enlist(&mut crew, "codex/gpt-x", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    crew.settings.ultracode = true;
    crew.settings.workflows.adapt_at_phase_ends = false;
    Ok(crew)
}

fn started_from(events: &[RunEvent]) -> Vec<Option<WorkflowRef>> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(
                argonauts_core::runs::events::workflow::WorkflowEvent::Started(started),
            ) => Some(started.from.clone()),
            _ => None,
        })
        .collect()
}

/// Ultracode on the crew's fixed phases, and the main agent would run the task as maker-checker. It may only size
/// the workflow, so the run stays on them and says why.
#[tokio::test]
async fn the_main_agent_never_swaps_the_workflow_for_another_workflow() -> anyhow::Result<()> {
    let crew = designing_crew(support::brief(
        Some("maker-checker"),
        serde_json::json!([]),
        None,
        "One small change, reviewed by another provider",
    ))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    let from = started_from(events);
    assert!(from.contains(&Some(WorkflowRef::new("base"))), "{from:?}");
    assert!(
        !from.contains(&Some(WorkflowRef::new("maker-checker"))),
        "{from:?}"
    );

    // It saw only the workflow, and was told why its pick didn't hold.
    let main_agent = crew
        .services
        .workers
        .fake("claude-code/opus")
        .expect("claude");
    let designs: Vec<_> = main_agent
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::DesignWorkflow))
        .collect();
    assert_eq!(designs.len(), 2);
    let text = |spec: &SessionSpec| {
        spec.prompt
            .iter()
            .map(|part| part.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(text(&designs[0]).contains("## The workflow's steps"));
    assert!(!text(&designs[0]).contains("### maker-checker"));
    assert!(
        text(&designs[1]).contains("keep `base`, the workflow's steps"),
        "{}",
        text(&designs[1])
    );
    assert!(
        notes(events).iter().any(|note| note.starts_with(
            "The main agent's workflow broke the rules twice, so base runs as written."
        )),
        "{:?}",
        notes(events)
    );
    Ok(())
}

/// A design that leaves out what the brief asks for is said in one plain line, and what the
/// validator said waits for Ctrl+O. The workflow runs as written.
#[tokio::test]
async fn an_incomplete_design_is_said_plainly() -> anyhow::Result<()> {
    let crew = designing_crew(serde_json::json!({ "why": "One small change" }))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    let note = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Note(note) if note.line().starts_with("The main agent's design") => {
                Some(note.clone())
            }
            _ => None,
        })
        .context("the design's note")?;
    assert_eq!(
        note.line(),
        "The main agent's design came back incomplete, so base runs as written."
    );
    let detail = note.detail().context("what the validator said")?;
    assert!(
        detail.contains("\"based_on\" is a required property"),
        "{detail}"
    );
    let from = started_from(events);
    assert!(from.contains(&Some(WorkflowRef::new("base"))), "{from:?}");
    Ok(())
}

/// The main agent may size the workflow, here leaving out the critique, and one line says so and
/// why.
#[tokio::test]
async fn the_main_agent_sizes_the_workflow_and_says_what_it_changed() -> anyhow::Result<()> {
    let mut sized =
        argonauts_crew::workflow::builtin::load("base").map_err(|err| anyhow::anyhow!(err))?;
    for phase in &mut sized.phases {
        for step in &mut phase.steps {
            step.steps
                .retain(|step| step.name.as_deref() != Some("critique"));
        }
    }
    let composed =
        argonauts_crew::workflow::yaml::print(&sized).map_err(|err| anyhow::anyhow!(err))?;
    let crew = designing_crew(support::brief(
        Some("base"),
        serde_json::json!([]),
        Some(&composed),
        "One task needs no debate.",
    ))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    let from = started_from(events);
    assert!(from.contains(&Some(WorkflowRef::new("base"))), "{from:?}");
    let said = notes(events);
    assert!(
        said.contains(
            &"The main agent changed the workflow's steps: left out critique, since one task needs no \
              debate."
        ),
        "{said:?}"
    );
    Ok(())
}

/// In autopilot even a question the main agent couldn't answer doesn't wait: it takes the crew's
/// recommended answer, as an assumption that says so, for the judge to check.
#[tokio::test]
async fn in_autopilot_a_question_the_main_agent_fails_takes_the_recommended_answer()
-> anyhow::Result<()> {
    let mut crew = asking_crew_about("scope", Some(Own::Crash))?;
    the_tie_breaker_falls_back_only_to_the_crew(&mut crew)?;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    assert_eq!(questions_asked(events), 0);
    // Asked once more after its crash.
    assert_eq!(main_agent_asked(&crew), 2);
    let goal = approved_goal(events).expect("the goal was approved");
    let assumed = goal
        .assumptions
        .iter()
        .find(|assumption| assumption.by_main_agent())
        .expect("the fallback answer is the main agent's assumption");
    assert!(assumed.text.ends_with(": debug"), "{assumed:?}");
    assert!(
        assumed.basis.contains(
            "autopilot took the crew's recommended answer, since the main agent couldn't"
        ),
        "{assumed:?}"
    );
    assert!(
        notes(events).iter().any(|note| note.starts_with(
            "The main agent couldn't answer the goal's questions, so autopilot takes the crew's"
        )),
        "{:?}",
        notes(events)
    );
    Ok(())
}

fn run_merged(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    })
}

fn merged(exit: &EngineExit) -> bool {
    matches!(
        exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::Merged)
    )
}

/// Ultracode with a Claude Code, a Codex, and an agy agent
/// leaves no judge of a provider outside the crew. Autopilot has a model of another provider
/// than the main agent's check the goal, the main agent's answers in it, and the run lands.
#[tokio::test]
async fn autopilot_with_every_provider_in_the_crew_lands() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    let keep = serde_json::json!({
        "template": "base",
        "params": [],
        "composed": null,
        "why": "It fits"
    });
    let own = vec![(StepName::DesignWorkflow, Own::Answer(keep))];
    enlist_with(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        None,
        "detail",
        own,
    );
    enlist(&mut crew, "codex/gpt-x", "openai");
    enlist(&mut crew, "antigravity-cli/gemini-3.8-flash-low", "google");
    crew.settings.ultracode = true;
    crew.settings.workflows.adapt_at_phase_ends = false;

    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        merged(&outcome.exit),
        "{:?}: open {:?}, notes {:#?}",
        outcome.exit,
        open_kinds(events),
        notes(events)
    );
    assert!(run_merged(events), "{:?}", names(events));
    // Every provider read the request back, so none was left outside its authors.
    let mut readers: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Read { worker, .. }) => Some(worker.agent.as_str()),
            _ => None,
        })
        .collect();
    readers.sort_unstable();
    readers.dedup();
    assert_eq!(readers, ["antigravity-cli", "claude-code", "codex"]);
    let goal = decided(events)
        .into_iter()
        .find(|(kind, _)| *kind == ApprovalKind::Understand)
        .map(|(_, by)| by);
    let Some(Party::Judge { model }) = goal else {
        panic!("the goal wasn't approved by a model: {goal:?}");
    };
    assert_ne!(
        model.agent.as_str(),
        "claude-code",
        "the main agent checked itself"
    );
    assert!(waited_for_you(events).is_empty());
    Ok(())
}

/// Nobody answers spot checks in autopilot, so it draws none, however many your settings
/// sample: the judge's approvals go through, none joins a queue of yours, and the run lands.
#[tokio::test]
async fn autopilot_draws_no_spot_check_of_its_own() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.spot_check_sample = 1.0;
    crew.settings.approvals.pause_auto_when_spot_checks_waiting = 1;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        merged(&outcome.exit),
        "{:?}: open {:?}, notes {:#?}",
        outcome.exit,
        open_kinds(events),
        notes(events)
    );
    assert!(run_merged(events));
    let by_a_model = decided(events)
        .into_iter()
        .filter(|(_, by)| matches!(by, Party::Judge { .. }))
        .count();
    assert!(by_a_model >= 2, "{by_a_model} approvals by the judge");
    let queued = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { .. })
            )
        })
        .count();
    assert_eq!(queued, 0, "autopilot asks you nothing");
    Ok(())
}

/// With no model of another provider to run, autopilot goes ahead on your rules alone, keeps the
/// main agent's answers as assumptions, and says so in plain words with where to pick a judge.
/// The crew's other member reviews its one task, so the merge rule keeps the work on its branch.
#[tokio::test]
async fn autopilot_with_no_judge_goes_ahead_on_your_rules() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    enlist(&mut crew, "claude-code/opus", "anthropic");
    enlist(&mut crew, "claude-code/sonnet", "anthropic");
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::BranchLeft),
        "open {:?}, notes {:#?}",
        open_kinds(events),
        notes(events)
    );
    assert!(!open_kinds(events).contains(&ApprovalKind::Merge));
    assert!(!run_merged(events));
    assert!(decided(events).contains(&(ApprovalKind::Understand, Party::Rules)));
    let said = notes(events);
    assert!(
        said.iter()
            .any(|note| note.starts_with(
                "Autopilot answered \"Merge this run into main?\" with \"Keep the branch\""
            ) && note.contains(", picked from its builder's own provider, reviewed ")),
        "{said:#?}"
    );
    assert!(
        said.contains(
            &"\"Run this workflow?\": No model you can run comes from another provider than \
              Anthropic, so no judge can judge this; your rules decided. Pick a judge in \
              /approvals, or install and log in to an agent from another provider."
        ),
        "{said:#?}"
    );
    Ok(())
}

/// A plan nothing can approve, here kept for you by `presence = required`, has the run wait with
/// one line saying why. Autopilot never abandons it.
#[tokio::test]
async fn autopilot_waits_on_a_plan_it_cant_approve() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.presence = argonauts_core::settings::approvals::Presence::Required;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(events), [ApprovalKind::Plan]);
    let said = notes(events);
    assert!(
        said.iter().any(|note| note
            == &"Autopilot left \"Approve this plan?\" to you, since your /approvals settings \
                 keep it for you at the terminal."),
        "{said:#?}"
    );
    Ok(())
}

/// A crew of Claude Code, Codex, and agy, with Codex ready to answer a question.
fn three_provider_crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    enlist(&mut crew, "claude-code/opus", "anthropic");
    let codex = FakeRunner::cli("codex/gpt-x", "openai").respond_with(|spec: &SessionSpec| {
        if spec.step.is_none() {
            return FakeSession::completed("Log to stderr only; src/cli.rs already has --verbose.");
        }
        answer(spec)
    });
    crew.services.workers.add(codex);
    crew.settings.crew.add("codex/gpt-x".parse()?);
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    Ok(crew)
}

/// The events of a run in autopilot where task-001, worked by Claude Code, stops on a question
/// meant for you.
async fn ask_in_autopilot(crew: &Crew) -> anyhow::Result<Vec<RunEvent>> {
    report_in_autopilot(
        crew,
        argonauts_core::workflows::steps::task::ReportStatus::Blocked,
    )
    .await
}

/// [`ask_in_autopilot`], the report ending as `status` says.
async fn report_in_autopilot(
    crew: &Crew,
    status: argonauts_core::workflows::steps::task::ReportStatus,
) -> anyhow::Result<Vec<RunEvent>> {
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let autopilot = RunEvent {
        seq: 1,
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
        ..started.clone()
    };
    let mut asking = Asking::new(crew, vec![started, autopilot])?;
    asking.report.status = status;
    with_git_env(
        crew.git_env(),
        argonauts_crew::blackboard::on_report(&asking.host, &asking.task, &asking.report),
    )
    .await?;
    Ok(asking.sink.events())
}

/// A running task whose agent's report asks a question meant for you.
struct Asking {
    host: argonauts_crew::engine::host::Host,
    sink: Arc<argonauts_testkit::MemorySink>,
    task: argonauts_core::TaskId,
    report: argonauts_core::workflows::steps::task::TaskReport,
}

impl Asking {
    fn new(crew: &Crew, history: Vec<RunEvent>) -> anyhow::Result<Self> {
        use argonauts_core::runs::events::task::{TaskEvent, TaskKind, TaskSpec, TaskState};
        use argonauts_core::workflows::steps::task::{ReportStatus, TaskReport};

        let state = argonauts_crew::state::RunState::fold(&history);
        let (mut context, sink, _commands) = crew.context(history)?;
        context.services.policy = real_policy(crew, &context.settings)?;
        let shared = crew.shared(&context, state)?;
        let host = argonauts_crew::engine::host::Host::new(Arc::new(shared));

        let task = argonauts_core::TaskId::new("task-001")?;
        let worker: ModelRef = "claude-code/opus".parse()?;
        let spec = TaskSpec {
            id: task.clone(),
            title: "Add the --verbose flag".to_owned(),
            task: String::new(),
            kind: TaskKind::Build,
            owned_paths: vec![FILE.to_owned()],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(worker),
            reviewers: Vec::new(),
            limit: Default::default(),
            predictions: None,
            item: None,
            way: None,
        };
        host.record(
            Some(&task),
            None,
            RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
        )?;
        host.record(
            Some(&task),
            None,
            RunEventKind::Task(TaskEvent::State {
                from: None,
                to: TaskState::Running,
                reason: String::new(),
            }),
        )?;
        // A report that stopped for the answer, so outside autopilot its question waits for
        // you; one that finished asks in passing.
        let report = TaskReport {
            status: ReportStatus::Blocked,
            summary: "Done.".to_owned(),
            files_changed: Vec::new(),
            tests_run: Vec::new(),
            concerns: Vec::new(),
            questions: vec!["Should --verbose also write to a log file?".to_owned()],
            change_requests: Vec::new(),
            memory_proposals: Vec::new(),
            answer: String::new(),
        };
        Ok(Self {
            host,
            sink,
            task,
            report,
        })
    }
}

/// Carry on without me on an agent's question: the question goes back to the agent at once,
/// which is told to decide and say what it assumed, a line says so, and the closing answer lists
/// it. Nothing waits.
#[tokio::test]
async fn a_question_you_leave_goes_back_to_its_agent() -> anyhow::Result<()> {
    use argonauts_core::runs::events::approval::LEFT_TO_THE_AGENT;
    use argonauts_core::settings::asking::WhenDeclined;

    let (opened, events) = leave_the_question(three_provider_crew()?).await?;
    assert_eq!(opened.when_declined, WhenDeclined::Decide);
    let answer = posted_answer(&events);
    assert_eq!(answer.from, Party::Declined);
    assert_eq!(answer.text, LEFT_TO_THE_AGENT);
    assert!(decided(&events).contains(&(ApprovalKind::Question, Party::Declined)));
    let line = "You left \"Question from build:add-the\" to the agent, which decides and says \
                what it assumed.";
    assert!(notes(&events).contains(&line), "{:#?}", notes(&events));
    let state = argonauts_crew::state::RunState::fold(&events);
    assert_eq!(
        argonauts_crew::approvals::autopilot::endings(&state),
        [line]
    );
    Ok(())
}

/// With `approvals.when_declined = "stop"` the question says so when it opens, and leaving it
/// tells its agent to stop and report rather than decide.
#[tokio::test]
async fn a_question_you_leave_with_stop_set_stops_its_agent() -> anyhow::Result<()> {
    use argonauts_core::settings::asking::WhenDeclined;
    use argonauts_crew::approvals::decline::STOP_HERE;

    let mut crew = three_provider_crew()?;
    crew.settings.approvals.when_declined = WhenDeclined::Stop;
    let (opened, events) = leave_the_question(crew).await?;
    assert_eq!(opened.when_declined, WhenDeclined::Stop);
    let answer = posted_answer(&events);
    assert_eq!(answer.from, Party::Declined);
    assert_eq!(answer.text, STOP_HERE);
    let line = "You left \"Question from build:add-the\", so its agent stops and reports.";
    assert!(notes(&events).contains(&line), "{:#?}", notes(&events));
    Ok(())
}

/// Opens a task's question for you and takes Carry on without me on it, returning the question as
/// it opened and the run's events after. Leaving it a second time is refused.
async fn leave_the_question(
    crew: Crew,
) -> anyhow::Result<(
    Box<argonauts_core::runs::events::approval::ApprovalOpened>,
    Vec<RunEvent>,
)> {
    use argonauts_core::runs::api::ApprovalAnswer;
    use argonauts_core::runs::events::approval::AnswerAction;

    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let asking = Asking::new(&crew, vec![started])?;
    let ask = with_git_env(
        crew.git_env(),
        argonauts_crew::blackboard::on_report(&asking.host, &asking.task, &asking.report),
    );
    let leave = async {
        let opened = loop {
            let open = asking
                .sink
                .events()
                .into_iter()
                .find_map(|event| match event.kind {
                    RunEventKind::Approval(ApprovalEvent::Opened(opened))
                        if opened.kind == ApprovalKind::Question =>
                    {
                        Some(opened)
                    }
                    _ => None,
                });
            if let Some(opened) = open {
                break opened;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let left = ApprovalAnswer {
            id: opened.id.clone(),
            action: AnswerAction::CarryOn,
            text: None,
            evidence_sha256: String::new(),
            tasks: Vec::new(),
        };
        argonauts_crew::approvals::answer(&asking.host, left.clone())
            .expect("the question is left");
        // It is decided now, so leaving it again has nothing to leave.
        let again = argonauts_crew::approvals::answer(&asking.host, left);
        assert_eq!(again, Err("already decided".to_owned()));
        opened
    };
    let (asked, opened) = tokio::join!(ask, leave);
    asked?;
    let events = asking.sink.events();
    assert!(open_kinds(&events).is_empty(), "something still waits");
    Ok((opened, events))
}

/// A run that stops to wait for your answer ends the wait that would pass it on; when its step
/// goes on, the task still blocked on the question takes the answer you gave meanwhile.
#[tokio::test]
async fn an_answer_given_while_the_run_was_waiting_reaches_its_task() -> anyhow::Result<()> {
    use argonauts_core::runs::api::ApprovalAnswer;
    use argonauts_core::runs::events::approval::AnswerAction;
    use argonauts_core::runs::events::task::TaskState;

    let crew = three_provider_crew()?;
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let asking = Asking::new(&crew, vec![started])?;
    let ask = with_git_env(
        crew.git_env(),
        argonauts_crew::blackboard::on_report(&asking.host, &asking.task, &asking.report),
    );
    let opened = async {
        loop {
            let open = asking
                .sink
                .events()
                .into_iter()
                .find_map(|event| match event.kind {
                    RunEventKind::Approval(ApprovalEvent::Opened(opened))
                        if opened.kind == ApprovalKind::Question =>
                    {
                        Some((opened.id, opened.evidence_sha256))
                    }
                    _ => None,
                });
            if let Some(id) = open {
                break id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    // Stopping to wait cancels the step that waits, so the wait never sees the answer.
    let (id, evidence_sha256) = tokio::select! {
        id = opened => id,
        _ = ask => panic!("the question waits for you"),
    };
    let state_of = || {
        let state = argonauts_crew::state::RunState::fold(&asking.sink.events());
        state.tasks.get(&asking.task).map(|record| record.state)
    };
    assert_eq!(state_of(), Some(TaskState::Blocked));
    argonauts_crew::approvals::answer(
        &asking.host,
        ApprovalAnswer {
            id,
            action: AnswerAction::Answer,
            text: Some("Use --verbose and -v both.".to_owned()),
            evidence_sha256,
            tasks: Vec::new(),
        },
    )
    .expect("the question is answered");
    assert!(argonauts_crew::blackboard::questions::answered_meanwhile(
        &asking.host,
        &asking.task
    )?);
    let answer = posted_answer(&asking.sink.events());
    assert_eq!(answer.text, "Use --verbose and -v both.");
    assert_eq!(state_of(), Some(TaskState::Running), "the task goes on");
    assert!(
        !argonauts_crew::blackboard::questions::answered_meanwhile(&asking.host, &asking.task)?,
        "passed on once"
    );
    Ok(())
}

/// The answer posted to the agent's question.
fn posted_answer(events: &[RunEvent]) -> argonauts_core::runs::events::shared::BlackboardMessage {
    use argonauts_core::runs::events::shared::{BlackboardEvent, BlackboardKind};
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::Posted(message))
                if message.kind == BlackboardKind::Answer =>
            {
                Some(message.clone())
            }
            _ => None,
        })
        .expect("the question was answered")
}

/// Autopilot: a question that stops an agent's work, from Claude Code with Claude Code the main
/// agent too, goes to a model of another provider, whose answer goes straight to the agent. No
/// approval opens for it. One line names the asking task's label and the models, never a task
/// id, and the run's status keeps the answer for you to review.
#[tokio::test]
async fn in_autopilot_a_question_that_blocks_is_answered_elsewhere_with_no_approval()
-> anyhow::Result<()> {
    let crew = three_provider_crew()?;
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert!(
        matches!(&answer.from, Party::Agent { worker, .. } if worker.agent.as_str() == "codex"),
        "{:?}",
        answer.from
    );
    assert!(answer.text.contains("stderr"), "{}", answer.text);
    assert!(open_kinds(&events).is_empty(), "{:?}", open_kinds(&events));
    assert!(decided(&events).is_empty(), "{:?}", decided(&events));
    let said = notes(&events);
    let answered: Vec<&AnsweredForYou> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)) => Some(answered),
            _ => None,
        })
        .collect();
    assert_eq!(answered.len(), 1, "{answered:#?}");
    assert_eq!(answered[0].asker, "build:add-the");
    assert_eq!(
        answered[0].answer,
        "Log to stderr only; src/cli.rs already has --verbose."
    );
    assert_eq!(
        answered[0].by.as_ref().map(|by| by.agent.as_str()),
        Some("codex")
    );
    assert!(
        !said.iter().any(|note| note.contains("task-001")),
        "{said:#?}"
    );
    let status = argonauts_crew::status::summary(&events, &argonauts_crew::status::Viewer::User);
    assert!(
        status.goal.iter().any(|line| line.starts_with(
            "Answered for you on autopilot · Should --verbose also write to a log file: Log to \
             stderr only"
        )),
        "{:#?}",
        status.goal
    );
    Ok(())
}

/// Autopilot: a question in a report that finished its work asks nobody. No model is started
/// and no approval opens: it stands as its agent's own call, said in one line.
#[tokio::test]
async fn in_autopilot_a_finished_reports_question_is_its_agents_own_call() -> anyhow::Result<()> {
    use argonauts_core::workflows::steps::task::ReportStatus;

    let crew = three_provider_crew()?;
    let events = report_in_autopilot(&crew, ReportStatus::Done).await?;
    let answer = posted_answer(&events);
    assert_eq!(answer.from, Party::Autopilot);
    assert_eq!(answer.text, argonauts_crew::approvals::autopilot::OWN_CALL);
    assert!(open_kinds(&events).is_empty(), "{:?}", open_kinds(&events));
    let started = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                RunEventKind::Session(
                    argonauts_core::runs::events::session::SessionEvent::Started(_)
                )
            )
        })
        .count();
    assert_eq!(started, 0, "{:?}", names(&events));
    let said = notes(&events);
    assert_eq!(
        said,
        [
            "Decided on autopilot · build:add-the (Opus on Claude Code) kept its own call on: \
             Should --verbose also write to a log file?"
        ],
    );
    Ok(())
}

/// A model you picked from the asker's own provider never answers it: the main agent's rule picks a
/// model of another provider instead.
#[tokio::test]
async fn a_picked_model_never_answers_its_own_providers_agent() -> anyhow::Result<()> {
    let mut crew = three_provider_crew()?;
    crew.settings.approvals.answers = Some(Answerer::Model("claude-code/opus".parse()?));
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert!(
        matches!(&answer.from, Party::Agent { worker, .. } if worker.agent.as_str() == "codex"),
        "{:?}",
        answer.from
    );
    Ok(())
}

/// A picked model that can't start, such as one no agent provides, leaves the answer to the
/// main agent, and one line says so. The task's report goes on.
#[tokio::test]
async fn a_picked_model_that_cant_answer_leaves_it_to_the_main_agent() -> anyhow::Result<()> {
    let mut crew = three_provider_crew()?;
    crew.settings.approvals.answers = Some(Answerer::Model("nobody/x".parse()?));
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert!(
        matches!(&answer.from, Party::Agent { worker, .. } if worker.agent.as_str() == "codex"),
        "{:?}",
        answer.from
    );
    let said = notes(&events).join("\n");
    assert!(
        said.contains(
            "couldn't answer the question from build:add-the, so GPT X on Codex answers it"
        ),
        "{said}"
    );
    Ok(())
}

/// At a phase's end too, the main agent may size what's left of the workflow, never swap it
/// for another template. Its pick of maker-checker is refused, and the run stays on the fixed phases.
#[tokio::test]
async fn at_a_phase_end_the_main_agent_keeps_the_workflow() -> anyhow::Result<()> {
    let picks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut crew = Crew::new()?;
    with_checks(&mut crew);
    let main_agent = {
        let picks = picks.clone();
        FakeRunner::cli("claude-code/opus", "anthropic").respond_with(move |spec: &SessionSpec| {
            if spec.step == Some(StepName::DesignWorkflow) {
                // The run's design keeps the workflow; each phase end wants another template.
                let template = if picks.fetch_add(1, Ordering::SeqCst) == 0 {
                    "base"
                } else {
                    "maker-checker"
                };
                return FakeSession::structured(support::brief(
                    Some(template),
                    serde_json::json!([]),
                    None,
                    "One small change",
                ));
            }
            if spec.step == Some(StepName::Build) {
                std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                    .expect("the build session writes its file");
                return answer(spec).with_event(support::wrote(FILE));
            }
            if spec.step == Some(StepName::PlanToTasks) {
                return FakeSession::structured(serde_json::json!({
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
                }));
            }
            answer(spec)
        })
    };
    crew.services.workers.add(main_agent);
    crew.settings.crew.main = "claude-code/opus".parse()?;
    crew.settings.crew.add("claude-code/opus".parse()?);
    enlist(&mut crew, "codex/gpt-x", "openai");
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(answer),
    );
    crew.settings.ultracode = true;
    crew.settings.workflows.adapt_at_phase_ends = true;

    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        picks.load(Ordering::SeqCst) > 1,
        "no phase end asked the main agent"
    );
    let changed = events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Workflow(
                argonauts_core::runs::events::workflow::WorkflowEvent::Changed(_)
            )
        )
    });
    assert!(!changed, "the workflow changed: {:?}", names(events));
    let from = started_from(events);
    assert!(
        !from.contains(&Some(WorkflowRef::new("maker-checker"))),
        "{from:?}"
    );
    let said = notes(events);
    assert!(
        said.iter().any(|note| note
            .contains("broke the rules twice, so the workflow goes on unchanged")
            && note.contains("keep `base`, the workflow's steps")),
        "{said:#?}"
    );
    Ok(())
}
