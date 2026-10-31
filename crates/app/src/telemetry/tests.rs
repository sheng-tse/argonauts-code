//! Every send here goes to a server on this computer that the test runs; nothing leaves it.

use std::collections::BTreeMap;

use argonauts_core::base::names::ModelRef;
use argonauts_core::runs::events::goal::{Criterion, GoalEvent, GoalRun};
use argonauts_core::runs::events::run::{CaptainOrCrewDecision, RunLifecycle, RunMode};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::agents::EndpointSettings;
use argonauts_core::workflows::goal::{CheckWith, GoalFrom, GoalScope, GoalSpec, KeptBy};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use super::*;
use crate::client::test_support::{event, run_started};
use crate::usage::usage_ledger::LedgerLine;
use crate::usage::work_ledger::{Work, WorkLine};

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
        billing: None,
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
        ..Found::default()
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
    let sent_at = body["timestamp"].as_str().unwrap();
    assert!(sent_at.ends_with("T00:00:00Z"), "the day alone: {sent_at}");
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
    assert_eq!(properties["features"]["ultracode"], 0);
    assert_eq!(properties["features"]["mods"], 2);
    assert_eq!(properties["fields"], 3);
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
    let mut settings = Settings::default();
    settings.telemetry.enabled = false;
    let dnt = |name: &str| (name == "DO_NOT_TRACK").then(|| "1".to_owned());
    let held = held(&settings, &dnt);
    let text = show(&paths, &settings, &Found::default(), &held, now(), false);
    assert!(text.starts_with("Usage reports are off"), "{text}");
    assert!(text.contains("DO_NOT_TRACK is set"), "{text}");
    assert!(
        text.contains("This is a debug build, so it sends nothing."),
        "{text}"
    );
    assert!(text.contains("\n  install    "), "{text}");
    assert!(!text.contains('\x1b'), "no colour off a terminal: {text}");
    assert!(!state_file(&paths).exists(), "show writes nothing");
}

/// Nothing goes before the usage line has shown, nor with CI, DO_NOT_TRACK or an agent's
/// session set, nor with reports off.
#[test]
fn nothing_is_sent_before_the_line_or_while_asked_for_quiet() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let none = |_: &str| None;
    assert!(!may_send(&paths, &on(), &none), "before the line");
    assert!(notice::claim_in(&paths, &on(), &none));
    assert!(may_send(&paths, &on(), &none));
    for quiet in ["CI", "DO_NOT_TRACK", "ARGONAUTS_SESSION"] {
        let set = move |name: &str| (name == quiet).then(|| "1".to_owned());
        assert!(!may_send(&paths, &on(), &set), "{quiet}");
    }
    let mut off = on();
    off.telemetry.enabled = false;
    assert!(!may_send(&paths, &off, &none));
}

/// A report goes at the first start of a later day, and never within a day of the last try.
#[test]
fn a_report_waits_a_whole_day_after_the_last() {
    let at = time::macros::datetime!(2026-10-20 14:00 UTC);
    let today = local_day(at);
    let mut state = State::new(today);
    assert_eq!(state.due(at), show::Due::Tomorrow, "counting began today");
    state.counted_from = today - Duration::days(1);
    state.tried = today - Duration::days(1);
    assert_eq!(state.due(at), show::Due::NextStart);
    state.tried_at = Some(at - Duration::hours(20));
    let show::Due::After { clock, .. } = state.due(at) else {
        panic!("{:?}", state.due(at));
    };
    assert_eq!(clock, super::clock(at + Duration::hours(4)));
    state.tried = today;
    state.tried_at = Some(at);
    assert!(matches!(state.due(at), show::Due::After { .. }));
    assert_eq!(state.due(at + Duration::hours(25)), show::Due::NextStart);
}

#[test]
fn reports_go_to_the_us_unless_the_build_names_a_host() {
    assert_eq!(
        send::capture_address(None),
        "https://us.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        send::capture_address(Some("")),
        "https://us.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        send::capture_address(Some("https://eu.i.posthog.com/")),
        "https://eu.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        send::capture_address(Some("https://ph.example.org/i/v0/e/")),
        "https://ph.example.org/i/v0/e/"
    );
}

/// `argonauts telemetry off` as `telemetry_off` draws it.
#[test]
fn off_says_so_and_how_to_turn_them_back_on() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    assert_eq!(turn_off(&paths).unwrap(), OFF);
    argonauts_testkit::pictures::picture_dump::dump("telemetry_off", OFF);
    let picture = include_str!("../../tests/golden/telemetry_off.txt");
    assert_eq!(OFF, picture.trim_end());
    let text = std::fs::read_to_string(paths.global_config()).unwrap();
    assert!(text.contains("enabled = false"), "{text}");
}

#[test]
fn no_test_build_sends() {
    assert_eq!(Sender::built_in().err(), Some(Held::DebugBuild));
}

/// The project's own key unless the build names another, which sends a fork's reports to its
/// own project, or an empty one, which turns them off for good. A host named without a key
/// sends nothing, so the project's key never reaches another host.
#[test]
fn a_build_can_point_its_reports_elsewhere_or_nowhere() {
    let ours = Sender::chosen(None, None).unwrap();
    assert!(
        ours.key.starts_with("phc_"),
        "a project key, never a personal one"
    );
    assert_eq!(ours.address, send::US_CAPTURE);
    let fork = Sender::chosen(Some("phc_fork_key"), Some("https://eu.i.posthog.com")).unwrap();
    assert_eq!(fork.key, "phc_fork_key");
    assert_eq!(fork.address, "https://eu.i.posthog.com/i/v0/e/");
    assert!(Sender::chosen(Some(""), None).is_none());
    assert!(Sender::chosen(Some("  "), Some("https://eu.i.posthog.com")).is_none());
    assert!(Sender::chosen(None, Some("https://posthog.fork.example")).is_none());
    let blank_host = Sender::chosen(None, Some("  ")).unwrap();
    assert_eq!(blank_host.address, send::US_CAPTURE);
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
    assert_eq!(
        count(&paths).features.ultracode,
        0,
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
    assert_eq!(features.ultracode, 1);

    let alone = tempfile::tempdir().unwrap();
    let alone = Paths::new(alone.path());
    used(&alone, Use::Goal);
    used(&alone, Use::Goal);
    used(&alone, Use::Ultracode);
    let features = count(&alone).features;
    assert_eq!(features.goals, 2);
    assert_eq!(features.ultracode, 1);
    let tomorrow = (day + Duration::days(1), day + Duration::days(2));
    let later = usage_between(&alone, &on(), &Found::default(), &State::new(day), tomorrow);
    assert_eq!(later.features.goals, 0, "a use counts on its own day");
}

/// A command counts by the word it is typed with and nothing typed after it; a crash by the
/// part of the code it came from, and a dependency's as `elsewhere`.
#[test]
fn commands_and_crashes_count_by_fixed_words() {
    use argonauts_core::input::{ResumeTarget, SlashCommand};

    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let model = SlashCommand::Model {
        role: None,
        worker: Some(ModelRef::new("opencode", Some("secret-model".to_owned()))),
        effort: Some("secret-effort".to_owned()),
    };
    used(&paths, Use::command(&model));
    used(&paths, Use::command(&model));
    used(&paths, Use::command(&SlashCommand::Resume { target: None }));
    let resume = SlashCommand::Resume {
        target: Some(ResumeTarget::Run(argonauts_core::RunId::generate())),
    };
    used(&paths, Use::command(&resume));
    let style = SlashCommand::OutputStyle {
        style: Some("secret-style".to_owned()),
    };
    used(&paths, Use::command(&style));
    used(&paths, Use::WorkflowView);
    for file in [
        "crates/tui/src/app/mod.rs",
        "crates/adapters/codex/src/parser/mod.rs",
        "/Users/someone/.cargo/registry/src/index.crates.io-0/ratatui-0.29.0/src/buffer.rs",
        "crates/secret-fork/src/lib.rs",
    ] {
        used(&paths, Use::crash_at(file));
    }

    let day = today();
    let usage = usage_between(
        &paths,
        &on(),
        &Found::default(),
        &State::new(day),
        (day, day + Duration::days(1)),
    );
    let counted: Vec<(&str, u32)> = usage
        .features
        .commands
        .iter()
        .map(|(name, count)| (name.as_str(), *count))
        .collect();
    assert_eq!(
        counted,
        [("/model", 2), ("/output-style", 1), ("/resume", 2)]
    );
    assert_eq!(usage.features.workflow_view, 1);
    let places: Vec<(&str, u32)> = usage
        .health
        .crashes
        .iter()
        .map(|(place, count)| (place.as_str(), *count))
        .collect();
    assert_eq!(places, [("adapters", 1), ("elsewhere", 2), ("tui", 1)]);
    let sent = serde_json::to_string(&usage).unwrap();
    assert!(!sent.contains("secret"), "{sent}");
}

/// The app notes each command it runs and each opening of a workflow view while reports are on,
/// and nothing once they're off.
#[tokio::test]
async fn the_app_counts_its_commands_and_workflow_views_only_while_reports_are_on() {
    use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
    use argonauts_core::input::SlashCommand;

    use crate::controller::base::test_support::{CliOptions, TestDriver};

    let mut counted = Vec::new();
    for flags in [Vec::new(), vec!["telemetry.enabled=false".to_owned()]] {
        let mut driver = TestDriver::start_with(CliOptions {
            flags,
            ..CliOptions::default()
        })
        .await;
        let paths = driver.env.paths.clone();
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Help,
            })
            .await;
        driver
            .send(UiCommand::Request {
                view: ViewRequest::Workflow {
                    run: argonauts_core::RunId::generate(),
                    workflow: None,
                },
            })
            .await;
        // The answer to a request sent after them says both were taken.
        driver
            .send(UiCommand::Request {
                view: ViewRequest::Config,
            })
            .await;
        driver
            .until(|event| {
                matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Config { .. }))
            })
            .await;
        let uses: Vec<Use> = uses::read_since(&paths, today())
            .into_iter()
            .map(|line| line.used)
            .filter(|used| !matches!(used, Use::Started { .. }))
            .collect();
        counted.push(uses);
        driver.exit().await;
    }
    let help = Use::Command {
        name: "/help".to_owned(),
    };
    assert_eq!(counted, [vec![help, Use::WorkflowView], Vec::new()]);
}

/// The event counts the work the Overview's records hold for Argonauts Code: the same run lines
/// give the Overview what ran through Argonauts Code and the event its workflows, and the same
/// ledger its tokens. What the import read from a CLI's own files fills the Overview and never the
/// event.
#[test]
fn the_event_counts_what_the_overviews_records_hold_and_nothing_imported() {
    use argonauts_core::app::overview::{ImportStatus, OverviewPeriod};
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
        clock: argonauts_tui::base::format::WallClock::default(),
        prices: &argonauts_core::models::pricing::PriceList::default(),
        names: &argonauts_core::look::display_names::DisplayNames::default(),
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

/// Each outcome count, from hand-made ledger lines of one council run and one of yours.
#[test]
fn the_event_counts_how_the_work_went() {
    use crate::usage::work_ledger::{ApprovedBy, FallbackCause};
    use argonauts_core::runs::events::run::RunOutcome;
    let day = today();
    let run = argonauts_core::RunId::generate();
    let task = argonauts_core::TaskId::new("task-001").unwrap();
    let luna = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
    let opus = ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()));
    let line = |what| WorkLine {
        day,
        repo: SECRET_REPO.to_owned(),
        run: run.clone(),
        what,
    };
    let review = |sent_back, findings, blocking| {
        line(Work::Review {
            by: luna.clone(),
            task: Some(task.clone()),
            judge: false,
            sent_back,
            reason: None,
            checks: Vec::new(),
            checks_failed: None,
            ran_own: false,
            findings,
            blocking,
        })
    };
    let mut yours = run_line(day, "acme-secret-flow");
    yours.run = argonauts_core::RunId::generate();
    let work = vec![
        review(true, 3, 1),
        review(false, 1, 0),
        line(Work::Session {
            role: "worker".to_owned(),
            by: luna.clone(),
            task: Some(task.clone()),
            kind: None,
            spent: Default::default(),
            took: 1,
            tools: 0,
            fallback_for: Some(opus.clone()),
            why: "usage limit, resets 3:45pm".to_owned(),
            window: None,
            cause: Some(FallbackCause::UsageLimit),
        }),
        line(Work::Task {
            task: task.clone(),
            by: opus.clone(),
            kind: None,
            sent_back: 1,
            merged: true,
            reviewed: true,
            checks_first: Some(false),
            checks_last: Some(true),
        }),
        line(run_line(day, "council").what),
        line(Work::Outcome {
            merged: false,
            replans: 0,
            merge: None,
            merged_by: None,
            checks_failed: false,
            ended: Some(RunOutcome::Abandoned),
            merge_approved_by: Some(ApprovedBy::Autopilot),
            merge_sent_back: 1,
        }),
        yours,
    ];
    let usage = event::count(
        "id",
        &[],
        &work,
        &[],
        &on(),
        &Found::default(),
        (day, day + Duration::days(1)),
    );
    assert_eq!(usage.runs["council"].abandoned, 1);
    assert_eq!(
        usage.features.workflows["custom"], 1,
        "yours goes by no name"
    );
    assert_eq!(usage.tasks.second_pass, 1);
    assert_eq!(usage.findings.given, 4);
    assert_eq!(usage.findings.blocking, 1);
    assert_eq!(
        usage.findings.fixed, 3,
        "the sent-back review's findings, once approved"
    );
    assert_eq!(usage.findings.by_reviewer["gpt-6-luna"], 4);
    assert_eq!(usage.findings.by_builder["claude-opus-5-5"], 4);
    assert_eq!((usage.checks.tasks_checked, usage.checks.first_try), (1, 0));
    assert_eq!(usage.checks.after_review, 1);
    assert_eq!((usage.merges.autopilot, usage.merges.sent_back), (1, 1));
    assert_eq!(usage.fallbacks.usage_limit, 1);
    assert_eq!(usage.fallbacks.to["gpt-6-luna"], 1);
    let sent = serde_json::to_string(&usage).unwrap();
    assert!(!sent.contains("acme"), "{sent}");
    assert!(!sent.contains("usage limit"), "no free text: {sent}");
}

/// The report's fields, under the version they go with (a map's own entries aside). A field
/// added, taken out or renamed needs a new version, so the notice shows once more, and
/// PRIVACY.md to name it.
#[test]
fn the_field_list_is_the_one_its_version_names() {
    fn walk(value: &Value, at: &str, fields: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let path = if at.is_empty() {
                        key.clone()
                    } else {
                        format!("{at}.{key}")
                    };
                    fields.push(path.clone());
                    walk(value, &path, fields);
                }
            }
            Value::Array(items) => {
                for item in items.iter().take(1) {
                    walk(item, &format!("{at}[]"), fields);
                }
            }
            _ => {}
        }
    }
    let day = today();
    let usage = event::count(
        "id",
        &[line(day, "claude-code", Some("claude-opus-5-5"), 10, 0.1)],
        &[],
        &[],
        &on(),
        &Found::default(),
        (day, day + Duration::days(1)),
    );
    let mut fields = Vec::new();
    walk(&serde_json::to_value(&usage).unwrap(), "", &mut fields);
    let expected = [
        "fields, install, version, os",
        "machine, machine.chip, machine.ram_gb, machine.cores, machine.terminal",
        "setup, setup.screen, setup.sandbox, setup.approvals, setup.autopilot, setup.git_repo",
        "clis, models, models[].cli, models[].model, models[].tokens, models[].input",
        "models[].output, models[].cached, models[].usd, models[].billing, models[].roles",
        "models[].efforts, runs, turns",
        "tasks, tasks.reviewed, tasks.first_pass, tasks.second_pass, tasks.more_rounds",
        "tasks.never_passed",
        "findings, findings.given, findings.blocking, findings.fixed, findings.by_reviewer",
        "findings.by_builder",
        "checks, checks.tasks_checked, checks.first_try, checks.after_review",
        "checks.final_check_failed",
        "merges, merges.you, merges.autopilot, merges.judge, merges.rules, merges.sent_back",
        "fallbacks, fallbacks.signed_out, fallbacks.usage_limit, fallbacks.down",
        "fallbacks.model_unavailable, fallbacks.stuck, fallbacks.other, fallbacks.to",
        "features, features.workflows, features.goals, features.ultracode, features.mods",
        "features.commands, features.workflow_view",
        "health, health.crashes, health.not_understood, health.startup",
        "behaviors, behavior_outcomes, behaviors_unlabelled",
    ];
    let expected: Vec<&str> = expected.iter().flat_map(|row| row.split(", ")).collect();
    assert_eq!(usage.fields, 3);
    assert_eq!(fields, expected);
}
