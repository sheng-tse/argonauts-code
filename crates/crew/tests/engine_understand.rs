//! Understand scenarios: three fake agents from three providers read a request back, the
//! comparers list how the readings are different, and code settles, drafts, and asks.

mod support;

use std::sync::Arc;

use serde_json::{Value, json};

use argonauts_core::engine::EngineExit;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText,
};
use argonauts_core::events::goal::{
    GoalChangeResolution, GoalEvent, GoalFile, GoalQuestion, LET_THE_MAIN_AGENT_ANSWER,
};
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::hash::sha256_hex;
use argonauts_core::names::MessageId;
use argonauts_core::session_runner::Access;
use argonauts_core::settings::approvals::{Answerer, ApprovalMode, GoalChanges};
use argonauts_core::steps::StepName;
use argonauts_core::steps::understand::DiffTag;
use argonauts_crew::state::RunState;
use argonauts_crew::understand::changes::{Route, route};
use argonauts_crew::understand::{file, required_hash};
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, Outcome, answer};

const REQUEST: &str = "Add a rate limit to the public API";

/// Understand alone, as `council` runs it, so these scenarios stop at the approved goal.
const UNDERSTAND_ALONE: &str = "\
name: understand-alone
description: Understand alone
phases:
  - name: Understand
    standard: understand
    steps:
      - kind: loop_until
        until: understood
        max: 2
        steps:
          - kind: read
          - kind: compare
          - kind: approval
            approval: understand
";

fn reading(summary: &str) -> Value {
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
            "basis": "2 of 3 readings read it this way"
        }
    })
}

fn listing(differences: Vec<Value>) -> Value {
    json!({
        "agreements": ["The public API gets a rate limit."],
        "differences": differences
    })
}

/// Three agents from three providers, each answering its reading and then, if it's picked as
/// a comparer, the listing. `rounds` queues that many rounds of answers.
fn crew(listing: &Value, rounds: usize) -> anyhow::Result<(Crew, Vec<Arc<FakeRunner>>)> {
    let mut crew = Crew::new()?;
    crew.repo.write(
        ".argonauts/workflows/understand-alone.yaml",
        UNDERSTAND_ALONE,
    )?;
    crew.repo.commit_all("Add the Understand workflow")?;
    crew.settings.workflow = "understand-alone".to_owned();
    // These cover the questions you answer; `engine_autonomy` covers the main agent answering them.
    crew.settings.approvals.answers = Some(Answerer::You);
    let summaries = [
        "Add a per user rate limit to the public API.",
        "Limit each user's requests to the public API.",
        "Add a rate limit per API key to the public API.",
    ];
    let mut workers = Vec::new();
    for ((worker, provider), summary) in [
        ("claude-code/opus", "anthropic"),
        ("codex", "gpt"),
        ("agy", "gemini"),
    ]
    .into_iter()
    .zip(summaries)
    {
        let fake = crew.worker(worker, provider);
        for _ in 0..rounds {
            fake.push(FakeSession::structured(reading(summary)));
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

fn opened(events: &[RunEvent], kind: ApprovalKind) -> Vec<&ApprovalOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.as_ref())
            }
            _ => None,
        })
        .collect()
}

fn questions_of(opened: &ApprovalOpened) -> Vec<GoalQuestion> {
    match &opened.packet {
        ApprovalPacket::Understand(packet) => packet.questions.clone(),
        other => panic!("an Understand approval with {other:?}"),
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

/// Answers the newest open approval of `kind` and drives the run on.
async fn decide(
    crew: &Crew,
    outcome: &Outcome,
    kind: ApprovalKind,
    action: AnswerAction,
    text: Option<&str>,
) -> anyhow::Result<Outcome> {
    let (command, reply) = answer(&outcome.events, kind, action, text)?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    Ok(next)
}

fn waiting(outcome: &Outcome) -> bool {
    matches!(outcome.exit, EngineExit::Waiting(_))
}

/// The goal file as Argonauts Code keeps it under its home. The run's branch never holds it.
fn committed_goal(crew: &Crew, events: &[RunEvent], file: &GoalFile) -> anyhow::Result<String> {
    let run = &events[0].run_id;
    let on_branch = crew.repo.git(&[
        "ls-tree",
        "-r",
        "--name-only",
        &format!("argonauts/{run}/merged"),
    ])?;
    let name = format!("{run}.md");
    anyhow::ensure!(
        !on_branch.lines().any(|path| path.ends_with(&name)),
        "{on_branch}"
    );
    let state_dir = crew.repo_context()?.state_dir().to_path_buf();
    Ok(std::fs::read_to_string(state_dir.join(&file.path))?)
}

#[tokio::test]
async fn goal_scope_difference_becomes_question() -> anyhow::Result<()> {
    let limit_scope = difference(
        "limit per user or per API key",
        "scope",
        &[("A", "per user"), ("B", "per user"), ("C", "per API key")],
    );
    let (mut crew, _) = crew(&listing(vec![limit_scope]), 1)?;
    crew.settings.approvals.understand.mode = ApprovalMode::Auto;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;

    // Two of three agree, but scope is always a question, and here you answer, even in auto.
    assert_eq!(settled(&outcome.events), (0, 1, vec![DiffTag::Scope], 0));
    assert!(waiting(&outcome), "{:?}", outcome.exit);
    let goal_tasks = opened(&outcome.events, ApprovalKind::Understand);
    assert_eq!(goal_tasks.len(), 1);
    let questions = questions_of(goal_tasks[0]);
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].text, "limit per user or per API key?");
    assert!(questions[0].options[0].recommended);
    assert_eq!(goal_tasks[0].title, "What should the goal say?");
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated.last().expect("the policy saw the question");
    assert_eq!(evidence.facts.open_questions, Some(1));
    assert!(approvals(&outcome.events).is_empty());

    // Answering the last question approves the goal with the answer written in.
    let next = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
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
    // No question is left, so the approval question opens with the assumption stated.
    let goal_tasks = opened(&outcome.events, ApprovalKind::Understand);
    assert_eq!(goal_tasks.len(), 1);
    assert!(questions_of(goal_tasks[0]).is_empty());
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
        let asked = opened(&outcome.events, ApprovalKind::Understand);
        assert_eq!(asked.len(), n);
        let newest = asked[n - 1];
        assert_eq!(
            newest.title,
            format!("What should the goal say? · {n} of 3")
        );
        assert_eq!(questions_of(newest).len(), 4 - n);
        assert!(approvals(&outcome.events).is_empty());
        outcome = decide(
            &crew,
            &outcome,
            ApprovalKind::Understand,
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
async fn the_understood_goal_hash_is_frozen() -> anyhow::Result<()> {
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let approved = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
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

/// With nothing to ask, Understand ends with the go-ahead: choose-only, with no typed edits.
#[tokio::test]
async fn goal_no_questions_opens_the_go_ahead() -> anyhow::Result<()> {
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    assert!(waiting(&outcome), "{:?}", outcome.exit);

    assert_eq!(settled(&outcome.events), (0, 0, Vec::new(), 0));
    let questions = opened(&outcome.events, ApprovalKind::Understand);
    assert_eq!(questions.len(), 1);
    let question = questions[0];
    assert_eq!(question.title, "Run this workflow?");
    assert_eq!(question.free_text, FreeText::None);
    assert!(questions_of(question).is_empty());
    let keys: Vec<char> = question.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, ['y', 'l', 'b', 'd', 'x']);
    let actions: Vec<&AnswerAction> = question
        .options
        .iter()
        .map(|option| &option.action)
        .collect();
    assert_eq!(
        actions,
        [
            &AnswerAction::Approve,
            &AnswerAction::Choose {
                value: LET_THE_MAIN_AGENT_ANSWER.to_owned()
            },
            &AnswerAction::SendBack,
            &AnswerAction::ShowDiff,
            &AnswerAction::Abandon,
        ]
    );
    // The draft is the consensus reading, with its criteria numbered.
    let draft = drafts(&outcome.events)[0];
    let ids: Vec<&str> = draft.criteria.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["G1", "G2"]);
    Ok(())
}

/// The go-ahead takes no typed edits: your own words go in the conversation.
#[tokio::test]
async fn goal_go_ahead_refuses_typed_edits() -> anyhow::Result<()> {
    let (crew, _) = crew(&listing(Vec::new()), 1)?;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let note = "+ Limits reset every minute";
    let (command, reply) = answer(
        &outcome.events,
        ApprovalKind::Understand,
        AnswerAction::ApproveWithEdits,
        Some(note),
    )?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert!(reply.await?.is_err());
    assert!(approvals(&next.events).is_empty());
    Ok(())
}

#[tokio::test]
async fn goal_send_back_reads_again() -> anyhow::Result<()> {
    let (mut crew, workers) = crew(&listing(Vec::new()), 2)?;
    // Every worker also compares, so each one's scripted reading and listing line up
    // round after round.
    crew.settings.understand.comparers = 3;
    let outcome = crew.start(RunMode::Crew, REQUEST).await?;
    let note = "Limit per API key, not per user";
    let next = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
        AnswerAction::SendBack,
        Some(note),
    )
    .await?;

    let readings = goal_events(&next.events)
        .into_iter()
        .filter(|event| matches!(event, GoalEvent::Read { .. }))
        .count();
    assert_eq!(readings, 6);
    assert_eq!(drafts(&next.events).len(), 2);
    assert_eq!(opened(&next.events, ApprovalKind::Understand).len(), 2);
    assert!(approvals(&next.events).is_empty());
    // Every reader's second session carries your note.
    for worker in &workers {
        let read_sessions: Vec<_> = worker
            .specs()
            .into_iter()
            .filter(|spec| spec.step == Some(StepName::Read))
            .collect();
        assert_eq!(read_sessions.len(), 2);
        let carries_note = |spec: &argonauts_core::session_runner::SessionSpec| {
            spec.prompt.iter().any(|part| part.text.contains(note))
        };
        assert!(!carries_note(&read_sessions[0]));
        assert!(carries_note(&read_sessions[1]));
    }
    let state = RunState::fold(&next.events);
    assert_eq!(state.goal.round, 2);
    // A `›` note says why the plan moved: the loop runs again.
    let notes: Vec<String> = next
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Note(note)) => {
                Some(note.text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        notes,
        ["Understand runs round 2, since understood doesn't hold yet"]
    );
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
        ApprovalKind::Understand,
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
        ApprovalKind::Understand,
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
