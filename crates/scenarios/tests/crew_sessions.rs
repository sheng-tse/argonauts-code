//! Reaching a working agent through the real binary with fake vendors: a mention delivered at
//! its next turn, steering delivered after its tool call, and the status tool it calls
//! mid-session.

use std::fs;
use std::io::{BufRead, BufReader, Lines, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

use argonauts_core::api::StatusSummary;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::session::{Delivery, InputPartKind, SessionEvent};
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{MessageId, SessionKey};
use argonauts_core::worker::Access;
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Key, LogLine};

const TASK: &str = "Add a Retry-After header to 429 responses";

const SOLO_CONFIG: &str = r#"main_agent = "claude-code"

[ui]
theme = "dark"
"#;

fn solo_scenario(file: &str) -> anyhow::Result<Scenario> {
    let scenario = Scenario::new(file)?;
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(config, SOLO_CONFIG)?;
    scenario.repo.write(
        "src/api/errors.ts",
        "export function tooMany() {\n  return { status: 429 };\n}\n",
    )?;
    scenario.repo.commit_all("the 429 handler")?;
    Ok(scenario)
}

/// Waits until the run's log holds an event that `found` picks, and returns it.
fn wait_for_event<T>(
    scenario: &Scenario,
    timeout: Duration,
    found: impl Fn(&RunEvent) -> Option<T>,
) -> anyhow::Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(events) = scenario.latest_run_events()
            && let Some(hit) = events.iter().find_map(&found)
        {
            return Ok(hit);
        }
        if Instant::now() >= deadline {
            bail!("no such event within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The card owner's session, once it has started building.
fn owner_session(scenario: &Scenario) -> anyhow::Result<SessionKey> {
    wait_for_event(scenario, WAIT, |event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if started.access == Access::Build =>
        {
            event.session.clone()
        }
        _ => None,
    })
}

/// What reached the fake vendor on stdin, in order.
fn stdin_lines(scenario: &Scenario) -> anyhow::Result<Vec<String>> {
    Ok(scenario
        .vendors
        .log()?
        .into_iter()
        .filter_map(|line| match line {
            LogLine::StdinLine { line, .. } => Some(line),
            _ => None,
        })
        .collect())
}

/// The message queued on the session with this text, and how it was to be delivered.
fn queued(events: &[RunEvent], session: &SessionKey, text: &str) -> Option<(MessageId, Delivery)> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Queued(queued))
            if event.session.as_ref() == Some(session) && queued.text.contains(text) =>
        {
            Some((queued.id.clone(), queued.delivery))
        }
        _ => None,
    })
}

fn delivered_via(events: &[RunEvent], session: &SessionKey, message: &MessageId) -> Vec<Delivery> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Delivered { message: id, via })
                if event.session.as_ref() == Some(session) && id == message =>
            {
                Some(*via)
            }
            _ => None,
        })
        .collect()
}

fn on_the_board(events: &[RunEvent], session: &SessionKey, message: &MessageId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
                RunEventKind::Blackboard(BlackboardEvent::Delivered { message: id, to })
                    if id == message && to == session)
        })
        .count()
}

#[test]
fn crew_mention_delivered_next_turn() -> anyhow::Result<()> {
    let mention = "Retry-After is whole seconds, not a date";
    let scenario = solo_scenario("crew/mention.toml")?;
    let mut pty = scenario.launch(&["run", "--solo", TASK])?;
    let owner = owner_session(&scenario)?;
    pty.wait_for("Reading the 429 handler", WAIT)?;

    // Typed while the owner's first turn is still going.
    pty.send(&format!("@claude-code {mention}"))?;
    pty.press(Key::Enter)?;
    scenario.shot(&pty, "crew-mention-queued")?;
    wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Session(SessionEvent::Ended(_)) if event.session.as_ref() == Some(&owner) => {
            Some(())
        }
        _ => None,
    })?;
    let events = scenario.latest_run_events()?;

    let (message, delivery) = queued(&events, &owner, mention).context("the mention is queued")?;
    assert_eq!(delivery, Delivery::NextTurn);
    assert_eq!(
        delivered_via(&events, &owner, &message),
        [Delivery::NextTurn]
    );
    assert_eq!(on_the_board(&events, &owner, &message), 1, "delivered once");
    // The owner got it on stdin only after its first turn's result, and the second turn used it.
    assert!(
        stdin_lines(&scenario)?
            .iter()
            .any(|line| line.contains(mention))
    );
    let second_turn = events.iter().any(|event| {
        event.session.as_ref() == Some(&owner)
            && matches!(&event.kind, RunEventKind::Agent(AgentEvent::Message { text, .. })
                if text.contains("whole seconds"))
    });
    assert!(second_turn);
    Ok(())
}

#[test]
fn crew_steer_delivered_after_tool() -> anyhow::Result<()> {
    let steer = "Use a sliding window instead";
    let scenario = solo_scenario("crew/steer.toml")?;
    let mut pty = scenario.launch(&["run", "--solo", TASK])?;
    let owner = owner_session(&scenario)?;
    // The call shows as `Bash(npm test)`, or folded into a group naming `$ npm test`.
    pty.wait_for(r"Bash\(npm test\)|\$ npm test", WAIT)?;

    // A solo run is one agent, so its panel row opens the owner's session directly,
    // where `m` messages it.
    pty.press(Key::Down)?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"m message it", WAIT)?;
    pty.press(Key::Char('m'))?;
    pty.send(steer)?;
    pty.press(Key::Enter)?;
    scenario.shot(&pty, "crew-steer-sent")?;
    wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Session(SessionEvent::Ended(_)) if event.session.as_ref() == Some(&owner) => {
            Some(())
        }
        _ => None,
    })?;
    let events = scenario.latest_run_events()?;

    // Claude Code takes steering on its open input, in the same session, after the tool call.
    let (message, delivery) = queued(&events, &owner, steer).context("the steer is queued")?;
    assert_eq!(delivery, Delivery::Steer);
    assert_eq!(delivered_via(&events, &owner, &message), [Delivery::Steer]);
    assert_eq!(on_the_board(&events, &owner, &message), 1);
    let tool_done = events
        .iter()
        .find(|event| {
            event.session.as_ref() == Some(&owner)
                && matches!(
                    event.kind,
                    RunEventKind::Agent(AgentEvent::ToolFinished { .. })
                )
        })
        .map(|event| event.seq)
        .context("the tool call finished")?;
    let steered = events
        .iter()
        .find(|event| {
            matches!(&event.kind, RunEventKind::Session(SessionEvent::Delivered { message: id, .. })
                if *id == message)
        })
        .map(|event| event.seq)
        .context("the steer was delivered")?;
    assert!(steered > tool_done, "after the current tool call");
    assert!(
        stdin_lines(&scenario)?
            .iter()
            .any(|line| line.contains(steer))
    );
    let sessions = events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Session(SessionEvent::Started(started))
                if started.access == Access::Build)
        })
        .count();
    assert_eq!(sessions, 1, "the same session, not a new one");
    Ok(())
}

#[test]
fn crew_status_tool_mid_session() -> anyhow::Result<()> {
    let scenario = solo_scenario("crew/status_tool.toml")?;
    let pty = scenario.launch(&["run", "--solo", TASK])?;
    let owner = owner_session(&scenario)?;
    pty.wait_for("Checking the run status", WAIT)?;

    // Injected at session start: the owner's first input carries the summary.
    let events = scenario.latest_run_events()?;
    let injected = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Input(input))
                if event.session.as_ref() == Some(&owner) =>
            {
                input
                    .parts
                    .iter()
                    .find(|part| part.kind == InputPartKind::Status)
                    .map(|part| part.text.clone())
            }
            _ => None,
        })
        .context("a status part at session start")?;
    assert!(injected.contains("task-001"), "{injected}");

    // Mid-session, the session's status file and its MCP tool both answer.
    let run = argonauts_testkit::journal::latest_run(scenario.repo.home()).context("the run")?;
    let dir = run.join("sessions").join(owner.as_str());
    let summary: StatusSummary = serde_json::from_slice(&wait_for_file(&dir.join("status.json"))?)?;
    assert!(
        summary
            .cards
            .iter()
            .any(|card| card.card.as_str() == "task-001")
    );
    assert!(!summary.goal.is_empty());
    let answer = call_status_tool(&dir.join("mcp.json"))?;
    assert!(answer.contains("task-001"), "{answer}");
    assert!(answer.contains("Retry-After"), "{answer}");
    scenario.shot(&pty, "crew-status-tool")?;
    Ok(())
}

fn wait_for_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(bytes) = fs::read(path) {
            return Ok(bytes);
        }
        if Instant::now() >= deadline {
            bail!("{} never appeared", path.display());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Starts the server the session's MCP config names, as the agent would, and calls
/// `argonauts_status` once. Returns the tool's text.
fn call_status_tool(config: &Path) -> anyhow::Result<String> {
    let config: Value = serde_json::from_slice(&wait_for_file(config)?)?;
    let server = config["mcpServers"]
        .as_object()
        .and_then(|servers| servers.values().next())
        .context("the MCP config names no server")?;
    let command = PathBuf::from(
        server["command"]
            .as_str()
            .context("the server has no command")?,
    );
    let args: Vec<&str> = server["args"]
        .as_array()
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let env: Vec<(&str, &str)> = server["env"]
        .as_object()
        .map(|env| {
            env.iter()
                .filter_map(|(name, value)| Some((name.as_str(), value.as_str()?)))
                .collect()
        })
        .unwrap_or_default();
    let mut child = Command::new(command)
        .args(args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting the status server")?;
    let mut input = child.stdin.take().context("no stdin")?;
    let mut output = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    let mut send = |frame: Value| -> anyhow::Result<()> {
        writeln!(input, "{frame}")?;
        input.flush()?;
        Ok(())
    };
    send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "crew-scenario", "version": "0" }
        }
    }))?;
    reply(&mut output, 1)?;
    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
    send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "argonauts_status", "arguments": {} }
    }))?;
    let answer = reply(&mut output, 2)?;
    let _ = child.kill();
    let _ = child.wait();
    let text = answer["result"]["content"]
        .as_array()
        .context("the tool returned no content")?
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

/// The response with this id, skipping notifications.
fn reply(output: &mut Lines<BufReader<ChildStdout>>, id: u64) -> anyhow::Result<Value> {
    for line in output.by_ref() {
        let frame: Value = serde_json::from_str(&line?)?;
        if frame["id"] == json!(id) {
            if let Some(error) = frame.get("error") {
                bail!("the status server refused: {error}");
            }
            return Ok(frame);
        }
    }
    bail!("the status server closed before answering {id}")
}
