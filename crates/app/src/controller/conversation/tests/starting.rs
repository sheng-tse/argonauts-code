//! A fresh session's start, kept from its CLI's first count, is what the Context tab shows of
//! the next one before that one counts: through the real controller loop.

use argonauts_core::agents::session_runner::{RunnerEvent, SessionControl};
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::runs::events::context::{
    ContextDetail, ContextKind, ContextUsage, StartHolds, StartedFrom, StartingCount, StartingTools,
};
use argonauts_testkit::{FakeRunner, FakeSession, Hold};

use crate::controller::base::test_support::{CliOptions, TestDriver};

/// Captain, so the turn is the main session's own, on `main`.
async fn driver_on(main: &str) -> TestDriver {
    let config = format!("crew.main = \"{main}\"\n\n[captain_or_crew]\nforce = \"captain\"\n");
    TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), &config).unwrap();
        })),
        ..CliOptions::default()
    })
    .await
}

fn init(model: &str, tools: &[&str]) -> RunnerEvent {
    RunnerEvent::Agent(AgentEvent::Init {
        session_id: Some("s-1".into()),
        model: Some(model.to_owned()),
        tools: tools.iter().map(|&tool| tool.to_owned()).collect(),
        plugins: Vec::new(),
        capabilities: Vec::new(),
        version: None,
    })
}

async fn turn(driver: &mut TestDriver, agent: &FakeRunner, session: FakeSession) {
    agent.push(session);
    driver.submit("hi").await;
    turn_ends(driver).await;
}

async fn turn_ends(driver: &mut TestDriver) {
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                        | ConversationEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Idle
                }
            )
        })
        .await;
}

async fn context_start(driver: &mut TestDriver) -> Option<StartingCount> {
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Context,
        })
        .await;
    let shown = driver
        .until(|event| matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Context { .. })))
        .await;
    let AppEvent::View { data } = shown else {
        unreachable!()
    };
    let ViewData::Context { view } = *data else {
        unreachable!()
    };
    view.main_start
}

/// The next start the screen hears of, with nothing asked.
async fn told_start(driver: &mut TestDriver) -> StartingCount {
    let told = driver
        .until(|event| matches!(event, AppEvent::MainStart { .. }))
        .await;
    let AppEvent::MainStart { start, .. } = told else {
        unreachable!()
    };
    *start
}

/// Whether the session was asked for its count by category, as Claude Code's /context counts.
fn asked_for_its_count(agent: &FakeRunner) -> bool {
    agent
        .controls()
        .iter()
        .any(|(_, control)| matches!(control, SessionControl::AskContext(ContextDetail::Summary)))
}

/// The session counts itself only when asked, as Claude Code does with nothing watching it:
/// its first reading of how full it is asks.
#[tokio::test]
async fn claude_codes_first_count_is_the_next_sessions_start_without_its_messages() {
    let usage: ContextUsage = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/claude-code/tests/data/context-usage-full.usage.json"
    )))
    .unwrap();
    let mut driver = driver_on("claude-code").await;
    let before = context_start(&mut driver).await.expect("an estimate");
    assert!(
        matches!(before.from, StartedFrom::Recording { .. }),
        "{before:?}"
    );

    let hold = Hold::new();
    let session = FakeSession::completed("Hi.")
        .with_event(init("claude-opus-5-5", &["Read"]))
        .with_event(RunnerEvent::Agent(AgentEvent::ContextFill {
            used: usage.total_tokens,
            size: Some(usage.window),
            sent: Some(1),
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ContextBreakdown {
            usage: Box::new(usage.clone()),
        }))
        .held_after(3, &hold);
    let agent = driver.agent.clone();
    agent.push(session);
    driver.submit("hi").await;
    while !asked_for_its_count(&agent) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    hold.release();
    turn_ends(&mut driver).await;
    let start = context_start(&mut driver).await.expect("the kept start");
    assert_eq!(start.from, StartedFrom::LastSession);
    let names: Vec<&str> = start.parts.iter().map(|part| part.name.as_str()).collect();
    let counted: Vec<&str> = usage
        .of_kind(ContextKind::Used)
        .map(|part| part.name.as_str())
        .filter(|name| *name != "Messages")
        .collect();
    assert_eq!(names, counted);
    assert_eq!((start.window, start.buffer), (Some(1_000_000), 33_000));
    assert_eq!(start.tools, None);
    driver.exit().await;
}

/// The screen has the main session's start before the Context tab first opens, and hears of
/// the one the session's first count keeps without asking.
#[tokio::test]
async fn the_screen_hears_the_start_before_the_tab_opens_and_once_it_is_counted() {
    let usage: ContextUsage = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/claude-code/tests/data/context-usage-full.usage.json"
    )))
    .unwrap();
    let mut driver = driver_on("claude-code").await;
    let before = told_start(&mut driver).await;
    assert!(
        matches!(before.from, StartedFrom::Recording { .. }),
        "{before:?}"
    );

    let hold = Hold::new();
    let session = FakeSession::completed("Hi.")
        .with_event(init("claude-opus-5-5", &["Read"]))
        .with_event(RunnerEvent::Agent(AgentEvent::ContextFill {
            used: usage.total_tokens,
            size: Some(usage.window),
            sent: Some(1),
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ContextBreakdown {
            usage: Box::new(usage.clone()),
        }))
        .held_after(3, &hold);
    let agent = driver.agent.clone();
    agent.push(session);
    driver.submit("hi").await;
    while !asked_for_its_count(&agent) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    hold.release();
    let kept = told_start(&mut driver).await;
    assert_eq!(kept.from, StartedFrom::LastSession);
    assert_eq!(Some(kept), context_start(&mut driver).await);
    driver.exit().await;
}

/// Codex works out how full its session is from its totals, so the screen hears its start as
/// an estimate, and Claude Code's as its own count.
#[tokio::test]
async fn the_screen_hears_whether_the_main_cli_estimates_how_full_it_is() {
    for (main, estimates) in [("codex/gpt-6-luna", true), ("claude-code", false)] {
        let mut driver = driver_on(main).await;
        let told = driver
            .until(|event| matches!(event, AppEvent::MainStart { .. }))
            .await;
        let AppEvent::MainStart { estimated, .. } = told else {
            unreachable!()
        };
        assert_eq!(estimated, estimates, "{main}");
        driver.exit().await;
    }
}

/// Left on its CLI's default or named by an alias, with no list read, the main session's start
/// has the window of the model it runs, from the shipped catalog.
#[tokio::test]
async fn a_main_on_its_clis_default_has_its_window_before_any_list() {
    for (main, window) in [
        ("claude-code", 1_000_000),
        ("claude-code/opus", 1_000_000),
        ("codex", 872_000),
        ("antigravity-cli", 1_048_576),
    ] {
        let mut driver = driver_on(main).await;
        let told = driver
            .until(|event| matches!(event, AppEvent::MainStart { .. }))
            .await;
        let AppEvent::MainStart { start, .. } = told else {
            unreachable!()
        };
        assert_eq!(start.window, Some(window), "{main}");
        driver.exit().await;
    }
}

/// On a very first run, Argonauts Code's own tools and the lines about them go where Claude
/// Code's own /context counts them, beside what was recorded.
#[tokio::test]
async fn claude_codes_first_run_counts_our_tools_and_their_lines() {
    let mut driver = driver_on("claude-code").await;
    let alone = context_start(&mut driver).await.expect("an estimate");
    driver.directory.taking_tools("claude-code");
    let with_tools = context_start(&mut driver).await.expect("an estimate");
    let tokens = |start: &StartingCount, name: &str| {
        start
            .parts
            .iter()
            .find(|part| part.name == name)
            .map(|part| part.tokens)
    };
    assert_eq!(tokens(&alone, "MCP tools"), None);
    assert_eq!(tokens(&alone, "MCP server instructions"), None);
    assert!(tokens(&with_tools, "MCP tools").is_some_and(|tools| tools > 100));
    assert!(tokens(&with_tools, "MCP server instructions").is_some_and(|lines| lines > 0));
    for own in ["System prompt", "System tools"] {
        assert_eq!(tokens(&with_tools, own), tokens(&alone, own), "{own}");
    }
    driver.exit().await;
}

#[tokio::test]
async fn agys_first_reading_is_the_next_sessions_start_with_the_tools_it_named() {
    let mut driver = driver_on("antigravity-cli/gemini-3.8-flash-low").await;
    let agy = driver
        .directory
        .add(FakeRunner::cli("antigravity-cli", "google"));
    let session = FakeSession::completed("Hi.")
        .with_event(init(
            "gemini-3.8-flash-low",
            &["view_file", "list_dir", "run_command"],
        ))
        .with_event(RunnerEvent::Agent(AgentEvent::Usage {
            input_tokens: 9_000,
            output_tokens: 40,
            cached_tokens: 0,
            cost_usd: None,
            cumulative: false,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ContextFill {
            used: 9_040,
            size: Some(1_000_000),
            sent: Some(10),
        }));
    turn(&mut driver, &agy, session).await;
    let start = context_start(&mut driver).await.expect("the kept start");
    assert_eq!(start.from, StartedFrom::LastSession, "{start:?}");
    let own = &start.parts[0];
    assert_eq!(
        (own.holds, own.tokens),
        (Some(StartHolds::OwnPromptAndTools), 8_990)
    );
    assert_eq!(
        start.tools,
        Some(StartingTools {
            named: 3,
            read_only: Some(5),
        })
    );
    assert_eq!(start.window, Some(1_000_000));
    driver.exit().await;
}
