//! Goal alignment scenarios: three fake agents from three families restate a request, the
//! differs list how the restatements differ, and code settles, drafts, and asks.

mod support;

use std::sync::Arc;

use serde_json::{Value, json};

use argonauts_core::engine::EngineExit;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket,
};
use argonauts_core::events::goal::{GoalChangeResolution, GoalEvent, GoalFile, GoalQuestion};
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::hash::sha256_hex;
use argonauts_core::names::MessageId;
use argonauts_core::settings::approvals::{ApprovalMode, GoalChanges};
use argonauts_core::steps::StepName;
use argonauts_core::steps::goal::DiffTag;
use argonauts_core::worker::Access;
use argonauts_core::workflow::WorkflowRef;
use argonauts_crew::goal::changes::{Route, route};
use argonauts_crew::goal::{file, required_hash};
use argonauts_crew::state::RunState;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

use support::{Crew, Outcome, answer};

const REQUEST: &str = "Add a rate limit to the public API";

/// Goal alignment alone, as `peer-crew` runs it, so these scenarios stop at the approved goal.
const GOAL_ALIGNMENT: &str = "\
name: goal-alignment
description: Goal alignment alone
phases:
  - name: Goal
    standard: understand
    steps:
      - kind: loop_until
        until: goal_approved
        max: 2
        steps:
          - kind: restate
          - kind: differ
          - kind: checkpoint
            checkpoint: goal
";

fn restatement(summary: &str) -> Value {
    json!({
        "summary": summary,
        "success_criteria": [
            "Requests over the limit get a 429 response",
            "The limit is configurable"
        ],
        "in_scope": ["the public HTTP API"],
        "out_of_scope": ["the admin API"],
        "assumptions": [],
        "open_questions": []
    })
}

fn difference(topic: &str, tag: &str, readings: &[(&str, &str)]) -> Value {
    let mut options: Vec<&str> = Vec::new();
    for (_, reading) in readings {
        if !options.contains(reading) {
            options.push(reading);
        }
    }
    let readings: Vec<Value> = readings
        .iter()
        .map(|(label, reading)| json!({"label": label, "reading": reading}))
        .collect();
    json!({
        "topic": topic,
        "readings": readings,
        "tag": tag,
        "question": {
            "text": format!("{topic}?"),
            "options": options,
            "recommended": 0,
            "basis": "2 of 3 restatements read it this way"
        }
    })
}

fn listing(differences: Vec<Value>) -> Value {
    json!({
        "agreements": ["The public API gets a rate limit."],
        "differences": differences
    })
}

/// Three agents from three families, each answering its restatement and then, if it's picked as
/// a differ, the listing. `rounds` queues that many rounds of answers.
fn crew(listing: &Value, rounds: usize) -> anyhow::Result<(Crew, Vec<Arc<FakeWorker>>)> {
    let mut crew = Crew::new()?;
    crew.repo
        .write(".argonauts/workflows/goal-alignment.yaml", GOAL_ALIGNMENT)?;
    crew.repo.commit_all("Add the goal alignment workflow")?;
    crew.settings.workflow = WorkflowRef::new("goal-alignment");
    let summaries = [
        "Add a per user rate limit to the public API.",
        "Limit each user's requests to the public API.",
        "Add a rate limit per API key to the public API.",
    ];
    let mut workers = Vec::new();
    for ((worker, family), summary) in [
        ("claude-code/opus", "claude"),
        ("codex", "gpt"),
        ("agy", "gemini"),
    ]
    .into_iter()
    .zip(summaries)
    {
        let fake = crew.worker(worker, family);
        for _ in 0..rounds {
            fake.push(FakeSession::structured(restatement(summary)));
            fake.push(FakeSession::structured(listing.clone()));
        }
        workers.push(fake);
    }
    Ok((crew, workers))
}

fn goal_events(events: &[RunEvent]) -> Vec<&GoalEvent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(goal) => Some(goal),
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent], kind: CheckpointKind) -> Vec<&CheckpointOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.as_ref())
            }
            _ => None,
        })
        .collect()
}

fn questions_of(opened: &CheckpointOpened) -> Vec<GoalQuestion> {
    match &opened.packet {
        CheckpointPacket::Goal(packet) => packet.questions.clone(),
        other => panic!("a goal checkpoint with {other:?}"),
    }
}

fn settled(events: &[RunEvent]) -> (usize, usize, Vec<DiffTag>, u32) {
    goal_events(events)
        .into_iter()
        .rev()
        .find_map(|event| match event {
            GoalEvent::Settled {
                assumptions,
                questions,
                dropped,
                ..
            } => Some((
                assumptions.len(),
                questions.len(),
                questions.iter().map(|question| question.tag).collect(),
                *dropped,
            )),
            _ => None,
        })
        .expect("the differences were settled")
}

fn drafts(events: &[RunEvent]) -> Vec<&GoalFile> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::Drafted(file) => Some(file),
            _ => None,
        })
        .collect()
}

fn approvals(events: &[RunEvent]) -> Vec<(&GoalFile, &Party)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::Approved { file, by } => Some((file, by)),
            _ => None,
        })
        .collect()
}

/// Answers the newest open checkpoint of `kind` and drives the run on.
async fn decide(
    crew: &Crew,
    outcome: &Outcome,
    kind: CheckpointKind,
    action: AnswerAction,
    text: Option<&str>,
) -> anyhow::Result<Outcome> {
    let (command, reply) = answer(&outcome.events, kind, action, text)?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    Ok(next)
}

fn parked(outcome: &Outcome) -> bool {
    matches!(outcome.exit, EngineExit::Parked(_))
}

/// The goal file as the conductor committed it on the integration branch.
fn committed_goal(crew: &Crew, events: &[RunEvent], file: &GoalFile) -> anyhow::Result<String> {
    let run = &events[0].run_id;
    crew.repo.git(&[
        "show",
        &format!(
            "argonauts/{run}/integration:{}",
            file.path.to_string_lossy()
        ),
    ])
}

#[tokio::test]
async fn goal_scope_difference_becomes_question() -> anyhow::Result<()> {
    let limit_scope = difference(
        "limit per user or per API key",
        "scope",
        &[("A", "per user"), ("B", "per user"), ("C", "per API key")],
    );
    let (mut crew, _) = crew(&listing(vec![limit_scope]), 1)?;
    crew.settings.approvals.goal.mode = ApprovalMode::Auto;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;

    // Two of three agree, but scope always reaches you, even in auto mode.
    assert_eq!(settled(&outcome.events), (0, 1, vec![DiffTag::Scope], 0));
    assert!(parked(&outcome), "{:?}", outcome.exit);
    let goal_cards = opened(&outcome.events, CheckpointKind::Goal);
    assert_eq!(goal_cards.len(), 1);
    let questions = questions_of(goal_cards[0]);
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].text, "limit per user or per API key?");
    assert!(questions[0].options[0].recommended);
    assert_eq!(goal_cards[0].title, "What should the goal say?");
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated.last().expect("the policy saw the question");
    assert_eq!(evidence.facts.open_questions, Some(1));
    assert!(approvals(&outcome.events).is_empty());

    // Answering the last question approves the goal with the answer written in.
    let next = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::Choose {
            value: "per API key".to_owned(),
        },
        None,
    )
    .await?;
    let approved = approvals(&next.events);
    assert_eq!(approved.len(), 1);
    assert_eq!(*approved[0].1, Party::User);
    assert_eq!(
        approved[0].0.answers,
        ["limit per user or per API key: per API key"]
    );
    Ok(())
}

#[tokio::test]
async fn goal_majority_becomes_assumption() -> anyhow::Result<()> {
    let detail = difference(
        "limit per user or per API key",
        "detail",
        &[("A", "per user"), ("B", "per user"), ("C", "per API key")],
    );
    let wording = difference("phrasing", "wording", &[("A", "limit"), ("B", "throttle")]);
    let (crew, _) = crew(&listing(vec![detail, wording]), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;

    assert_eq!(settled(&outcome.events), (1, 0, Vec::new(), 1));
    let draft = drafts(&outcome.events)
        .last()
        .copied()
        .expect("a goal draft")
        .clone();
    assert_eq!(draft.assumptions.len(), 1);
    assert_eq!(
        draft.assumptions[0].text,
        "limit per user or per API key: per user"
    );
    assert_eq!(
        draft.assumptions[0].basis,
        "a detail, and 2 of 3 read it that way"
    );
    // No question is left, so the approval card opens with the assumption stated.
    let goal_cards = opened(&outcome.events, CheckpointKind::Goal);
    assert_eq!(goal_cards.len(), 1);
    assert!(questions_of(goal_cards[0]).is_empty());
    let committed = committed_goal(&crew, &outcome.events, &draft)?;
    assert!(committed.contains("limit per user or per API key: per user"));
    Ok(())
}

#[tokio::test]
async fn goal_at_most_three_questions() -> anyhow::Result<()> {
    let split = [("A", "one way"), ("B", "another way"), ("C", "a third way")];
    let differences = vec![
        difference("how detailed the logs are", "detail", &split),
        difference("what it costs to run", "cost", &split),
        difference("which data is kept", "data", &split),
        difference("who may raise the limit", "security", &split),
        difference("which endpoints are limited", "scope", &split),
    ];
    let (crew, _) = crew(&listing(differences), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;

    let (assumptions, questions, tags, _) = settled(&outcome.events);
    assert_eq!(questions, 3);
    assert_eq!(tags, [DiffTag::Scope, DiffTag::Security, DiffTag::Data]);
    // The two left over are stated as assumptions with their recommended answer.
    assert_eq!(assumptions, 2);

    let mut outcome = outcome;
    for n in 1..=3 {
        let cards = opened(&outcome.events, CheckpointKind::Goal);
        assert_eq!(cards.len(), n);
        let card = cards[n - 1];
        assert_eq!(card.title, format!("What should the goal say? · {n} of 3"));
        assert_eq!(questions_of(card).len(), 4 - n);
        assert!(approvals(&outcome.events).is_empty());
        outcome = decide(
            &crew,
            &outcome,
            CheckpointKind::Goal,
            AnswerAction::Choose {
                value: "one way".to_owned(),
            },
            None,
        )
        .await?;
    }
    let approved = approvals(&outcome.events);
    assert_eq!(approved.len(), 1);
    let goal = approved[0].0;
    assert_eq!(goal.answers.len(), 3);
    assert!(goal.answers[0].starts_with("which endpoints are limited: "));
    let stated: Vec<&str> = goal.assumptions.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(
        stated,
        [
            "what it costs to run: one way",
            "how detailed the logs are: one way"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn goal_approved_hash_frozen() -> anyhow::Result<()> {
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let approved = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::Approve,
        None,
    )
    .await?;

    let approvals = approvals(&approved.events);
    assert_eq!(approvals.len(), 1);
    let (goal, by) = approvals[0];
    assert_eq!(*by, Party::User);
    let committed = committed_goal(&crew, &approved.events, goal)?;
    assert_eq!(committed.trim_end(), file::render(goal).trim_end());
    assert_eq!(goal.sha256, sha256_hex(file::render(goal).as_bytes()));

    // Replaying the log changes nothing: no second approval, the same hash.
    let replayed = crew.resume(approved.events.clone(), Vec::new()).await?;
    assert_eq!(
        replayed
            .events
            .iter()
            .filter(|event| matches!(&event.kind, RunEventKind::Goal(GoalEvent::Approved { .. })))
            .count(),
        1
    );
    let state = RunState::fold(&replayed.events);
    assert_eq!(state.goal.approved_sha256(), Some(goal.sha256.as_str()));
    assert_eq!(required_hash(&state)?, goal.sha256);
    Ok(())
}

#[tokio::test]
async fn goal_no_questions_opens_approval_card() -> anyhow::Result<()> {
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    assert!(parked(&outcome), "{:?}", outcome.exit);

    assert_eq!(settled(&outcome.events), (0, 0, Vec::new(), 0));
    let cards = opened(&outcome.events, CheckpointKind::Goal);
    assert_eq!(cards.len(), 1);
    let card = cards[0];
    assert_eq!(card.title, "Approve this goal?");
    assert!(questions_of(card).is_empty());
    let keys: Vec<char> = card.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, ['y', 'e', 'b', 'x', 'd']);
    let actions: Vec<&AnswerAction> = card.options.iter().map(|option| &option.action).collect();
    assert_eq!(
        actions,
        [
            &AnswerAction::Approve,
            &AnswerAction::ApproveWithEdits,
            &AnswerAction::SendBack,
            &AnswerAction::Abandon,
            &AnswerAction::ShowDiff,
        ]
    );
    // The draft is the consensus restatement, with its criteria numbered.
    let draft = drafts(&outcome.events)[0];
    let ids: Vec<&str> = draft.criteria.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["G1", "G2"]);
    Ok(())
}

#[tokio::test]
async fn goal_approve_with_edits_rehashes() -> anyhow::Result<()> {
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let first = drafts(&outcome.events)[0].clone();

    let note = "+ Limits reset every minute\nG2: The limit is set per route";
    let next = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::ApproveWithEdits,
        Some(note),
    )
    .await?;
    let approved = approvals(&next.events);
    assert_eq!(approved.len(), 1);
    let goal = approved[0].0;
    assert_ne!(goal.sha256, first.sha256);
    assert_eq!(goal.sha256, file::sha256(goal));
    let criteria: Vec<(&str, &str)> = goal
        .criteria
        .iter()
        .map(|c| (c.id.as_str(), c.text.as_str()))
        .collect();
    assert_eq!(
        criteria,
        [
            ("G1", "Requests over the limit get a 429 response"),
            ("G2", "The limit is set per route"),
            ("G3", "Limits reset every minute"),
        ]
    );
    assert_eq!(
        goal.answers,
        ["Your edits: + Limits reset every minute; G2: The limit is set per route"]
    );
    let committed = committed_goal(&crew, &next.events, goal)?;
    assert!(committed.contains("G3: Limits reset every minute"));
    Ok(())
}

#[tokio::test]
async fn goal_send_back_restates_again() -> anyhow::Result<()> {
    let (mut crew, workers) = crew(&listing(Vec::new()), 2)?;
    // Every worker also differs, so each one's scripted restatement and listing line up
    // round after round.
    crew.settings.goal.differs = 3;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let note = "Limit per API key, not per user";
    let next = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::SendBack,
        Some(note),
    )
    .await?;

    let restated = goal_events(&next.events)
        .into_iter()
        .filter(|event| matches!(event, GoalEvent::Restated { .. }))
        .count();
    assert_eq!(restated, 6);
    assert_eq!(drafts(&next.events).len(), 2);
    assert_eq!(opened(&next.events, CheckpointKind::Goal).len(), 2);
    assert!(approvals(&next.events).is_empty());
    // Every restater's second session carries your note.
    for worker in &workers {
        let restates: Vec<_> = worker
            .specs()
            .into_iter()
            .filter(|spec| spec.step == Some(StepName::Restate))
            .collect();
        assert_eq!(restates.len(), 2);
        let carries_note = |spec: &argonauts_core::worker::SessionSpec| {
            spec.prompt.iter().any(|part| part.text.contains(note))
        };
        assert!(!carries_note(&restates[0]));
        assert!(carries_note(&restates[1]));
    }
    let state = RunState::fold(&next.events);
    assert_eq!(state.goal.round, 2);
    Ok(())
}

#[tokio::test]
async fn goal_changes_ask_allow_deny() -> anyhow::Result<()> {
    // What each setting does with a clarification and with a real change.
    assert_eq!(route(GoalChanges::Ask, true), Route::AskYou);
    assert_eq!(route(GoalChanges::Ask, false), Route::AskYou);
    assert_eq!(route(GoalChanges::AllowClarifications, true), Route::Record);
    assert_eq!(
        route(GoalChanges::AllowClarifications, false),
        Route::AskYou
    );
    assert_eq!(route(GoalChanges::Deny, true), Route::Refuse);
    assert_eq!(route(GoalChanges::Deny, false), Route::Refuse);

    // A frozen goal only moves on an accepted change, and then by version.
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let approved = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::Approve,
        None,
    )
    .await?;
    let mut events = approved.events.clone();
    let frozen = RunState::fold(&events).goal.approved.expect("approved");
    let id = MessageId::numbered(90);
    let mut push = |kind: GoalEvent| {
        let mut event = events.last().expect("a log").clone();
        event.seq += 1;
        event.kind = RunEventKind::Goal(kind);
        events.push(event);
    };
    push(GoalEvent::ChangeRequested {
        id: id.clone(),
        from: Party::User,
        text: "Also limit the admin API".to_owned(),
        clarification: false,
    });
    push(GoalEvent::ChangeResolved {
        id: id.clone(),
        resolution: GoalChangeResolution::Refused,
        new_sha256: None,
    });
    let state = RunState::fold(&events);
    assert_eq!(state.goal.approved_sha256(), Some(frozen.sha256.as_str()));
    assert_eq!(
        state.goal.change(&id).and_then(|change| change.resolved()),
        Some(GoalChangeResolution::Refused)
    );
    Ok(())
}

#[tokio::test]
async fn build_refused_without_goal_hash() -> anyhow::Result<()> {
    assert!(required_hash(&RunState::default()).is_err());

    let (crew, workers) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let state = RunState::fold(&outcome.events);
    assert!(state.goal.drafted.is_some());
    assert!(required_hash(&state).is_err());
    // Until the goal is approved, nothing but read-only and no-tools sessions started.
    for worker in &workers {
        for spec in worker.specs() {
            assert!(
                matches!(spec.access, Access::ReadOnly | Access::NoTools),
                "{:?} started with {:?}",
                spec.step,
                spec.access
            );
            assert_eq!(spec.goal_sha256, None);
        }
    }

    let approved = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::Approve,
        None,
    )
    .await?;
    let state = RunState::fold(&approved.events);
    assert_eq!(
        required_hash(&state)?,
        approvals(&approved.events)[0].0.sha256
    );
    Ok(())
}
