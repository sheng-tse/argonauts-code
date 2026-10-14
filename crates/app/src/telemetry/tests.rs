//! Every send here goes to a server on this computer that the test runs; nothing leaves it.

use std::collections::BTreeMap;

use argonauts_core::events::goal::{Criterion, GoalEvent, GoalRun};
use argonauts_core::events::run::{CaptainOrCrewDecision, RunLifecycle, RunMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::goal::{CheckWith, GoalFrom, GoalScope, GoalSpec, KeptBy};
use argonauts_core::names::ModelRef;
use argonauts_core::settings::agents::EndpointSettings;
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use super::*;
use crate::client::test_support::{event, run_started};
use crate::usage_ledger::LedgerLine;
use crate::work_ledger::{Work, WorkLine};

const SECRET_REPO: &str = "/Users/someone/acme-secret-repo";

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn today() -> Date {
    local_day(now())
}

fn line(day: Date, agent: &str, model: Option<&str>, tokens: u64, cost: f64) -> LedgerLine {
    LedgerLine {
        day,
        repo: SECRET_REPO.to_owned(),
        run: None,
        agent: agent.to_owned(),
        model: model.map(str::to_owned),
        agent_id: agent.to_owned(),
        input: tokens,
        cached: 0,
        output: 0,
        cost,
        estimated: false,
        partial: false,
        hour: None,
        conversation: None,
        took: 0,
    }
}

fn run_line(day: Date, workflow: &str) -> WorkLine {
    WorkLine {
        day,
        repo: SECRET_REPO.to_owned(),
        run: argonauts_core::RunId::generate(),
        what: Work::Run {
            by: Some(ModelRef::new("claude-code".to_owned(), None)),
            took: 60,
            workflow: Some(workflow.to_owned()),
            goals: 0,
            ultracode: false,
            answered: 0,
        },
    }
}

fn on() -> Settings {
    let mut settings = Settings::default();
    settings.telemetry.enabled = true;
    settings
}

/// A capture address on this computer that answers `status` to each request and hands back
/// each body it got.
async fn fake_posthog(status: u16) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}/i/v0/e/", listener.local_addr().unwrap());
    let (bodies, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        let _ = bodies.send(body.to_owned());
                        break;
                    }
                }
            }
            let answer = format!("HTTP/1.1 {status} OK\r\ncontent-length: 2\r\n\r\n{{}}");
            let _ = stream.write_all(answer.as_bytes()).await;
        }
    });
    (address, received)
}

fn sender(address: String) -> Sender {
    Sender {
        address,
        key: "phc_test_key".to_owned(),
    }
}

#[test]
fn the_first_day_sends_nothing_and_starts_the_count() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    assert!(due_event(&paths, &on(), &Found::default(), now()).is_none());
    let state = State::load(&state_file(&paths)).expect("the state is kept");
    assert_eq!(state.counted_from, today());
    assert!(!state.install.is_empty());
}

#[tokio::test]
async fn a_day_later_one_anonymous_event_goes_and_counts_each_day_once() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let yesterday = today() - Duration::days(1);
    let mut state = State::new(yesterday);
    state.tried = yesterday;
    state.save(&state_file(&paths)).unwrap();
    UsageLedger::new(&paths)
        .append(&[
            line(
                yesterday,
                "claude-code",
                Some("claude-opus-5-5"),
                1_000,
                1.234,
            ),
            line(yesterday, "claude-code", Some("claude-opus-5-5"), 500, 0.5),
            line(
                yesterday,
                "my-gpu-box",
                Some("acme-private-finetune"),
                70,
                0.0,
            ),
        ])
        .unwrap();
    WorkLedger::beside(&paths.usage_dir())
        .append(&[
            run_line(yesterday, "council"),
            run_line(yesterday, "acme-secret-flow"),
        ])
        .unwrap();

    let (address, mut bodies) = fake_posthog(200).await;
    let found = Found {
        clis: BTreeMap::from([("claude-code".to_owned(), "2.1.295".to_owned())]),
        mods: 2,
    };
    assert!(send_due(&paths, &on(), &found, &sender(address.clone()), now()).await);

    let body = bodies.recv().await.expect("one event");
    assert!(
        !body.contains("acme"),
        "nothing names a repo, a model or a workflow of yours"
    );
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["api_key"], "phc_test_key");
    assert_eq!(body["event"], "daily_usage");
    assert_eq!(body["distinct_id"].as_str(), Some(state.install.as_str()));
    let properties = &body["properties"];
    assert_eq!(properties["$process_person_profile"], false);
    assert_eq!(properties["clis"]["claude-code"], "2.1.295");
    assert_eq!(properties["models"][0]["model"], "claude-opus-5-5");
    assert_eq!(properties["models"][0]["tokens"], 1_500);
    assert_eq!(properties["models"][0]["usd"], 1.73);
    assert_eq!(properties["models"][0]["billing"], "plan");
    assert_eq!(properties["models"][1]["cli"], "endpoint");
    assert_eq!(properties["models"][1]["model"], "other");
    assert_eq!(properties["features"]["workflows"]["council"], 1);
    assert_eq!(properties["features"]["workflows"]["custom"], 1);
    assert_eq!(properties["features"]["goals"], 0);
    assert_eq!(properties["features"]["ultracode"], false);
    assert_eq!(properties["features"]["mods"], 2);
    assert!(properties.get("install").is_none());

    let kept = State::load(&state_file(&paths)).unwrap();
    assert_eq!(kept.counted_from, today());
    // The same day again: nothing is due, and nothing goes.
    assert!(!send_due(&paths, &on(), &found, &sender(address), now()).await);
    assert!(bodies.try_recv().is_err());
}

#[tokio::test]
async fn a_failed_send_waits_for_the_next_day_and_keeps_its_days() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let two_days_ago = today() - Duration::days(2);
    let mut state = State::new(two_days_ago);
    state.tried = two_days_ago;
    state.save(&state_file(&paths)).unwrap();

    let (address, mut bodies) = fake_posthog(503).await;
    assert!(
        !send_due(
            &paths,
            &on(),
            &Found::default(),
            &sender(address.clone()),
            now()
        )
        .await
    );
    assert!(bodies.recv().await.is_some(), "it was tried");
    let kept = State::load(&state_file(&paths)).unwrap();
    assert_eq!(kept.tried, today());
    assert_eq!(
        kept.counted_from, two_days_ago,
        "the days wait for the next send"
    );
    assert!(due_event(&paths, &on(), &Found::default(), now()).is_none());
}

#[test]
fn an_endpoint_on_this_computer_and_a_listed_model_are_named_as_they_are() {
    let mut settings = on();
    settings.endpoints.insert(
        "ollama".to_owned(),
        EndpointSettings {
            local: true,
            ..EndpointSettings::default()
        },
    );
    let day = today();
    let usage = event::count(
        "id",
        &[line(day, "ollama", Some("claude-opus-5-5"), 10, 0.0)],
        &[],
        &[],
        &settings,
        &Found::default(),
        (day, day + Duration::days(1)),
    );
    assert_eq!(usage.models[0].model, "claude-opus-5-5");
    assert_eq!(usage.models[0].billing, "local");
}

#[test]
fn show_says_it_is_off_and_that_this_build_sends_nothing() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let text = show(&paths, &Settings::default(), &Found::default(), true, now());
    assert!(text.starts_with("Telemetry is off"), "{text}");
    assert!(text.contains("DO_NOT_TRACK is set"), "{text}");
    assert!(text.contains("no PostHog key"), "{text}");
    assert!(text.contains("\"event\": \"daily_usage\""), "{text}");
    assert!(!state_file(&paths).exists(), "show writes nothing");
}

#[test]
fn no_test_build_carries_a_key() {
    assert!(Sender::built_in().is_none());
}

/// A run the Captain-or-crew rules sent to a workflow for ultracode, with a goal of its own set
/// twice and one for a task.
fn a_run_with_goals() -> Vec<RunEvent> {
    let mut started = run_started(0, "tidy the helpers", None);
    if let RunEventKind::Run(RunLifecycle::Started(run)) = &mut started.kind {
        run.captain_or_crew = Some(CaptainOrCrewDecision {
            mode: RunMode::Crew,
            move_to_crew: false,
            rule: "ultracode".to_owned(),
            reason: "Ultracode is on".to_owned(),
            captain: None,
        });
    }
    let spec = GoalSpec::from_lines(
        "the helpers' tests pass",
        vec![Criterion::new("G1", "cargo test passes")],
        CheckWith::Auto,
        GoalFrom::Command,
    );
    let set = GoalRun::new(&spec, GoalScope::Workflow, KeptBy::Argonauts);
    let mut of_a_task = set.clone();
    of_a_task.task = Some(argonauts_core::TaskId::new("task-001").unwrap());
    let goal = |seq, goal: &GoalRun| {
        event(
            seq,
            RunEventKind::Goal(GoalEvent::Set(Box::new(goal.clone()))),
        )
    };
    vec![started, goal(1, &set), goal(2, &of_a_task), goal(3, &set)]
}

#[test]
fn goals_and_ultracode_count_from_runs_and_from_the_conversation() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let day = today();
    let days = (day, day + Duration::days(1));
    let count =
        |paths: &Paths| usage_between(paths, &on(), &Found::default(), &State::new(day), days);
    assert_eq!(count(&paths).features.goals, 0);
    assert!(
        !count(&paths).features.ultracode,
        "ultracode that's on but unused isn't sent"
    );

    WorkLedger::beside(&paths.usage_dir()).run_ended(
        Path::new(SECRET_REPO),
        &argonauts_core::RunId::generate(),
        &a_run_with_goals(),
        now(),
    );
    let features = count(&paths).features;
    assert_eq!(features.goals, 2, "a goal set again is still one");
    assert!(features.ultracode);

    let alone = tempfile::tempdir().unwrap();
    let alone = Paths::new(alone.path());
    used(&alone, Use::Goal);
    used(&alone, Use::Goal);
    used(&alone, Use::Ultracode);
    let features = count(&alone).features;
    assert_eq!(features.goals, 2);
    assert!(features.ultracode);
    let tomorrow = (day + Duration::days(1), day + Duration::days(2));
    let later = usage_between(&alone, &on(), &Found::default(), &State::new(day), tomorrow);
    assert_eq!(later.features.goals, 0, "a use counts on its own day");
}

/// The event counts the work the Overview's records hold for Argonauts Code: the same run lines
/// give the Overview what ran through Argonauts Code and the event its workflows, and the same
/// ledger its tokens. What the import read from a CLI's own files fills the Overview and never the
/// event.
#[test]
fn the_event_counts_what_the_overviews_records_hold_and_nothing_imported() {
    use argonauts_core::overview::{ImportStatus, OverviewPeriod};
    use argonauts_core::settings::overview::YearColors;

    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let day = today();
    UsageLedger::new(&paths)
        .append(&[line(day, "codex", Some("gpt-5.6"), 1_000, 0.5)])
        .unwrap();
    WorkLedger::beside(&paths.usage_dir())
        .append(&[
            run_line(day, "council"),
            run_line(day, "council"),
            run_line(day, "acme-secret-flow"),
        ])
        .unwrap();
    let imported = crate::overview::Store::new(&paths);
    imported
        .append(&[crate::overview::Record {
            at: now(),
            cli: "claude-code".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            folder: SECRET_REPO.to_owned(),
            input: 9_000,
            output: 1_000,
            cache_read: 0,
            cache_write: 0,
            session: 1,
            subagent: false,
            whole_day: false,
        }])
        .unwrap();

    let sent = usage_between(
        &paths,
        &on(),
        &Found::default(),
        &State::new(day),
        (day, day + Duration::days(1)),
    );
    let records = imported.records();
    let lines = UsageLedger::new(&paths).read_since(None);
    let work = WorkLedger::beside(&paths.usage_dir()).read_since(None);
    let sources = crate::overview::Sources {
        records: &records,
        lines: &lines,
        work: &work,
        clock: argonauts_tui::format::WallClock::default(),
        prices: &argonauts_core::pricing::PriceList::default(),
        names: &argonauts_core::display_names::DisplayNames::default(),
    };
    let shown = crate::overview::build_overview(
        &sources,
        OverviewPeriod::All,
        day,
        ImportStatus::Done,
        YearColors::Cli,
        &|id: &str| id.to_owned(),
    );

    let through = shown.argonauts.expect("what ran through Argonauts Code");
    let runs: u32 = sent.features.workflows.values().sum();
    assert_eq!(runs, through.workflows);
    assert_eq!(sent.features.workflows.get("council"), Some(&2));
    assert_eq!(
        sent.features.workflows.get("custom"),
        Some(&1),
        "a workflow of yours goes by no name"
    );
    assert!(
        shown
            .models
            .iter()
            .any(|model| model.agent == "claude-code"),
        "the Overview shows what the import read"
    );
    assert!(
        sent.models.iter().all(|model| model.cli != "claude-code"),
        "{:?}",
        sent.models
    );
    let tokens: u64 = sent.models.iter().map(|model| model.tokens).sum();
    assert_eq!(tokens, 1_000);
}
