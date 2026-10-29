//! The council's talk and its tasks as events reach the screen.

use std::time::Instant;

use argonauts_core::RunId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::council::{
    CouncilMessage, FloorReason, MessageKind, Seat, SeatRole, Speaker, Stance, TextPart,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::WorkflowId;
use argonauts_core::names::{ModelRef, SeatId};
use argonauts_core::settings::ui::{Bubbles, CouncilView};
use time::macros::datetime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::bridge::{Bridge, Effects};
use crate::council::views::CouncilSay;
use crate::model::ViewModel;
use crate::options::UiOptions;
use crate::render::{Clock, RenderCtx};
use crate::theme::Theme;

#[path = "question_golden_tests.rs"]
pub(crate) mod questions;

fn step() -> StepRef {
    StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    }
}

fn send(bridge: &mut Bridge, model: &mut ViewModel, seq: u64, kind: CouncilEventKind) {
    let event = RunEvent {
        seq,
        ts: datetime!(2026-10-25 14:41 UTC),
        run_id: RunId::new("run-rate-limit").expect("a run id"),
        task_id: None,
        session: None,
        source: "argonauts-code".to_owned(),
        kind: RunEventKind::Council(CouncilEvent { talk: step(), kind }),
    };
    bridge.on_event(
        model,
        &AppEvent::Run {
            event: Box::new(event),
        },
        Instant::now(),
    );
}

fn seat(n: u32, agent: &str, model: &str) -> Seat {
    Seat {
        id: SeatId::numbered(n),
        model: ModelRef::new(agent, Some(model.to_owned())),
        role: SeatRole::Proposer,
        number: None,
    }
}

fn message(id: &str, from: Speaker, words: &str) -> CouncilEventKind {
    CouncilEventKind::Message(Box::new(CouncilMessage {
        id: MessageId::parse(id).expect("a message id"),
        from,
        kind: MessageKind::Talk,
        text: vec![TextPart::Words(words.to_owned())],
        reply_to: None,
        mentions: Vec::new(),
        stances: Vec::new(),
        hidden: 0,
        hidden_names: Vec::new(),
        guesses_hidden: 0,
        rewritten: false,
        declined: false,
        started_before: None,
    }))
}

fn rows(model: &mut ViewModel) -> (Vec<String>, Vec<String>) {
    let theme = Theme::dark();
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let printed = model
        .history
        .drain_printable(&ctx)
        .iter()
        .map(|line| {
            crate::text::plain_text(std::slice::from_ref(line))
                .trim_end()
                .to_owned()
        })
        .collect();
    let live = model
        .history
        .active_cells()
        .flat_map(|(_, cell)| crate::HistoryCell::lines(cell, &ctx))
        .map(|line| {
            crate::text::plain_text(std::slice::from_ref(&line))
                .trim_end()
                .to_owned()
        })
        .collect();
    (printed, live)
}

fn bridge() -> Bridge {
    let mut options = UiOptions::default();
    options.settings.council_view = CouncilView::Messages;
    options.settings.bubbles = Bubbles::Blocks;
    Bridge::new(&options)
}

/// A seat beside its CLI's logo reads its model without the maker's word where the logo draws
/// as a picture, as every row that names a model next to its logo does. Beside a dot, and in a
/// sentence, it keeps its whole name.
#[test]
fn a_seat_drops_its_makers_word_only_beside_its_drawn_logo() {
    let seating = Seating {
        seats: vec![
            seat(1, "claude-code", "claude-opus-5-5"),
            seat(2, "codex", "gpt-5.6-luna"),
        ],
        notes: None,
    };
    let faces = super::faces(&seating, &DisplayNames::default());
    let named = |name: &dyn Fn(&crate::council::Face) -> String| -> Vec<String> {
        faces.ids().map(|seat| name(&faces.face(seat))).collect()
    };
    let pictures = crate::kit::logo::golden::pictured(&Theme::dark());
    assert_eq!(
        named(&|face| face.beside(&pictures)),
        ["Opus 5.5", "5.6 Luna"]
    );
    assert_eq!(
        named(&|face| face.beside(&Theme::dark())),
        ["Opus 5.5", "GPT 5.6 Luna"]
    );
    assert_eq!(named(&|face| face.label()), ["Opus 5.5", "GPT 5.6 Luna"]);
}

/// A seat wears the logo of whoever serves its model: the CLI for Claude Code, Codex and agy,
/// and for a model OpenCode runs, the provider behind it: Ollama's for a local one, OpenCode's
/// for one OpenCode Zen serves.
#[test]
fn each_seat_wears_the_logo_of_whoever_serves_its_model() {
    use argonauts_core::logos::LogoMark;

    let seating = Seating {
        seats: vec![
            seat(1, "claude-code", "claude-opus-5-5"),
            seat(2, "codex", "gpt-5.6-luna"),
            seat(3, "antigravity-cli", "gemini-3.8-flash"),
            seat(4, "opencode", "ollama/qwen3-coder:30b"),
            seat(5, "opencode", "opencode/kimi-k2"),
        ],
        notes: None,
    };
    let faces = super::faces(&seating, &DisplayNames::default());
    let marks: Vec<Option<LogoMark>> = faces.ids().map(|seat| faces.face(seat).logo.mark).collect();
    assert_eq!(
        marks,
        [
            Some(LogoMark::ClaudeCode),
            Some(LogoMark::Codex),
            Some(LogoMark::Antigravity),
            Some(LogoMark::Ollama),
            Some(LogoMark::OpenCode),
        ]
    );
}

#[test]
fn a_message_stays_live_for_its_tapbacks_and_prints_when_the_next_lands() {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    send(
        &mut bridge,
        &mut model,
        0,
        CouncilEventKind::Seated(Seating {
            seats: vec![
                seat(1, "claude-code", "claude-opus-5-5"),
                seat(2, "codex", "gpt-5.6-luna"),
            ],
            notes: Some(SeatId::numbered(1)),
        }),
    );
    send(
        &mut bridge,
        &mut model,
        1,
        CouncilEventKind::Writing {
            seat: SeatId::numbered(2),
            why: FloorReason::RepliedTo {
                by: SeatId::numbered(1),
            },
        },
    );
    let (printed, live) = rows(&mut model);
    assert!(
        printed.iter().any(|row| row.trim() == "Council"),
        "{printed:?}"
    );
    assert!(
        live.iter()
            .any(|row| row.contains("• • •") && row.contains("replied to its message")),
        "{live:?}"
    );
    send(
        &mut bridge,
        &mut model,
        2,
        message(
            "m-1",
            Speaker::Seat(SeatId::numbered(2)),
            "A window counter is simpler.",
        ),
    );
    send(
        &mut bridge,
        &mut model,
        3,
        CouncilEventKind::Tapback {
            message: MessageId::parse("m-1").expect("a message id"),
            seat: SeatId::numbered(1),
            stance: Stance::Object,
        },
    );
    let (printed, live) = rows(&mut model);
    assert!(printed.iter().all(|row| !row.contains("window counter")));
    assert!(live.iter().any(|row| row.contains("✘")), "{live:?}");
    assert!(!live.iter().any(|row| row.contains("• • •")), "{live:?}");
    send(
        &mut bridge,
        &mut model,
        4,
        message("m-2", Speaker::You, "Keep /health open."),
    );
    let (printed, _) = rows(&mut model);
    let text = printed.join("\n");
    assert!(
        text.contains("A window counter is simpler.") && text.contains("✘"),
        "{text}"
    );
}

fn seated(bridge: &mut Bridge, model: &mut ViewModel) {
    send(
        bridge,
        model,
        0,
        CouncilEventKind::Seated(Seating {
            seats: vec![
                seat(1, "claude-code", "claude-opus-5-5"),
                seat(2, "codex", "gpt-5.6-luna"),
            ],
            notes: Some(SeatId::numbered(1)),
        }),
    );
}

fn press(bridge: &mut Bridge, model: &mut ViewModel, code: KeyCode, shift: bool) -> Effects {
    let modifiers = if shift {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    let key = UiAction::CouncilKey(KeyEvent::new(code, modifiers));
    bridge.on_action(model, &key, Instant::now())
}

/// Shift+↑ picks the newest message, ↑ an older one, `r` replies to the one picked and
/// `t` opens its thread; Esc goes back, and sending takes the reply's row away.
#[test]
fn shift_up_picks_a_message_to_reply_to_or_open_its_thread() {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    seated(&mut bridge, &mut model);
    assert!(model.bottom.council.sits);
    let first = MessageId::parse("m-1").expect("a message id");
    send(
        &mut bridge,
        &mut model,
        1,
        message(
            "m-1",
            Speaker::Seat(SeatId::numbered(2)),
            "A window counter is simpler.",
        ),
    );
    let mut reply = message("m-2", Speaker::You, "Keep /health open.");
    if let CouncilEventKind::Message(message) = &mut reply {
        message.reply_to = Some(argonauts_core::events::council::ReplyTo {
            message: first.clone(),
            quote: "A window counter is simpler.".to_owned(),
        });
    }
    send(&mut bridge, &mut model, 2, reply);

    press(&mut bridge, &mut model, KeyCode::Up, true);
    assert_eq!(
        model.bottom.council.picking.as_deref(),
        Some("↑ ↓  to pick another · t to open its thread · r to reply · esc to go back")
    );
    assert_eq!(model.bottom.council.picked, MessageId::parse("m-2"));
    press(&mut bridge, &mut model, KeyCode::Up, false);
    assert_eq!(model.bottom.council.picked.as_ref(), Some(&first));
    press(&mut bridge, &mut model, KeyCode::Char('r'), false);
    assert_eq!(model.bottom.council.picked, None);
    assert_eq!(model.bottom.council.picking, None);
    let replying = model.bottom.replying.as_ref().expect("a reply's row");
    assert_eq!(replying.message, first);

    bridge.on_action(&mut model, &UiAction::submit("Agreed."), Instant::now());
    assert!(model.bottom.replying.is_none());

    press(&mut bridge, &mut model, KeyCode::Up, true);
    let opened = press(&mut bridge, &mut model, KeyCode::Char('t'), false);
    assert_eq!(opened.views.len(), 1, "the thread's view");
    assert_eq!(model.bottom.council.picking, None);

    press(&mut bridge, &mut model, KeyCode::Up, true);
    press(&mut bridge, &mut model, KeyCode::Esc, false);
    assert_eq!(model.bottom.council.picking, None);
}

/// What you send names the message it answers and the seats it mentions, and a message that
/// starts with a seat's name goes out as your message rather than to a worker by that name.
#[test]
fn what_you_send_names_the_message_it_answers_and_its_seats() {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    assert_eq!(
        model.bottom.council.say("Hello", None),
        None,
        "no council here"
    );
    seated(&mut bridge, &mut model);
    let luna = SeatId::numbered(2);
    send(
        &mut bridge,
        &mut model,
        1,
        message(
            "m-1",
            Speaker::Seat(luna.clone()),
            "A window counter is simpler.",
        ),
    );
    press(&mut bridge, &mut model, KeyCode::Up, true);
    press(&mut bridge, &mut model, KeyCode::Char('r'), false);

    let name = model.bottom.council.faces.face(&luna).label();
    let text = format!("@{name} why a bucket?");
    let say = model
        .bottom
        .council
        .say(&text, model.bottom.replying.as_ref());
    assert_eq!(
        say,
        Some(CouncilSay::ToCouncil {
            text: text.clone(),
            reply_to: MessageId::parse("m-1"),
            mentions: vec![luna],
        })
    );
    let sent = bridge.on_action(&mut model, &UiAction::submit(text.clone()), Instant::now());
    assert_eq!(
        sent.commands,
        say.and_then(|say| say.command())
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert!(model.bottom.replying.is_none());
}

/// The prompt that starts a council shows as your bubble, so its grey echo goes once the
/// bubble lands.
#[test]
fn your_bubble_takes_the_place_of_the_prompts_echo() {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    let prompt = "Add rate limiting to the public API.";
    bridge.on_action(&mut model, &UiAction::submit(prompt), Instant::now());
    let echoes = |model: &ViewModel| {
        model
            .history
            .active_cells()
            .filter(|(_, cell)| matches!(cell, Cell::User(_)))
            .count()
    };
    assert_eq!(echoes(&model), 1);
    seated(&mut bridge, &mut model);
    send(
        &mut bridge,
        &mut model,
        1,
        message("m-1", Speaker::You, prompt),
    );
    assert_eq!(echoes(&model), 0);
    let (_, live) = rows(&mut model);
    assert!(
        live.iter().any(|row| row.contains("rate limiting")),
        "{live:?}"
    );
}

/// The `@` list starts with the seats once the council sits down.
#[test]
fn the_at_list_starts_with_the_seats() {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    seated(&mut bridge, &mut model);
    let names: Vec<&str> = model
        .bottom
        .menus
        .mentions
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    let conversation = bridge.council.newest().expect("a council");
    let seats: Vec<String> = conversation
        .faces
        .iter()
        .map(|(_, face)| face.label())
        .collect();
    assert_eq!(seats.len(), 2);
    assert_eq!(
        names.get(..2),
        Some(&[seats[0].as_str(), seats[1].as_str()][..])
    );
}

/// Without a council on the screen, the key does nothing and nothing stays picked.
#[test]
fn with_no_council_nothing_is_picked() {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    press(&mut bridge, &mut model, KeyCode::Up, true);
    assert_eq!(model.bottom.council.picking, None);
    assert!(!model.bottom.council.sits);
}

mod tasks {
    use std::path::PathBuf;

    use argonauts_core::event::{TodoItem, TodoStatus};
    use argonauts_core::events::run::{RunLifecycle, RunMode, RunStarted};
    use argonauts_core::events::session::SessionStarted;
    use argonauts_core::events::task::{TaskKind, TaskLimit, TaskWay};
    use argonauts_core::names::{AgentId, RoleName};
    use argonauts_core::session_runner::{Access, RunnerKind};
    use argonauts_core::settings::sessions::SessionLifetime;
    use serde_json::json;

    use super::*;

    pub(super) fn run() -> RunId {
        RunId::new("run-rate-limit").expect("a run id")
    }

    pub(super) fn task(n: u32) -> TaskId {
        TaskId::new(format!("task-{n:03}")).expect("a task id")
    }

    /// `kind` as it happens in `task`'s `session`, `seconds` after the run starts.
    pub(super) fn happen(
        bridge: &mut Bridge,
        model: &mut ViewModel,
        seq: u64,
        kind: RunEventKind,
        on: Option<(u32, u32)>,
        seconds: i64,
    ) {
        let event = RunEvent {
            seq,
            ts: datetime!(2026-10-25 14:41 UTC) + time::Duration::seconds(seconds),
            run_id: run(),
            task_id: on.map(|(n, _)| task(n)),
            // Session 0 is none: the event is the task's own.
            session: on
                .filter(|(_, session)| *session > 0)
                .map(|(_, session)| SessionKey::numbered(session)),
            source: "argonauts-code".to_owned(),
            kind,
        };
        bridge.on_event(
            model,
            &AppEvent::Run {
                event: Box::new(event),
            },
            Instant::now(),
        );
    }

    pub(super) fn started() -> RunEventKind {
        RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
            request: "Rate limit the API".into(),
            title: "Rate limit the API".into(),
            mode: RunMode::Crew,
            forced: false,
            workflow: "council".into(),
            estimate: None,
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: PathBuf::from("/Users/you/shop-api"),
            base_branch: Some("main".into()),
            base_commit: "8b41d07aa".into(),
            uncommitted: vec![],
            uncommitted_included: false,
            settings_sha256: String::new(),
            settings: serde_json::Value::Null,
            overrides: vec![],
            cli_versions: Default::default(),
            version: "0.2.0".into(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
            starts_from: None,
        })))
    }

    pub(super) fn planned(n: u32, title: &str, worker: ModelRef, how: Way) -> RunEventKind {
        RunEventKind::Task(TaskEvent::Created(Box::new(TaskSpec {
            id: task(n),
            title: title.into(),
            task: title.into(),
            kind: TaskKind::Build,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(worker),
            reviewers: Vec::new(),
            limit: TaskLimit {
                limit: None,
                max_minutes: None,
                max_tool_calls: None,
                inactivity_min: None,
            },
            predictions: None,
            item: None,
            way: Some(TaskWay {
                how,
                asked_by_you: false,
                steps: None,
            }),
        })))
    }

    pub(super) fn session(worker: ModelRef, seat: Option<u32>, label: &str) -> RunEventKind {
        RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
            agent_id: AgentId(worker.agent.clone()),
            worker,
            provider: "test".to_owned(),
            kind: RunnerKind::Cli,
            role: RoleName::Worker,
            step: None,
            access: Access::Build,
            lifetime: if seat.is_some() {
                SessionLifetime::Seat
            } else {
                SessionLifetime::Task
            },
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: Some(label.to_owned()),
            asked: None,
            asked_by: None,
            asked_why: None,
            forked: false,
            seat: seat.map(SeatId::numbered),
            billing: None,
        })))
    }

    pub(super) fn luna() -> ModelRef {
        ModelRef::new("codex", Some("gpt-5.6-luna".to_owned()))
    }

    pub(super) fn flash() -> ModelRef {
        ModelRef::new("agy", Some("gemini-3.8-flash".to_owned()))
    }

    pub(super) fn seated_three(bridge: &mut Bridge, model: &mut ViewModel) {
        happen(bridge, model, 0, started(), None, 0);
        send(
            bridge,
            model,
            1,
            CouncilEventKind::Seated(Seating {
                seats: vec![
                    seat(1, "claude-code", "claude-opus-5-5"),
                    seat(2, "codex", "gpt-5.6-luna"),
                    seat(3, "agy", "gemini-3.8-flash"),
                ],
                notes: Some(SeatId::numbered(1)),
            }),
        );
    }

    /// A task done in its seat's own session shows its box once its session starts, with its
    /// latest tool call and its to-dos, and prints as it stood when the task is merged.
    #[test]
    fn an_own_session_task_shows_its_latest_call_and_to_dos() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let plan = planned(2, "Plan limits", luna(), Way::OwnSession);
        happen(&mut bridge, &mut model, 2, plan, None, 0);
        happen(
            &mut bridge,
            &mut model,
            3,
            session(luna(), Some(2), "Plan limits"),
            Some((2, 7)),
            0,
        );
        let call = AgentEvent::ToolStarted {
            id: "toolu_1".to_owned(),
            name: "Bash".to_owned(),
            input: json!({ "command": "npm run migrate:new add_rate_limit_to_plans" }),
            parent: None,
        };
        happen(
            &mut bridge,
            &mut model,
            4,
            RunEventKind::Agent(call),
            Some((2, 7)),
            60,
        );
        let todo = |text: &str, status| TodoItem {
            text: text.to_owned(),
            status,
        };
        let todos = AgentEvent::Todo {
            items: vec![
                todo("Add the column", TodoStatus::Completed),
                todo("Read the plan", TodoStatus::Pending),
                todo("Test it", TodoStatus::Pending),
            ],
        };
        happen(
            &mut bridge,
            &mut model,
            5,
            RunEventKind::Agent(todos),
            Some((2, 7)),
            216,
        );
        let (_, live) = rows(&mut model);
        let text = live.join("\n");
        assert!(text.contains("Own session  Plan limits"), "{text}");
        assert!(
            text.contains("⏺  Bash(npm run migrate:new add_rate_limit_to_plans)"),
            "{text}"
        );
        assert!(text.contains("1/3 to-dos · 3m36s"), "{text}");

        let merged = RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Running),
            to: TaskState::Merged,
            reason: String::new(),
        });
        happen(&mut bridge, &mut model, 6, merged, Some((2, 7)), 300);
        let (printed, live) = rows(&mut model);
        assert!(
            printed
                .iter()
                .any(|row| row.contains("Own session  Plan limits"))
        );
        assert!(
            !live.iter().any(|row| row.contains("Plan limits")),
            "{live:?}"
        );
    }

    /// A task done by agents says its seat started them, then lists each agent's part with the
    /// word they share kept apart; shift+↑ picks the task, which then says Enter opens it.
    #[test]
    fn an_agents_task_lists_its_parts_and_can_be_picked() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let plan = planned(4, "Route tests", flash(), Way::Agents { count: 3 });
        happen(&mut bridge, &mut model, 2, plan, None, 0);
        for (n, part) in [
            (11, "Orders routes"),
            (12, "Users routes"),
            (13, "Import routes"),
        ] {
            happen(
                &mut bridge,
                &mut model,
                2 + u64::from(n),
                session(flash(), None, part),
                Some((4, n)),
                64,
            );
        }
        let (printed, live) = rows(&mut model);
        assert!(
            printed
                .iter()
                .any(|row| row.contains("3.8 Flash started 3 agents")
                    || row.contains("started 3 agents")),
            "{printed:?}"
        );
        let text = live.join("\n");
        assert!(text.contains("3 agents  Route tests"), "{text}");
        assert!(
            text.contains("Orders routes") && text.contains("Import routes"),
            "{text}"
        );
        assert!(text.contains("0/3 agents"), "{text}");

        press(&mut bridge, &mut model, KeyCode::Up, true);
        assert_eq!(
            model.bottom.council.picking.as_deref(),
            Some("↑ ↓  to pick another · enter to open its agents · esc to go back to the input")
        );
        let (_, live) = rows(&mut model);
        assert!(
            live.iter().any(|row| row.contains("enter to open")),
            "{live:?}"
        );
        press(&mut bridge, &mut model, KeyCode::Enter, false);
        assert_eq!(model.bottom.council.picking, None);
    }
}

mod seat_sessions {
    use super::tasks::*;
    use super::*;

    /// Enter on the row of the fresh session that checks a council's task opens that check's
    /// own session, which tells only you who made the change; a session that checks nothing of
    /// the council's opens no view of the council's.
    #[test]
    fn enter_on_a_checks_row_opens_its_own_session() {
        use argonauts_core::events::council::{CheckPick, CheckRung};
        use argonauts_core::names::RoleName;

        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let opus = ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()));
        let plan = planned(1, "Redis bucket", opus.clone(), Way::OwnSession);
        happen(&mut bridge, &mut model, 2, plan, Some((1, 0)), 0);
        let builds = session(opus.clone(), Some(1), "Redis bucket");
        happen(&mut bridge, &mut model, 3, builds, Some((1, 7)), 0);
        let RunEventKind::Session(SessionEvent::Started(mut checks)) =
            session(opus.clone(), None, "Review")
        else {
            unreachable!("a session starts");
        };
        checks.role = RoleName::Reviewer;
        let started = RunEventKind::Session(SessionEvent::Started(checks));
        happen(&mut bridge, &mut model, 4, started, Some((1, 8)), 60);
        let open = |bridge: &mut Bridge, model: &mut ViewModel| {
            let agent = crate::bottom::AgentRef {
                run: Some(run().to_string()),
                session: SessionKey::numbered(8).to_string(),
            };
            bridge.on_action(model, &UiAction::OpenAgent(agent), Instant::now())
        };
        let kind = |effects: Effects| format!("{:?}", effects.views[0]);
        let plain = kind(open(&mut bridge, &mut model));
        assert!(!plain.starts_with("SeatView"), "no pick, no check: {plain}");
        let pick = CheckPick {
            task: TaskId::numbered(1),
            checker: Some(opus),
            rung: CheckRung::SameMaker,
        };
        send(&mut bridge, &mut model, 5, CouncilEventKind::Check(pick));
        let check = kind(open(&mut bridge, &mut model));
        assert!(check.starts_with("SeatView"), "{check}");
        assert!(check.contains("a fresh Opus 5.5 session"), "{check}");
        assert!(
            check.contains("the same model; the check was never told"),
            "{check}"
        );
    }

    /// Enter on a picked seat's message opens that seat's own session, which says it is private
    /// to that seat; a message of yours opens nothing.
    #[test]
    fn enter_on_a_seats_message_opens_its_session() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        happen(
            &mut bridge,
            &mut model,
            2,
            session(luna(), Some(2), "Seat"),
            Some((0, 7)),
            0,
        );
        send(
            &mut bridge,
            &mut model,
            3,
            message(
                "m-1",
                Speaker::Seat(SeatId::numbered(2)),
                "A window counter is simpler.",
            ),
        );
        press(&mut bridge, &mut model, KeyCode::Up, true);
        assert_eq!(
            model.bottom.council.picking.as_deref(),
            Some("↑ ↓  to pick another · enter to open its session · r to reply · esc to go back")
        );
        let opened = press(&mut bridge, &mut model, KeyCode::Enter, false);
        assert_eq!(opened.views.len(), 1, "the seat's session view");
        // It asks for the seat's key: the letters the seat reads the council's seats by.
        let asks = opened.views[0].asks_on_open();
        let Some(argonauts_core::app::ViewRequest::SeatKey { seats, reader, .. }) = asks else {
            panic!("{asks:?}");
        };
        assert_eq!(seats, (1..=3).map(SeatId::numbered).collect::<Vec<_>>());
        assert_eq!(reader, SeatId::numbered(2));
    }
}

mod working_line {
    use argonauts_core::app::AppMode;
    use argonauts_core::events::council::{Point, PointState, Side};

    use super::tasks::*;
    use super::*;

    /// While the council's run is the work on screen, its line parleys against the talk's token
    /// limit and counts the seats that agree, with the council's tip once; once a task has its
    /// box it rows and counts the tasks.
    #[test]
    fn the_working_line_follows_the_talk_then_the_work() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let mode = AppEvent::Mode {
            mode: AppMode::Working { run: run() },
        };
        bridge.on_event(&mut model, &mode, Instant::now());
        send(
            &mut bridge,
            &mut model,
            2,
            CouncilEventKind::Limits {
                tokens: 200_000,
                minutes: 15,
            },
        );
        let line = model.bottom.working.as_ref().expect("a working line");
        assert_eq!(line.verb, crate::council::TALKING);
        assert_eq!(line.tokens_limit, Some(200_000));
        assert_eq!(line.details, ["0/3 seats agree"]);
        assert_eq!(line.tip, Some(crate::council::TIP));
        assert_eq!(line.note, None, "no points yet");

        let open = Point {
            number: 1,
            words: "Where counts live".to_owned(),
            state: PointState::Open,
            raised_by: None,
            sides: ["Redis", "memory"]
                .map(|words| Side {
                    words: words.to_owned(),
                    backers: Vec::new(),
                    reason: String::new(),
                })
                .to_vec(),
        };
        send(
            &mut bridge,
            &mut model,
            3,
            CouncilEventKind::Points { points: vec![open] },
        );
        let line = model.bottom.working.as_ref().expect("a working line");
        assert_eq!(line.note.as_deref(), Some("Still open: Redis or memory"));

        let plan = planned(2, "Plan limits", luna(), Way::OwnSession);
        happen(&mut bridge, &mut model, 4, plan, Some((2, 0)), 0);
        happen(
            &mut bridge,
            &mut model,
            5,
            session(luna(), Some(2), "Plan limits"),
            Some((2, 7)),
            0,
        );
        let line = model.bottom.working.as_ref().expect("a working line");
        assert_eq!(line.verb, crate::council::WORKING);
        assert_eq!(line.tokens_limit, None);
        assert_eq!(line.details, ["0/1 tasks"]);
        assert_eq!(line.note, None);
    }
}

mod plan {
    use argonauts_core::events::approval::{
        AnswerAction, AnswerOption, ApprovalKind, ApprovalOpened, FreeText, PlanPacket,
        WayObjection, WaysOffer,
    };
    use argonauts_core::events::council::TextPart;
    use argonauts_core::events::plan::{LimitPlan, TaskGraph};
    use argonauts_core::names::ApprovalId;
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith, WhenDeclined};
    use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
    use argonauts_core::workflow::WorkflowRef;

    use argonauts_core::events::approval::ApprovalDecided;
    use argonauts_core::events::council::{CheckPick, CheckRung};
    use argonauts_core::events::run::Party;
    use argonauts_core::events::task::ReviewEvent;
    use argonauts_core::steps::judgment::{ReviewOutput, ReviewVerdict};

    use super::tasks::{flash, happen, luna, planned, run, seated_three, started};
    use super::*;
    use crate::bottom::QuestionEvent;
    use crate::council::edits::{EditKey, note};

    fn opus() -> ModelRef {
        ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()))
    }

    pub(super) fn spec(n: u32, title: &str, worker: ModelRef, how: Way, asked: bool) -> TaskSpec {
        let RunEventKind::Task(TaskEvent::Created(spec)) = planned(n, title, worker, how) else {
            unreachable!("a planned task is created");
        };
        let mut spec = *spec;
        if let Some(way) = spec.way.as_mut() {
            way.asked_by_you = asked;
        }
        spec
    }

    fn plan_opens(tasks: Vec<TaskSpec>) -> RunEventKind {
        opens(packet(tasks, Vec::new()))
    }

    pub(super) fn packet(tasks: Vec<TaskSpec>, objections: Vec<WayObjection>) -> PlanPacket {
        PlanPacket {
            summary: "Rate limit the API".into(),
            judging: None,
            near_tie: false,
            decisions: Vec::new(),
            graph: TaskGraph {
                tasks,
                critical_path: Vec::new(),
            },
            acceptance_tests: Vec::new(),
            limit: LimitPlan {
                lines: Vec::new(),
                total: LimitAmount {
                    unit: LimitUnit::Usd,
                    amount: 20.0,
                },
                estimate_low: 0.0,
                estimate_high: 0.0,
            },
            new_dependencies: Vec::new(),
            flags: Vec::new(),
            workflow: None,
            seats: Vec::new(),
            objections,
            offer: None,
        }
    }

    fn opens(packet: PlanPacket) -> RunEventKind {
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(ApprovalOpened {
            id: ApprovalId::numbered(1),
            kind: ApprovalKind::Plan,
            title: "Approve this plan?".into(),
            task: None,
            packet: ApprovalPacket::Plan(Box::new(packet)),
            evidence_sha256: "0".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            when_declined: WhenDeclined::Decide,
            options: vec![AnswerOption {
                key: 'y',
                label: "Approve the plan".into(),
                action: AnswerAction::Approve,
                recommended: true,
                detail: None,
            }],
            free_text: FreeText::Guidance,
            needs_you: true,
        })))
    }

    fn row_with<'a>(rows: &'a [String], words: &str) -> &'a str {
        rows.iter()
            .find(|row| row.contains(words))
            .unwrap_or_else(|| panic!("a row with {words:?} in {rows:#?}"))
    }

    /// When the plan opens for your approval, the note keeper's who-does-what lists each task
    /// with its seat, its way, "You asked" for a way you named, and who checks it.
    #[test]
    fn the_plan_to_approve_shows_who_does_what() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let mut bucket = spec(
            1,
            "Redis bucket",
            opus(),
            Way::Workflow {
                name: WorkflowRef::new("maker-checker"),
            },
            true,
        );
        bucket.reviewers = vec![luna()];
        let tasks = vec![
            bucket,
            spec(2, "Plan limits", luna(), Way::OwnSession, true),
            spec(4, "Route tests", flash(), Way::Agents { count: 3 }, false),
        ];
        happen(
            &mut bridge,
            &mut model,
            2,
            plan_opens(tasks.clone()),
            None,
            0,
        );
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let head = row_with(&all, "Task            Who");
        assert!(head.contains("How              Why"), "{head}");
        let bucket = row_with(&all, "Redis bucket");
        assert!(
            bucket.contains("Opus 5.5") && bucket.contains("Maker-checker    You asked"),
            "{bucket}"
        );
        let limits = row_with(&all, "Plan limits");
        // Names follow the app's display names, and each column fits its widest cell.
        assert!(
            limits.contains("5.6 Luna") && limits.contains("Own session      You asked"),
            "{limits}"
        );
        let tests = row_with(&all, "Route tests");
        assert!(
            tests.contains("3.8 Flash") && tests.contains(" 3 agents       "),
            "{tests}"
        );
        assert!(!tests.contains("You asked"), "{tests}");
        row_with(&all, "Only you see Checked by");

        // The same plan opened again adds nothing.
        happen(&mut bridge, &mut model, 3, plan_opens(tasks), None, 0);
        let (printed, live) = rows(&mut model);
        assert!(
            !printed
                .iter()
                .chain(&live)
                .any(|row| row.contains("Only you see Checked by")),
            "{printed:?} {live:?}"
        );
    }

    /// With two seats of one model, the plan names the seat it gives each task, by its number,
    /// where the plan says which; a task it gives no seat names the model.
    #[test]
    fn the_plan_names_the_seat_of_a_model_that_sits_twice() {
        use argonauts_core::events::approval::TaskSeat;

        let mut bridge = bridge();
        let mut model = ViewModel::default();
        happen(&mut bridge, &mut model, 0, started(), None, 0);
        let twice = |n: u32| Seat {
            number: Some(u8::try_from(n).unwrap()),
            ..seat(n, "claude-code", "claude-opus-5-5")
        };
        send(
            &mut bridge,
            &mut model,
            1,
            CouncilEventKind::Seated(Seating {
                seats: vec![twice(1), twice(2), seat(3, "codex", "gpt-5.6-luna")],
                notes: Some(SeatId::numbered(1)),
            }),
        );
        let tasks = vec![
            spec(1, "Redis bucket", opus(), Way::OwnSession, false),
            spec(2, "Plan limits", opus(), Way::OwnSession, false),
            spec(3, "Route tests", opus(), Way::OwnSession, false),
        ];
        let mut plan = packet(tasks, Vec::new());
        plan.seats = vec![
            TaskSeat {
                task: super::tasks::task(1),
                seat: SeatId::numbered(2),
            },
            TaskSeat {
                task: super::tasks::task(2),
                seat: SeatId::numbered(1),
            },
        ];
        happen(&mut bridge, &mut model, 2, opens(plan), None, 0);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let bucket = row_with(&all, "Redis bucket");
        assert!(bucket.contains("Opus 5.5 · 2"), "{bucket}");
        let limits = row_with(&all, "Plan limits");
        assert!(limits.contains("Opus 5.5 · 1"), "{limits}");
        let tests = row_with(&all, "Route tests");
        assert!(
            tests.contains("Opus 5.5") && !tests.contains(" · "),
            "{tests}"
        );
    }

    /// Approve with edits lists the plan's tasks with their ways, offers what the council's
    /// setup allows, and writes a changed way as the approval's edit line.
    #[test]
    fn approve_with_edits_lists_the_plans_tasks_and_writes_your_edits() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let tasks = vec![
            spec(2, "Plan limits", luna(), Way::OwnSession, true),
            spec(4, "Route tests", flash(), Way::Agents { count: 3 }, false),
        ];
        let mut plan = packet(tasks, Vec::new());
        let not_offered = "Not offered: Best-of-n · this council's setup lists one workflow · Set \
                           up Council changes it";
        plan.offer = Some(WaysOffer {
            ways: vec![
                Way::OwnSession,
                Way::Workflow {
                    name: WorkflowRef::new("maker-checker"),
                },
                Way::Agents { count: 2 },
                Way::Agents { count: 3 },
            ],
            not_offered: Some(not_offered.to_owned()),
        });
        happen(&mut bridge, &mut model, 2, opens(plan.clone()), None, 0);
        let mut sheet = bridge
            .council
            .edit_sheet(&run(), &plan, &bridge.state.names)
            .expect("a sheet for the plan");
        let named: Vec<(&str, &Way)> = sheet
            .tasks
            .iter()
            .map(|task| (task.name.as_str(), &task.way))
            .collect();
        assert_eq!(
            named,
            [
                ("Plan limits", &Way::OwnSession),
                ("Route tests", &Way::Agents { count: 3 })
            ]
        );
        assert_eq!(sheet.not_offered.as_deref(), Some(not_offered));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        sheet.key(key(KeyCode::Right));
        let EditKey::Approve(tasks) = sheet.key(key(KeyCode::Enter)) else {
            panic!("enter approves");
        };
        assert_eq!(note(&tasks), "task-002 way maker-checker");
    }

    /// On a council's plan, Approve with edits opens its sheet in the question's place: → moves
    /// the task's way, Esc goes back to the question, and Enter answers the approval with the
    /// edit line.
    #[test]
    fn approve_with_edits_opens_its_sheet_in_the_questions_place() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let mut plan = packet(
            vec![spec(2, "Plan limits", luna(), Way::OwnSession, false)],
            Vec::new(),
        );
        plan.offer = Some(WaysOffer {
            ways: vec![
                Way::OwnSession,
                Way::Workflow {
                    name: WorkflowRef::new("maker-checker"),
                },
            ],
            not_offered: None,
        });
        let RunEventKind::Approval(ApprovalEvent::Opened(mut opened)) = opens(plan) else {
            unreachable!("a plan approval opens");
        };
        opened.options.push(AnswerOption {
            key: 'e',
            label: "Approve with edits".into(),
            action: AnswerAction::ApproveWithEdits,
            recommended: false,
            detail: None,
        });
        let opened = RunEventKind::Approval(ApprovalEvent::Opened(opened));
        happen(&mut bridge, &mut model, 2, opened, None, 0);

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let question = model.bottom.question.as_mut().expect("the plan's question");
        question.focused = true;
        assert_eq!(
            question.handle_key(key(KeyCode::Char('2'))),
            QuestionEvent::Moved
        );
        let theme = Theme::dark();
        let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
        let text = |lines: Vec<ratatui::text::Line<'static>>| -> Vec<String> {
            lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect()
                })
                .collect()
        };
        let shown = text(question.lines(&ctx));
        assert!(shown[1].starts_with("◆ Approve with edits"), "{shown:#?}");
        assert!(row_with(&shown, "Plan limits").contains("‹ Own session ›"));

        assert_eq!(question.handle_key(key(KeyCode::Esc)), QuestionEvent::Moved);
        let back = text(question.lines(&ctx));
        assert!(
            back.iter().any(|row| row.contains("2. Approve with edits")),
            "{back:#?}"
        );

        question.handle_key(key(KeyCode::Enter));
        question.handle_key(key(KeyCode::Right));
        assert_eq!(
            question.handle_key(key(KeyCode::Enter)),
            QuestionEvent::Answered {
                index: 1,
                note: Some("task-002 way maker-checker".to_owned()),
                checked: Vec::new(),
            }
        );
    }

    /// A seat still against a way you asked for shows under that task, on the plan and on
    /// Approve with edits, naming the seats its reason names as the council does. A seat that
    /// objected with a tapback alone just objects.
    #[test]
    fn a_seats_objection_to_your_way_shows_under_its_task() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let tasks = vec![
            spec(2, "Plan limits", luna(), Way::OwnSession, true),
            spec(3, "Memory store", luna(), Way::OwnSession, true),
            spec(4, "Route tests", flash(), Way::Agents { count: 3 }, false),
        ];
        let objections = vec![
            WayObjection {
                task: TaskId::numbered(2),
                seat: SeatId::numbered(3),
                reason: vec![
                    TextPart::Words("Every request reads this table, as ".to_owned()),
                    TextPart::Seat(SeatId::numbered(1)),
                    TextPart::Words(" said".to_owned()),
                ],
            },
            WayObjection {
                task: TaskId::numbered(3),
                seat: SeatId::numbered(1),
                reason: Vec::new(),
            },
        ];
        let plan = packet(tasks, objections);
        happen(&mut bridge, &mut model, 2, opens(plan.clone()), None, 0);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        // A centred line under the plan says who objects and that only you change it.
        let objected = row_with(&all, "object to 2 ways you asked for; only you change them");
        assert!(
            objected.contains("3.8 Flash and ") && objected.contains(" Opus 5.5 object"),
            "{objected}"
        );
        let at = all
            .iter()
            .position(|row| row.contains("Plan limits"))
            .expect("the task's row");
        let under = &all[at + 1];
        assert!(under.contains("✘"), "{under}");
        assert!(
            under.contains("3.8 Flash: Every request reads this table, as Opus 5.5 said"),
            "{under}"
        );
        let at = all
            .iter()
            .position(|row| row.contains("Memory store"))
            .expect("the task's row");
        assert!(
            all[at + 1].contains("✘  ●  Opus 5.5 objects "),
            "{}",
            all[at + 1]
        );
        assert!(!all[at + 2].contains("✘"), "{all:#?}");

        let offered = PlanPacket {
            offer: Some(WaysOffer {
                ways: vec![Way::OwnSession],
                not_offered: None,
            }),
            ..plan.clone()
        };
        let sheet = bridge
            .council
            .edit_sheet(&run(), &offered, &bridge.state.names)
            .expect("a sheet for the plan");
        let limits = &sheet.tasks[0];
        assert_eq!(
            limits.objection,
            Some((
                SeatId::numbered(3),
                "Every request reads this table, as Opus 5.5 said".to_owned()
            ))
        );
        assert_eq!(
            sheet.tasks[1].objection,
            Some((SeatId::numbered(1), String::new()))
        );
        assert_eq!(sheet.tasks[2].objection, None);
    }

    /// A task whose model two seats run names the model, never a seat it can't tell.
    #[test]
    fn a_model_that_sits_twice_does_its_task_by_name() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        happen(&mut bridge, &mut model, 0, started(), None, 0);
        let mut second = seat(2, "claude-code", "claude-opus-5-5");
        second.number = Some(2);
        let mut first = seat(1, "claude-code", "claude-opus-5-5");
        first.number = Some(1);
        send(
            &mut bridge,
            &mut model,
            1,
            CouncilEventKind::Seated(Seating {
                seats: vec![first, second, seat(3, "codex", "gpt-5.6-luna")],
                notes: Some(SeatId::numbered(3)),
            }),
        );
        let tasks = vec![spec(1, "Redis bucket", opus(), Way::OwnSession, false)];
        happen(&mut bridge, &mut model, 2, plan_opens(tasks), None, 0);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let bucket = row_with(&all, "Redis bucket");
        assert!(bucket.contains("Opus 5.5 "), "{bucket}");
        assert!(!bucket.contains("Opus 5.5 · "), "{bucket}");
    }

    fn decided(by: Party, action: AnswerAction) -> RunEventKind {
        RunEventKind::Approval(ApprovalEvent::Decided(ApprovalDecided {
            id: ApprovalId::numbered(1),
            action,
            text: None,
            by,
            evidence_sha256: "0".repeat(64),
            auto_preview: None,
            tasks: Vec::new(),
        }))
    }

    /// Each task of the plan checked by a fresh session of `checker`.
    fn checked_by(checker: &ModelRef, tasks: Vec<TaskSpec>) -> Vec<TaskSpec> {
        tasks
            .into_iter()
            .map(|mut spec| {
                spec.reviewers = vec![checker.clone()];
                spec
            })
            .collect()
    }

    /// Once you approve the plan, a line says how many tasks it holds and who checks them all,
    /// by the whole name a sentence gives a model; approved in your place, it says nothing.
    #[test]
    fn approving_the_plan_says_how_many_tasks_and_who_checks_them() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let tasks = checked_by(
            &flash(),
            vec![
                spec(1, "Redis bucket", opus(), Way::OwnSession, false),
                spec(2, "Plan limits", luna(), Way::OwnSession, false),
            ],
        );
        let approve = AnswerAction::Approve;
        happen(
            &mut bridge,
            &mut model,
            2,
            plan_opens(tasks.clone()),
            None,
            0,
        );
        happen(
            &mut bridge,
            &mut model,
            3,
            decided(Party::Autopilot, approve.clone()),
            None,
            9,
        );
        let (printed, live) = rows(&mut model);
        assert!(
            !printed
                .iter()
                .chain(&live)
                .any(|row| row.contains("You approved")),
            "{printed:#?}"
        );
        happen(&mut bridge, &mut model, 4, plan_opens(tasks), None, 10);
        happen(
            &mut bridge,
            &mut model,
            5,
            decided(Party::User, approve),
            None,
            20,
        );
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let line = row_with(&all, "You approved the plan");
        assert!(
            line.trim_end()
                .ends_with(" · You approved the plan · 2 tasks · Gemini 3.8 Flash checks each one"),
            "{line}"
        );
    }

    /// On autopilot a plan that breaks a way you asked for opens and is approved at once in your
    /// place: the note keeper's who-does-what still shows, then the line that says what the plan
    /// lacks, and nothing says you approved it.
    #[test]
    fn a_plan_autopilot_settles_still_shows_who_does_what() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let tasks = vec![
            spec(1, "Redis bucket", opus(), Way::OwnSession, false),
            spec(2, "Plan limits", luna(), Way::OwnSession, true),
        ];
        happen(&mut bridge, &mut model, 2, plan_opens(tasks), None, 0);
        let approve = decided(Party::Autopilot, AnswerAction::Approve);
        happen(&mut bridge, &mut model, 3, approve, None, 0);
        let lacks = "Autopilot approved the plan instead of asking you, though it breaks a way \
                     you asked for. Plan limits: Own session can't do it alone.";
        let warning = RunEventKind::Note(argonauts_core::Note {
            level: argonauts_core::NoteLevel::Warn,
            text: lacks.to_owned(),
        });
        happen(&mut bridge, &mut model, 4, warning, None, 0);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        assert!(
            row_with(&all, "Task            Who").contains("How            Why"),
            "{all:#?}"
        );
        let bucket = row_with(&all, "Redis bucket");
        assert!(
            bucket.contains("Opus 5.5") && bucket.contains("Own session"),
            "{bucket}"
        );
        let limits = row_with(&all, "Plan limits");
        assert!(
            limits.contains("5.6 Luna") && limits.contains("You asked"),
            "{limits}"
        );
        let plan_at = all.iter().position(|row| row.contains("Redis bucket"));
        let said_at = all
            .iter()
            .position(|row| row.contains("Autopilot approved the plan"));
        assert!(plan_at < said_at && said_at.is_some(), "{all:#?}");
        assert!(
            !all.iter().any(|row| row.contains("You approved")),
            "{all:#?}"
        );
    }

    /// Where only the maker of every task's work checks it, the line says a fresh session of
    /// it does; where a seat objected to a way you asked for and you approved as it was, the
    /// line says that way stays.
    #[test]
    fn the_approved_plan_names_a_same_maker_check_and_the_ways_that_stay() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        let tasks = checked_by(
            &opus(),
            vec![spec(1, "Redis bucket", opus(), Way::OwnSession, false)],
        );
        happen(&mut bridge, &mut model, 2, plan_opens(tasks), None, 0);
        let approve = decided(Party::User, AnswerAction::Approve);
        happen(&mut bridge, &mut model, 3, approve.clone(), None, 9);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let line = row_with(&all, "You approved the plan");
        assert!(
            line.trim_end()
                .ends_with("1 task · a fresh Opus 5.5 session checks each one"),
            "{line}"
        );

        let tasks = checked_by(
            &flash(),
            vec![
                spec(1, "Redis bucket", opus(), Way::OwnSession, false),
                spec(2, "Plan limits", luna(), Way::OwnSession, true),
            ],
        );
        let objection = WayObjection {
            task: TaskId::numbered(2),
            seat: SeatId::numbered(3),
            reason: vec![TextPart::Words("Every request reads this table".to_owned())],
        };
        happen(
            &mut bridge,
            &mut model,
            4,
            opens(packet(tasks, vec![objection])),
            None,
            10,
        );
        happen(&mut bridge, &mut model, 5, approve, None, 20);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let stays: Vec<&String> = all
            .iter()
            .filter(|row| row.contains("You approved the plan"))
            .collect();
        assert!(
            stays.last().is_some_and(|row| row
                .trim_end()
                .ends_with(" · Plan limits stays as you asked")),
            "{stays:#?}"
        );
    }

    fn pick(task: u32, checker: ModelRef, rung: CheckRung) -> CouncilEventKind {
        CouncilEventKind::Check(CheckPick {
            task: TaskId::numbered(task),
            checker: Some(checker),
            rung,
        })
    }

    fn reviewed(checker: ModelRef, verdict: ReviewVerdict) -> RunEventKind {
        RunEventKind::Review(ReviewEvent::Reviewed {
            reviewer: checker,
            blind: true,
            round: 1,
            review: ReviewOutput {
                verdict,
                summary: "Reads well.".to_owned(),
                comments: Vec::new(),
                tamper_suspected: false,
            },
            picked_by: None,
        })
    }

    /// Each check says who checked which task without knowing who made it and what it found,
    /// and the council's first says what a check reads; a check by the maker of the change
    /// says so every time.
    #[test]
    fn each_check_says_what_it_found_without_knowing_who_made_it() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        for (n, title) in [(1, "Redis bucket"), (2, "Plan limits"), (3, "Route tests")] {
            let plan = planned(n, title, opus(), Way::OwnSession);
            happen(&mut bridge, &mut model, u64::from(n) + 1, plan, None, 0);
        }
        send(
            &mut bridge,
            &mut model,
            5,
            pick(1, flash(), CheckRung::OtherMaker),
        );
        let accepted = reviewed(flash(), ReviewVerdict::Approve);
        happen(&mut bridge, &mut model, 6, accepted, Some((1, 0)), 60);
        send(
            &mut bridge,
            &mut model,
            7,
            pick(2, luna(), CheckRung::OtherMaker),
        );
        let sent_back = reviewed(luna(), ReviewVerdict::RequestChanges);
        happen(&mut bridge, &mut model, 8, sent_back, Some((2, 0)), 90);
        send(
            &mut bridge,
            &mut model,
            9,
            pick(3, opus(), CheckRung::SameMaker),
        );
        let same = reviewed(opus(), ReviewVerdict::Approve);
        happen(&mut bridge, &mut model, 10, same, Some((3, 0)), 120);
        let (printed, live) = rows(&mut model);
        let all: Vec<String> = printed.into_iter().chain(live).collect();
        let said: Vec<&str> = all
            .iter()
            .map(|row| row.trim())
            .filter(|row| {
                row.contains("checked")
                    || row.starts_with("It read")
                    || row.starts_with("Same maker")
            })
            .collect();
        assert_eq!(said.len(), 5, "{said:#?}");
        assert!(
            said[0].starts_with("✔")
                && said[0].ends_with(
                    "3.8 Flash checked Redis bucket without knowing who made it, and accepted it"
                ),
            "{said:#?}"
        );
        assert_eq!(
            said[1],
            "It read the plan, the task, the change and the test output · not the talk, not who made it"
        );
        assert!(
            said[2].starts_with("✘")
                && said[2].ends_with(
                    "5.6 Luna checked Plan limits without knowing who made it, and sent it back"
                ),
            "{said:#?}"
        );
        assert!(said[3].ends_with("A fresh Opus 5.5 session checked Route tests without knowing who made it, and accepted it"), "{said:#?}");
        assert_eq!(
            said[4],
            "Same maker: Opus 5.5 made it too, as no other maker is set up · it may favour its own style"
        );
    }
}

mod workflow_task {
    use argonauts_core::events::session::SessionEvent;
    use argonauts_core::workflow::WorkflowRef;

    use super::tasks::{happen, planned, run, seated_three, session, task};
    use super::*;
    use crate::state::WorkflowState;

    /// Opus 5.5's task "Redis bucket", which Maker-checker does: its session works in the
    /// run's second workflow.
    fn maker_checker(bridge: &mut Bridge, model: &mut ViewModel) -> ModelRef {
        seated_three(bridge, model);
        let opus = ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()));
        let way = Way::Workflow {
            name: WorkflowRef::new("maker-checker"),
        };
        happen(
            bridge,
            model,
            2,
            planned(1, "Redis bucket", opus.clone(), way),
            None,
            0,
        );
        let mut build = session(opus.clone(), Some(1), "Build");
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut build {
            started.workflow_step = Some(StepRef {
                workflow: WorkflowId::numbered(2),
                phase: 0,
                path: vec![0],
            });
        }
        happen(bridge, model, 3, build, Some((1, 1)), 10);
        let state = bridge.state.runs.get_mut(&run()).expect("the run");
        let workflow = |n, name: &str, description: &str, parent| WorkflowState {
            id: WorkflowId::numbered(n),
            name: name.to_owned(),
            from: None,
            description: description.to_owned(),
            phases: vec!["Build".to_owned(), "Review".to_owned()],
            parent,
            steps: Vec::new(),
        };
        state
            .workflows
            .push(workflow(1, "council", "Seats talk, then work", None));
        state.workflows.push(workflow(
            2,
            "maker-checker",
            "A worker builds, a reviewer sends it back until it holds",
            Some(step()),
        ));
        opus
    }

    /// A task a workflow does names its workflow view's top: the way, whose task it is in which
    /// council, and the way's own line. A task in its seat's own session has none.
    #[test]
    fn a_workflow_tasks_view_says_whose_task_it_is() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        let opus = maker_checker(&mut bridge, &mut model);
        happen(
            &mut bridge,
            &mut model,
            4,
            planned(2, "Plan limits", opus, Way::OwnSession),
            None,
            0,
        );

        let state = &bridge.state.runs[&run()];
        let header = bridge
            .council
            .task_header(state, &task(1))
            .expect("the workflow's header");
        assert_eq!(header.workflow, WorkflowId::numbered(2));
        assert_eq!(header.seat, SeatId::numbered(1));
        assert_eq!(
            (header.way.as_str(), header.task.as_str()),
            ("Maker-checker", "Redis bucket")
        );
        assert_eq!(header.council, "Rate limit the API");
        assert_eq!(
            header.about,
            "A worker builds, a reviewer sends it back until it holds"
        );
        assert_eq!(bridge.council.task_header(state, &task(2)), None);
    }

    /// Enter on a picked task that a workflow does opens the run's view on that workflow.
    #[test]
    fn enter_on_a_workflow_task_opens_its_workflow() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        maker_checker(&mut bridge, &mut model);
        press(&mut bridge, &mut model, KeyCode::Up, true);
        assert_eq!(
            model.bottom.council.picking.as_deref(),
            Some("↑ ↓  to pick another · enter to open its workflow · esc to go back to the input")
        );
        let opened = press(&mut bridge, &mut model, KeyCode::Enter, false);
        assert_eq!(
            model.bottom.workflows.opening,
            Some(format!("{}#{}", run(), WorkflowId::numbered(2)))
        );
        assert!(
            opened.commands.iter().any(|command| matches!(
                command,
                argonauts_core::app::UiCommand::Request {
                    view: argonauts_core::app::ViewRequest::Workflow { run: asked, .. }
                } if *asked == run()
            )),
            "{:?}",
            opened.commands
        );
    }
}

mod talking {
    use argonauts_core::events::run::{RunLifecycle, RunOutcome};

    use super::tasks::*;
    use super::*;

    /// What you write goes to the council's seats while it talks, and as any other message
    /// once it agrees, stops or its run finishes.
    #[test]
    fn the_input_writes_to_the_council_only_while_it_talks() {
        let mut bridge = bridge();
        let mut model = ViewModel::default();
        seated_three(&mut bridge, &mut model);
        assert!(model.bottom.council.talking);
        assert!(
            model
                .bottom
                .council
                .say("Keep /health open.", None)
                .is_some()
        );

        send(
            &mut bridge,
            &mut model,
            2,
            CouncilEventKind::Agreed { points: 4 },
        );
        assert!(
            model.bottom.council.sits,
            "its messages can still be picked"
        );
        assert!(!model.bottom.council.talking);
        assert_eq!(model.bottom.council.say("Thanks.", None), None);

        send(
            &mut bridge,
            &mut model,
            3,
            message("m-9", Speaker::You, "Send it back: keep /health open."),
        );
        assert!(
            model.bottom.council.talking,
            "the plan went back to talk on"
        );

        let finished = RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        });
        happen(&mut bridge, &mut model, 4, finished, None, 60);
        assert!(!model.bottom.council.talking);
    }
}
