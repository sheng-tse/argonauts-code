//! A pull request a run opened is followed after the run ends: a round goes back to the task's
//! builder, resumed in its own session, then to its reviewer, and the fix is pushed to the same
//! branch without asking, unless it touches a protected path. Replies post as you, so a round
//! only keeps them for you to see first.

use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_core::runs::events::task::MergeEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_crew::follow::watch::ToAnswer;
use argonauts_crew::follow::{Due, Followed, Reply, Work, draft};
use argonauts_testkit::FakeSession;
use argonauts_testkit::fake_clis::fake_gh::GhPost;
use serde_json::json;

use crate::issue_draft::{BRANCH, OnAnIssue, answering_the_draft, ended_as, on_an_issue};
use crate::support::{Crew, Outcome, smallest_answer, wrote};

/// The run on the issue, ended as its draft, with `work` due on the pull request.
async fn with_a_round_due(work: Work) -> anyhow::Result<(OnAnIssue, Outcome)> {
    let on = on_an_issue()?;
    let (outcome, _) = answering_the_draft(&on.crew, draft::PUSH).await?;
    ended_as(&outcome, RunOutcome::PullRequest)?;
    let state = on.crew.repo_context()?.state_dir().to_owned();
    let mut followed = Followed::open(&state).remove(0);
    followed.due = Some(Due {
        work,
        fixed: None,
        reviewed: false,
        replies: Vec::new(),
    });
    followed.save(&state)?;
    Ok((on, outcome))
}

fn followed(crew: &Crew) -> anyhow::Result<Followed> {
    let state = crew.repo_context()?.state_dir().to_owned();
    Followed::open(&state)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("the pull request is still followed"))
}

/// The labels of the agents that started after the pull request opened.
fn round_agents(events: &[RunEvent]) -> Vec<String> {
    let opened = events
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                RunEventKind::Merge(MergeEvent::PullRequest { url: Some(_), .. })
            )
        })
        .unwrap_or(events.len());
    events[opened..]
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => started.label.clone(),
            _ => None,
        })
        .collect()
}

fn on_the_remote(on: &OnAnIssue) -> anyhow::Result<String> {
    let remote = on.remote.to_string_lossy().into_owned();
    Ok(on
        .crew
        .repo
        .git(&[
            "--git-dir",
            &remote,
            "show",
            &format!("{BRANCH}:limiter.txt"),
        ])?
        .trim()
        .to_owned())
}

fn check() -> Work {
    Work::Check {
        check: "test (ubuntu)".to_owned(),
        log: "FAILED test_rate_limit - KeyError: 'x-api-key'".to_owned(),
    }
}

/// A round's agent, which carries on its task's session under its own name.
fn in_the_round(spec: &SessionSpec) -> bool {
    spec.step.is_none() && spec.task.is_some()
}

/// The builder's sessions in a round write `text`, as a fix would.
fn fixes_with(on: &OnAnIssue, text: &'static str) {
    on.builder.respond(move |spec| {
        if !in_the_round(spec) {
            return smallest_answer(spec);
        }
        std::fs::write(spec.workdir.join("limiter.txt"), text).expect("the fix writes its file");
        smallest_answer(spec).with_event(wrote("limiter.txt"))
    });
}

#[tokio::test]
async fn a_failed_check_goes_to_the_builder_then_its_reviewer_then_the_branch() -> anyhow::Result<()>
{
    let (on, outcome) = with_a_round_due(check()).await?;
    fixes_with(&on, "per key, fixed\n");
    let builds_before = on.builder.specs().len();
    let round = on.crew.resume(outcome.events.clone(), Vec::new()).await?;
    assert_eq!(round.exit, EngineExit::Finished(RunOutcome::PullRequest));
    assert_eq!(
        round_agents(&round.events),
        ["fix:ci-round-1", "review:ci-round-1"]
    );

    let fix = &on.builder.specs()[builds_before];
    assert_eq!(fix.resume.as_deref(), Some("fake-session"), "resumed");
    let told: String = fix.prompt.iter().map(|part| part.text.as_str()).collect();
    assert!(
        told.contains("CI failed on pull request #57: test (ubuntu)."),
        "{told}"
    );
    assert!(told.contains("<<<log\nFAILED test_rate_limit"), "{told}");

    assert_eq!(on_the_remote(&on)?, "per key, fixed");
    let after = followed(&on.crew)?;
    assert_eq!(
        (after.rounds, after.due.is_none(), after.held.as_deref()),
        (1, true, None)
    );
    let remote = on.remote.to_string_lossy().into_owned();
    let pushed = on
        .crew
        .repo
        .git(&["--git-dir", &remote, "rev-parse", BRANCH])?;
    assert_eq!(after.head.as_deref(), Some(pushed.trim()));
    assert!(
        matches!(on.fake.posted()?.as_slice(), [GhPost::Pull { .. }]),
        "the push is the round's only write"
    );
    Ok(())
}

#[tokio::test]
async fn a_fix_its_reviewer_turns_down_waits_for_you_and_isnt_pushed() -> anyhow::Result<()> {
    let (on, outcome) = with_a_round_due(check()).await?;
    fixes_with(&on, "per key, hidden\n");
    on.reviewer.respond(|spec| {
        if !in_the_round(spec) {
            return smallest_answer(spec);
        }
        FakeSession::structured(json!({
            "verdict": "request_changes",
            "summary": "It hides the error instead of fixing it.",
            "comments": [],
            "tamper_suspected": false,
        }))
    });
    let round = on.crew.resume(outcome.events.clone(), Vec::new()).await?;
    assert_eq!(round.exit, EngineExit::Finished(RunOutcome::PullRequest));
    assert_eq!(on_the_remote(&on)?, "per key", "nothing was pushed");
    let after = followed(&on.crew)?;
    assert_eq!(
        after.held.as_deref(),
        Some("codex/gpt-x asked for changes to the fix: It hides the error instead of fixing it.")
    );
    assert!(after.due.is_none());
    Ok(())
}

/// A fix that changes a protected path, such as the CI workflow a comment asked about, is
/// held for you: GitHub would run it with the repository's secrets.
#[tokio::test]
async fn a_fix_that_touches_github_waits_for_you() -> anyhow::Result<()> {
    let (on, outcome) = with_a_round_due(check()).await?;
    on.builder.respond(move |spec| {
        if !in_the_round(spec) {
            return smallest_answer(spec);
        }
        let workflows = spec.workdir.join(".github/workflows");
        std::fs::create_dir_all(&workflows).expect("the workflows folder");
        std::fs::write(workflows.join("ci.yml"), "run: env\n").expect("the fix writes it");
        smallest_answer(spec).with_event(wrote(".github/workflows/ci.yml"))
    });
    let round = on.crew.resume(outcome.events.clone(), Vec::new()).await?;
    assert_eq!(round.exit, EngineExit::Finished(RunOutcome::PullRequest));
    assert_eq!(on_the_remote(&on)?, "per key", "nothing was pushed");
    let after = followed(&on.crew)?;
    assert_eq!(
        after.held.as_deref(),
        Some("the fix changes .github/workflows/ci.yml, which only you may push")
    );
    Ok(())
}

#[tokio::test]
async fn a_comment_answered_in_words_keeps_its_reply_for_you() -> anyhow::Result<()> {
    let comment = ToAnswer {
        id: "701".to_owned(),
        author: "coderabbitai[bot]".to_owned(),
        at: Some("limiter.txt:1".to_owned()),
        body: "Name the bucket after the key.".to_owned(),
        thread: Some(701),
    };
    let (on, outcome) = with_a_round_due(Work::Comments {
        comments: vec![comment],
    })
    .await?;
    on.builder.respond(|spec| {
        if in_the_round(spec) && spec.json_schema.is_some() {
            return FakeSession::structured(json!({
                "answers": [{"comment": 1, "changed": false, "reply": "It is named per key already."}]
            }));
        }
        smallest_answer(spec)
    });
    let round = on.crew.resume(outcome.events.clone(), Vec::new()).await?;
    assert_eq!(round.exit, EngineExit::Finished(RunOutcome::PullRequest));
    assert_eq!(round_agents(&round.events), ["fix:comments-round-1"]);
    let after = followed(&on.crew)?;
    assert_eq!(
        after.replies,
        [Reply {
            to: "coderabbitai[bot]".to_owned(),
            thread: Some(701),
            body: "It is named per key already.".to_owned(),
        }]
    );
    assert_eq!(
        (after.rounds, after.answered.as_slice()),
        (1, ["701".to_owned()].as_slice())
    );
    assert_eq!(on_the_remote(&on)?, "per key", "a reply changes no code");
    assert!(
        matches!(on.fake.posted()?.as_slice(), [GhPost::Pull { .. }]),
        "the reply waits for you"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_with_no_round_due_ends_at_once() -> anyhow::Result<()> {
    let on = on_an_issue()?;
    let (outcome, _) = answering_the_draft(&on.crew, draft::PUSH).await?;
    let again = on.crew.resume(outcome.events.clone(), Vec::new()).await?;
    assert_eq!(again.exit, EngineExit::Finished(RunOutcome::PullRequest));
    assert_eq!(
        again.events.len(),
        outcome.events.len(),
        "nothing more was done"
    );
    Ok(())
}
