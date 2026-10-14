//! The blackboard: delivery at delivery points, the change-request rules, settled questions,
//! blind sessions, and the consultant.

mod support;

use std::sync::Arc;

use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use argonauts_core::events::run::Party;
use argonauts_core::events::session::{Delivery, InputPartKind, SessionEvent, SessionStarted};
use argonauts_core::events::shared::{
    BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
};
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskSpec, TaskState};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::MentionTarget;
use argonauts_core::names::{AgentId, MessageId, ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerKind, SessionControl};
use argonauts_core::settings::Settings;
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::task::{ChangeRequestOut, ReportStatus, TaskReport};
use argonauts_core::{RunId, TaskId};
use argonauts_crew::blackboard::rules::{self, Decider, Tally};
use argonauts_crew::blackboard::{self, Post, commands, delivery, records};
use argonauts_crew::consultant;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::live::LiveSession;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::state::RunState;
use argonauts_crew::state::tasks::TaskEntry;
use argonauts_testkit::runner::FakeSession;
use argonauts_testkit::services::MemorySink;

fn task(id: &str) -> TaskId {
    TaskId::new(id).unwrap()
}

fn worker(name: &str) -> ModelRef {
    ModelRef::new(name, None)
}

fn spec(id: &str, who: &str, owned: &[&str], reads: &[&str], depends_on: &[&str]) -> TaskSpec {
    let strings = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
    TaskSpec {
        id: task(id),
        title: format!("Build {id}"),
        task: String::new(),
        kind: TaskKind::Build,
        owned_paths: strings(owned),
        read_only_paths: strings(reads),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: depends_on.iter().map(|id| task(id)).collect(),
        criteria: Vec::new(),
        worker: Some(worker(who)),
        reviewers: Vec::new(),
        limit: Default::default(),
        predictions: None,
        item: None,
    }
}

fn record(spec: TaskSpec, state: TaskState) -> TaskEntry {
    TaskEntry {
        worker: spec.worker.clone(),
        spec,
        state,
        tries: 1,
        reassigned: 0,
        worktree: None,
        branch: None,
        base: None,
        commits: Vec::new(),
        reports: Vec::new(),
        checks: Vec::new(),
        reviews: Vec::new(),
        fallbacks: 0,
        debt: None,
    }
}

/// Five tasks: task-001 asks; task-002 owns the limiter, whose interface task-003 reads;
/// task-004 builds on task-002; task-005 is already merged.
fn crew_state() -> RunState {
    let mut state = RunState::default();
    let tasks = [
        (
            spec("task-001", "codex", &["src/api/**"], &[], &[]),
            TaskState::Running,
        ),
        (
            spec("task-002", "claude-code", &["src/limiter/**"], &[], &[]),
            TaskState::Running,
        ),
        (
            spec(
                "task-003",
                "agy",
                &["src/store/**"],
                &["src/limiter/mod.rs"],
                &[],
            ),
            TaskState::Running,
        ),
        (
            spec("task-004", "codex", &["docs/**"], &[], &["task-002"]),
            TaskState::Waiting,
        ),
        (
            spec("task-005", "claude-code", &["src/old/**"], &[], &[]),
            TaskState::Merged,
        ),
    ];
    for (spec, state_now) in tasks {
        state
            .tasks
            .by_id
            .insert(spec.id.clone(), record(spec, state_now));
    }
    state
}

fn request(paths: &[&str]) -> ChangeRequestOut {
    ChangeRequestOut {
        text: "Change the limiter's key type".to_owned(),
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
    }
}

#[test]
fn change_request_decision_rules() {
    let state = crew_state();
    let mut settings = Settings::default();
    settings.limits.protected_paths = vec!["infra/**".to_owned()];
    let asker = task("task-001");
    let decide =
        |paths: &[&str]| rules::decider_for(&state, &settings, Some(&asker), &request(paths));

    // Local and reversible: only the worker of that piece.
    assert_eq!(
        decide(&["src/limiter/redis.rs"]),
        Decider::Worker(worker("claude-code"))
    );
    // Cross-cutting: a frozen interface another task reads.
    assert_eq!(
        decide(&["src/limiter/mod.rs"]),
        Decider::Workers(vec![worker("claude-code"), worker("agy")])
    );
    // Irreversible, or nobody's piece: you.
    assert_eq!(decide(&["db/migrations/0004_keys.sql"]), Decider::User);
    // The approval names the outside actions, each once, for your allowlist.
    let paths =
        |paths: &[&str]| -> Vec<String> { paths.iter().map(|path| (*path).to_owned()).collect() };
    assert_eq!(
        rules::irreversible_actions(&paths(&[
            "db/migrations/0004_keys.sql",
            "db/migrations/0005_ttl.sql",
            ".github/workflows/release.yml",
            "src/limiter/redis.rs",
        ])),
        ["migrate", "release"]
    );
    assert!(rules::irreversible_actions(&paths(&["src/limiter/redis.rs"])).is_empty());
    assert_eq!(decide(&["Cargo.toml"]), Decider::User);
    // A merged task's worker has no next report to vote in.
    assert_eq!(decide(&["src/old/limits.rs"]), Decider::User);
    // The goal file: `approvals.goal_changes` decides.
    assert_eq!(decide(&[".argonauts/goal/run-01.md"]), Decider::Goal);
    // Into a protected path: back to the plan approval.
    assert_eq!(decide(&["infra/terraform/main.tf"]), Decider::Replan);
    assert_eq!(decide(&[".argonauts/config.toml"]), Decider::Replan);

    // Dependents: workers and readers of the paths, and tasks built on them.
    assert_eq!(
        rules::dependents(&state, &request(&["src/limiter/mod.rs"])),
        [task("task-002"), task("task-003"), task("task-004")]
    );

    // Majority wins; the tie-breaker breaks a tie only when it isn't a party.
    let (a, b, c) = (worker("claude-code"), worker("agy"), worker("codex"));
    let votes = |ballot: &[(&ModelRef, bool)]| -> Vec<(ModelRef, bool)> {
        ballot
            .iter()
            .map(|(who, yes)| ((*who).clone(), *yes))
            .collect()
    };
    assert_eq!(
        rules::tally(&votes(&[(&a, true), (&b, true), (&c, false)]), None),
        Tally::Accepted
    );
    assert_eq!(
        rules::tally(&votes(&[(&a, false), (&b, false), (&c, true)]), Some(&a)),
        Tally::Rejected
    );
    let tie = votes(&[(&a, true), (&b, false)]);
    assert_eq!(
        rules::tally(&tie, Some(&c)),
        Tally::TieBreakerDecides(c.clone())
    );
    assert_eq!(rules::tally(&tie, Some(&a)), Tally::YouDecide);
    assert_eq!(rules::tally(&tie, None), Tally::YouDecide);
}

fn posted(n: u32, kind: BlackboardKind, text: &str, refs: &[&str]) -> BlackboardMessage {
    BlackboardMessage {
        id: MessageId::numbered(n),
        kind,
        from: Party::Agent {
            session: SessionKey::numbered(1),
            worker: worker("codex"),
        },
        to: Recipient::Task {
            task: task("task-002"),
        },
        text: text.to_owned(),
        in_reply_to: None,
        refs: refs.iter().map(|item| (*item).to_owned()).collect(),
    }
}

fn envelope(kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq: 0,
        ts: OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01").unwrap(),
        task_id: Some(task("task-001")),
        session: None,
        source: "argonauts-code".to_owned(),
        kind,
    }
}

#[test]
fn settled_question_reopens_only_with_new_evidence() {
    let mut state = crew_state();
    let asked = "Should Retry-After be in seconds or an HTTP date?";
    let mut post = |message: BlackboardMessage| {
        state.apply(&envelope(RunEventKind::Blackboard(
            BlackboardEvent::Posted(message),
        )));
    };
    post(posted(1, BlackboardKind::Question, asked, &[]));
    let mut answer = posted(2, BlackboardKind::Answer, "Seconds, as an integer.", &[]);
    answer.in_reply_to = Some(MessageId::numbered(1));
    post(answer);
    let again = posted(
        3,
        BlackboardKind::Question,
        "should Retry-After be in seconds, or an HTTP date",
        &[],
    );
    let citing_record = posted(4, BlackboardKind::Question, asked, &["m-002"]);
    let with_evidence = posted(
        5,
        BlackboardKind::Question,
        "Should Retry-After be in seconds or an HTTP date? retry_after::date fails",
        &["retry_after::date"],
    );
    let other = posted(
        6,
        BlackboardKind::Question,
        "Which Redis client should we use?",
        &[],
    );
    for message in [&again, &citing_record, &with_evidence, &other] {
        post(message.clone());
    }

    let record = |message: &BlackboardMessage| records::settled_by(&state, message);
    let settled = state.blackboard.get(&MessageId::numbered(2)).unwrap();
    // Asked again with nothing new: answered with the record's id.
    assert_eq!(record(&again), Some(MessageId::numbered(2)));
    assert!(!records::reopens(settled, &again));
    // Citing the record itself isn't new evidence.
    assert_eq!(record(&citing_record), Some(MessageId::numbered(2)));
    assert!(!records::reopens(settled, &citing_record));
    // A failing test the record didn't have reopens it.
    assert_eq!(record(&with_evidence), Some(MessageId::numbered(2)));
    assert!(records::reopens(settled, &with_evidence));
    // Another question isn't settled by it.
    assert_eq!(record(&other), None);
}

/// A host over the fakes, with the sink it records to.
fn host(crew: &support::Crew) -> anyhow::Result<(Host, Arc<MemorySink>)> {
    let (context, sink, _commands) = crew.context(Vec::new())?;
    let host = Host::new(Arc::new(Shared::new(&context, RunState::default())?));
    Ok((host, sink))
}

/// Records a task's creation and moves it to `state`.
fn add_task(host: &Host, spec: TaskSpec, state: TaskState) -> anyhow::Result<()> {
    let id = spec.id.clone();
    host.record(
        Some(&id),
        None,
        RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
    )?;
    host.record(
        Some(&id),
        None,
        RunEventKind::Task(TaskEvent::State {
            from: None,
            to: state,
            reason: String::new(),
        }),
    )?;
    Ok(())
}

/// Records a running session and registers its control channel, which the test reads.
fn run_session(
    host: &Host,
    n: u32,
    role: RoleName,
    task: Option<&str>,
    by: &str,
    blind: bool,
) -> anyhow::Result<(SessionKey, mpsc::Receiver<SessionControl>)> {
    let key = SessionKey::numbered(n);
    let task = task.map(TaskId::new).transpose()?;
    host.record(
        task.as_ref(),
        Some(&key),
        RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
            worker: worker(by),
            agent_id: AgentId(by.to_owned()),
            provider: by.to_owned(),
            kind: RunnerKind::Cli,
            role,
            step: None,
            access: Access::Build,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: u8::try_from(n).ok(),
            workflow_step: None,
            label: None,
            asked: None,
        }))),
    )?;
    let (control, controls) = mpsc::channel(8);
    host.shared().live.insert(
        key.clone(),
        LiveSession {
            role,
            task,
            blind,
            control,
            cancel: CancellationToken::new(),
        },
    );
    Ok((key, controls))
}

fn delivered(events: &[RunEvent], message: &MessageId) -> Vec<SessionKey> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::Delivered { message: id, to })
                if id == message =>
            {
                Some(to.clone())
            }
            _ => None,
        })
        .collect()
}

fn delivered_now(
    controls: &mut mpsc::Receiver<SessionControl>,
) -> Vec<(MessageId, String, Delivery)> {
    let mut got = Vec::new();
    while let Ok(control) = controls.try_recv() {
        if let SessionControl::Deliver(queued) = control {
            got.push((queued.id, queued.text, queued.delivery));
        }
    }
    got
}

#[tokio::test]
async fn blackboard_delivered_at_next_turn() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, sink) = host(&crew)?;
    add_task(
        &host,
        spec("task-002", "codex", &["src/limiter/**"], &[], &[]),
        TaskState::Running,
    )?;
    add_task(
        &host,
        spec("task-003", "agy", &["src/store/**"], &[], &[]),
        TaskState::Ready,
    )?;
    let (worker_session, mut controls) =
        run_session(&host, 1, RoleName::Worker, Some("task-002"), "codex", false)?;

    // Guidance reaches the running session at its next turn, once.
    let guidance = commands::guidance(&host, "Retry-After is in seconds.".to_owned())
        .await
        .map_err(anyhow::Error::msg)?;
    let got = delivered_now(&mut controls);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, guidance);
    assert_eq!(got[0].2, Delivery::NextTurn);
    assert!(got[0].1.contains("Retry-After is in seconds."));
    delivery::queue(&host, &guidance, &worker_session)?;
    assert!(delivered_now(&mut controls).is_empty(), "never twice");
    assert_eq!(
        delivered(&sink.events(), &guidance),
        std::slice::from_ref(&worker_session)
    );
    assert!(sink.events().iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Session(SessionEvent::Queued(queued)) if queued.id == guidance
    )));

    // A question for a task with no running session waits for its next session start.
    let question = blackboard::post(
        &host,
        Post {
            kind: BlackboardKind::Question,
            from: Party::Agent {
                session: worker_session.clone(),
                worker: worker("codex"),
            },
            to: Recipient::Task {
                task: task("task-003"),
            },
            text: "Does the store expire keys itself?".to_owned(),
            in_reply_to: None,
            refs: Vec::new(),
            from_task: Some(task("task-002")),
        },
    )?;
    assert!(delivery::to_running(&host, &question)?.is_empty());
    assert!(delivered(&sink.events(), &question).is_empty());
    let (store, _store_controls) =
        run_session(&host, 2, RoleName::Worker, Some("task-003"), "agy", false)?;
    let parts = delivery::at_session_start(&host, &store)?;
    let texts: Vec<&str> = parts.iter().map(|part| part.text.as_str()).collect();
    assert_eq!(parts.len(), 2, "the guidance and the question: {texts:?}");
    assert!(parts.iter().all(|part| part.kind == InputPartKind::Message));
    assert!(texts[1].contains("Does the store expire keys itself?"));
    assert!(texts[1].contains(&format!("answer {question}:")));
    assert!(delivery::at_session_start(&host, &store)?.is_empty());
    assert_eq!(delivered(&sink.events(), &question), [store]);
    Ok(())
}

#[tokio::test]
async fn mention_to_blind_session_is_text_only() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, _sink) = host(&crew)?;
    add_task(
        &host,
        spec("task-002", "codex", &["src/limiter/**"], &[], &[]),
        TaskState::Running,
    )?;
    let (_draft_session, mut drafting_agent) =
        run_session(&host, 1, RoleName::Draft, None, "agy", true)?;
    let (worker_session, mut owned) =
        run_session(&host, 2, RoleName::Worker, Some("task-002"), "codex", false)?;

    let text = "Keep the counters in Redis.";
    commands::mention(&host, MentionTarget::All, text.to_owned())
        .await
        .map_err(anyhow::Error::msg)?;
    let blind = delivered_now(&mut drafting_agent);
    assert_eq!(blind.len(), 1);
    assert_eq!(blind[0].1, text, "only your text, nothing attached");
    let seeing = delivered_now(&mut owned);
    assert!(seeing[0].1.starts_with("Message m-"));
    assert!(seeing[0].1.ends_with(text));

    // An agent's message never reaches a blind session.
    let question = blackboard::post(
        &host,
        Post {
            kind: BlackboardKind::Question,
            from: Party::Agent {
                session: worker_session,
                worker: worker("codex"),
            },
            to: Recipient::All,
            text: "Which plan uses Redis?".to_owned(),
            in_reply_to: None,
            refs: Vec::new(),
            from_task: Some(task("task-002")),
        },
    )?;
    delivery::to_running(&host, &question)?;
    assert!(delivered_now(&mut drafting_agent).is_empty());
    Ok(())
}

fn report(concerns: &[&str], change_requests: Vec<ChangeRequestOut>) -> TaskReport {
    TaskReport {
        status: ReportStatus::Done,
        summary: "Done.".to_owned(),
        files_changed: Vec::new(),
        tests_run: Vec::new(),
        concerns: concerns.iter().map(|line| (*line).to_owned()).collect(),
        questions: Vec::new(),
        change_requests,
        memory_proposals: Vec::new(),
        answer: String::new(),
    }
}

fn task_state(host: &Host, id: &str) -> Option<TaskState> {
    host.read(|state| state.tasks.get(&task(id)).map(|record| record.state))
}

/// A message counts as delivered only once the session's control queue holds a place for it.
/// A session whose queue is full, or that ended, gets nothing recorded, and the message waits
/// for the task's next session.
#[tokio::test]
async fn hand_over_records_nothing_a_session_cannot_take() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, sink) = host(&crew)?;
    add_task(
        &host,
        spec("task-002", "codex", &["src/limiter/**"], &[], &[]),
        TaskState::Running,
    )?;
    let (worker_session, controls) =
        run_session(&host, 1, RoleName::Worker, Some("task-002"), "codex", false)?;
    let queued = |events: &[RunEvent], message: &MessageId| {
        events.iter().any(|event| {
            matches!(&event.kind, RunEventKind::Session(SessionEvent::Queued(queued)) if &queued.id == message)
        })
    };

    // Its queue is full.
    let held = (0..8)
        .map(|_| host.reserve_control(&worker_session))
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::msg)?;
    let first = commands::guidance(&host, "Keys expire after an hour.".to_owned())
        .await
        .map_err(anyhow::Error::msg)?;
    let refused = delivery::queue(&host, &first, &worker_session);
    assert!(
        refused
            .as_ref()
            .is_err_and(|err| err.to_string().contains("too many controls")),
        "{refused:?}"
    );
    drop(held);

    // It ended.
    drop(controls);
    let second = commands::guidance(&host, "Count in seconds.".to_owned())
        .await
        .map_err(anyhow::Error::msg)?;
    let refused = delivery::queue(&host, &second, &worker_session);
    assert!(
        refused
            .as_ref()
            .is_err_and(|err| err.to_string().contains("has ended")),
        "{refused:?}"
    );
    let events = sink.events();
    for message in [&first, &second] {
        assert!(delivered(&events, message).is_empty(), "{message}");
        assert!(!queued(&events, message), "{message}");
    }

    // Both reach the task's next session when it starts.
    let (next, _next_controls) =
        run_session(&host, 2, RoleName::Worker, Some("task-002"), "codex", false)?;
    let parts = delivery::at_session_start(&host, &next)?;
    assert_eq!(parts.len(), 2);
    let events = sink.events();
    assert_eq!(delivered(&events, &first), std::slice::from_ref(&next));
    assert_eq!(delivered(&events, &second), std::slice::from_ref(&next));
    Ok(())
}

#[tokio::test]
async fn change_request_pauses_dependents() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, sink) = host(&crew)?;
    let running = [
        spec("task-001", "codex", &["src/api/**"], &[], &[]),
        spec("task-002", "claude-code", &["src/limiter/**"], &[], &[]),
        spec("task-003", "agy", &["src/store/**"], &[], &["task-002"]),
        spec("task-004", "codex", &["docs/**"], &[], &[]),
    ];
    for spec in running {
        add_task(&host, spec, TaskState::Running)?;
    }
    run_session(&host, 1, RoleName::Worker, Some("task-001"), "codex", false)?;
    let (_limiter, mut limiter) = run_session(
        &host,
        2,
        RoleName::Worker,
        Some("task-002"),
        "claude-code",
        false,
    )?;

    let asked = request(&["src/limiter/redis.rs"]);
    blackboard::on_report(&host, &task("task-001"), &report(&[], vec![asked])).await?;

    // The worker of the limiter decides; its task and the task built on it wait meanwhile.
    assert_eq!(task_state(&host, "task-002"), Some(TaskState::Blocked));
    assert_eq!(task_state(&host, "task-003"), Some(TaskState::Blocked));
    assert_eq!(task_state(&host, "task-004"), Some(TaskState::Running));
    let events = sink.events();
    let request = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::Posted(message))
                if message.kind == BlackboardKind::ChangeRequest =>
            {
                Some((event.seq, message.clone()))
            }
            _ => None,
        })
        .expect("the request is posted");
    assert_eq!(
        request.1.to,
        Recipient::Worker {
            worker: worker("claude-code")
        }
    );
    let blocked_at = events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Task(TaskEvent::State {
                    to: TaskState::Blocked,
                    ..
                })
            )
        })
        .map(|event| event.seq)
        .max()
        .unwrap();
    let queued_at = events
        .iter()
        .find(|event| {
            matches!(&event.kind, RunEventKind::Session(SessionEvent::Queued(queued)) if queued.id == request.1.id)
        })
        .map(|event| event.seq)
        .expect("queued for the worker's next turn");
    assert!(blocked_at < queued_at, "dependents pause first");
    assert!(
        delivered_now(&mut limiter)[0]
            .1
            .contains(&format!("accept {}", request.1.id))
    );

    // The worker accepts in its next report: the record settles it and the tasks go on.
    let vote = format!("accept {}: a trait keeps the store swappable", request.1.id);
    blackboard::on_report(&host, &task("task-002"), &report(&[&vote], Vec::new())).await?;
    let record = host
        .read(|state| state.blackboard.settled(&request.1.id).cloned())
        .expect("a decision record");
    assert!(record.text.starts_with("Accepted: "), "{}", record.text);
    assert_eq!(record.from, Party::ArgonautsCode);
    assert_eq!(task_state(&host, "task-002"), Some(TaskState::Running));
    assert_eq!(task_state(&host, "task-003"), Some(TaskState::Running));
    Ok(())
}

#[tokio::test]
async fn settled_question_answered_with_record_id() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, sink) = host(&crew)?;
    add_task(
        &host,
        spec("task-002", "codex", &["src/api/**"], &[], &[]),
        TaskState::Running,
    )?;
    add_task(
        &host,
        spec("task-003", "agy", &["src/limiter/**"], &[], &[]),
        TaskState::Running,
    )?;
    run_session(&host, 1, RoleName::Worker, Some("task-002"), "codex", false)?;
    run_session(&host, 2, RoleName::Worker, Some("task-003"), "agy", false)?;
    let question = "Does src/limiter/mod.rs count per key or per user?";
    let asking = TaskReport {
        questions: vec![question.to_owned()],
        ..report(&[], Vec::new())
    };
    blackboard::on_report(&host, &task("task-002"), &asking).await?;
    let first = host
        .read(|state| {
            state
                .blackboard
                .messages
                .first()
                .map(|message| message.id.clone())
        })
        .expect("the question is posted");
    let answer = format!("answer {first}: per key");
    blackboard::on_report(&host, &task("task-003"), &report(&[&answer], Vec::new())).await?;
    let record = host
        .read(|state| {
            state
                .blackboard
                .settled(&first)
                .map(|record| record.id.clone())
        })
        .expect("answered");

    // Asked again by the task's next session with nothing new, Argonauts Code answers with the
    // record's id.
    run_session(&host, 3, RoleName::Worker, Some("task-002"), "codex", false)?;
    let before = sink.events().len();
    blackboard::on_report(&host, &task("task-002"), &asking).await?;
    let replies: Vec<BlackboardMessage> = sink.events()[before..]
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::Posted(message))
                if message.kind == BlackboardKind::Answer =>
            {
                Some(message.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].from, Party::ArgonautsCode);
    assert!(replies[0].refs.contains(&record.to_string()));
    Ok(())
}

/// A task blocked on a question for another task's worker goes back to work when that worker
/// answers in its report, as it does when anyone else answers.
#[tokio::test]
async fn an_answer_in_another_tasks_report_wakes_the_task_that_asked() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, _sink) = host(&crew)?;
    add_task(
        &host,
        spec("task-002", "codex", &["src/api/**"], &[], &[]),
        TaskState::Blocked,
    )?;
    add_task(
        &host,
        spec("task-003", "agy", &["src/limiter/**"], &[], &[]),
        TaskState::Running,
    )?;
    run_session(&host, 1, RoleName::Worker, Some("task-002"), "codex", false)?;
    run_session(&host, 2, RoleName::Worker, Some("task-003"), "agy", false)?;
    let asking = TaskReport {
        status: ReportStatus::Blocked,
        questions: vec!["Does src/limiter/mod.rs count per key or per user?".to_owned()],
        ..report(&[], Vec::new())
    };
    blackboard::on_report(&host, &task("task-002"), &asking).await?;
    let question = host
        .read(|state| {
            let asked = state.blackboard.messages.first()?;
            Some((asked.id.clone(), asked.to.clone()))
        })
        .expect("the question is posted");
    assert_eq!(
        question.1,
        Recipient::Task {
            task: task("task-003")
        },
        "the question is for the task that owns the file"
    );
    assert_eq!(task_state(&host, "task-002"), Some(TaskState::Blocked));

    let answer = format!("answer {}: per key", question.0);
    blackboard::on_report(&host, &task("task-003"), &report(&[&answer], Vec::new())).await?;
    assert!(
        host.read(|state| state.blackboard.settled(&question.0).is_some()),
        "the answer settles the question"
    );
    assert_ne!(
        task_state(&host, "task-002"),
        Some(TaskState::Blocked),
        "the task that asked carries on"
    );
    Ok(())
}

#[tokio::test]
async fn consultant_answers_question_owns_nothing() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    let advisor = crew.worker("claude-code", "anthropic");
    advisor.push(FakeSession::completed(
        "Use a token bucket per key; src/limiter/mod.rs already has the key.",
    ));
    crew.settings.roles.insert(
        RoleName::Consultant,
        RoleSettings {
            models: vec![worker("claude-code")],
            ..RoleSettings::default()
        },
    );
    let (host, sink) = host(&crew)?;
    add_task(
        &host,
        spec("task-002", "codex", &["src/api/**"], &[], &[]),
        TaskState::Running,
    )?;
    let (asker, _controls) =
        run_session(&host, 1, RoleName::Worker, Some("task-002"), "codex", false)?;
    let question = blackboard::post(
        &host,
        Post {
            kind: BlackboardKind::Question,
            from: Party::Agent {
                session: asker,
                worker: worker("codex"),
            },
            to: Recipient::Role {
                role: RoleName::Consultant,
            },
            text: "Token bucket or sliding window?".to_owned(),
            in_reply_to: None,
            refs: Vec::new(),
            from_task: Some(task("task-002")),
        },
    )?;
    let before = sink.events().len();

    let answer = consultant::answer(&host, &question).await?;
    let posted = host
        .read(|state| state.blackboard.get(&answer).cloned())
        .expect("the answer is posted");
    assert_eq!(posted.kind, BlackboardKind::Answer);
    assert_eq!(posted.in_reply_to, Some(question.clone()));
    assert_eq!(
        posted.to,
        Recipient::Task {
            task: task("task-002")
        },
        "back to the asker, for its next turn"
    );
    assert!(posted.text.contains("token bucket"));

    let specs = advisor.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].role, RoleName::Consultant);
    assert_eq!(specs[0].access, Access::ReadOnly);
    assert_eq!(specs[0].task, None);
    assert!(
        specs[0]
            .prompt
            .iter()
            .any(|part| part.kind == InputPartKind::Status)
    );
    assert!(
        specs[0]
            .prompt
            .iter()
            .any(|part| part.text.contains("Token bucket or sliding window?"))
    );
    // It owns no task: nothing it did touched one.
    for event in &sink.events()[before..] {
        assert!(
            !matches!(event.kind, RunEventKind::Task(_)),
            "{:?}",
            event.kind
        );
    }
    // Asked again, the recorded answer comes back without another session.
    assert_eq!(consultant::answer(&host, &question).await?, answer);
    assert_eq!(advisor.specs().len(), 1);
    Ok(())
}
