//! Your message on its way to a workflow that talks, whose conversation opens with it: the `❯`
//! line, the main agent's words and its tool rows print only when the reply isn't a change for
//! the talk, and show live under it until then. A change for the talk keeps your line live until
//! the talk shows it, and prints it when the run never gets that far.

use std::time::Instant;

use argonauts_core::app::{AppEvent, ConversationEvent, TurnReply};
use argonauts_core::base::log::NoteLevel;
use argonauts_core::base::names::{MessageId, ModelRef, TurnId, WorkflowId};
use argonauts_core::runs::events::council::{
    CouncilEvent, CouncilEventKind, CouncilMessage, MessageKind, Speaker, TextPart,
};
use argonauts_core::runs::events::run::{CaptainOrCrewDecision, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::workflows::steps::intake::ChangeEstimate;
use argonauts_core::workflows::workflow::StepRef;
use argonauts_core::{AgentEvent, RunId, Spend, Verbosity};
use time::macros::datetime;

use super::TestDriver;
use crate::app::compose_live;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};
use crate::bottom::working::WorkingLine;
use crate::cells::cell::{Cell, UserMessage};

const ASKED: &str = "Add rate limiting to the public API";
const HANDED_OVER: &str = "This changes code, so the council takes it.";

fn conversation(event: ConversationEvent) -> AppEvent {
    AppEvent::Conversation { event }
}

fn started(opens_a_talk: bool) -> AppEvent {
    conversation(ConversationEvent::TurnStarted {
        turn: TurnId::numbered(1),
        text: ASKED.into(),
        worker: ModelRef::new("claude-code", Some("haiku".into())),
        read_only: true,
        commit: "8b41d07".into(),
        uncommitted: vec![],
        settings_sha256: None,
        continues: false,
        opens_a_talk,
    })
}

fn agent(event: AgentEvent) -> AppEvent {
    conversation(ConversationEvent::Agent {
        turn: TurnId::numbered(1),
        event,
    })
}

fn said(text: &str) -> AppEvent {
    agent(AgentEvent::TextDelta {
        text: text.into(),
        parent: None,
    })
}

fn finished(reply: TurnReply) -> AppEvent {
    conversation(ConversationEvent::TurnFinished {
        turn: TurnId::numbered(1),
        reply,
        spend: Spend::default(),
        elapsed_ms: 2_000,
    })
}

fn change(mode: RunMode) -> TurnReply {
    change_saying(mode, HANDED_OVER)
}

/// A change whose reply says `markdown`, which repeats what streamed as the CLI's reply does.
fn change_saying(mode: RunMode, markdown: &str) -> TurnReply {
    TurnReply::Change {
        markdown: markdown.into(),
        estimate: ChangeEstimate {
            task_type: "api-feature".into(),
            summary: "Rate limit the API".into(),
            files_likely: vec!["src/api.rs".into()],
            file_count: 1,
            risk_labels: Vec::new(),
            tests_exist: true,
            unknowns: Vec::new(),
        },
        captain_or_crew: CaptainOrCrewDecision {
            mode,
            move_to_crew: false,
            rule: "chosen_workflow".into(),
            reason: "you chose Council".into(),
            captain: None,
        },
        instructions: Vec::new(),
    }
}

fn run_event(kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 1,
            ts: datetime!(2026-10-25 14:41 UTC),
            run_id: RunId::new("run-rate-limit").expect("a run id"),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        }),
    }
}

/// The talk's first message, yours, as the council opens with it.
fn talk_opens() -> AppEvent {
    let message = CouncilMessage {
        id: MessageId::parse("m-1").expect("a message id"),
        from: Speaker::You,
        kind: MessageKind::Talk,
        text: vec![TextPart::Words(ASKED.to_owned())],
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
    run_event(RunEventKind::Council(CouncilEvent {
        talk: StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 1,
            path: vec![0],
        },
        kind: CouncilEventKind::Message(Box::new(message)),
    }))
}

fn kept(driver: &TestDriver) -> Vec<String> {
    driver
        .model
        .history
        .cells()
        .iter()
        .map(|cell| format!("{cell:?}"))
        .collect()
}

/// The composer's grey echo of what you sent while idle, as the input leaves it.
fn sent(driver: &mut TestDriver) {
    let mut echo = UserMessage::new(ASKED);
    echo.pending = true;
    driver.model.history.push_active(echo);
}

fn live_prompt(driver: &TestDriver) -> bool {
    driver
        .model
        .history
        .active_cells()
        .any(|(_, cell)| matches!(cell, Cell::User(message) if message.text == ASKED))
}

/// The live region's rows, top to bottom.
fn live(driver: &TestDriver) -> Vec<String> {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    driver
        .model
        .history
        .live_blocks(&ctx)
        .iter()
        .flat_map(|block| block.lines.iter())
        .map(|line| plain_text(std::slice::from_ref(line)))
        .collect()
}

#[test]
fn a_held_turns_words_show_live_under_your_message() {
    let mut driver = TestDriver::new();
    sent(&mut driver);
    driver.send(started(true));
    driver.send(said("Let me look through "));
    driver.send(said("the repo first."));
    assert_eq!(
        driver.printed(),
        "",
        "nothing prints while the reply is due"
    );
    let rows = live(&driver);
    let asked = rows.iter().position(|row| row.contains(ASKED));
    let words = rows
        .iter()
        .position(|row| row.contains("Let me look through the repo first."));
    assert!(
        asked.zip(words).is_some_and(|(asked, words)| asked < words),
        "{rows:#?}"
    );
}

#[test]
fn a_change_for_the_talk_leaves_your_message_to_it() {
    let mut driver = TestDriver::new();
    sent(&mut driver);
    driver.send(started(true));
    driver.send(said(HANDED_OVER));
    assert!(live_prompt(&driver), "the message waits for the reply");
    assert_eq!(
        driver.printed(),
        "",
        "nothing prints while the reply is due"
    );
    driver.send(finished(change(RunMode::Crew)));
    assert!(live_prompt(&driver), "the message waits for the talk");
    assert!(
        !live(&driver).iter().any(|row| row.contains(HANDED_OVER)),
        "{:#?}",
        live(&driver)
    );
    assert_eq!(driver.printed(), "");
    driver.send(talk_opens());
    assert!(!live_prompt(&driver));
    assert!(live(&driver).is_empty(), "{:#?}", live(&driver));
    let printed = driver.printed();
    assert!(!printed.contains(ASKED), "{printed}");
    assert!(!printed.contains(HANDED_OVER), "{printed}");
    let kept = kept(&driver);
    let at = |text: &str| kept.iter().position(|cell| cell.contains(text));
    assert!(
        at(ASKED).zip(at(HANDED_OVER)).is_some_and(|(a, h)| a < h),
        "Ctrl+O keeps both, in order: {kept:?}"
    );
}

#[test]
fn a_run_that_fails_to_start_prints_your_message() {
    let mut driver = TestDriver::new();
    sent(&mut driver);
    driver.send(started(true));
    driver.send(finished(change(RunMode::Crew)));
    assert_eq!(driver.printed(), "");
    driver.send(AppEvent::Notice {
        level: NoteLevel::Error,
        text: "The run couldn't start: no CLI is signed in".into(),
    });
    assert!(!live_prompt(&driver));
    let printed = driver.printed();
    let at = |text: &str| printed.find(text);
    let order = [at(ASKED), at(HANDED_OVER), at("couldn't start")];
    assert!(
        order.iter().all(Option::is_some) && order.is_sorted(),
        "{printed}"
    );
}

#[test]
fn a_run_that_ends_before_its_talk_opens_prints_your_message() {
    let mut driver = TestDriver::new();
    sent(&mut driver);
    driver.send(started(true));
    driver.send(finished(change(RunMode::Crew)));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: Some("a limit was hit".into()),
    })));
    assert!(!live_prompt(&driver));
    assert!(driver.printed().contains(ASKED), "{}", driver.printed());
    driver.send(talk_opens());
    assert_eq!(
        kept(&driver)
            .iter()
            .filter(|cell| cell.contains(ASKED))
            .count(),
        1,
        "a talk that opens later doesn't print it again"
    );
}

#[test]
fn a_question_prints_your_message_then_its_answer_once() {
    let answer = "The limiter lives in src/api.rs.";
    let mut driver = TestDriver::new();
    sent(&mut driver);
    driver.send(started(true));
    driver.send(said(answer));
    assert_eq!(driver.printed(), "");
    driver.send(finished(TurnReply::Answer {
        markdown: answer.into(),
    }));
    let printed = driver.printed();
    let (asked, answered) = (printed.find(ASKED), printed.find(answer));
    assert!(
        asked
            .zip(answered)
            .is_some_and(|(asked, answered)| asked < answered),
        "{printed}"
    );
    assert_eq!(printed.matches(answer).count(), 1, "{printed}");
}

fn read(id: &str, path: &str) -> [AppEvent; 2] {
    [
        agent(AgentEvent::ToolStarted {
            id: id.into(),
            name: "Read".into(),
            input: serde_json::json!({ "file_path": path }),
            parent: None,
        }),
        agent(AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: Some("1 line".into()),
            parent: None,
        }),
    ]
}

const LOOKING: &str = "Let me look through the repo first.";
const PLAN: &str = "Plan: one bucket per key in src/api.rs. Open question: how long a key stays.";

/// The reply of a turn that looked around and planned: all it said.
fn planned() -> String {
    format!("{LOOKING}\n\n{PLAN}")
}

/// The main agent looks around and plans before its verdict, as a model may however it's asked.
fn looks_and_plans(driver: &mut TestDriver) {
    sent(driver);
    driver.send(started(true));
    driver.send(said(LOOKING));
    for event in read("tool-1", "src/api.rs") {
        driver.send(event);
    }
    driver.send(said(PLAN));
}

#[test]
fn a_held_turns_rows_wait_off_the_screen_and_the_working_line_counts_them() {
    let mut driver = TestDriver::new();
    driver.model.bottom.working = Some(WorkingLine::new("Thinking", Instant::now()));
    looks_and_plans(&mut driver);
    assert!(live_prompt(&driver), "the message waits for the reply");
    assert_eq!(
        driver.printed(),
        "",
        "nothing prints while the reply is due"
    );
    let rows = live(&driver).join("\n");
    let at = |text: &str| rows.find(text);
    assert!(
        at(ASKED)
            .zip(at("Plan: one bucket"))
            .is_some_and(|(a, p)| a < p),
        "your message and the words streaming now are live: {rows}"
    );
    for aside in [LOOKING, "Read"] {
        assert!(!rows.contains(aside), "{aside:?} is live:\n{rows}");
    }
    let note = driver
        .model
        .bottom
        .working
        .as_ref()
        .and_then(|line| line.note.clone());
    assert_eq!(note.as_deref(), Some("1 tool call so far"));
}

/// The live region has room for half the screen, so rows kept live under your held message
/// would squeeze and then hide it. Set aside, they leave it on screen at any count, and print
/// in order under it once the answer comes.
#[test]
fn many_tool_rows_leave_your_held_message_on_screen() {
    let mut driver = TestDriver::new();
    driver.settings.verbosity = Verbosity::Full;
    driver.model.bottom.working = Some(WorkingLine::new("Thinking", Instant::now()));
    sent(&mut driver);
    driver.send(started(true));
    driver.send(said(LOOKING));
    let paths: Vec<String> = (0..25).map(|n| format!("src/part_{n:02}.rs")).collect();
    for (n, path) in paths.iter().enumerate() {
        for event in read(&format!("tool-{n}"), path) {
            driver.send(event);
        }
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
        let blocks = driver.model.history.live_blocks(&ctx);
        let view = compose_live(
            &blocks,
            &driver.model.history,
            &driver.model.bottom,
            &ctx,
            38,
        );
        let screen = plain_text(&view.lines);
        assert!(
            screen.contains(ASKED) && !screen.contains("more working"),
            "after {} reads:\n{screen}",
            n + 1
        );
    }
    driver.send(said(PLAN));
    assert_eq!(driver.printed_at(120), "");
    driver.send(finished(TurnReply::Answer {
        markdown: planned(),
    }));
    assert!(!live_prompt(&driver));
    let printed = driver.printed_at(120);
    let mut order = vec![printed.find(ASKED), printed.find(LOOKING)];
    order.extend(paths.iter().map(|path| printed.find(path.as_str())));
    order.push(printed.find("Plan: one bucket"));
    assert!(
        order.iter().all(Option::is_some) && order.is_sorted(),
        "{printed}"
    );
    for path in &paths {
        assert_eq!(printed.matches(path.as_str()).count(), 1, "{printed}");
    }
}

#[test]
fn a_question_with_tool_rows_prints_them_in_order_once_answered() {
    let mut driver = TestDriver::new();
    looks_and_plans(&mut driver);
    driver.send(finished(TurnReply::Answer {
        markdown: planned(),
    }));
    assert!(!live_prompt(&driver));
    let printed = driver.printed();
    let at = |text: &str| printed.find(text);
    let order = [at(ASKED), at(LOOKING), at("Read"), at("Plan: one bucket")];
    assert!(
        order.iter().all(Option::is_some) && order.is_sorted(),
        "{printed}"
    );
    assert_eq!(printed.matches(LOOKING).count(), 1, "{printed}");
    assert_eq!(printed.matches("Plan: one bucket").count(), 1, "{printed}");
}

#[test]
fn a_change_for_the_talk_takes_the_turns_rows_to_ctrl_o() {
    let mut driver = TestDriver::new();
    looks_and_plans(&mut driver);
    driver.send(finished(change_saying(RunMode::Crew, &planned())));
    assert!(live_prompt(&driver), "the message waits for the talk");
    let rows = live(&driver).join("\n");
    for gone in [LOOKING, "Read", "Plan: one bucket"] {
        assert!(!rows.contains(gone), "{gone:?} is live:\n{rows}");
    }
    driver.send(talk_opens());
    assert!(live(&driver).is_empty(), "{:#?}", live(&driver));
    let printed = driver.printed();
    for gone in [ASKED, LOOKING, "Read", "Plan: one bucket"] {
        assert!(!printed.contains(gone), "{gone:?} printed:\n{printed}");
    }
    let kept = kept(&driver);
    let at = |text: &str| kept.iter().position(|cell| cell.contains(text));
    let order = [
        at(ASKED),
        at(LOOKING),
        at("src/api.rs"),
        at("Plan: one bucket"),
    ];
    assert!(
        order.iter().all(Option::is_some) && order.is_sorted(),
        "Ctrl+O keeps them all, in order: {kept:#?}"
    );
    assert_eq!(
        kept.iter().filter(|cell| cell.contains(LOOKING)).count(),
        1,
        "{kept:#?}"
    );
}

#[test]
fn a_run_that_fails_to_start_prints_the_rows_under_your_message() {
    let mut driver = TestDriver::new();
    looks_and_plans(&mut driver);
    driver.send(finished(change_saying(RunMode::Crew, &planned())));
    driver.send(AppEvent::Notice {
        level: NoteLevel::Error,
        text: "The run couldn't start: no CLI is signed in".into(),
    });
    let printed = driver.printed();
    let at = |text: &str| printed.find(text);
    let order = [
        at(ASKED),
        at(LOOKING),
        at("Read"),
        at("Plan: one bucket"),
        at("couldn't start"),
    ];
    assert!(
        order.iter().all(Option::is_some) && order.is_sorted(),
        "{printed}"
    );
}

#[test]
fn a_turn_for_no_talk_prints_your_message_as_it_starts() {
    let mut driver = TestDriver::new();
    sent(&mut driver);
    driver.send(started(false));
    assert!(!live_prompt(&driver));
    assert!(driver.printed().contains(ASKED));
}
