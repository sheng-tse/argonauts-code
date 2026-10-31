//! A council's talk end to end with fake seats: first answers alone, the note keeper's points,
//! tapbacks, one seat writing at a time until every seat agrees, and the plan that goes on to
//! your approval; and a talk that goes nowhere, stopped by its guard, with you settling what's
//! still open.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::SessionControl;
use argonauts_core::agents::session_runner::{Access, SessionSpec};
use argonauts_core::base::names::{MessageId, SeatId, WorkflowId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::engine::EngineCommand;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalPacket, CouncilQuestion,
};
use argonauts_core::runs::events::council::{
    CouncilAction, CouncilEvent, CouncilEventKind, CouncilMessage, FloorReason, HeldChoice,
    HeldWhy, MessageKind, PointState, Seat, SeatRole, Seating, Speaker, Stance, TalkStop, TextPart,
};
use argonauts_core::runs::events::plan::PlanEvent;
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_core::runs::events::task::{TaskEvent, TaskKind, TaskLimit, TaskSpec};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::council::{CouncilSeat, WhenYouWrite};
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::StepRef;
use argonauts_crew::council::lexicon::Lexicon;
use argonauts_crew::council::talk;
use argonauts_crew::task::worktree;
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use argonauts_testkit::pty::deadline;

use crate::support::{self, smallest_answer};

const SEATS: [(&str, &str); 2] = [("claude-code/opus", "anthropic"), ("codex/gpt-5", "openai")];

/// Which move a turn's prompt asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    First,
    Points,
    Tapback,
    Message,
    Plan,
    Private,
}

pub(crate) fn asked(spec: &SessionSpec) -> Option<Asked> {
    if spec.step != Some(StepName::Talk) {
        return None;
    }
    let prompt = text(spec);
    let found = [
        ("Write your first answer", Asked::First),
        ("You keep the council's notes", Asked::Points),
        ("A point changed. React", Asked::Tapback),
        ("It's your turn to write", Asked::Message),
        ("Every point is settled", Asked::Plan),
        ("The user wrote to you in private", Asked::Private),
    ];
    found
        .iter()
        .find(|(cue, _)| prompt.contains(cue))
        .map(|(_, asked)| *asked)
}

pub(crate) fn text(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn turn(message: &str, stances: Value, extra: Value) -> FakeSession {
    let mut answer = json!({
        "message": message,
        "reply_to": null,
        "quote": null,
        "stances": stances,
        "new_points": [],
        "reword": [],
        "points": [],
        "plan": null,
    });
    if let (Some(answer), Some(extra)) = (answer.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            answer.insert(key.clone(), value.clone());
        }
    }
    FakeSession::structured(answer)
}

pub(crate) fn stance(point: u32, stance: &str, because: Option<&str>) -> Value {
    json!({ "point": point, "stance": stance, "because": because, "message": null, "quote": null })
}

/// The note keeper's list, whichever seat keeps the notes.
pub(crate) fn points() -> FakeSession {
    turn(
        "Two points: where counters live, and the shape of the limit.",
        json!([]),
        json!({ "points": [
            { "words": "A token bucket per key", "sides": [] },
            { "words": "Counters live in Redis", "sides": [
                { "words": "Redis", "seats": [] },
                { "words": "In memory", "seats": [] },
            ] },
        ] }),
    )
}

/// [`points`], each side of the open point with its reason.
fn points_with_reasons() -> FakeSession {
    turn(
        "Two points: where counters live, and the shape of the limit.",
        json!([]),
        json!({ "points": [
            { "words": "A token bucket per key", "sides": [] },
            { "words": "Counters live in Redis", "sides": [
                { "words": "Redis", "seats": [], "why": "Every node reads one count." },
                { "words": "In memory", "seats": [], "why": "There is no service to run." },
            ] },
        ] }),
    )
}

pub(crate) fn plan() -> FakeSession {
    turn(
        "One task: the limiter, by the first seat.",
        json!([]),
        json!({ "plan": {
            "summary": "A token bucket per key, its counters in Redis.",
            "approach": "Add the limiter as middleware.",
            "pieces": [],
            "choices": [],
            "risks": [],
            "test_strategy": "Unit tests for the bucket.",
            "new_dependencies": [],
        } }),
    )
}

/// The plan as one task, the limiter.
pub(crate) fn one_task() -> FakeSession {
    FakeSession::structured(json!({ "tasks": [{
        "key": "limiter",
        "title": "Add the rate limiter",
        "task": "Add a token bucket per key, its counters in Redis.",
        "kind": "build",
        "owned_paths": ["src/limiter.rs"],
        "read_only_paths": [],
        "depends_on": [],
        "criteria": [],
    }] }))
}

/// A council of two seats, each answering its moves with `answer`.
fn talking_crew(
    answer: impl Fn(&str, Asked, &SessionSpec) -> FakeSession + Send + Sync + Clone + 'static,
) -> anyhow::Result<(support::Crew, Vec<Arc<FakeRunner>>)> {
    seated_crew(&SEATS, answer)
}

/// [`talking_crew`], with a seat for each of `seats`' workers and providers.
pub(crate) fn seated_crew(
    seats: &[(&str, &str)],
    answer: impl Fn(&str, Asked, &SessionSpec) -> FakeSession + Send + Sync + Clone + 'static,
) -> anyhow::Result<(support::Crew, Vec<Arc<FakeRunner>>)> {
    let mut crew = support::Crew::new()?;
    let mut fakes = Vec::new();
    for &(worker, provider) in seats {
        let answer = answer.clone();
        let name = worker.to_owned();
        let fake = FakeRunner::cli(worker, provider).respond_with(move |spec| match asked(spec) {
            Some(move_asked) => answer(&name, move_asked, spec),
            None if spec.step == Some(StepName::PlanToTasks) => one_task(),
            None => smallest_answer(spec),
        });
        fakes.push(crew.enlist(fake));
    }
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.divide.contracts_first = false;
    Ok((crew, fakes))
}

fn council_events(events: &[RunEvent]) -> Vec<CouncilEventKind> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Council(CouncilEvent { kind, .. }) => Some(kind.clone()),
            _ => None,
        })
        .collect()
}

/// Starts the run and answers every approval it waits on: open points with `point`, everything
/// else with its recommended option.
pub(crate) async fn drive(
    crew: &support::Crew,
    point: Option<&str>,
) -> anyhow::Result<(EngineExit, Vec<RunEvent>)> {
    drive_asking(crew, "add per-key rate limiting to the public API", point).await
}

/// [`drive`], for a run whose request is `request`.
async fn drive_asking(
    crew: &support::Crew,
    request: &str,
    point: Option<&str>,
) -> anyhow::Result<(EngineExit, Vec<RunEvent>)> {
    let pick = point.map(|side| AnswerAction::Choose {
        value: side.to_owned(),
    });
    drive_answering(crew, request, pick).await
}

/// [`drive`], answering a question on a point left open with `answer`.
async fn drive_answering(
    crew: &support::Crew,
    request: &str,
    answer: Option<AnswerAction>,
) -> anyhow::Result<(EngineExit, Vec<RunEvent>)> {
    let outcome = crew.start(RunMode::Crew, request).await?;
    let mut exit = outcome.exit;
    let mut history = outcome.events;
    for _ in 0..20 {
        if !matches!(exit, EngineExit::Waiting(_)) {
            break;
        }
        let open_point = open_council_question(&history);
        let (command, reply) = match (open_point, &answer) {
            (Some(()), Some(action)) => {
                support::answer(&history, ApprovalKind::Question, action.clone(), None)?
            }
            _ => support::recommended(&history)
                .ok_or_else(|| anyhow::anyhow!("nothing to answer"))?,
        };
        let resumed = crew.resume(history.clone(), vec![command]).await?;
        assert_eq!(reply.await?, Ok(()));
        exit = resumed.exit;
        // Each stretch hands back the whole log.
        history = resumed.events;
    }
    Ok((exit, history))
}

fn open_council_question(events: &[RunEvent]) -> Option<()> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if matches!(opened.packet, ApprovalPacket::Council(_)) =>
            {
                open.push(opened.id.clone());
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|id| id != &decided.id);
            }
            _ => {}
        }
    }
    (!open.is_empty()).then_some(())
}

/// The seats answer alone, the note keeper lists two points, the second seat objects to one,
/// and the first rewords it until both agree. The plan goes on to your approval, and the run
/// builds it.
#[tokio::test]
async fn the_seats_talk_until_every_seat_agrees_on_every_point() -> anyhow::Result<()> {
    let (crew, seats) = talking_crew(|worker, asked, spec| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::First if first => {
                turn("A token bucket per key, in Redis.", json!([]), json!({}))
            }
            Asked::First => turn("A fixed window, kept in memory.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback if text(spec).contains("Redis, one key per route") => {
                turn("", json!([stance(2, "agree", None)]), json!({}))
            }
            Asked::Tapback if first => turn(
                "",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Tapback => turn(
                "",
                json!([stance(1, "agree", None), stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Message if first => turn(
                "Then one key per route keeps Redis cheap.",
                json!([]),
                json!({ "reword": [{ "point": 2, "words": "Counters live in Redis, one key per route" }] }),
            ),
            Asked::Message => turn(
                "Redis is a round trip on every request.",
                json!([stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let council = council_events(&events);
    assert!(!council.is_empty(), "{:?}", support::names(&events));
    let CouncilEventKind::Seated(seating) = &council[0] else {
        panic!("the council didn't sit first: {council:?}");
    };
    assert_eq!(seating.seats.len(), 2);
    assert!(seating.notes.is_some());

    let firsts: Vec<&str> = council
        .iter()
        .filter_map(|kind| match kind {
            CouncilEventKind::Message(message) if message.kind == MessageKind::FirstAnswer => {
                Some(message.words(|_| String::new()))
            }
            _ => None,
        })
        .map(|words| {
            if words.contains("bucket") {
                "bucket"
            } else {
                "window"
            }
        })
        .collect();
    assert_eq!(firsts.len(), 2, "{council:?}");

    // The second seat objected, so it took the floor first, then the quietest.
    let floors: Vec<FloorReason> = council
        .iter()
        .filter_map(|kind| match kind {
            CouncilEventKind::Writing { why, .. } => Some(why.clone()),
            _ => None,
        })
        .filter(|why| !matches!(why, FloorReason::FirstAnswer | FloorReason::Notes))
        .collect();
    assert_eq!(floors, [FloorReason::Objected, FloorReason::Quietest]);

    let last_points = council
        .iter()
        .rev()
        .find_map(|kind| match kind {
            CouncilEventKind::Points { points } => Some(points.clone()),
            _ => None,
        })
        .expect("points were listed");
    let states: Vec<(&str, PointState)> = last_points
        .iter()
        .map(|point| (point.words.as_str(), point.state))
        .collect();
    assert_eq!(
        states,
        [
            ("A token bucket per key", PointState::Agreed),
            (
                "Counters live in Redis, one key per route",
                PointState::Agreed
            ),
        ]
    );
    // The words the first seat gave point 2 are its position, which both seats came to back.
    let mut councils = talk::Councils::default();
    for event in &events {
        if let RunEventKind::Council(event) = &event.kind {
            councils.apply(event);
        }
    }
    let positions = councils.newest().expect("a council sat").positions();
    let raised: Vec<(SeatId, usize)> = positions
        .iter()
        .map(|position| (position.raised_by.clone(), position.backers.len()))
        .collect();
    assert_eq!(raised, [(SeatId::numbered(1), 2)]);
    // The run review looked at the council once. With one seat of each maker, its lift says
    // nothing, so it records none.
    let reviews: Vec<usize> = council
        .iter()
        .filter_map(|kind| match kind {
            CouncilEventKind::RunReview { lift, .. } => Some(lift.len()),
            _ => None,
        })
        .collect();
    assert_eq!(reviews, [0]);
    let agreed = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Agreed { points: 2 }))
        .expect("the talk agreed");
    let plan_message = council
        .iter()
        .rposition(
            |kind| matches!(kind, CouncilEventKind::Message(m) if m.kind == MessageKind::Notes),
        )
        .expect("the note keeper wrote the plan");
    assert!(plan_message > agreed);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, RunEventKind::Plan(PlanEvent::FastPath { .. }))),
        "the plan goes on as the chosen one"
    );

    // Every tapback was a short turn with no tools, every other turn only read, and each seat
    // carried its own session on in a folder of its own.
    let mut sessions: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
    let mut folders: BTreeMap<String, BTreeSet<std::path::PathBuf>> = BTreeMap::new();
    for seat in &seats {
        for spec in seat.specs() {
            let Some(move_asked) = asked(&spec) else {
                continue;
            };
            let only_reads = if move_asked == Asked::Tapback {
                Access::NoTools
            } else {
                Access::ReadOnly
            };
            assert_eq!(spec.access, only_reads, "{move_asked:?}");
            sessions
                .entry(spec.model.to_string())
                .or_default()
                .push(spec.resume.clone());
            folders
                .entry(spec.model.to_string())
                .or_default()
                .insert(spec.workdir.clone());
        }
    }
    let own: Vec<&std::path::PathBuf> = folders
        .values()
        .map(|seen| {
            assert_eq!(
                seen.len(),
                1,
                "one folder from the first answer on: {seen:?}"
            );
            seen.first().expect("one folder")
        })
        .collect();
    assert_eq!(own.len(), 2);
    assert_ne!(own[0], own[1], "two seats never share a folder");
    for folder in own {
        let name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        assert!(
            name.as_deref()
                .is_some_and(|name| name.starts_with("council-")),
            "{folder:?}"
        );
    }
    // The plan's first task of each seat's model opens in that seat's folder.
    let workers: BTreeMap<TaskId, String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => {
                Some((spec.id.clone(), spec.worker.as_ref()?.to_string()))
            }
            _ => None,
        })
        .collect();
    let mut first_trees: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    for event in &events {
        if let RunEventKind::Task(TaskEvent::Worktree { path, .. }) = &event.kind
            && let Some(worker) = event.task_id.as_ref().and_then(|task| workers.get(task))
        {
            first_trees
                .entry(worker.clone())
                .or_insert_with(|| path.clone());
        }
    }
    assert!(!first_trees.is_empty(), "the plan was built: {workers:?}");
    for (worker, tree) in &first_trees {
        assert!(
            folders.get(worker).is_some_and(|seen| seen.contains(tree)),
            "{worker} builds in {tree:?}, not where it talked: {folders:?}"
        );
    }

    for (model, resumes) in &sessions {
        assert_eq!(
            resumes[0], None,
            "{model}'s first answer starts fresh: {resumes:?}"
        );
        assert!(
            resumes[1..].iter().all(Option::is_some),
            "{model} resumes its own session: {resumes:?}"
        );
    }

    // The seat that objected writes first, and its turn reads as the council talks: the point
    // the round agreed on, then its move and the point still open, its sides in either order.
    let objector = seats[1]
        .specs()
        .iter()
        .find(|spec| asked(spec) == Some(Asked::Message))
        .map(text)
        .expect("the second seat wrote");
    assert!(
        objector.contains(
            "From the council · Point 1 agreed: A token bucket per key · 1 of 2 points agreed"
        ),
        "{objector}"
    );
    assert!(
        objector.contains(
            "Your move: write your message. Point 2, Counters live in Redis, is open: Redis or \
             In memory."
        ) || objector.contains(
            "Your move: write your message. Point 2, Counters live in Redis, is open: In \
             memory or Redis."
        ),
        "{objector}"
    );
    assert!(
        !objector.contains("[m-"),
        "no message ids reach a seat: {objector}"
    );

    // A seat reads its frame as its session begins, and then carries it on. The task its seat
    // builds carries the talk's session on too.
    let mut built = Vec::new();
    for seat in &seats {
        let specs = seat.specs();
        let framed: Vec<bool> = specs
            .iter()
            .filter(|spec| asked(spec).is_some())
            .map(|spec| text(spec).contains("one of 2 seats in a council"))
            .collect();
        assert_eq!(framed.first(), Some(&true), "{framed:?}");
        assert!(framed[1..].iter().all(|framed| !framed), "{framed:?}");
        built.extend(
            specs
                .iter()
                .find(|spec| spec.step == Some(StepName::Build))
                .map(|spec| spec.resume.clone()),
        );
    }
    assert!(!built.is_empty(), "a seat built the task");
    assert!(
        built.iter().all(Option::is_some),
        "the task carries its seat's session on: {built:?}"
    );
    let seated: BTreeSet<Option<SeatId>> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.step == Some(StepName::Talk) =>
            {
                Some(started.seat.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        seated,
        BTreeSet::from([Some(SeatId::numbered(1)), Some(SeatId::numbered(2))]),
        "every talk turn speaks for its seat"
    );

    // Nothing a seat reads names a model, its maker or its CLI.
    let models: Vec<argonauts_core::base::names::ModelRef> = SEATS
        .iter()
        .map(|(model, _)| model.parse().expect("a model"))
        .collect();
    let provider = |model: &argonauts_core::base::names::ModelRef| {
        SEATS
            .iter()
            .find(|(seat, _)| model.to_string() == *seat)
            .map_or_else(String::new, |(_, provider)| (*provider).to_owned())
    };
    let lexicon = Lexicon::built_in()
        .with_models(&models, &DisplayNames::default(), &provider)
        .with_topic("add per-key rate limiting to the public API");
    for seat in &seats {
        for spec in seat.specs().iter().filter(|spec| asked(spec).is_some()) {
            let read = text(spec);
            let named: Vec<&str> = lexicon
                .find(&read)
                .into_iter()
                .map(|hit| &read[hit.range])
                .collect();
            assert!(named.is_empty(), "a seat read {named:?} in:\n{read}");
        }
    }

    // The second seat read the first seat's message as from a letter, never by its model.
    let read_by_second: Vec<String> = seats[1]
        .specs()
        .iter()
        .filter(|spec| asked(spec) == Some(Asked::Tapback))
        .map(text)
        .collect();
    assert!(
        read_by_second
            .iter()
            .any(|turn| turn.contains("From the council · Seat ")
                && turn.contains("one key per route")),
        "{read_by_second:?}"
    );
    Ok(())
}

/// A way your request names for a task sits as a point of yours from the start, before any
/// seat answers, and the note keeper's list goes on after it.
#[tokio::test]
async fn a_way_your_request_names_is_your_point_from_the_start() -> anyhow::Result<()> {
    let (crew, _seats) = talking_crew(|_, asked, _| {
        let agree = || json!([stance(2, "agree", None), stance(3, "agree", None)]);
        match asked {
            Asked::First => turn("A token bucket per key, in Redis.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback => turn("", agree(), json!({})),
            Asked::Message => turn("Both hold.", agree(), json!({})),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let (exit, events) = drive_asking(
        &crew,
        "add per-key rate limiting to the public API, the limiter by Maker-checker",
        None,
    )
    .await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let council = council_events(&events);
    let lists: Vec<Vec<(u32, String, PointState)>> = council
        .iter()
        .filter_map(|kind| match kind {
            CouncilEventKind::Points { points } => Some(
                points
                    .iter()
                    .map(|point| (point.number, point.words.clone(), point.state))
                    .collect(),
            ),
            _ => None,
        })
        .collect();
    let yours = (
        1,
        "the limiter by Maker-checker".to_owned(),
        PointState::Yours,
    );
    assert_eq!(lists.first(), Some(&vec![yours.clone()]), "{council:?}");
    let first_answer = council.iter().position(
        |kind| matches!(kind, CouncilEventKind::Message(m) if m.kind == MessageKind::FirstAnswer),
    );
    let first_list = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Points { .. }));
    assert!(first_list < first_answer, "{council:?}");
    assert_eq!(
        lists.last(),
        Some(&vec![
            yours,
            (2, "A token bucket per key".to_owned(), PointState::Agreed),
            (3, "Counters live in Redis".to_owned(), PointState::Agreed),
        ]),
        "{lists:?}"
    );
    Ok(())
}

/// A seat your request names reaches the others by its letter, in the point your request
/// binds and in the goal, never by its model.
#[tokio::test]
async fn a_seat_your_request_names_reads_as_its_letter() -> anyhow::Result<()> {
    let (crew, seats) = talking_crew(|_, asked, _| {
        let agree = || json!([stance(2, "agree", None), stance(3, "agree", None)]);
        match asked {
            Asked::First => turn("A token bucket per key, in Redis.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback => turn("", agree(), json!({})),
            Asked::Message => turn("Both hold.", agree(), json!({})),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let (exit, events) = drive_asking(
        &crew,
        "add per-key rate limiting to the public API, the limiter Opus writes by Maker-checker",
        None,
    )
    .await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");
    let yours = council_events(&events)
        .into_iter()
        .find_map(|kind| match kind {
            CouncilEventKind::Points { points } => points.first().cloned(),
            _ => None,
        })
        .expect("your point");
    assert_eq!(yours.state, PointState::Yours);
    assert!(
        !yours.words.contains("Opus") && yours.words.contains("[seat-"),
        "{yours:?}"
    );
    let read: Vec<String> = seats[1]
        .specs()
        .iter()
        .filter(|spec| asked(spec).is_some())
        .map(text)
        .collect();
    assert!(!read.is_empty());
    assert!(
        read.iter().all(|prompt| !prompt.contains("Opus")),
        "{read:?}"
    );
    Ok(())
}

/// Long first answers are the talk's start, which the stop guard doesn't count: two seats of
/// 110k tokens each pass the 200k guard and the talk still runs to agreement.
#[tokio::test]
async fn the_guard_counts_the_talk_from_the_note_keepers_list() -> anyhow::Result<()> {
    let costly = argonauts_core::Spend {
        input_tokens: 110_000,
        ..argonauts_core::Spend::default()
    };
    let (crew, _seats) = talking_crew(move |worker, asked, spec| {
        let first = worker == SEATS[0].0;
        let agree = || json!([stance(1, "agree", None), stance(2, "agree", None)]);
        match asked {
            Asked::First => {
                turn("A token bucket per key.", json!([]), json!({})).with_spend(costly)
            }
            Asked::Points => points(),
            Asked::Tapback if first || text(spec).contains("one key per route") => {
                turn("", agree(), json!({}))
            }
            Asked::Tapback => turn(
                "",
                json!([stance(1, "agree", None), stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Message => turn(
                "Then one key per route keeps Redis cheap.",
                agree(),
                json!({ "reword": [{ "point": 2, "words": "Counters live in Redis, one key per route" }] }),
            ),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");
    let council = council_events(&events);
    assert!(
        !council
            .iter()
            .any(|kind| matches!(kind, CouncilEventKind::Stopped { .. })),
        "{council:?}"
    );
    assert!(
        council
            .iter()
            .any(|kind| matches!(kind, CouncilEventKind::Agreed { points: 2 }))
    );
    assert!(
        council
            .iter()
            .any(|kind| matches!(kind, CouncilEventKind::Worked { .. })),
        "the talk's moves after the list are counted"
    );
    Ok(())
}

/// A note keeper that lists no points gets one more turn, then the talk stops for you: it
/// never ends with every seat agreeing on nothing.
#[tokio::test]
async fn a_list_with_no_points_is_asked_again_then_stops() -> anyhow::Result<()> {
    let (crew, seats) = talking_crew(|_, asked, _| match asked {
        Asked::First => turn("A token bucket per key.", json!([]), json!({})),
        Asked::Points => turn("Nothing to list.", json!([]), json!({ "points": [] })),
        Asked::Plan => plan(),
        _ => turn("", json!([]), json!({})),
    })?;
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");
    let council = council_events(&events);
    assert!(
        council.contains(&CouncilEventKind::Stopped {
            why: TalkStop::NoPoints
        }),
        "{council:?}"
    );
    assert!(
        !council
            .iter()
            .any(|kind| matches!(kind, CouncilEventKind::Agreed { .. })),
        "{council:?}"
    );
    let lists: usize = seats
        .iter()
        .map(|fake| {
            fake.specs()
                .iter()
                .filter(|spec| asked(spec) == Some(Asked::Points))
                .count()
        })
        .sum();
    assert_eq!(lists, 2);
    Ok(())
}

/// The planning loop's last round has no send back, and its plan question says so.
#[tokio::test]
async fn the_last_plan_question_says_it_cant_go_back_again() -> anyhow::Result<()> {
    let (crew, _seats) = talking_crew(|_, asked, _| match asked {
        Asked::Points => points(),
        Asked::Tapback => turn(
            "",
            json!([stance(1, "agree", None), stance(2, "agree", None)]),
            json!({}),
        ),
        Asked::Plan => plan(),
        _ => turn("Agreed.", json!([]), json!({})),
    })?;
    let mut outcome = crew
        .start(RunMode::Crew, "add per-key rate limiting to the public API")
        .await?;
    let mut plans: Vec<argonauts_core::runs::events::approval::ApprovalOpened> = Vec::new();
    for _ in 0..12 {
        if !matches!(outcome.exit, EngineExit::Waiting(_)) {
            break;
        }
        let opened = outcome
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                    Some(opened.as_ref().clone())
                }
                _ => None,
            });
        let plan_open = opened
            .as_ref()
            .is_some_and(|opened| opened.kind == ApprovalKind::Plan);
        if let Some(opened) = opened.filter(|_| plan_open) {
            plans.push(opened);
        }
        let (command, _reply) = if plan_open && plans.len() < 3 {
            support::answer(
                &outcome.events,
                ApprovalKind::Plan,
                AnswerAction::SendBack,
                Some("Talk it over again."),
            )?
        } else {
            support::recommended(&outcome.events).ok_or_else(|| anyhow::anyhow!("nothing open"))?
        };
        outcome = crew.resume(outcome.events, vec![command]).await?;
    }
    assert_eq!(plans.len(), 3, "{plans:?}");
    let flags =
        |opened: &argonauts_core::runs::events::approval::ApprovalOpened| match &opened.packet {
            ApprovalPacket::Plan(packet) => packet.flags.clone(),
            _ => Vec::new(),
        };
    let last = |opened| {
        flags(opened)
            .iter()
            .any(|flag| flag.starts_with("This is the last round of planning"))
    };
    assert!(!last(&plans[0]) && !last(&plans[1]), "{plans:?}");
    assert!(last(&plans[2]), "{:?}", flags(&plans[2]));
    assert!(
        !plans[2]
            .options
            .iter()
            .any(|option| option.action == AnswerAction::SendBack)
    );
    Ok(())
}

/// The second seat sits as the skeptic. When every other seat backs a point that names no
/// evidence and the skeptic doesn't, code gives the skeptic one turn on it before anyone else,
/// and tells it why. Once it agrees, the point closes and the plan is written.
#[tokio::test]
async fn the_skeptic_gets_one_turn_on_a_point_about_to_close_unproven() -> anyhow::Result<()> {
    let (mut crew, seats) = talking_crew(|worker, asked, spec| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::First => turn("A token bucket per key, in Redis.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback if first => turn(
                "",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Tapback => turn(
                "",
                json!([stance(1, "object", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Message if text(spec).contains("You are the skeptic") => turn(
                "One noisy key could still starve the rest. Prove it holds.",
                json!([stance(
                    1,
                    "agree",
                    Some("one bucket per key keeps keys apart")
                )]),
                json!({}),
            ),
            Asked::Message => turn("A bucket per key.", json!([]), json!({})),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    crew.settings.council.seats = vec![
        CouncilSeat {
            model: SEATS[0].0.parse()?,
            role: SeatRole::Proposer,
        },
        CouncilSeat {
            model: SEATS[1].0.parse()?,
            role: SeatRole::Skeptic,
        },
    ];
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let council = council_events(&events);
    let floors: Vec<(SeatId, FloorReason)> = council
        .iter()
        .filter_map(|kind| match kind {
            CouncilEventKind::Writing { seat, why } => Some((seat.clone(), why.clone())),
            _ => None,
        })
        .filter(|(_, why)| !matches!(why, FloorReason::FirstAnswer | FloorReason::Notes))
        .collect();
    assert_eq!(
        floors,
        [(SeatId::numbered(2), FloorReason::Unproven { point: 1 })]
    );
    assert!(
        council
            .iter()
            .any(|kind| matches!(kind, CouncilEventKind::Agreed { points: 2 })),
        "{council:?}"
    );
    let challenge = seats[1]
        .specs()
        .iter()
        .filter(|spec| asked(spec) == Some(Asked::Message))
        .map(text)
        .next()
        .expect("the skeptic wrote");
    assert!(
        challenge.contains("Point 1 is about to close with no evidence named"),
        "{challenge}"
    );
    Ok(())
}

fn planned(n: u32) -> TaskSpec {
    TaskSpec {
        id: TaskId::numbered(n),
        title: format!("Task {n}"),
        task: "Add the limiter".to_owned(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(SEATS[0].0.parse().expect("a model")),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}

/// Two seats of one model each build the task the note keeper's plan gave them by letter, not
/// the first task of their model.
#[tokio::test]
async fn two_seats_of_one_model_build_the_tasks_the_plan_gives_them() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let started = crew.run_started(RunMode::Crew, "add per-key rate limiting")?;
    let opus: argonauts_core::base::names::ModelRef = SEATS[0].0.parse()?;
    let task = |n: u32, title: &str| TaskSpec {
        title: title.to_owned(),
        task: format!("Add the {}.", title.to_lowercase()),
        ..planned(n)
    };
    let tasks = [task(1, "Limiter"), task(2, "Logger")];
    let talk = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    };
    let (first, second) = (SeatId::numbered(1), SeatId::numbered(2));
    let mut kinds: Vec<RunEventKind> = tasks
        .iter()
        .map(|spec| RunEventKind::Task(TaskEvent::Created(Box::new(spec.clone()))))
        .collect();
    kinds.push(RunEventKind::Divide(
        argonauts_core::runs::events::plan::DivideEvent::Graph(
            argonauts_core::runs::events::plan::TaskGraph {
                tasks: tasks.to_vec(),
                critical_path: Vec::new(),
            },
        ),
    ));
    let council = |kind| {
        RunEventKind::Council(CouncilEvent {
            talk: talk.clone(),
            kind,
        })
    };
    kinds.push(council(CouncilEventKind::Seated(Seating {
        seats: [&first, &second]
            .into_iter()
            .map(|id| Seat {
                id: id.clone(),
                model: opus.clone(),
                role: SeatRole::Proposer,
                number: None,
            })
            .collect(),
        notes: Some(first.clone()),
    })));
    kinds.push(council(CouncilEventKind::Agreed { points: 1 }));
    let plan = CouncilMessage {
        id: MessageId::numbered(3),
        from: Speaker::Seat(first.clone()),
        kind: MessageKind::Notes,
        text: vec![
            TextPart::Words("The limiter goes to ".to_owned()),
            TextPart::Seat(second.clone()),
            TextPart::Words(". The logger goes to ".to_owned()),
            TextPart::Seat(first.clone()),
            TextPart::Words(".".to_owned()),
        ],
        reply_to: None,
        mentions: Vec::new(),
        stances: Vec::new(),
        hidden: 0,
        hidden_names: Vec::new(),
        guesses_hidden: 0,
        rewritten: false,
        declined: false,
        started_before: None,
    };
    kinds.push(council(CouncilEventKind::Message(Box::new(plan))));
    let mut history = vec![started.clone()];
    for (n, kind) in kinds.into_iter().enumerate() {
        let mut event = started.clone();
        event.seq = n as u64 + 1;
        event.kind = kind;
        history.push(event);
    }
    let (host, _sink) = crew.host_over(history)?;
    // The plan you approve names the same seats.
    let planned = talk::folder::planned(&host, &tasks);
    assert_eq!(
        planned,
        [
            argonauts_core::runs::events::approval::TaskSeat {
                task: TaskId::numbered(1),
                seat: second.clone(),
            },
            argonauts_core::runs::events::approval::TaskSeat {
                task: TaskId::numbered(2),
                seat: first.clone(),
            },
        ]
    );
    talk::folder::ensure(&host, &talk, &first).await?;
    talk::folder::ensure(&host, &talk, &second).await?;
    let handed = talk::folder::hand_to_builders(&host).await?;
    assert_eq!(
        handed,
        [(second, TaskId::numbered(1)), (first, TaskId::numbered(2)),]
    );
    Ok(())
}

/// The task a seat builds takes the seat's folder over: the same place, now on the task's own
/// branch, so the seat's session carries on there. A second task never takes the same folder,
/// and a folder a talk turn left changes in stays with its seat.
#[tokio::test]
async fn the_task_a_seat_builds_takes_over_the_seats_folder() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let started = crew.run_started(RunMode::Crew, "add per-key rate limiting")?;
    let mut history = vec![started.clone()];
    for n in 1..=3 {
        let mut created = started.clone();
        created.seq = u64::from(n);
        created.task_id = Some(TaskId::numbered(n));
        created.kind = RunEventKind::Task(TaskEvent::Created(Box::new(planned(n))));
        history.push(created);
    }
    let talk = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    };
    let (first, second) = (SeatId::numbered(1), SeatId::numbered(2));
    let mut seated = started.clone();
    seated.seq = 4;
    seated.kind = RunEventKind::Council(CouncilEvent {
        talk: talk.clone(),
        kind: CouncilEventKind::Seated(Seating {
            seats: [&first, &second]
                .into_iter()
                .zip(SEATS)
                .map(|(id, (model, _))| Seat {
                    id: id.clone(),
                    model: model.parse().expect("a model"),
                    role: SeatRole::Proposer,
                    number: None,
                })
                .collect(),
            notes: None,
        }),
    });
    history.push(seated);
    let (host, _sink) = crew.host_over(history)?;
    let folder = talk::folder::ensure(&host, &talk, &first).await?;
    assert!(folder.starts_with(&host.repo().worktrees), "{folder:?}");
    assert_eq!(talk::folder::ensure(&host, &talk, &first).await?, folder);
    let head = |dir: &std::path::Path| -> anyhow::Result<String> {
        let dir = dir.to_string_lossy();
        crew.repo.git(&["-C", &dir, "rev-parse", "HEAD"])
    };
    assert_eq!(
        head(&folder)?,
        crew.repo.head()?,
        "a seat starts at the run's base"
    );

    let task = TaskId::numbered(1);
    assert!(talk::folder::hand_to_task(&host, &talk, &first, &task).await?);
    let tree = worktree::ensure(&host, &task).await?;
    assert_eq!(tree.path, folder, "the task works where the seat talked");
    assert_eq!(
        talk::folder::seat_of(&host, &task).map(|(_, seat)| seat),
        Some(first.clone())
    );
    assert_eq!(tree.branch, format!("argonauts/{}/{task}", host.run_id()));
    let dir = folder.to_string_lossy();
    let branch = crew
        .repo
        .git(&["-C", &dir, "symbolic-ref", "--short", "HEAD"])?;
    assert_eq!(branch.trim(), tree.branch);
    assert!(
        !talk::folder::hand_to_task(&host, &talk, &first, &TaskId::numbered(2)).await?,
        "one folder, one task"
    );
    assert_eq!(talk::folder::seat_of(&host, &TaskId::numbered(2)), None);

    let other = talk::folder::ensure(&host, &talk, &second).await?;
    std::fs::write(other.join("notes.txt"), "a turn wrote this\n")?;
    assert!(!talk::folder::hand_to_task(&host, &talk, &second, &TaskId::numbered(3)).await?);
    assert!(
        other.join("notes.txt").exists(),
        "a changed folder stays as it was"
    );
    Ok(())
}

/// Seats that only say the same again stop after six messages that move nobody, and the point
/// left open comes to you as one question, whose side becomes the council's.
/// A task a script starts, which the plan's graph doesn't hold, takes the folder of a seat of
/// its worker's model once the talk has ended, and only one task takes each folder.
#[tokio::test]
async fn a_task_a_script_starts_takes_over_the_folder_of_a_seat_of_its_model() -> anyhow::Result<()>
{
    let crew = support::Crew::new()?;
    let started = crew.run_started(RunMode::Crew, "add per-key rate limiting")?;
    let talk = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    };
    let (first, second) = (SeatId::numbered(1), SeatId::numbered(2));
    let seats: Vec<Seat> = [&first, &second]
        .into_iter()
        .zip(SEATS)
        .map(|(id, (model, _))| Seat {
            id: id.clone(),
            model: model.parse().expect("a model"),
            role: SeatRole::Proposer,
            number: None,
        })
        .collect();
    let mut history = vec![started.clone()];
    let kinds = [
        CouncilEventKind::Seated(Seating {
            seats: seats.clone(),
            notes: None,
        }),
        CouncilEventKind::Agreed { points: 1 },
    ];
    for (seq, kind) in (1..).zip(kinds) {
        let mut event = started.clone();
        event.seq = seq;
        event.kind = RunEventKind::Council(CouncilEvent {
            talk: talk.clone(),
            kind,
        });
        history.push(event);
    }
    for n in 1..=3 {
        let mut created = started.clone();
        created.seq = 2 + u64::from(n);
        created.task_id = Some(TaskId::numbered(n));
        created.kind = RunEventKind::Task(TaskEvent::Created(Box::new(planned(n))));
        history.push(created);
    }
    let (host, _sink) = crew.host_over(history)?;
    for seat in [&first, &second] {
        talk::folder::ensure(&host, &talk, seat).await?;
    }

    let (one, two, three) = (
        TaskId::numbered(1),
        TaskId::numbered(2),
        TaskId::numbered(3),
    );
    assert!(talk::folder::hand_to_script_task(&host, &one, &seats[1].model).await?);
    assert_eq!(
        talk::folder::seat_of(&host, &one).map(|(_, seat)| seat),
        Some(second.clone())
    );
    assert!(
        !talk::folder::hand_to_script_task(&host, &two, &seats[1].model).await?,
        "one folder, one task"
    );
    assert!(talk::folder::hand_to_script_task(&host, &three, &seats[0].model).await?);
    assert_eq!(
        talk::folder::seat_of(&host, &three).map(|(_, seat)| seat),
        Some(first)
    );
    Ok(())
}

#[tokio::test]
async fn a_talk_that_moves_nobody_stops_and_you_settle_what_is_open() -> anyhow::Result<()> {
    let (crew, _seats) = talking_crew(|worker, asked, _| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::First => turn("My answer.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback | Asked::Message if first => turn(
                "Redis, still.",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Tapback | Asked::Message => turn(
                "In memory, still.",
                json!([stance(1, "agree", None), stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let (exit, events) = drive(&crew, Some("In memory")).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");
    let council = council_events(&events);
    let stopped = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Stopped { .. }))
        .expect("the talk stopped");
    let talk = council[..stopped]
        .iter()
        .filter(|kind| {
            matches!(kind, CouncilEventKind::Message(m)
                if m.kind == MessageKind::Talk && m.from != Speaker::You)
        })
        .count();
    assert_eq!(talk, 6, "six messages moved nobody");
    assert!(council.contains(&CouncilEventKind::Stopped {
        why: TalkStop::MovedNobody { messages: 6 }
    }));
    let settled = council
        .iter()
        .rev()
        .find_map(|kind| match kind {
            CouncilEventKind::Points { points } => Some(points.clone()),
            _ => None,
        })
        .expect("points");
    assert_eq!(settled[1].words, "In memory");
    assert_eq!(settled[1].state, PointState::Settled);
    assert!(
        !council[..stopped]
            .iter()
            .any(|kind| matches!(kind, CouncilEventKind::Agreed { .. })),
        "the talk never agreed"
    );
    let tapbacks: Vec<Stance> = council
        .iter()
        .filter_map(|kind| match kind {
            CouncilEventKind::Tapback { stance, .. } => Some(*stance),
            _ => None,
        })
        .collect();
    assert_eq!(
        tapbacks.len(),
        2,
        "only the note keeper's list changed a point: {tapbacks:?}"
    );
    let yours = council[..stopped]
        .iter()
        .filter(|kind| matches!(kind, CouncilEventKind::Message(m) if m.from == Speaker::You))
        .count();
    assert_eq!(yours, 0);
    Ok(())
}

/// Each side of a point left open says its reason under it, and a question you leave to the
/// council goes the side the most seats back, here the first, as the council's vote.
#[tokio::test]
async fn a_point_you_leave_to_the_council_goes_the_way_most_seats_took() -> anyhow::Result<()> {
    let (crew, _seats) = talking_crew(|worker, asked, _| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::First => turn("My answer.", json!([]), json!({})),
            Asked::Points => points_with_reasons(),
            Asked::Tapback | Asked::Message if first => turn(
                "Redis, still.",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Tapback | Asked::Message => turn(
                "In memory, still.",
                json!([stance(1, "agree", None), stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let request = "add per-key rate limiting to the public API";
    let (exit, events) = drive_answering(&crew, request, Some(AnswerAction::CarryOn)).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let question = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if matches!(opened.packet, ApprovalPacket::Council(_)) =>
            {
                Some(opened.clone())
            }
            _ => None,
        })
        .expect("the open point came to you");
    let reasons: Vec<Option<&str>> = question
        .options
        .iter()
        .map(|option| option.detail.as_deref())
        .collect();
    assert_eq!(
        reasons,
        [
            Some("Every node reads one count."),
            Some("There is no service to run."),
            None,
            None
        ]
    );
    let settled = council_events(&events)
        .iter()
        .rev()
        .find_map(|kind| match kind {
            CouncilEventKind::Points { points } => Some(points.clone()),
            _ => None,
        })
        .expect("points");
    assert_eq!(settled[1].words, "Redis");
    assert_eq!(settled[1].state, PointState::Voted);
    Ok(())
}

/// Your question on a point left open offers to let the seats talk on. Choosing it sets the
/// talk's limits again from what it spent, the talk goes on, and here the second seat then
/// comes round, so every seat agrees.
#[tokio::test]
async fn you_let_a_stopped_talk_go_on_and_the_seats_agree() -> anyhow::Result<()> {
    let messages = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&messages);
    let (crew, _seats) = talking_crew(move |worker, asked, _| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::First => turn("My answer.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback | Asked::Message if first => turn(
                "Redis, still.",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Message if counted.fetch_add(1, Ordering::SeqCst) >= 3 => turn(
                "Redis then: the nodes need one count.",
                json!([
                    stance(1, "agree", None),
                    stance(2, "agree", Some("three nodes share one count"))
                ]),
                json!({}),
            ),
            Asked::Tapback | Asked::Message => turn(
                "In memory, still.",
                json!([stance(1, "agree", None), stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Plan => plan(),
            Asked::Private => turn("", json!([]), json!({})),
        }
    })?;
    let (exit, events) = drive(&crew, Some("Talk on, up to 100k more tokens")).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let question = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if matches!(opened.packet, ApprovalPacket::Council(_)) =>
            {
                Some(opened.clone())
            }
            _ => None,
        })
        .expect("the open point came to you");
    assert_eq!(question.title, "Counters live in Redis: which way?");
    let labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Redis",
            "In memory",
            "Talk on, up to 100k more tokens",
            "Type my own answer"
        ]
    );

    let council = council_events(&events);
    let stopped = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Stopped { .. }))
        .expect("the talk stopped");
    let raised = council[stopped..]
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Limits { tokens, minutes: 8.. } if *tokens >= 100_000))
        .expect("talking on set the limits again")
        + stopped;
    let agreed = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Agreed { points: 2 }))
        .expect("the seats agreed once they talked on");
    assert!(agreed > raised);
    assert_eq!(
        messages.load(Ordering::SeqCst),
        4,
        "one more message after the stop"
    );
    Ok(())
}

/// How long a test waits for a lasting state of the run.
const DEADLINE: Duration = Duration::from_secs(60);

/// Polls `found` until it gives something or the deadline passes.
async fn eventually<T>(what: &str, mut found: impl FnMut() -> Option<T>) -> anyhow::Result<T> {
    let until = deadline::after(DEADLINE);
    loop {
        if let Some(value) = found() {
            return Ok(value);
        }
        if Instant::now() > until {
            anyhow::bail!("{what} never happened");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Seat one objects to the second point, so it takes the floor after the note keeper's list;
/// its first message waits until it's stopped, and `then` answers every later turn.
fn holding_crew(
    then: impl Fn(&str, Asked, &SessionSpec) -> FakeSession + Send + Sync + Clone + 'static,
) -> anyhow::Result<(support::Crew, Vec<Arc<FakeRunner>>)> {
    let holding = Arc::new(AtomicBool::new(true));
    talking_crew(move |worker, asked, spec| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::First => turn("My answer.", json!([]), json!({})),
            Asked::Points => points(),
            Asked::Tapback if first => turn(
                "",
                json!([stance(1, "agree", None), stance(2, "object", None)]),
                json!({}),
            ),
            Asked::Tapback => turn(
                "",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Message if first && holding.swap(false, Ordering::SeqCst) => {
                FakeSession::until_stopped(Vec::new())
            }
            _ => then(worker, asked, spec),
        }
    })
}

/// Starts the run, approves its goal, and goes on in a task of its own until seat one is
/// writing its held message.
async fn while_seat_one_writes(
    crew: &support::Crew,
    seats: &[Arc<FakeRunner>],
) -> anyhow::Result<support::Spawned> {
    let first = crew
        .start(RunMode::Crew, "add per-key rate limiting to the public API")
        .await?;
    let (approve, _reply) =
        support::recommended(&first.events).ok_or_else(|| anyhow::anyhow!("no goal question"))?;
    let run = crew.resume_spawned(first.events, vec![approve]).await?;
    eventually("seat one writing", || {
        seats[0]
            .specs()
            .iter()
            .any(|spec| asked(spec) == Some(Asked::Message))
            .then_some(())
    })
    .await?;
    Ok(run)
}

async fn tell(run: &support::Spawned, action: CouncilAction) -> anyhow::Result<Result<(), String>> {
    let (reply, replied) = oneshot::channel();
    run.commands
        .send(EngineCommand::Council { action, reply })
        .await
        .map_err(|_| anyhow::anyhow!("the run's inbox closed"))?;
    Ok(replied.await?)
}

fn write(text: &str) -> CouncilAction {
    CouncilAction::Write {
        text: text.to_owned(),
        reply_to: None,
        mentions: Vec::new(),
        points: Vec::new(),
    }
}

async fn stop(run: support::Spawned) -> anyhow::Result<()> {
    run.shutdown.cancel();
    run.exit.await?;
    Ok(())
}

/// Your message mid-talk shows at once, and the Claude Code seat that is writing reads it at its
/// next tool call, inside the same turn. The Codex seat, which isn't writing, reads it at its next
/// turn.
#[tokio::test]
async fn your_message_reaches_the_writing_claude_code_seat_inside_its_turn() -> anyhow::Result<()> {
    let (crew, seats) = holding_crew(|_, _, _| turn("Fine.", json!([]), json!({})))?;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    assert_eq!(tell(&run, write("Keep /health open.")).await?, Ok(()));

    let delivered = eventually("the message delivered mid-turn", || {
        seats[0]
            .controls()
            .into_iter()
            .find_map(|(_, control)| match control {
                SessionControl::Deliver(message) => Some(message.text),
                _ => None,
            })
    })
    .await?;
    assert_eq!(delivered, "From the council · the user: Keep /health open.");
    let council = council_events(&run.sink.events());
    let yours = council
        .iter()
        .find_map(|kind| match kind {
            CouncilEventKind::Message(message) if message.from == Speaker::You => {
                Some(message.id.clone())
            }
            _ => None,
        })
        .expect("your message came in");
    assert!(council.iter().any(|kind| matches!(
        kind,
        CouncilEventKind::Read { message, seat } if *message == yours && seat.as_str() == "seat-001"
    )));
    assert!(
        seats[1]
            .controls()
            .iter()
            .all(|(_, control)| !matches!(control, SessionControl::Deliver(_))),
        "the Codex seat isn't writing, so nothing reaches it mid-turn"
    );
    stop(run).await
}

/// With `council.when_you_write = "restart"` the writing seat stops and writes its message again
/// with yours, and code gives it the floor back first.
#[tokio::test]
async fn restart_stops_the_writing_seat_and_it_writes_again_with_yours() -> anyhow::Result<()> {
    let (mut crew, seats) = holding_crew(|_, asked, _| match asked {
        Asked::Message => turn("Then /health stays open.", json!([]), json!({})),
        _ => turn("", json!([]), json!({})),
    })?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    assert_eq!(tell(&run, write("Keep /health open.")).await?, Ok(()));

    let sink = run.sink.clone();
    let council = eventually("seat one writing again", || {
        let council = council_events(&sink.events());
        let ended = council.iter().position(
            |kind| matches!(kind, CouncilEventKind::WritingEnded { seat } if seat.as_str() == "seat-001"),
        )?;
        let again = council[ended..].iter().any(|kind| {
            matches!(kind, CouncilEventKind::Message(m)
                if m.from == Speaker::Seat(SeatId::numbered(1)))
        });
        again.then_some(council)
    })
    .await?;
    // Seat one had the floor for its objection, stopped for yours, and got it straight back.
    let ended = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::WritingEnded { .. }))
        .expect("seat one stopped");
    let floor = |kind: &CouncilEventKind| match kind {
        CouncilEventKind::Writing { seat, why } => Some((seat.clone(), why.clone())),
        _ => None,
    };
    assert_eq!(
        council[..ended].iter().rev().find_map(floor),
        Some((SeatId::numbered(1), FloorReason::Objected))
    );
    assert_eq!(
        council[ended..].iter().find_map(floor),
        Some((SeatId::numbered(1), FloorReason::You))
    );
    let rewritten = seats[0]
        .specs()
        .iter()
        .filter(|spec| asked(spec) == Some(Asked::Message))
        .nth(1)
        .map(text)
        .expect("a second message turn");
    assert!(rewritten.contains("Keep /health open."), "{rewritten}");
    stop(run).await
}

/// Your private message reads every seat it names by the reader's own letters, as your
/// messages to the council do.
#[tokio::test]
async fn your_private_message_names_seats_by_their_letters() -> anyhow::Result<()> {
    let (mut crew, seats) = holding_crew(|_, _, _| turn("Fine.", json!([]), json!({})))?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    let private = CouncilAction::Private {
        seat: SeatId::numbered(2),
        text: "Do you agree with Opus here?".to_owned(),
    };
    assert_eq!(tell(&run, private).await?, Ok(()));
    assert_eq!(tell(&run, write("Go on.")).await?, Ok(()));
    let read = eventually("seat two reading you in private", || {
        seats[1]
            .specs()
            .iter()
            .map(text)
            .find(|prompt| prompt.contains("From the user, privately:"))
    })
    .await?;
    assert!(!read.contains("Opus"), "{read}");
    assert!(read.contains("Do you agree with Seat "), "{read}");
    stop(run).await
}

/// Seat two's turn after you wrote it in private: an innocent message, with your private words
/// in `extra`, such as a stance's reason or a reworded point.
async fn private_words_in_a_turn(extra: Value) -> anyhow::Result<()> {
    const PRIVATE: &str = "Between us, the billing team wants the window kept until March.";
    let (mut crew, seats) = holding_crew(move |worker, asked, _| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::Private => turn("Understood.", json!([]), json!({})),
            Asked::Message if first => turn("Then the window stays for now.", json!([]), json!({})),
            Asked::Message => turn("I'd keep it.", json!([]), extra.clone()),
            _ => turn("", json!([]), json!({})),
        }
    })?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    let private = CouncilAction::Private {
        seat: SeatId::numbered(2),
        text: PRIVATE.to_owned(),
    };
    assert_eq!(tell(&run, private).await?, Ok(()));
    assert_eq!(tell(&run, write("Go on.")).await?, Ok(()));
    let exit = run.exit.await?;
    assert!(matches!(exit, EngineExit::Waiting(_)), "{exit:?}");
    let events = run.sink.events();
    let council = council_events(&events);
    let held = council
        .iter()
        .find_map(|kind| match kind {
            CouncilEventKind::Held(held) => Some(held.as_ref().clone()),
            _ => None,
        })
        .expect("the turn was held");
    assert_eq!(held.seat, SeatId::numbered(2));
    assert!(!held.turn.is_empty(), "{held:?}");
    // Your private message itself is recorded too, so only what a turn changes counts here.
    let recorded = |council: &[CouncilEventKind]| {
        council
            .iter()
            .filter(|kind| {
                matches!(
                    kind,
                    CouncilEventKind::Points { .. }
                        | CouncilEventKind::Stances { .. }
                        | CouncilEventKind::Moved(_)
                )
            })
            .any(|kind| format!("{kind:?}").contains("until March"))
    };
    assert!(
        !recorded(&council),
        "nothing of the turn counts while it waits"
    );
    let read_by_one = |fake: &FakeRunner| -> String {
        fake.specs()
            .iter()
            .filter(|spec| asked(spec).is_some())
            .map(text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(!read_by_one(&seats[0]).contains("until March"));

    let (drop, _reply) = support::answer(
        &events,
        ApprovalKind::Question,
        AnswerAction::Choose {
            value: "drop".to_owned(),
        },
        None,
    )?;
    let after = crew.resume(events, vec![drop]).await?;
    let council = council_events(&after.events);
    assert!(council.contains(&CouncilEventKind::Released {
        message: held.message.clone(),
        choice: HeldChoice::Drop,
    }));
    assert!(!recorded(&council), "a dropped turn never counts");
    assert!(!read_by_one(&seats[0]).contains("until March"));
    Ok(())
}

/// A stance's reason that repeats your private words holds the whole turn, however innocent
/// its message.
#[tokio::test]
async fn a_reason_that_repeats_your_private_words_holds_the_turn() -> anyhow::Result<()> {
    let because = "the billing team wants the window kept until March, as I was shown";
    private_words_in_a_turn(json!({ "stances": [stance(2, "object", Some(because))] })).await
}

/// So does a point the seat rewords with them.
#[tokio::test]
async fn a_point_reworded_with_your_private_words_holds_the_turn() -> anyhow::Result<()> {
    let words = "Counters stay in memory: the billing team wants the window kept until March";
    private_words_in_a_turn(json!({ "reword": [{ "point": 2, "words": words }] })).await
}

/// What you write to one seat stays in its session. When that seat's council message repeats
/// your private words, code holds it before any seat reads it and asks you; sharing sends your
/// private message to every seat, and the held one goes out with it.
#[tokio::test]
async fn a_message_that_repeats_your_private_words_waits_for_you() -> anyhow::Result<()> {
    const PRIVATE: &str = "Between us, the billing team wants the window kept until March.";
    let (mut crew, seats) = holding_crew(|worker, asked, _| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::Private => turn(
                "Understood, I'll keep that to myself.",
                json!([]),
                json!({}),
            ),
            Asked::Message if first => turn("Then the window stays for now.", json!([]), json!({})),
            Asked::Message => turn(
                "I hear the billing team wants the window kept until March, so keep it.",
                json!([]),
                json!({}),
            ),
            _ => turn("", json!([]), json!({})),
        }
    })?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    let private = CouncilAction::Private {
        seat: SeatId::numbered(2),
        text: PRIVATE.to_owned(),
    };
    assert_eq!(tell(&run, private).await?, Ok(()));
    assert_eq!(tell(&run, write("Go on.")).await?, Ok(()));
    let exit = run.exit.await?;
    assert!(matches!(exit, EngineExit::Waiting(_)), "{exit:?}");
    let events = run.sink.events();

    let council = council_events(&events);
    let held = council
        .iter()
        .find_map(|kind| match kind {
            CouncilEventKind::Held(held) => Some(held.as_ref().clone()),
            _ => None,
        })
        .expect("the message was held");
    assert_eq!(held.seat, SeatId::numbered(2));
    assert!(
        matches!(held.why, HeldWhy::WordsInARow { count } if count >= 6),
        "{held:?}"
    );
    let asked_you = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => match &opened.packet {
            ApprovalPacket::Council(packet) => Some(packet.about.clone()),
            _ => None,
        },
        _ => None,
    });
    // It carries when you wrote the private message, which your screen writes in its zone.
    let wrote_at = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Council(CouncilEvent {
            kind: CouncilEventKind::Private { message, .. },
            ..
        }) if *message == held.private => Some(event.ts),
        _ => None,
    });
    assert!(wrote_at.is_some());
    assert_eq!(
        asked_you,
        Some(CouncilQuestion::Held {
            message: held.message.clone(),
            why: Some(held.why.clone()),
            wrote_at,
        })
    );
    // The question names the seat as the talk does, and says what each choice does.
    let question = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if matches!(opened.packet, ApprovalPacket::Council(_)) =>
            {
                Some(opened.clone())
            }
            _ => None,
        })
        .expect("the held message came to you");
    assert!(
        question
            .title
            .ends_with("'s message repeats your private words"),
        "{}",
        question.title
    );
    let ApprovalPacket::Council(packet) = &question.packet else {
        unreachable!()
    };
    assert!(
        packet.text.ends_with(
            " words in a row match what you wrote to it in private. The talk goes on \
             without it."
        ),
        "{}",
        packet.text
    );
    let labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert!(labels[0].starts_with("Ask "), "{labels:?}");
    assert!(
        labels[0].ends_with(" to say it without your words"),
        "{labels:?}"
    );
    assert_eq!(
        labels[1..],
        [
            "Share your private message with the council",
            "Send it as written: the seats read these words, not your message",
            "Drop it: no seat ever reads it",
        ]
    );
    let read_by_one = |fake: &FakeRunner| -> String {
        fake.specs()
            .iter()
            .filter(|spec| asked(spec).is_some())
            .map(text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        !read_by_one(&seats[0]).contains("until March"),
        "your private words never reach another seat"
    );
    assert!(read_by_one(&seats[1]).contains(&format!("From the user, privately: {PRIVATE}")));

    let (share, _reply) = support::answer(
        &events,
        ApprovalKind::Question,
        AnswerAction::Choose {
            value: "share".to_owned(),
        },
        None,
    )?;
    let after = crew.resume(events, vec![share]).await?;
    let council = council_events(&after.events);
    assert!(council.contains(&CouncilEventKind::Shared {
        message: held.private.clone()
    }));
    assert!(council.contains(&CouncilEventKind::Released {
        message: held.message.clone(),
        choice: HeldChoice::Share,
    }));
    let one = read_by_one(&seats[0]);
    assert!(one.contains(PRIVATE), "the shared message reaches seat one");
    assert!(one.contains("so keep it."), "and so does the held one");
    Ok(())
}

/// A private message to the seat that is writing stops it, as your messages to the council do
/// where messages restart the writer: it reads yours, then writes again.
#[tokio::test]
async fn a_private_message_to_the_writing_seat_has_it_write_again() -> anyhow::Result<()> {
    const PRIVATE: &str = "Keep the window until March.";
    let (mut crew, seats) = holding_crew(|_, asked, _| match asked {
        Asked::Message => FakeSession::until_stopped(Vec::new()),
        _ => turn("", json!([]), json!({})),
    })?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    let private = CouncilAction::Private {
        seat: SeatId::numbered(1),
        text: PRIVATE.to_owned(),
    };
    assert_eq!(tell(&run, private).await?, Ok(()));
    eventually("seat one reading your words and writing again", || {
        let specs = seats[0].specs();
        let read = specs
            .iter()
            .any(|spec| asked(spec) == Some(Asked::Private) && text(spec).contains(PRIVATE));
        let messages = specs
            .iter()
            .filter(|spec| asked(spec) == Some(Asked::Message))
            .count();
        (read && messages >= 2).then_some(())
    })
    .await?;
    let failed = run.sink.events().iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Run(argonauts_core::runs::events::run::RunLifecycle::Finished { .. })
        )
    });
    assert!(!failed, "the run goes on");
    stop(run).await
}

/// The note keeper's plan that repeats what you told it in private waits for you before it is
/// sealed, since every task reads the plan: asked to write it again, the keeper is told only
/// that it repeated your words, and the plan sealed is the one without them.
#[tokio::test]
async fn a_plan_that_repeats_your_private_words_waits_for_you() -> anyhow::Result<()> {
    const PRIVATE: &str = "Between us, the billing team wants the window kept until March.";
    let (mut crew, seats) = holding_crew(|worker, asked, spec| {
        let first = worker == SEATS[0].0;
        match asked {
            Asked::Private => turn(
                "Understood, I'll keep that to myself.",
                json!([]),
                json!({}),
            ),
            Asked::Message if first => turn(
                "Then a bucket.",
                json!([stance(2, "agree", Some("three nodes share one count"))]),
                json!({}),
            ),
            Asked::Message => turn(
                "Agreed.",
                json!([stance(1, "agree", None), stance(2, "agree", None)]),
                json!({}),
            ),
            Asked::Plan if !text(spec).contains("Your plan repeated") => turn(
                "One task.",
                json!([]),
                json!({ "plan": {
                    "summary": "The billing team wants the window kept until March, so a bucket.",
                    "approach": "Add the limiter as middleware.",
                    "pieces": [],
                    "choices": [],
                    "risks": [],
                    "test_strategy": "Unit tests for the bucket.",
                    "new_dependencies": [],
                } }),
            ),
            Asked::Plan => plan(),
            _ => turn("", json!([]), json!({})),
        }
    })?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    let private = CouncilAction::Private {
        seat: SeatId::numbered(1),
        text: PRIVATE.to_owned(),
    };
    // The keeper is writing, so your private message stops it and it writes again with yours.
    assert_eq!(tell(&run, private).await?, Ok(()));
    let exit = run.exit.await?;
    assert!(matches!(exit, EngineExit::Waiting(_)), "{exit:?}");
    let events = run.sink.events();
    let sealed = |events: &[RunEvent]| -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Plan(PlanEvent::Sealed { summary, .. }) => Some(summary.clone()),
                _ => None,
            })
            .collect()
    };
    assert!(
        sealed(&events).is_empty(),
        "nothing sealed while you decide"
    );
    let question = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if matches!(opened.packet, ApprovalPacket::Council(_)) =>
            {
                Some(opened.clone())
            }
            _ => None,
        })
        .expect("the plan came to you");
    let ApprovalPacket::Council(packet) = &question.packet else {
        unreachable!()
    };
    assert!(
        matches!(&packet.about, CouncilQuestion::HeldPlan { keeper, wrote_at: Some(_), .. }
            if *keeper == SeatId::numbered(1)),
        "{:?}",
        packet.about
    );
    assert!(
        question
            .title
            .ends_with("'s plan repeats your private words"),
        "{}",
        question.title
    );
    let labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert!(
        labels[0].ends_with(" to write the plan again without your words"),
        "{labels:?}"
    );
    assert_eq!(
        labels[1..],
        [
            "Share your private message with the council",
            "Seal it as written: every task reads these words, not your message",
        ]
    );
    let (rewrite, _reply) = support::answer(
        &events,
        ApprovalKind::Question,
        AnswerAction::Choose {
            value: "rewrite".to_owned(),
        },
        None,
    )?;
    let after = crew.resume(events, vec![rewrite]).await?;
    assert_eq!(
        sealed(&after.events),
        ["A token bucket per key, its counters in Redis."],
        "the plan sealed is the one written again"
    );
    let asked_again = seats[0]
        .specs()
        .iter()
        .any(|spec| asked(spec) == Some(Asked::Plan) && text(spec).contains("Your plan repeated"));
    assert!(asked_again, "the keeper writes the plan again");
    Ok(())
}

/// A seat that carries on in its own CLI sits out: the floor passes it by, and when it comes back
/// it reads what it missed.
#[tokio::test]
async fn a_seat_that_carries_on_alone_sits_out_and_rejoins() -> anyhow::Result<()> {
    let messages = Arc::new(AtomicUsize::new(0));
    let (mut crew, seats) = holding_crew(move |_, asked, _| match asked {
        // Seat one's message after yours lands, and the next waits, so the talk is still
        // going when seat two comes back.
        Asked::Message if messages.fetch_add(1, Ordering::SeqCst) == 0 => {
            turn("Still a window.", json!([]), json!({}))
        }
        Asked::Message => FakeSession::until_stopped(Vec::new()),
        _ => turn("", json!([]), json!({})),
    })?;
    crew.settings.council.when_you_write = WhenYouWrite::Restart;
    let _env = crew.repo.env_guard();
    let run = while_seat_one_writes(&crew, &seats).await?;
    let carry_on = CouncilAction::CarryOn {
        seat: SeatId::numbered(2),
    };
    assert_eq!(tell(&run, carry_on).await?, Ok(()));
    assert_eq!(tell(&run, write("Go on without it.")).await?, Ok(()));
    eventually("seat one writing its third message", || {
        let turns = seats[0]
            .specs()
            .iter()
            .filter(|spec| asked(spec) == Some(Asked::Message))
            .count();
        (turns >= 3).then_some(())
    })
    .await?;
    // What you typed to it in its own CLI comes back as your private messages, read there.
    let rejoin = CouncilAction::Rejoin {
        seat: SeatId::numbered(2),
        said: vec!["Keep the window until March.".to_owned()],
    };
    assert_eq!(tell(&run, rejoin).await?, Ok(()));
    let council = council_events(&run.sink.events());
    let private = council
        .iter()
        .find_map(|kind| match kind {
            CouncilEventKind::Private { seat, message } if *seat == SeatId::numbered(2) => {
                Some(message.clone())
            }
            _ => None,
        })
        .expect("what you typed there is a private message");
    assert!(council.contains(&CouncilEventKind::Read {
        message: private,
        seat: SeatId::numbered(2),
    }));
    let sat_out = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::SatOut { .. }))
        .expect("seat two sat out");
    let back = council
        .iter()
        .position(|kind| matches!(kind, CouncilEventKind::Rejoined { .. }))
        .expect("seat two came back");
    assert_eq!(
        council[back],
        CouncilEventKind::Rejoined {
            seat: SeatId::numbered(2),
            missed: 2,
        },
        "it missed your message and seat one's"
    );
    assert!(
        !council[sat_out..back].iter().any(|kind| matches!(
            kind,
            CouncilEventKind::Writing { seat, .. } if *seat == SeatId::numbered(2)
        )),
        "the floor passes a seat that sits out"
    );
    assert_eq!(
        tell(&run, write("")).await?,
        Err("Write something for the council first.".to_owned())
    );
    stop(run).await
}

/// A plan you send back reopens the council with your note as your message, and the note keeper
/// writes the plan again with it.
#[tokio::test]
async fn a_plan_sent_back_reopens_the_talk_with_your_note() -> anyhow::Result<()> {
    const NOTE: &str = "Keep /health out of the limit.";
    let (crew, seats) = talking_crew(|_, asked, _| match asked {
        Asked::Points => points(),
        Asked::Tapback => turn(
            "",
            json!([stance(1, "agree", None), stance(2, "agree", None)]),
            json!({}),
        ),
        Asked::Plan => plan(),
        _ => turn("Agreed.", json!([]), json!({})),
    })?;
    let mut outcome = crew
        .start(RunMode::Crew, "add per-key rate limiting to the public API")
        .await?;
    let mut sent_back = false;
    for _ in 0..10 {
        if !matches!(outcome.exit, EngineExit::Waiting(_)) {
            break;
        }
        let plan_open = outcome
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(opened.kind),
                _ => None,
            })
            == Some(ApprovalKind::Plan);
        let (command, _reply) = if plan_open && !sent_back {
            sent_back = true;
            support::answer(
                &outcome.events,
                ApprovalKind::Plan,
                AnswerAction::SendBack,
                Some(NOTE),
            )?
        } else {
            support::recommended(&outcome.events).ok_or_else(|| anyhow::anyhow!("nothing open"))?
        };
        outcome = crew.resume(outcome.events, vec![command]).await?;
    }
    assert!(sent_back, "the plan approval never opened");
    let council = council_events(&outcome.events);
    let note = council
        .iter()
        .position(|kind| {
            matches!(kind, CouncilEventKind::Message(m)
                if m.from == Speaker::You && m.words(|_| String::new()) == NOTE)
        })
        .expect("your note joined the talk");
    let plans: Vec<usize> = council
        .iter()
        .enumerate()
        .filter(|(_, kind)| {
            matches!(kind, CouncilEventKind::Message(m)
                if m.kind == MessageKind::Notes && m.text.iter().any(|part| matches!(part,
                    argonauts_core::runs::events::council::TextPart::Words(words) if words.contains("One task"))))
        })
        .map(|(at, _)| at)
        .collect();
    assert!(plans.len() >= 2, "the note keeper wrote the plan again");
    assert!(plans[0] < note && note < plans[1]);
    let keeper_read_it = seats.iter().any(|seat| {
        seat.specs()
            .iter()
            .filter(|spec| asked(spec) == Some(Asked::Plan))
            .nth(1)
            .is_some_and(|spec| text(spec).contains(NOTE))
    });
    assert!(keeper_read_it, "the second plan was written with your note");
    let plans_sealed = outcome
        .events
        .iter()
        .filter(|event| matches!(event.kind, RunEventKind::Plan(PlanEvent::FastPath { .. })))
        .count();
    assert_eq!(plans_sealed, plans.len());
    // A plan reopened the talk, but nobody reacts to the plan itself.
    let tapbacks_on_plans = council
        .iter()
        .filter(|kind| matches!(kind, CouncilEventKind::Tapback { .. }))
        .count();
    assert_eq!(
        tapbacks_on_plans, 2,
        "only the note keeper's list was reacted to"
    );
    Ok(())
}
