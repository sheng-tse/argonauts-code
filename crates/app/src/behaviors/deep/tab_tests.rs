use std::fs;
use std::path::Path;

use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::behavior_map::{BehaviorMap, MapRequest, Placing};
use argonauts_core::deep_map::{DeepDone, DeepScope, DeepStart};
use argonauts_core::settings::behaviors::BehaviorMap as SettingsMap;
use argonauts_testkit::FakeSession;
use serde_json::json;

use super::ALREADY_RUNNING;
use crate::controller::test_support::{CliOptions, TestDriver};

/// One run in the home, its session running `commands` through Bash.
fn record_run(home: &Path, commands: &[&str]) {
    let run = home.join("state/council-3f9a1c2b4d5e/runs/run-01");
    fs::create_dir_all(&run).unwrap();
    let started = json!({"seq": 0, "ts": "2026-10-17T07:17:00Z", "run_id": "run-01",
        "source": "claude-code", "session": "s-001",
        "kind": {"type": "session", "data": {"event": "started", "worker": "claude-code/haiku",
            "agent_id": "claude-code", "provider": "anthropic", "kind": "cli", "role": "worker",
            "step": "build", "access": "build", "lifetime": "task", "resumed_from": null,
            "follow_key": 1}}});
    let mut lines = format!("{started}\n");
    for (at, command) in commands.iter().enumerate() {
        let line = json!({"seq": at + 1, "ts": "2026-10-17T07:17:01Z", "run_id": "run-01",
            "source": "claude-code", "session": "s-001",
            "kind": {"type": "agent", "data": {"type": "tool_started", "id": format!("t{at}"),
                "name": "Bash", "input": {"command": command}, "parent": null}}});
        lines.push_str(&format!("{line}\n"));
    }
    fs::write(run.join("events.jsonl"), lines).unwrap();
}

fn with_haiku() -> UiCommand {
    UiCommand::DeepStart {
        start: DeepStart {
            scope: DeepScope::Everything,
            summarizer: "claude-code/haiku".parse().unwrap(),
            download_embedder: false,
            redo: false,
        },
    }
}

async fn done(driver: &mut TestDriver) -> DeepDone {
    match driver
        .until(|event| matches!(event, AppEvent::DeepDone { .. }))
        .await
    {
        AppEvent::DeepDone { done } => done,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn d_asks_with_the_recorded_actions_and_calls_no_model() {
    let mut driver = TestDriver::start().await;
    record_run(driver.repo.home(), &["cargo test", "git status", "ls"]);
    driver
        .send(UiCommand::Request {
            view: ViewRequest::DeepOffer,
        })
        .await;
    let offered = driver
        .until(|event| {
            matches!(event, AppEvent::View { data } if matches!(**data, ViewData::DeepOffer { .. }))
        })
        .await;
    let AppEvent::View { data } = offered else {
        unreachable!()
    };
    let ViewData::DeepOffer { offer } = *data else {
        unreachable!()
    };
    assert_eq!(offer.scopes[0].scope, DeepScope::Everything);
    assert_eq!(offer.scopes[0].actions, 3);
    assert!(offer.made_by.is_none());
    assert!(driver.agent.specs().is_empty(), "no session was started");
}

#[tokio::test]
async fn an_update_summarizes_through_the_apps_sessions_and_says_how_far_it_got() {
    let mut driver = TestDriver::start().await;
    record_run(driver.repo.home(), &["cargo test", "git status", "ls"]);
    driver.agent.push(FakeSession::completed(
        "1. Run the test suite\n2. Check what changed\n3. List the files",
    ));
    driver.send(with_haiku()).await;
    let done = done(&mut driver).await;
    assert_eq!((done.written, done.failed), (3, None));
    assert!(done.by_words, "no embedding model answers in a test");
    let progress: Vec<(usize, usize)> = driver
        .seen()
        .iter()
        .filter_map(|event| match event {
            AppEvent::DeepProgress { progress } => Some((progress.done, progress.total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress.last(), Some(&(3, 3)));
    assert_eq!(driver.agent.specs().len(), 1, "ten actions to a request");
    let kept = fs::read_to_string(driver.repo.home().join("behaviors/summaries.jsonl")).unwrap();
    assert!(kept.contains("Run the test suite"), "{kept}");
}

#[tokio::test]
async fn x_stops_the_update_and_another_waits_for_it() {
    let mut driver = TestDriver::start().await;
    record_run(driver.repo.home(), &["cargo test"]);
    driver.agent.push(FakeSession::until_stopped(Vec::new()));
    driver.send(with_haiku()).await;
    driver.send(with_haiku()).await;
    driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text == ALREADY_RUNNING))
        .await;
    driver.send(UiCommand::DeepStop).await;
    let done = done(&mut driver).await;
    assert!(done.stopped);
    assert_eq!(done.written, 0);
    // Once it stopped, another may start.
    driver
        .agent
        .push(FakeSession::completed("1. Run the test suite"));
    driver.send(with_haiku()).await;
    assert_eq!(done_written(&mut driver).await, 1);
}

async fn done_written(driver: &mut TestDriver) -> usize {
    done(driver).await.written
}

async fn the_map_sent(driver: &mut TestDriver) -> argonauts_core::behavior_map::BehaviorMap {
    let shown = driver
        .until(|event| {
            matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Behaviors { .. }))
        })
        .await;
    let AppEvent::View { data } = shown else {
        unreachable!()
    };
    let ViewData::Behaviors { map } = *data else {
        unreachable!()
    };
    *map
}

async fn the_map(driver: &mut TestDriver, request: MapRequest) -> BehaviorMap {
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Behaviors(request),
        })
        .await;
    the_map_sent(driver).await
}

#[tokio::test]
async fn the_tab_offers_summaries_until_they_are_made_then_shows_them() {
    let mut driver = TestDriver::start().await;
    record_run(driver.repo.home(), &["cargo test", "git status", "ls"]);
    let first = the_map(&mut driver, MapRequest::default()).await;
    assert_eq!(first.placing, Placing::Labels);
    assert!(first.offer_summaries);
    assert_eq!(first.actions.len(), 3);
    driver.agent.push(FakeSession::completed(
        "1. Run the test suite\n2. Check what changed\n3. List the files",
    ));
    driver.send(with_haiku()).await;
    let made = done(&mut driver).await;
    assert_eq!(
        (made.written, made.failed.as_deref()),
        (3, None),
        "{made:?}, sessions started: {:?}",
        driver
            .agent
            .specs()
            .iter()
            .map(|spec| spec.key.to_string())
            .collect::<Vec<_>>()
    );
    // Made from the tab, Summaries comes to it unasked.
    let made = the_map_sent(&mut driver).await;
    assert_eq!(made.request.map, Some(SettingsMap::Summaries));
    assert!(
        matches!(&made.placing, Placing::Summaries { by } if by == "Opus 5.5"),
        "{:?}",
        made.placing
    );
    assert!(!made.offer_summaries);
    assert_eq!(made.unsummarized, 0);
    assert!(made.by_words, "no embedding model answers in a test");
    let summaries: Vec<&str> = made
        .actions
        .iter()
        .filter_map(|action| action.summary.as_deref())
        .collect();
    assert_eq!(summaries.len(), 3);
    assert!(summaries.contains(&"Run the test suite"), "{summaries:?}");
    // d goes back to the labels, which no longer offer Summaries.
    let labels = the_map(
        &mut driver,
        MapRequest {
            map: Some(SettingsMap::Labels),
            ..MapRequest::default()
        },
    )
    .await;
    assert_eq!(labels.placing, Placing::Labels);
    assert!(!labels.offer_summaries);
}

#[tokio::test]
async fn behaviors_map_summaries_opens_on_summaries_and_counts_what_they_lack() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec!["behaviors.map=summaries".to_owned()],
        ..CliOptions::default()
    })
    .await;
    record_run(driver.repo.home(), &["cargo test", "git status"]);
    driver.agent.push(FakeSession::completed(
        "1. Run the test suite\n2. Check what changed",
    ));
    driver.send(with_haiku()).await;
    done(&mut driver).await;
    record_run(driver.repo.home(), &["cargo test", "git status", "ls"]);
    let map = the_map(&mut driver, MapRequest::default()).await;
    assert!(matches!(map.placing, Placing::Summaries { .. }));
    assert_eq!(map.actions.len(), 2);
    assert_eq!(map.unsummarized, 1);
}

/// Opening the tab, on either map and however it's grouped, reads what's kept: no session
/// starts, and the summarizer and embedder the settings name, a server that fails if called,
/// hear nothing.
#[tokio::test]
async fn opening_the_tab_calls_no_model() {
    let server = argonauts_testkit::FakeOpenAi::start().unwrap();
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec![
            format!("endpoints.ollama.base_url={}", server.base_url()),
            "behaviors.embedder=ollama/nomic-embed-text".to_owned(),
        ],
        ..CliOptions::default()
    })
    .await;
    record_run(driver.repo.home(), &["cargo test", "git status", "ls"]);
    driver.agent.push(FakeSession::completed(
        "1. Run the test suite\n2. Check what changed\n3. List the files",
    ));
    driver.send(with_haiku()).await;
    done(&mut driver).await;
    let sessions = driver.agent.specs().len();
    let asked = server.requests().len();
    use argonauts_core::behavior_map::{Grouping, Scope};
    use argonauts_core::overview::OverviewPeriod;
    for request in [
        MapRequest::default(),
        MapRequest {
            map: Some(SettingsMap::Summaries),
            ..MapRequest::default()
        },
        MapRequest {
            map: Some(SettingsMap::Summaries),
            grouping: Grouping::Cli,
            ..MapRequest::default()
        },
        MapRequest {
            period: OverviewPeriod::Week,
            ..MapRequest::default()
        },
        MapRequest {
            scope: Scope::ThisProject,
            ..MapRequest::default()
        },
    ] {
        the_map(&mut driver, request).await;
    }
    assert_eq!(driver.agent.specs().len(), sessions, "no session started");
    assert_eq!(server.requests().len(), asked, "{:?}", server.requests());
}
