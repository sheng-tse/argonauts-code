//! Direct solo through the real controller loop, with fake CLIs: the solo preset and `/solo`
//! send your words to the main session in your checkout, it ends with its own answer, it asks
//! like its CLI in supervised mode and not in auto, it hands off when it runs out, `/rewind`
//! puts files back, and checked solo still runs as a run.

use std::path::Path;
use std::time::Duration;

use argonauts_core::app::{AnswerTarget, AppEvent, ChatEvent, ChoiceKind, TurnReply, UiCommand};
use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::{InstructionOutcome, RunMode};
use argonauts_core::events::session::{EndReason, InputPartKind};
use argonauts_core::input::SlashCommand;
use argonauts_core::worker::{Access, SessionSpec, WorkerEvent};
use argonauts_core::{Asks, sha256_hex};
use argonauts_testkit::{FakeSession, FakeWorker};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::controller::test_support::{Harness, HarnessOptions, WAIT};

const TASK: &str = "The tests in tests/test_money.py fail. Fix invoice/money.py so every test \
                    passes, without changing the tests.";
const ANSWER: &str = "Fixed both functions in invoice/money.py. All 6 tests pass.";

/// The solo preset, with whatever else `config` adds to your global settings.
fn solo_preset(config: &'static str) -> HarnessOptions {
    HarnessOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            repo.write("invoice/money.py", "sign = 1\n").unwrap();
            repo.commit_all("money").unwrap();
            std::fs::write(
                repo.global_config(),
                format!("{config}\n[solo_or_crew]\nforce = \"solo\"\n"),
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    }
}

const SUPERVISED: &str = "[approvals.goal]\nmode = \"supervised\"\n";

/// A session that edits `invoice/money.py` in the folder it was given, runs the tests, and
/// answers, slowly enough for a test to ask a question meanwhile.
fn fixing(delay: Duration) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static {
    move |spec: &SessionSpec| {
        std::fs::write(spec.workdir.join("invoice/money.py"), "negative = True\n").unwrap();
        FakeSession::completed(ANSWER)
            .with_event(tool("t1", "Edit", json!({"file_path": "invoice/money.py"})))
            .with_event(WorkerEvent::Agent(AgentEvent::ToolFinished {
                id: "t1".into(),
                ok: true,
                output: Some("Added 1 line, removed 1 line".into()),
                parent: None,
            }))
            .with_delay(delay)
    }
}

fn tool(id: &str, name: &str, input: Value) -> WorkerEvent {
    WorkerEvent::Agent(AgentEvent::ToolStarted {
        id: id.into(),
        name: name.into(),
        input,
        parent: None,
    })
}

fn chat(event: &AppEvent) -> Option<&ChatEvent> {
    match event {
        AppEvent::Chat { event } => Some(event),
        _ => None,
    }
}

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        chat(event),
        Some(ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. })
    )
}

fn choose(id: argonauts_core::names::ChoiceId, value: &str) -> UiCommand {
    UiCommand::Answer {
        to: AnswerTarget::Choice { id },
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        text: None,
        evidence_sha256: None,
        cards: Vec::new(),
        remember: false,
    }
}

fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap()
}

/// Asks the turn's listener as the session's MCP server does, and waits for the answer.
async fn ask(spec: &SessionSpec, ask: Ask) -> AskReply {
    let asking = spec.asking.clone().expect("the turn listens for questions");
    let mut stream = TcpStream::connect(&asking.endpoint).await.unwrap();
    let line = AskLine {
        token: asking.token,
        ask,
    };
    let text = format!("{}\n", serde_json::to_string(&line).unwrap());
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap()
}

async fn first_spec(harness: &Harness) -> SessionSpec {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Some(spec) = harness.agent.specs().into_iter().next() {
            return spec;
        }
        assert!(tokio::time::Instant::now() < deadline, "no session started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A direct turn in the solo preset: the message goes to Claude Code in your checkout as
/// you typed it, the file changes there, and the turn ends with the agent's own answer. No run,
/// worktree, card, report, recap, or recipe.
#[tokio::test]
async fn a_direct_turn_edits_your_checkout_and_ends_with_the_agents_answer() {
    let mut harness = Harness::start_with(solo_preset("")).await;
    let root = harness.repo.root().to_owned();
    harness.agent.respond(fixing(Duration::ZERO));
    let worker = harness.agent.clone();
    let text = format!("{TASK} See @README.md");
    harness.submit(&text).await;
    let ended = harness.until(turn_ended).await;
    let money = read(&root, "invoice/money.py");
    let events = harness.exit().await;

    assert!(matches!(
        chat(&ended),
        Some(ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. }) if markdown == ANSWER
    ));
    let spec = worker.specs().remove(0);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.workdir, root);
    assert!(spec.json_schema.is_none());
    assert_eq!(spec.goal_sha256, Some(sha256_hex(text.as_bytes())));
    // Your words as typed, then the file you mentioned, and nothing of Argonauts' own.
    let kinds: Vec<_> = spec.prompt.iter().map(|part| part.kind).collect();
    assert_eq!(kinds, [InputPartKind::Message, InputPartKind::File]);
    assert_eq!(spec.prompt[0].text, text);
    assert_eq!(money, "negative = True\n");

    let started = events.iter().find_map(|event| match chat(event) {
        Some(ChatEvent::TurnStarted { read_only, .. }) => Some(*read_only),
        _ => None,
    });
    assert_eq!(started, Some(false), "a direct turn says it may write");
    assert!(harness_starts_nothing(&events));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Recap { .. })),
        "no recap restates the answer"
    );
}

fn harness_starts_nothing(events: &[AppEvent]) -> bool {
    !events.iter().any(|event| {
        matches!(event, AppEvent::Run { .. })
            || matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::StartWork)
    })
}

/// `/solo` is direct too, without the preset, and checked solo still starts a run.
#[tokio::test]
async fn slash_solo_is_direct_and_checked_solo_is_a_run() {
    let solo = |config: &'static str| HarnessOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), config).unwrap();
        })),
        ..HarnessOptions::default()
    };
    let slash = UiCommand::Slash {
        command: SlashCommand::Solo {
            task: "fix the typo in the README".to_owned(),
        },
    };

    let mut harness = Harness::start_with(solo("")).await;
    harness
        .agent
        .push(FakeSession::completed("Fixed the typo."));
    harness.send(slash.clone()).await;
    harness.until(turn_ended).await;
    let conductor = harness.conductor.clone();
    let specs = harness.agent.specs();
    harness.exit().await;
    assert_eq!(specs[0].access, Access::Direct);
    assert!(conductor.starts().is_empty());

    for checked in ["[solo]\nkind = \"checked\"\n", "[solo]\nreview = true\n"] {
        let harness = Harness::start_with(solo(checked)).await;
        harness.send(slash.clone()).await;
        let conductor = harness.conductor.clone();
        let deadline = tokio::time::Instant::now() + WAIT;
        while conductor.starts().is_empty() {
            assert!(tokio::time::Instant::now() < deadline, "{checked}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let start = conductor.starts().remove(0);
        assert_eq!((start.mode, start.forced), (RunMode::Solo, true));
        assert!(harness.agent.specs().is_empty());
        harness.exit().await;
    }
}

/// Supervised mode asks what Claude Code asks: an edit becomes a card, Yes lets it through,
/// "allow edits" answers later edits itself for this conversation, "don't ask again" saves the
/// command's pattern at "This project, just me", and No stops the turn.
#[tokio::test]
async fn supervised_asks_like_the_cli_and_remembers_what_you_allow() {
    let mut harness = Harness::start_with(solo_preset(SUPERVISED)).await;
    harness
        .agent
        .push(FakeSession::completed(ANSWER).with_delay(Duration::from_secs(2)));
    harness.submit(TASK).await;
    let spec = first_spec(&harness).await;
    let asking = spec.asking.clone().expect("supervised asks");
    assert_eq!(asking.asks, Asks::LikeCli);

    let edit = Ask::Permission {
        tool_name: "Edit".into(),
        input: json!({"file_path": "invoice/money.py", "old_string": "sign = 1", "new_string": "x"}),
        tool_use_id: Some("t1".into()),
    };
    let answered = tokio::spawn({
        let spec = spec.clone();
        let edit = edit.clone();
        async move { ask(&spec, edit).await }
    });
    let card = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Permission))
        .await;
    let AppEvent::Choice { choice } = card else {
        unreachable!()
    };
    assert_eq!(choice.title, "Make this edit to invoice/money.py?");
    assert_eq!(choice.body, ["- sign = 1", "+ x"]);
    harness.send(choose(choice.id, "always")).await;
    let reply = answered.await.unwrap();
    assert!(reply.text.contains("\"allow\""), "{reply:?}");

    // Edits are allowed for the rest of the conversation: no card for this one.
    let again = ask(&spec, edit).await;
    assert!(again.text.contains("\"allow\""));

    let command = Ask::Permission {
        tool_name: "Bash".into(),
        input: json!({"command": "python -m pytest -q"}),
        tool_use_id: None,
    };
    let answered = tokio::spawn({
        let spec = spec.clone();
        async move { ask(&spec, command).await }
    });
    let card = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Permission))
        .await;
    let AppEvent::Choice { choice } = card else {
        unreachable!()
    };
    assert_eq!(choice.title, "Run this command?");
    assert!(
        choice.options[1].label.contains("python -m pytest"),
        "{:?}",
        choice.options
    );
    harness.send(choose(choice.id, "always")).await;
    assert!(answered.await.unwrap().text.contains("\"allow\""));
    let local = harness.repo.root().join(".argonauts/config.local.toml");
    let saved = std::fs::read_to_string(&local).unwrap();
    assert!(saved.contains("python -m pytest *"), "{saved}");

    // No denies the call and stops the turn, as in Claude Code.
    let fetch = Ask::Permission {
        tool_name: "WebFetch".into(),
        input: json!({"url": "https://example.com"}),
        tool_use_id: None,
    };
    let answered = tokio::spawn({
        let spec = spec.clone();
        async move { ask(&spec, fetch).await }
    });
    let card = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Permission))
        .await;
    let AppEvent::Choice { choice } = card else {
        unreachable!()
    };
    assert_eq!(choice.title, "Let Claude Code use WebFetch?");
    harness
        .send(UiCommand::SetAside {
            to: AnswerTarget::Choice { id: choice.id },
        })
        .await;
    assert!(answered.await.unwrap().text.contains("\"deny\""));
    harness
        .until(|event| matches!(chat(event), Some(ChatEvent::TurnCancelled { .. })))
        .await;
    harness.exit().await;
}

/// Auto mode asks nothing: the session gets no approval tool, and no card comes.
#[tokio::test]
async fn auto_mode_asks_nothing() {
    let mut harness = Harness::start_with(solo_preset("")).await;
    harness.agent.push(FakeSession::completed(ANSWER));
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    let spec = harness.agent.specs().remove(0);
    let events = harness.exit().await;
    assert!(
        spec.asking
            .as_ref()
            .is_none_or(|asking| asking.asks == Asks::Never),
        "{:?}",
        spec.asking
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
    );
}

/// Full auto asks nothing either: with its switch on, the session gets no approval tool though
/// `solo.asks` would have it ask as its CLI does, and no card comes.
#[tokio::test]
async fn autopilot_asks_nothing() {
    let mut harness = Harness::start_with(solo_preset(
        "[approvals]\nautopilot = true\n[solo]\nasks = \"like_cli\"\n",
    ))
    .await;
    harness.agent.push(FakeSession::completed(ANSWER));
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    let spec = harness.agent.specs().remove(0);
    let events = harness.exit().await;
    assert!(
        spec.asking
            .as_ref()
            .is_none_or(|asking| asking.asks == Asks::Never),
        "{:?}",
        spec.asking
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
    );
}

/// Running out mid-turn: Codex carries on the same request in the same folder, told what
/// changed, under a `↷` line naming both.
#[tokio::test]
async fn running_out_hands_the_turn_to_the_next_agent() {
    let mut options = solo_preset("");
    options.flags = vec![r#"fallback.main.order=["codex"]"#.to_owned()];
    let mut harness = Harness::start_with(options).await;
    let codex = harness
        .directory
        .add(FakeWorker::harness("codex", "openai").then(FakeSession::completed(ANSWER)));
    harness.agent.respond(|spec: &SessionSpec| {
        std::fs::write(spec.workdir.join("invoice/money.py"), "half\n").unwrap();
        FakeSession::ended(EndReason::UsageLimit { resets_at: None })
    });
    harness.submit(TASK).await;
    let ended = harness.until(turn_ended).await;
    let events = harness.exit().await;
    assert!(matches!(
        chat(&ended),
        Some(ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. }) if markdown == ANSWER
    ));
    let handed = events.iter().find_map(|event| match chat(event) {
        Some(ChatEvent::HandedOff { from, to, told, .. }) => Some((from, to, told)),
        _ => None,
    });
    let (from, to, told) = handed.expect("a fallback line");
    assert_eq!(
        (from.worker.as_str(), to.worker.as_str()),
        ("claude-code", "codex")
    );
    assert!(told.contains("invoice/money.py changed"), "{told}");
    let spec = codex.specs().remove(0);
    assert_eq!(spec.access, Access::Direct);
    let note: String = spec
        .prompt
        .iter()
        .filter(|part| part.kind == InputPartKind::Handoff)
        .map(|part| part.text.clone())
        .collect();
    assert!(note.contains("invoice/money.py"), "{note}");
    let started = events
        .iter()
        .filter(|event| matches!(chat(event), Some(ChatEvent::TurnStarted { .. })))
        .count();
    assert_eq!(started, 1, "the turn goes on, it doesn't start again");
}

/// With `handoff.mode = "wait"` the turn ends the way the CLI ends it, with a line saying what
/// it changed.
#[tokio::test]
async fn with_handoff_off_the_turn_ends_and_says_what_changed() {
    let mut options = solo_preset("[fallback]\nmode = \"wait\"\n");
    options.flags = vec![r#"fallback.main.order=["codex"]"#.to_owned()];
    let mut harness = Harness::start_with(options).await;
    let codex = harness
        .directory
        .add(FakeWorker::harness("codex", "openai"));
    harness.agent.respond(|spec: &SessionSpec| {
        std::fs::write(spec.workdir.join("invoice/money.py"), "half\n").unwrap();
        FakeSession::ended(EndReason::UsageLimit { resets_at: None })
    });
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    let stopped = harness
        .until(
            |event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Stopped")),
        )
        .await;
    harness.exit().await;
    assert!(codex.specs().is_empty());
    let AppEvent::Notice { text, .. } = stopped else {
        unreachable!()
    };
    assert_eq!(
        text,
        "Stopped · invoice/money.py (+1 -1) changed · /rewind puts it back"
    );
}

/// `/rewind` lists the messages whose turns changed files and puts back what you pick.
#[tokio::test]
async fn rewind_puts_back_a_direct_turns_files() {
    let mut harness = Harness::start_with(solo_preset("")).await;
    harness.agent.respond(fixing(Duration::ZERO));
    let root = harness.repo.root().to_owned();
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    assert_eq!(read(&root, "invoice/money.py"), "negative = True\n");

    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Rewind { card: None },
        })
        .await;
    let card = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RewindTurn))
        .await;
    let AppEvent::Choice { choice } = card else {
        unreachable!()
    };
    assert_eq!(choice.options.len(), 1);
    let AnswerAction::Choose { value } = &choice.options[0].action else {
        panic!("{:?}", choice.options[0])
    };
    harness.send(choose(choice.id, value)).await;
    let closed = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await;
    assert!(
        matches!(&closed, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Put back invoice/money.py"),
        "{closed:?}"
    );
    assert_eq!(read(&root, "invoice/money.py"), "sign = 1\n");
    harness.exit().await;
}

/// agy can't ask in a headless run: the commands it was refused come after its answer, and
/// "Yes, and continue" resumes it for one turn that runs commands without asking.
#[tokio::test]
async fn agys_refused_commands_are_offered_after_its_answer() {
    let mut harness = Harness::start_with(solo_preset(
        "main_agent = \"antigravity-cli\"\n[approvals.goal]\nmode = \"supervised\"\n",
    ))
    .await;
    let refused =
        FakeSession::completed("I couldn't run the tests: running a command needs approval.")
            .with_event(tool(
                "s2",
                "run_command",
                json!({"CommandLine": "python -m pytest -q"}),
            ))
            .with_event(WorkerEvent::Agent(AgentEvent::ToolFinished {
                id: "s2".into(),
                ok: false,
                output: Some(
                    "permission check failed for unsandboxed \"python -m pytest -q\"".into(),
                ),
                parent: None,
            }))
            .with_vendor_session("conv-1");
    let agy = harness.directory.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .then(refused)
            .then(FakeSession::completed("6 passed.")),
    );
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    let card = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RefusedCommands))
        .await;
    let AppEvent::Choice { choice } = card else {
        unreachable!()
    };
    assert_eq!(choice.title, "Let Antigravity CLI run what it was refused?");
    assert_eq!(choice.body, ["python -m pytest -q"]);
    assert_eq!(
        agy.specs()[0].asking.as_ref().map(|a| a.asks),
        Some(Asks::LikeCli)
    );
    harness.send(choose(choice.id, "continue")).await;
    harness.until(turn_ended).await;
    harness.exit().await;
    let resumed = agy.specs().remove(1);
    assert_eq!(resumed.access, Access::Direct);
    assert_eq!(resumed.resume.as_deref(), Some("conv-1"));
    assert!(
        resumed
            .asking
            .as_ref()
            .is_none_or(|a| a.asks == Asks::Never)
    );
    assert_eq!(
        resumed.prompt[0].text,
        "You may run the commands you were refused."
    );
}

/// One of the main session's tools, called by the session of the turn in flight.
fn main_tool(name: &str, arguments: Value) -> Ask {
    Ask::Tool {
        name: name.to_owned(),
        arguments,
    }
}

const START_WORKFLOW: &str = "argonauts_start_workflow";

/// `argonauts_start_workflow` in solo: refused when your message asked for no run, and
/// otherwise a run starts for your message as you typed it, with its checks, so the run
/// says what was followed. A word picked out of your message starts nothing, and the agent's
/// own restatement only ever follows your words, marked as its own.
#[tokio::test]
async fn in_solo_the_main_session_starts_a_run_only_when_you_asked_for_one() {
    let harness = Harness::start_with(solo_preset("")).await;
    harness.directory.taking_tools("claude-code");
    harness.directory.add(FakeWorker::harness("codex", "gpt"));
    harness
        .directory
        .add(FakeWorker::harness("antigravity-cli", "gemini"));
    harness
        .agent
        .push(FakeSession::completed(ANSWER).with_delay(Duration::from_secs(3)));
    let typed = "Fix the rounding in invoice/money.py. Have grok build it and gemini review it, \
                 and merge without asking me.";
    harness.submit(typed).await;
    let spec = first_spec(&harness).await;
    let asking = spec.asking.as_ref().expect("the turn listens");
    assert_eq!(
        asking.tools,
        [
            START_WORKFLOW,
            "argonauts_set_goal",
            "argonauts_setup",
            "argonauts_guide",
            "argonauts_lookup"
        ]
    );
    let standing = asking.standing.as_deref().expect("standing lines");
    assert!(
        standing.contains("`argonauts_start_workflow`"),
        "{standing}"
    );
    // Your message goes as you typed it, and the setup in use follows it once.
    assert_eq!(spec.prompt[0].text, typed);
    let setup = &spec.prompt.last().expect("the setup note").text;
    assert!(setup.contains("Argonauts setup: solo is in use"), "{setup}");

    // Words you didn't write, and a lone word picked out of your message, start nothing.
    for how_to_run in [
        vec![json!({"says": "use the whole crew", "kind": "crew"})],
        vec![json!({"says": "the", "kind": "mode", "value": "crew"})],
    ] {
        let unasked = ask(
            &spec,
            main_tool(
                START_WORKFLOW,
                json!({
                    "request": "Delete tests/test_money.py and rewrite the module from scratch",
                    "how_to_run": how_to_run,
                }),
            ),
        )
        .await;
        assert_eq!(
            unasked.text,
            "The user didn't ask for a workflow, so do it yourself."
        );
    }
    assert!(harness.conductor.starts().is_empty());

    let asked = ask(
        &spec,
        main_tool(
            START_WORKFLOW,
            json!({
                "request": "Fix the rounding in invoice/money.py, where the sign comes late",
                "how_to_run": [
                    {"says": "have grok build it", "kind": "build", "who": "grok"},
                    {"says": "gemini review it", "kind": "review", "who": "gemini"},
                    {"says": "merge without asking me", "kind": "merge"},
                ],
            }),
        ),
    )
    .await;
    assert!(
        asked
            .text
            .starts_with("A solo run started on this, as the user asked."),
        "{asked:?}"
    );
    let conductor = harness.conductor.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    while conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let start = conductor.starts().remove(0);
    // Your message as you typed it comes first, and the agent's account is marked as its own.
    assert_eq!(
        start.request,
        format!(
            "{typed}\n\nFrom the conversation, as the main agent put it: Fix the rounding in \
             invoice/money.py, where the sign comes late"
        )
    );
    assert_eq!(start.mode, RunMode::Solo);
    assert!(start.triage.as_ref().is_some_and(|t| t.as_you_asked()));
    let outcome = |says: &str| {
        start
            .instructions
            .iter()
            .find(|instruction| instruction.says == says)
            .map(|instruction| instruction.outcome.clone())
            .unwrap_or_else(|| panic!("no {says:?} in {:#?}", start.instructions))
    };
    assert!(matches!(
        outcome("gemini review it"),
        InstructionOutcome::Followed { ref said, .. } if said == "Antigravity CLI reviews"
    ));
    assert!(matches!(
        outcome("have grok build it"),
        InstructionOutcome::NotFollowed { ref why } if why.contains("isn't installed")
            || why.starts_with("no agent here is called grok")
    ));
    assert!(matches!(
        outcome("merge without asking me"),
        InstructionOutcome::NotFollowed { ref why } if why.contains("is a safety setting")
    ));
    assert!(
        start
            .request_settings
            .iter()
            .any(|setting| setting == "roles.reviewer.workers=[\"antigravity-cli\"]"),
        "{:?}",
        start.request_settings
    );
    assert!(
        start
            .request_settings
            .iter()
            .any(|setting| setting == "solo.review=true")
    );
    // One run a turn.
    let again = ask(
        &spec,
        main_tool(
            START_WORKFLOW,
            json!({
                "request": "",
                "how_to_run": [{"says": "gemini review it", "kind": "review", "who": "gemini"}],
            }),
        ),
    )
    .await;
    assert!(
        again.text.starts_with("A workflow already started"),
        "{again:?}"
    );
    // A tool nothing registers is an error the session can read, and starts nothing.
    let unknown = ask(&spec, main_tool("argonauts_nothing", json!({}))).await;
    assert!(unknown.is_error, "{unknown:?}");
    assert_eq!(harness.conductor.starts().len(), 1);
    harness.exit().await;
}

/// Inside a crew preset, a solo the triage rules pick is checked by default. With
/// `triage.solo_style = "direct"` the session that planned it goes ahead in your checkout.
#[tokio::test]
async fn a_triage_pick_runs_direct_only_when_its_style_says_so() {
    let small = json!({
        "task_type": "bugfix",
        "summary": "Fix the rounding",
        "files_likely": ["invoice/money.py"],
        "file_count": 1,
        "risk_labels": [],
        "tests_exist": true,
        "unknowns": []
    });
    let change = || {
        FakeSession::structured(json!({
            "kind": "change",
            "answer_markdown": "The sign is applied too late.",
            "estimate": small.clone()
        }))
        .with_vendor_session("estimate-1")
    };
    let styled = |config: &'static str| HarnessOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), config).unwrap();
        })),
        ..HarnessOptions::default()
    };

    let mut harness = Harness::start_with(styled("[solo_or_crew]\nsolo_kind = \"direct\"\n")).await;
    harness.agent.push(change());
    harness.agent.push(FakeSession::completed(ANSWER));
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    harness.until(turn_ended).await;
    let conductor = harness.conductor.clone();
    let specs = harness.agent.specs();
    let seen = harness.exit().await;
    assert!(conductor.starts().is_empty());
    assert_eq!(specs[1].access, Access::Direct);
    assert_eq!(specs[1].resume.as_deref(), Some("estimate-1"));
    assert_eq!(specs[1].prompt[0].text, "Go ahead.");
    // The turn is your request, shown once: the one going ahead continues the estimate's.
    let started: Vec<(String, bool)> = seen
        .iter()
        .filter_map(|event| match chat(event) {
            Some(ChatEvent::TurnStarted {
                text, continues, ..
            }) => Some((text.clone(), *continues)),
            _ => None,
        })
        .collect();
    assert_eq!(started, [(TASK.to_owned(), false), (TASK.to_owned(), true)]);
    // The estimate's reply carries the Triage line of a solo in your checkout.
    assert!(seen.iter().any(|event| matches!(
        chat(event),
        Some(ChatEvent::TurnFinished {
            reply: TurnReply::Change { triage, .. },
            ..
        }) if triage.solo == Some(argonauts_core::settings::protocol::SoloStyle::Direct)
    )));

    let mut harness = Harness::start().await;
    harness.agent.push(change());
    harness.submit(TASK).await;
    harness.until(turn_ended).await;
    let conductor = harness.conductor.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    while conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(conductor.starts()[0].mode, RunMode::Solo);
    assert_eq!(harness.agent.specs().len(), 1);
    harness.exit().await;
}

/// In a direct conversation a CLI command works in your checkout, as `/init` does in Claude
/// Code, with the command alone as its prompt.
#[tokio::test]
async fn a_vendor_command_in_a_direct_conversation_works_in_your_checkout() {
    use argonauts_core::vendor_commands::{CommandKind, CommandSource, VendorCommand};

    let (mut parts, _) = crate::controller::test_support::TestCtx::new(solo_preset(""));
    parts.agent.push(FakeSession::completed("Wrote CLAUDE.md."));
    let mut chat = super::ChatState::new(&parts.ctx).unwrap();
    let init = VendorCommand {
        name: "init".into(),
        aliases: Vec::new(),
        args: String::new(),
        help: "Write CLAUDE.md".into(),
        kind: CommandKind::Vendor,
        source: CommandSource::BuiltIn,
    };
    super::vendor_command(&mut parts.ctx, &mut chat, &init, "", "/init".into()).unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    while parts.agent.specs().is_empty() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let spec = parts.agent.specs().remove(0);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.prompt.len(), 1);
    assert_eq!(spec.prompt[0].text, "/init");
}

/// `solo.asks = "every_change"` with a CLI that can't ask before each edit runs checked, and
/// says why.
#[tokio::test]
async fn every_change_with_codex_runs_in_a_checked_copy() {
    let mut harness = Harness::start_with(solo_preset(
        "main_agent = \"codex\"\n[approvals.goal]\nmode = \"supervised\"\n[solo]\nasks = \"every_change\"\n",
    ))
    .await;
    let codex = harness
        .directory
        .add(FakeWorker::harness("codex", "openai"));
    harness.submit(TASK).await;
    let notice = harness
        .until(
            |event| matches!(event, AppEvent::Notice { text, .. } if text.contains("checked copy")),
        )
        .await;
    let conductor = harness.conductor.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    while conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    harness.exit().await;
    assert!(matches!(
        notice,
        AppEvent::Notice { text, .. }
            if text == "Codex can't ask before each edit in a headless run, so this runs in a checked copy."
    ));
    assert_eq!(conductor.starts()[0].mode, RunMode::Solo);
    assert!(codex.specs().is_empty());
}

/// Esc stops a direct turn: what it changed so far stays, and a line names it and `/rewind`.
#[tokio::test]
async fn esc_keeps_the_edits_and_says_how_to_put_them_back() {
    let mut harness = Harness::start_with(solo_preset("")).await;
    harness.agent.respond(|spec: &SessionSpec| {
        std::fs::write(spec.workdir.join("invoice/money.py"), "half\n").unwrap();
        FakeSession::until_stopped(vec![tool(
            "t1",
            "Edit",
            json!({"file_path": "invoice/money.py"}),
        )])
    });
    harness.submit(TASK).await;
    first_spec(&harness).await;
    harness
        .until(|event| matches!(chat(event), Some(ChatEvent::Agent { .. })))
        .await;
    harness.send(UiCommand::CancelTurn).await;
    harness
        .until(|event| matches!(chat(event), Some(ChatEvent::TurnCancelled { .. })))
        .await;
    let line = harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("/rewind")))
        .await;
    let root = harness.repo.root().to_owned();
    assert_eq!(read(&root, "invoice/money.py"), "half\n");
    harness.exit().await;
    assert!(matches!(
        line,
        AppEvent::Notice { text, .. } if text == "invoice/money.py (+1 -1) changed · /rewind puts it back"
    ));
}

/// Plan first in the solo preset: `/plan <task>`, and a message while plan first is on, each run
/// a read-only turn that ends with its plan. Nothing changes until "Go ahead with this plan?" is
/// answered Yes, which resumes the session that planned, in your checkout.
#[tokio::test]
async fn plan_first_in_direct_solo_changes_nothing_until_you_go_ahead() {
    for by_command in [true, false] {
        let mut harness = Harness::start_with(solo_preset("")).await;
        let root = harness.repo.root().to_owned();
        harness.agent.respond(|spec: &SessionSpec| {
            if spec.access == Access::Direct {
                std::fs::write(spec.workdir.join("invoice/money.py"), "negative = True\n").unwrap();
                FakeSession::completed(ANSWER)
            } else {
                FakeSession::completed("1. Read the sign first.\n2. Apply it once.")
                    .with_vendor_session("plan-1")
            }
        });
        if by_command {
            harness
                .send(UiCommand::Slash {
                    command: SlashCommand::Plan {
                        task: Some(TASK.into()),
                    },
                })
                .await;
        } else {
            harness
                .send(UiCommand::Slash {
                    command: SlashCommand::Plan { task: None },
                })
                .await;
            harness.submit(TASK).await;
        }
        let card = harness
            .until(|event| {
                matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::GoAhead)
            })
            .await;
        let AppEvent::Choice { choice } = card else {
            unreachable!()
        };
        assert_eq!(choice.title, "Go ahead with this plan?");
        let specs = harness.agent.specs();
        assert_eq!(specs.len(), 1, "{by_command}");
        assert_eq!(specs[0].access, Access::ReadOnly);
        assert!(
            specs[0]
                .prompt
                .iter()
                .any(|part| part.text.contains("Plan first is on")),
            "{:?}",
            specs[0].prompt
        );
        assert_eq!(read(&root, "invoice/money.py"), "sign = 1\n");
        assert!(harness.conductor.starts().is_empty());

        harness.send(choose(choice.id, "yes")).await;
        harness
            .until(|event| {
                matches!(
                    chat(event),
                    Some(ChatEvent::TurnFinished {
                        reply: TurnReply::Answer { markdown },
                        ..
                    }) if markdown == ANSWER
                )
            })
            .await;
        let specs = harness.agent.specs();
        assert_eq!(specs[1].access, Access::Direct);
        assert_eq!(specs[1].resume.as_deref(), Some("plan-1"));
        assert_eq!(specs[1].prompt[0].text, "Go ahead with this plan.");
        assert_eq!(read(&root, "invoice/money.py"), "negative = True\n");
        assert!(harness.conductor.starts().is_empty());
        harness.exit().await;
    }
}

/// The handoff card: with `handoff.mode = "ask"`, and in auto mode when the only one left is a
/// paid API, a direct turn that runs out asks who carries it on, from the shortlist a run's card
/// would get. Your pick carries it on; Stop ends it with a line saying what changed.
#[tokio::test]
async fn running_out_asks_who_carries_it_on_when_it_should() {
    use std::sync::Arc;

    use argonauts_core::registry::{ModelEntry, ModelRegistry};
    use argonauts_testkit::services::StaticRegistry;

    let api_key = || -> Arc<dyn ModelRegistry> {
        let registry = StaticRegistry::default();
        registry.add(
            serde_json::from_value::<ModelEntry>(json!({
                "id": "codex",
                "family": "gpt",
                "kind": "harness",
                "access": "api-key",
                "cost": { "unit": "usd", "input_per_mtok": 1.0, "output_per_mtok": 4.0 },
            }))
            .unwrap(),
        );
        Arc::new(registry)
    };
    for (config, registry, pick) in [
        ("[fallback]\nmode = \"ask\"\n", None, "codex"),
        ("", Some(api_key()), "stop"),
    ] {
        let mut options = solo_preset(config);
        options.flags = vec![r#"fallback.main.order=["codex"]"#.to_owned()];
        options.registry = registry;
        let mut harness = Harness::start_with(options).await;
        let codex = harness
            .directory
            .add(FakeWorker::harness("codex", "gpt").then(FakeSession::completed(ANSWER)));
        harness.agent.respond(|spec: &SessionSpec| {
            std::fs::write(spec.workdir.join("invoice/money.py"), "half\n").unwrap();
            FakeSession::ended(EndReason::UsageLimit { resets_at: None })
        });
        harness.submit(TASK).await;
        let card = harness
            .until(|event| {
                matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::HandOff)
            })
            .await;
        let AppEvent::Choice { choice } = card else {
            unreachable!()
        };
        assert!(
            choice.title.ends_with("Who carries it on?"),
            "{}",
            choice.title
        );
        let keys: Vec<char> = choice.options.iter().map(|option| option.key).collect();
        assert_eq!(keys, ['1', 's'], "{pick}");
        // Nothing carried on before you answered.
        assert!(codex.specs().is_empty(), "{pick}");
        if registry_is_paid(&choice) {
            assert_eq!(
                choice.options[0].detail.as_deref(),
                Some("an API key, billed to your account")
            );
        }
        let value = match pick {
            "stop" => "stop".to_owned(),
            _ => "codex".to_owned(),
        };
        harness.send(choose(choice.id, &value)).await;
        let ended = harness.until(turn_ended).await;
        harness.exit().await;
        match pick {
            "stop" => {
                assert!(matches!(chat(&ended), Some(ChatEvent::TurnFailed { .. })));
                assert!(codex.specs().is_empty());
            }
            _ => {
                assert!(matches!(
                    chat(&ended),
                    Some(ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
                        if markdown == ANSWER
                ));
                assert_eq!(codex.specs()[0].access, Access::Direct);
            }
        }
    }
}

fn registry_is_paid(choice: &argonauts_core::app::AppChoice) -> bool {
    choice
        .options
        .first()
        .and_then(|option| option.detail.as_deref())
        .is_some_and(|detail| detail.contains("API key"))
}

/// Ignored files a turn writes: nothing kept how they were, so the turn's line and `/rewind`
/// list them as ones it can't put back, and `/rewind` never removes them. The tracked file goes
/// back.
/// With our sandbox off, a shell command that writes Argonauts' own files has what it named
/// put back too. A file it didn't name is only said.
#[tokio::test]
async fn a_shell_command_that_writes_argonauts_own_files_is_put_back_too() {
    let options = HarnessOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            repo.write(".argonauts/config.toml", "workflow = \"solo\"\n")
                .unwrap();
            repo.write(".argonauts/notes.md", "mine\n").unwrap();
            repo.commit_all("start").unwrap();
            std::fs::write(
                repo.global_config(),
                "[triage]\nforce = \"solo\"\n[sandbox]\nmode = \"tool\"\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    };
    let mut harness = Harness::start_with(options).await;
    let root = harness.repo.root().to_owned();
    harness.agent.respond(|spec: &SessionSpec| {
        let root = &spec.workdir;
        std::fs::write(root.join(".argonauts/config.toml"), "workflow = \"x\"\n").unwrap();
        std::fs::write(root.join(".argonauts/notes.md"), "a script's\n").unwrap();
        FakeSession::completed(ANSWER).with_event(tool(
            "t1",
            "Bash",
            json!({"command": "cd .argonauts && sed -i '' s/solo/x/ config.toml"}),
        ))
    });
    harness.submit("Use the x workflow.").await;
    harness.until(turn_ended).await;
    harness
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text.starts_with("This turn edited .argonauts/config.toml, which only a setup"))
        })
        .await;
    assert_eq!(
        read(&root, ".argonauts/config.toml"),
        "workflow = \"solo\"\n"
    );
    // Nothing named it, so it stays, and a line says it changed.
    assert_eq!(read(&root, ".argonauts/notes.md"), "a script's\n");
    harness.exit().await;
}

/// With our sandbox off, an edit the main session made to Argonauts' own files goes back when
/// its turn ends, and a line says so: a setup changes them.
#[tokio::test]
async fn an_edit_to_argonauts_own_files_goes_back_when_our_sandbox_is_off() {
    let options = HarnessOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            repo.write(".argonauts/notes.md", "mine\n").unwrap();
            repo.write("app.py", "print(1)\n").unwrap();
            repo.commit_all("start").unwrap();
            std::fs::write(
                repo.global_config(),
                "[solo_or_crew]\nforce = \"solo\"\n[sandbox]\nmode = \"tool\"\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    };
    let mut harness = Harness::start_with(options).await;
    let root = harness.repo.root().to_owned();
    harness.agent.respond(|spec: &SessionSpec| {
        let root = &spec.workdir;
        std::fs::write(root.join(".argonauts/notes.md"), "the model's\n").unwrap();
        std::fs::write(root.join("app.py"), "print(2)\n").unwrap();
        FakeSession::completed(ANSWER)
            .with_event(tool(
                "t1",
                "Write",
                json!({"file_path": ".argonauts/notes.md"}),
            ))
            .with_event(tool("t2", "Edit", json!({"file_path": "app.py"})))
    });
    harness.submit("Print 2, and note it.").await;
    harness.until(turn_ended).await;
    let told = harness
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text.starts_with("This turn edited .argonauts/notes.md, which only a setup changes"))
        })
        .await;
    assert!(matches!(told, AppEvent::Notice { .. }));
    assert_eq!(read(&root, ".argonauts/notes.md"), "mine\n");
    assert_eq!(read(&root, "app.py"), "print(2)\n");
    harness.exit().await;
}

#[tokio::test]
async fn rewind_lists_the_ignored_files_a_turn_wrote_and_leaves_them() {
    let options = HarnessOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            repo.write(".gitignore", ".env\nconfig/local.toml\n")
                .unwrap();
            repo.write("app.py", "print(1)\n").unwrap();
            repo.commit_all("start").unwrap();
            repo.write(".env", "TOKEN=yours\n").unwrap();
            std::fs::write(repo.global_config(), "[solo_or_crew]\nforce = \"solo\"\n").unwrap();
        })),
        ..HarnessOptions::default()
    };
    let mut harness = Harness::start_with(options).await;
    let root = harness.repo.root().to_owned();
    harness.agent.respond(|spec: &SessionSpec| {
        let root = &spec.workdir;
        std::fs::write(root.join(".env"), "TOKEN=changed-by-agent\n").unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/local.toml"), "debug = true\n").unwrap();
        std::fs::write(root.join("app.py"), "print(2)\n").unwrap();
        let env = root.join(".env").to_string_lossy().into_owned();
        FakeSession::completed(ANSWER)
            .with_event(tool("t1", "Write", json!({"file_path": env})))
            .with_event(tool(
                "t2",
                "Write",
                json!({"file_path": "config/local.toml"}),
            ))
            .with_event(tool("t3", "Edit", json!({"file_path": "app.py"})))
    });
    harness.submit("Turn on debug and print 2.").await;
    harness.until(turn_ended).await;

    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Rewind { card: None },
        })
        .await;
    let card = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RewindTurn))
        .await;
    let AppEvent::Choice { choice } = card else {
        unreachable!()
    };
    assert_eq!(choice.options[0].detail.as_deref(), Some("3 files"));
    let AnswerAction::Choose { value } = &choice.options[0].action else {
        panic!("{:?}", choice.options[0])
    };
    harness.send(choose(choice.id, value)).await;
    let closed = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await;
    let AppEvent::ChoiceClosed { outcome, .. } = closed else {
        unreachable!()
    };
    assert_eq!(
        outcome,
        "Put back app.py · can't put back, as nothing kept how they were: .env, \
         config/local.toml"
    );
    assert_eq!(read(&root, "app.py"), "print(1)\n");
    assert_eq!(read(&root, ".env"), "TOKEN=changed-by-agent\n");
    assert_eq!(read(&root, "config/local.toml"), "debug = true\n");
    harness.exit().await;
}

/// A protected file a turn made where the sandbox couldn't hold it, as on Linux or in a
/// container, is set aside in the turn's own folder, where nothing loads it, and a line says so.
#[tokio::test]
async fn a_protected_file_a_turn_planted_is_set_aside() {
    let mut harness = Harness::start_with(solo_preset("")).await;
    let root = harness.repo.root().to_owned();
    harness.agent.respond(|spec: &SessionSpec| {
        std::fs::write(
            spec.workdir.join(".mcp.json"),
            r#"{"mcpServers": {"x": {"command": "sh"}}}"#,
        )
        .unwrap();
        std::fs::write(spec.workdir.join("invoice/money.py"), "negative = True\n").unwrap();
        FakeSession::completed(ANSWER)
    });
    harness.submit(TASK).await;
    let notice = harness
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.starts_with("This turn made .mcp.json"))
        })
        .await;
    let AppEvent::Notice { text, .. } = notice else {
        unreachable!()
    };
    assert!(!root.join(".mcp.json").exists());
    assert_eq!(read(&root, "invoice/money.py"), "negative = True\n");
    let aside = text
        .split(" set aside in ")
        .nth(1)
        .and_then(|rest| rest.split(" where").next())
        .expect("where it went");
    assert!(Path::new(aside).join(".mcp.json").is_file(), "{text}");
    harness.exit().await;
}

/// Claude Code sends nothing while its question waits on a card (live, 2.1.289), and that
/// silence isn't shown as a stall. Without a card open, a stall still shows.
#[tokio::test]
async fn a_question_waiting_on_you_is_no_stall() {
    use argonauts_core::event::ErrorCategory;

    let stalled = || {
        WorkerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Stalled,
            message: "Claude Code has sent nothing for 30 seconds, not even its heartbeat".into(),
            retryable: true,
        })
    };
    let shows_a_stall = |events: &[AppEvent]| {
        events.iter().any(|event| {
            matches!(
                chat(event),
                Some(ChatEvent::Agent {
                    event: AgentEvent::Error {
                        category: ErrorCategory::Stalled,
                        ..
                    },
                    ..
                })
            )
        })
    };
    for card_open in [true, false] {
        let mut harness = Harness::start_with(solo_preset(SUPERVISED)).await;
        harness.agent.push(
            FakeSession::completed(ANSWER)
                .with_event(stalled())
                .with_delay(Duration::from_millis(800)),
        );
        harness.submit(TASK).await;
        let spec = first_spec(&harness).await;
        if card_open {
            let edit = Ask::Permission {
                tool_name: "Edit".into(),
                input: json!({"file_path": "invoice/money.py"}),
                tool_use_id: None,
            };
            tokio::spawn(async move { ask(&spec, edit).await });
            harness
                .until(|event| {
                    matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Permission)
                })
                .await;
        }
        harness.until(turn_ended).await;
        let events = harness.exit().await;
        assert_eq!(shows_a_stall(&events), !card_open, "task open: {card_open}");
    }
}

/// The solo preset sends what you type to the main session, but a command that names a run
/// still starts it: `/workflow` starts a crew run and never becomes a direct turn
/// (commands come first). `/goal` names no run: it sets a condition, and starts nothing.
#[tokio::test]
async fn under_the_solo_preset_workflow_starts_its_run_and_goal_starts_nothing() {
    let harness = Harness::start_with(solo_preset("")).await;
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Goal {
                text: Some("every test passes".to_owned()),
            },
        })
        .await;
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::WorkflowRun {
                task: Some("fix the typo in the README".to_owned()),
            },
        })
        .await;
    let conductor = harness.conductor.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    while conductor.starts().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "/workflow started no run"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let starts = conductor.starts();
    let [start] = starts.as_slice() else {
        panic!("one run, the workflow's: {starts:?}");
    };
    assert_eq!(start.mode, RunMode::Crew);
    assert_eq!(start.request, "fix the typo in the README");
    // The run carries the goal set before it.
    assert!(
        start
            .overrides
            .contains(&"goal.until=\"every test passes\"".to_owned()),
        "{:?}",
        start.overrides
    );
    assert!(harness.agent.specs().is_empty(), "no direct turn");
    harness.exit().await;
}
