use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::{RunState as RunStatus, RunSummary};
use argonauts_core::app::{AppEvent, ChatEvent, ReplayTurn, TurnReply};
use argonauts_core::events::RetroEvent;
use argonauts_core::events::card::{AttemptCause, CardEvent, CardState, GateKind, GateOutcome};
use argonauts_core::events::run::{
    ChangedFile, CheckLine, ClosingAnswer, ParkReason, Party, PhaseChange, RunLifecycle, RunMode,
    RunOutcome, RunPhase, RunStarted,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::log::RETRO_FAILED;
use argonauts_core::names::{TurnId, WorkerRef};
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::{AgentEvent, CardId, Note, NoteLevel, PhaseStatus, RunId, Spend, Verbosity};
use time::macros::datetime;

use super::{ChatTurns, Transcript};
use crate::bridge::{Cx, Effects};
use crate::cell::{Cell, HistoryCell, ToolKind};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::{AppState, Replay};
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

struct Harness {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
}

impl Harness {
    fn new() -> Self {
        let mut transcript =
            Transcript::with_clock(WallClock::pinned(datetime!(2026-09-30 10:42 UTC)));
        transcript.chat = ChatTurns::with_tool_kinds(|_, name| match name {
            "Read" => ToolKind::Read,
            _ => ToolKind::Other,
        });
        Self {
            model: ViewModel::default(),
            state: AppState::default(),
            settings: UiSettings::default(),
            transcript,
        }
    }

    /// Folds the replay markers the way `AppState::apply` does, then offers the event.
    fn send(&mut self, event: AppEvent) {
        match &event {
            AppEvent::Attached { summary, .. } => {
                self.state.replay = Replay::Replaying {
                    run: summary.run.clone(),
                };
            }
            AppEvent::ReplayDone { .. } => self.state.replay = Replay::Live,
            _ => {}
        }
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.transcript.on_event(&event, &mut cx);
    }

    fn printed(&mut self) -> String {
        self.printed_at(80)
    }

    fn printed_at(&mut self, width: u16) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        plain_text(&self.model.history.drain_printable(&ctx))
    }
}

fn run_id() -> RunId {
    RunId::new("run-01").expect("a valid run id")
}

fn card_id() -> CardId {
    CardId::new("card-001").expect("a valid card id")
}

fn run_event(kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-09-30 10:00 UTC),
            run_id: run_id(),
            card_id: Some(card_id()),
            session: None,
            source: "conductor".into(),
            kind,
        }),
    }
}

fn note(level: NoteLevel, text: &str) -> AppEvent {
    run_event(RunEventKind::Note(Note {
        level,
        text: text.into(),
    }))
}

fn started() -> AppEvent {
    run_event(RunEventKind::Run(RunLifecycle::Started(Box::new(
        RunStarted {
            request: "add a --json flag".into(),
            title: "Add a --json flag".into(),
            mode: RunMode::Solo,
            forced: false,
            preset: "peer-crew".into(),
            estimate: None,
            triage: None,
            chat_turn: None,
            continues: None,
            repo_root: PathBuf::from("/repo"),
            base_branch: Some("main".into()),
            base_commit: "8b41d07aa".into(),
            uncommitted: vec![],
            uncommitted_included: false,
            settings_sha256: String::new(),
            settings: serde_json::Value::Null,
            overrides: vec![],
            harness_versions: Default::default(),
            version: "0.0.1".into(),
            front_end: None,
            instructions: Vec::new(),
        },
    ))))
}

fn gate(name: &str, passed: bool) -> AppEvent {
    run_event(RunEventKind::Gate(GateOutcome {
        name: name.into(),
        kind: GateKind::Command,
        passed,
        command: None,
        exit_code: None,
        duration_ms: 10,
        summary: String::new(),
        output_tail: String::new(),
        violations: vec![],
        sandboxed: true,
        skipped: false,
    }))
}

fn gated() -> AppEvent {
    run_event(RunEventKind::Card(CardEvent::State {
        from: Some(CardState::Gating),
        to: CardState::InReview,
        reason: String::new(),
    }))
}

fn paused() -> AppEvent {
    run_event(RunEventKind::Run(RunLifecycle::Paused {
        sessions_stopped: 1,
        committed: vec![card_id()],
    }))
}

fn chat_turn(n: u32, question: &str, answer: &str) -> Vec<AppEvent> {
    let turn = TurnId::numbered(n);
    let chat = |event| AppEvent::Chat { event };
    vec![
        chat(ChatEvent::TurnStarted {
            turn: turn.clone(),
            text: question.into(),
            worker: WorkerRef::new("claude-code", Some("haiku".into())),
            read_only: true,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        }),
        chat(ChatEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::ToolStarted {
                id: "tool-1".into(),
                name: "Read".into(),
                input: serde_json::json!({ "file_path": "src/cli.rs" }),
                parent: None,
            },
        }),
        chat(ChatEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::ToolFinished {
                id: "tool-1".into(),
                ok: true,
                output: None,
                parent: None,
            },
        }),
        chat(ChatEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::TextDelta {
                text: answer.into(),
                parent: None,
            },
        }),
        chat(ChatEvent::TurnFinished {
            turn,
            reply: TurnReply::Answer {
                markdown: answer.into(),
            },
            spend: Spend::default(),
            elapsed_ms: 3_000,
        }),
    ]
}

#[test]
fn replay_prints_summaries_only() {
    let mut harness = Harness::new();
    harness.send(AppEvent::Attached {
        summary: RunSummary {
            run: run_id(),
            title: "Add a --json flag".into(),
            mode: RunMode::Crew,
            preset: "peer-crew".into(),
            state: RunStatus::Parked,
            stopped_at: "plan checkpoint, waiting for you".into(),
            updated: datetime!(2026-09-30 10:00 UTC),
            spent: Spend::default(),
            sessions_to_resume: 2,
            waiting: vec![],
            live: false,
            remind: false,
            name: None,
        },
        started_here: false,
    });
    harness.send(started());
    for n in 0..400 {
        harness.send(note(NoteLevel::Info, &format!("replayed note {n}")));
        harness.send(gate("tests", true));
        harness.send(gated());
    }
    for event in chat_turn(1, "what does export do?", "It writes CSV.") {
        harness.send(event);
    }
    harness.send(paused());
    let kept = harness.model.history.cells().len();
    assert!(kept > 800, "every replayed item stays for Ctrl+O: {kept}");
    assert_eq!(
        harness.printed(),
        "",
        "nothing prints while the run replays"
    );

    harness.send(AppEvent::CondensedReplay {
        run: run_id(),
        turns: vec![
            ReplayTurn {
                from: Party::User,
                text: "add a --json flag".into(),
            },
            ReplayTurn {
                from: Party::Agent {
                    session: argonauts_core::names::SessionKey::parse("s-1")
                        .expect("a valid session key"),
                    worker: WorkerRef::new("claude-code", Some("opus".into())),
                },
                text: "I'll run it with the crew.".into(),
            },
        ],
        milestones: vec!["Goal approved · 3 criteria".into(), "Plan drafted".into()],
        recap: "The plan waits for you.".into(),
        recap_model: None,
    });
    harness.send(AppEvent::ReplayDone {
        run: run_id(),
        seq: 1_200,
    });
    let printed = harness.printed();
    assert_eq!(
        printed,
        "● Add a --json flag\n  ⎿  Parked · plan checkpoint, waiting for you · $0.00 spent · 2 sessions to\n     resume\n     ctrl+o shows the whole history\n\n❯ add a --json flag\n\n● I'll run it with the crew.\n\n● So far in this run\n  ⎿  Goal approved · 3 criteria\n     Plan drafted\n\n※ recap: The plan waits for you. (disable recaps in /config)"
    );
    assert!(!printed.contains("replayed note"));
    assert!(harness.model.history.cells().len() > kept);

    // Live events print again.
    harness.send(paused());
    assert!(harness.printed().contains("Paused the run"));
}

/// A run nobody could take a seat over in says so, not that every agent is out, and promises to
/// go on by itself only when a retry is set.
#[test]
fn a_park_nobody_could_take_a_seat_in_says_so_and_when_it_tries_again() {
    let parked = |until| {
        run_event(RunEventKind::Run(RunLifecycle::Parked {
            reason: ParkReason::NoStandIn {
                detail: "nobody could take over as the lead".into(),
                until,
            },
            waiting_on: Vec::new(),
        }))
    };
    let mut harness = Harness::new();
    harness.send(started());
    harness.send(parked(Some(datetime!(2026-09-30 10:47 UTC))));
    let retrying = harness.printed();
    assert!(
        retrying.contains("Parked the run · nobody could take over as the lead"),
        "{retrying}"
    );
    assert!(retrying.contains("It tries again at "), "{retrying}");
    assert!(retrying.contains("· /resume tries now"), "{retrying}");
    assert!(!retrying.contains("every agent is out"), "{retrying}");
    assert!(!retrying.contains("when one is back"), "{retrying}");

    harness.send(parked(None));
    let given_up = harness.printed();
    assert!(given_up.contains("/resume tries again"), "{given_up}");
    assert!(!given_up.contains("tries again at"), "{given_up}");
}

fn running_summary() -> RunSummary {
    RunSummary {
        run: run_id(),
        title: "Add a --json flag".into(),
        mode: RunMode::Crew,
        preset: "peer-crew".into(),
        state: RunStatus::Running,
        stopped_at: "working".into(),
        updated: datetime!(2026-09-30 10:00 UTC),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: vec![],
        live: true,
        remind: false,
        name: None,
    }
}

#[test]
fn a_run_started_here_prints_no_header() {
    let mut harness = Harness::new();
    harness.send(AppEvent::Attached {
        summary: running_summary(),
        started_here: true,
    });
    harness.send(AppEvent::ReplayDone {
        run: run_id(),
        seq: 0,
    });
    // The prompt that started it is right above, so a `● <title>` row would only repeat it.
    assert_eq!(harness.printed().trim(), "");

    // A run opened again with no history still gets its header.
    harness.send(AppEvent::Attached {
        summary: running_summary(),
        started_here: false,
    });
    harness.send(AppEvent::ReplayDone {
        run: run_id(),
        seq: 0,
    });
    assert!(
        harness
            .printed()
            .starts_with("● Add a --json flag\n  ⎿  Running · working · $0.00 spent"),
    );
}

#[test]
fn an_empty_gates_run_is_not_a_failed_gate() {
    let mut harness = Harness::new();
    // What the engine records when `gates.run` is empty: skipped, and no command ran.
    harness.send(run_event(RunEventKind::Gate(GateOutcome {
        name: "gates.run".into(),
        kind: GateKind::Command,
        passed: true,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary: "No command gates set · /config gates.run".into(),
        output_tail: String::new(),
        violations: vec![],
        sandboxed: false,
        skipped: true,
    })));
    harness.send(gated());
    let printed = harness.printed();
    assert!(
        printed.contains("built-in checks)\n  ⎿  No command gates set · /config gates.run"),
        "{printed}"
    );
    assert!(!printed.contains("failed"), "{printed}");
}

fn retro_phase(status: PhaseStatus, summary: Option<&str>) -> AppEvent {
    run_event(RunEventKind::Phase(PhaseChange {
        phase: RunPhase::Retro,
        status,
        summary: summary.map(Into::into),
    }))
}

#[test]
fn a_failed_retro_is_said_once_and_not_called_saved() {
    let mut harness = Harness::new();
    harness.send(started());
    harness.send(retro_phase(PhaseStatus::Started, None));
    harness.send(run_event(RunEventKind::Retro(RetroEvent::Scored {
        card: card_id(),
        metric: "p_pass_gates_first_try".into(),
        predicted: 0.8,
        outcome: true,
        brier: 0.04,
    })));
    harness.send(retro_phase(PhaseStatus::Failed, Some(SCHEMA_MISS)));
    harness.send(note(NoteLevel::Warn, RETRO_FAILED));
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = harness.printed();
    assert_eq!(printed.matches("The retro failed").count(), 1, "{printed}");
    assert!(printed.contains("Run finished"), "{printed}");
    assert!(!printed.contains("retro saved"), "{printed}");
}

fn closing() -> ClosingAnswer {
    let file = |path: &str, insertions| ChangedFile {
        path: path.to_owned(),
        insertions,
        deletions: 1,
    };
    ClosingAnswer {
        text: "Fixed parse() and format_cents() in invoice/money.py, and all six tests pass."
            .to_owned(),
        by: Some("claude-code/haiku".parse().unwrap()),
        files: (1..=6).map(|n| file(&format!("src/f{n}.py"), n)).collect(),
        checks: vec![CheckLine {
            what: "tests".to_owned(),
            passed: true,
            result: "6 passed".to_owned(),
        }],
        tests: vec!["python3 -m unittest: OK".to_owned()],
        left: vec![
            "format_cents() still rounds half down.".to_owned(),
            "No test covers huge amounts.".to_owned(),
            "The README is out of date.".to_owned(),
        ],
    }
}

fn ended_with_closing(verbosity: Verbosity) -> String {
    let mut harness = Harness::new();
    harness.settings.verbosity = verbosity;
    harness.send(started());
    harness.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(closing()),
    ))));
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    harness.printed()
}

#[test]
fn a_run_ends_with_its_closing_answer_under_the_merge() {
    let printed = ended_with_closing(Verbosity::Normal);
    let merged = printed.find("Merged the run").expect(&printed);
    let answer = printed.find("● Fixed parse()").expect(&printed);
    let end = printed.find(" for ").expect(&printed);
    assert!(merged < answer && answer < end, "{printed}");
    for line in [
        "Changed src/f1.py +1 -1, src/f2.py +2 -1",
        "and 2 more",
        "Checked tests: 6 passed",
        "Left format_cents() still rounds half down; No test",
    ] {
        assert!(printed.contains(line), "{line}\n{printed}");
    }
    assert!(
        !printed.contains("README"),
        "normal keeps two of what's left: {printed}"
    );
    assert!(!printed.contains("Tests run"), "{printed}");
}

#[test]
fn quiet_keeps_the_closing_words_and_full_keeps_every_list() {
    let quiet = ended_with_closing(Verbosity::Quiet);
    assert!(quiet.contains("Fixed parse()"), "{quiet}");
    assert!(!quiet.contains("Changed"), "{quiet}");
    let full = ended_with_closing(Verbosity::Full);
    for line in [
        "src/f6.py +6 -1",
        "Tests run python3 -m unittest: OK",
        "README",
    ] {
        assert!(full.contains(line), "{line}\n{full}");
    }
}

const SCHEMA_MISS: &str =
    "step: output didn't match the schema: [\"Additional properties are not allowed\"]";

/// A schema miss is plumbing: the main view says the retro failed in plain words, and the
/// reason waits for Ctrl+O.
#[test]
fn a_failed_retro_keeps_its_raw_reason_for_ctrl_o() {
    let mut harness = Harness::new();
    harness.send(started());
    harness.send(retro_phase(PhaseStatus::Started, None));
    harness.send(retro_phase(PhaseStatus::Failed, Some(SCHEMA_MISS)));
    harness.send(note(NoteLevel::Warn, RETRO_FAILED));
    let printed = harness.printed();
    assert!(
        printed.contains("● The retro failed. The run's result stands. (ctrl+o to expand)"),
        "{printed}"
    );
    assert!(!printed.contains("schema"), "{printed}");

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now())).with_expanded(true);
    let expanded: String = harness
        .model
        .history
        .cells()
        .iter()
        .map(|cell| plain_text(&cell.lines(&ctx)))
        .collect();
    assert!(
        expanded.contains("⎿  step: output didn't match the schema"),
        "{expanded}"
    );
    assert!(!expanded.contains("ctrl+o to expand"), "{expanded}");
}

/// A run's note says what happened in its first line, and the lines after it, such as what the
/// validator said of the lead's design, wait for Ctrl+O.
#[test]
fn a_notes_detail_waits_for_ctrl_o() {
    let mut harness = Harness::new();
    harness.send(started());
    let incomplete = Note::with_detail(
        NoteLevel::Warn,
        "The lead's design came back incomplete, so coordinated-crew runs as written.",
        "output didn't match the schema:\n\"based_on\" is a required property",
    );
    harness.send(note(NoteLevel::Warn, &incomplete.text));
    let printed = harness.printed();
    assert!(
        printed.contains(
            "● The lead's design came back incomplete, so coordinated-crew runs as written."
        ),
        "{printed}"
    );
    assert!(!printed.contains("schema"), "{printed}");
    assert!(!printed.contains("required property"), "{printed}");

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let drawn = |ctx: &RenderCtx<'_>| -> String {
        harness
            .model
            .history
            .cells()
            .iter()
            .map(|cell| plain_text(&cell.lines(ctx)))
            .collect()
    };
    // Where the hint fits beside the line, it says Ctrl+O shows more.
    let wide = drawn(&RenderCtx::new(120, &theme, Clock::still(Instant::now())));
    assert!(
        wide.contains("runs as written. (ctrl+o to expand)"),
        "{wide}"
    );
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now())).with_expanded(true);
    let expanded = drawn(&ctx);
    assert!(
        expanded.contains("⎿  output didn't match the schema:"),
        "{expanded}"
    );
    assert!(
        expanded.contains("\"based_on\" is a required property"),
        "{expanded}"
    );
}

/// Another run's failed retro is said again, though its note is word for word the same.
#[test]
fn each_runs_failed_retro_is_said() {
    let mut harness = Harness::new();
    for _ in 0..2 {
        harness.send(started());
        harness.send(retro_phase(PhaseStatus::Failed, None));
        harness.send(note(NoteLevel::Warn, RETRO_FAILED));
        harness.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        })));
    }
    let printed = harness.printed();
    assert_eq!(printed.matches("The retro failed").count(), 2, "{printed}");
    assert!(!printed.contains("ctrl+o"), "{printed}");
}

#[test]
fn warning_notice_shows_once() {
    let mut harness = Harness::new();
    let warning = "An always-on agent is running · approvals stay yours";
    harness.send(AppEvent::Notice {
        level: NoteLevel::Warn,
        text: warning.into(),
    });
    harness.send(note(NoteLevel::Warn, warning));
    harness.send(AppEvent::Notice {
        level: NoteLevel::Warn,
        text: format!("  {warning}  "),
    });
    harness.send(AppEvent::Notice {
        level: NoteLevel::Info,
        text: "Saved.".into(),
    });
    harness.send(AppEvent::Notice {
        level: NoteLevel::Info,
        text: "Saved.".into(),
    });
    let printed = harness.printed();
    assert_eq!(printed.matches(warning).count(), 1, "{printed}");
    assert_eq!(printed.matches("Saved.").count(), 2, "{printed}");
}

fn chatter() -> Vec<AppEvent> {
    let mut events = vec![
        started(),
        note(NoteLevel::Info, "Worktree ready"),
        AppEvent::Narrator {
            line: NarratorLine {
                ts: datetime!(2026-09-30 10:05 UTC),
                run: Some(run_id()),
                kind: NarratorLineKind::PhaseEnd {
                    phase: argonauts_core::events::run::RunPhase::Goal,
                },
                text: "The goal is settled.".into(),
                model: WorkerRef::new("claude-code", Some("haiku".into())),
                spend: Spend::default(),
            },
        },
        AppEvent::Recap {
            text: "Work goes on.".into(),
            model: None,
        },
        gate("tests", false),
        gated(),
        paused(),
    ];
    events.extend(chat_turn(1, "hi", "Hello."));
    events
}

#[test]
fn quiet_shows_milestones_only() {
    let mut quiet = Harness::new();
    quiet.settings.verbosity = Verbosity::Quiet;
    for event in chatter() {
        quiet.send(event);
    }
    let printed = quiet.printed();
    for shown in [
        "● Triage(solo, checked)",
        "Gates(tests)",
        "Paused the run",
        "❯ hi",
        "Hello.",
    ] {
        assert!(printed.contains(shown), "{shown} missing from:\n{printed}");
    }
    for hidden in ["Worktree ready", "narrator:", "recap:", "Read 1 file"] {
        assert!(
            !printed.contains(hidden),
            "{hidden} shows at quiet:\n{printed}"
        );
    }

    // `normal` folds the read into one line, and `full` shows it with its result.
    for (level, call) in [
        (Verbosity::Normal, "Read 1 file"),
        (Verbosity::Full, "Read(src/cli.rs)"),
    ] {
        let mut harness = Harness::new();
        harness.settings.verbosity = level;
        for event in chatter() {
            harness.send(event);
        }
        let printed = harness.printed();
        for shown in ["Worktree ready", "narrator:", "recap:", call, "Triage"] {
            assert!(printed.contains(shown), "{shown} missing at {level:?}");
        }
    }
}

fn notice(level: NoteLevel, text: &str) -> AppEvent {
    AppEvent::Notice {
        level,
        text: text.into(),
    }
}

fn welcome_with(agent: &str, preset: &str) -> AppEvent {
    AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/github/argonauts".into(),
        preset: preset.into(),
        agents: vec![],
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        default_agent: Some(WorkerRef::new(agent, None)),
        effort: None,
    }
}

#[test]
fn a_repeated_welcome_prints_one_header() {
    let mut harness = Harness::new();
    harness.send(welcome_with("claude-code", "peer-crew"));
    harness.send(welcome_with("claude-code", "peer-crew"));
    harness.send(note(NoteLevel::Warn, "codex is logged out"));
    let cells = harness.model.history.cells();
    assert!(
        matches!(
            cells,
            [crate::cell::Cell::Header(_), crate::cell::Cell::Notice(_)]
        ),
        "{cells:?}"
    );
}

fn headers(harness: &Harness) -> usize {
    harness
        .model
        .history
        .cells()
        .iter()
        .filter(|cell| matches!(cell, Cell::Header(_)))
        .count()
}

/// Once the setup saves, the header changes where it stands instead of printing a second one
/// under the setup's answers. A warning it repeats shows once.
#[test]
fn the_header_changes_in_place_when_the_setup_saves() {
    let mut harness = Harness::new();
    harness.send(welcome_with("claude-code", "peer-crew"));
    harness.send(notice(NoteLevel::Warn, "a typo in your settings"));
    harness.printed();
    harness.send(welcome_with("antigravity-cli", "solo"));
    harness.send(notice(NoteLevel::Warn, "a typo in your settings"));

    assert_eq!(harness.printed(), "", "nothing prints under the old header");
    let redrawn = harness
        .model
        .history
        .take_header_redrawn()
        .expect("the header, redrawn");
    let redrawn = plain_text(&redrawn);
    assert!(redrawn.contains("Antigravity CLI · solo"), "{redrawn}");
    assert!(!redrawn.contains("Claude Code"), "{redrawn}");
    assert_eq!(headers(&harness), 1, "the transcript keeps one header");
}

/// After the conversation starts, the header still changes where it stands rather than
/// printing again under the chat.
#[test]
fn a_header_after_the_conversation_starts_never_prints_twice() {
    let mut harness = Harness::new();
    harness.send(welcome_with("claude-code", "peer-crew"));
    harness.send(AppEvent::Report {
        title: "Doctor".into(),
        lines: vec!["git 2.50.1".into()],
    });
    harness.printed();
    harness.send(welcome_with("codex", "solo"));
    let printed = harness.printed();
    assert!(!printed.contains("Codex · solo"), "{printed}");
    let redrawn = harness.model.history.take_header_redrawn();
    assert!(
        redrawn.is_some_and(|lines| plain_text(&lines).contains("Codex · solo")),
        "the header on screen changes"
    );
    assert_eq!(headers(&harness), 1);

    // The same header again changes nothing.
    harness.send(welcome_with("codex", "solo"));
    harness.printed();
    assert_eq!(harness.model.history.take_header_redrawn(), None);
}

#[test]
fn the_header_waits_for_the_default_agent() {
    use argonauts_core::app::StatusLine;
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::ui::NarratorMode;

    let mut harness = Harness::new();
    harness.send(AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/github/argonauts".into(),
        preset: "peer-crew".into(),
        agents: vec![],
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        default_agent: None,
        effort: None,
    });
    assert!(harness.model.history.cells().is_empty());
    assert_eq!(harness.model.history.active_cells().count(), 1);
    harness.send(AppEvent::Status {
        status: StatusLine {
            title: "argonauts".into(),
            repo: "~/github/argonauts".into(),
            preset: "peer-crew".into(),
            all_hands: false,
            run: None,
            default_agent: WorkerRef::new("claude-code", Some("haiku".into())),
            quotas: vec![],
            spent: None,
            limit: None,
            narrator: NarratorMode::OnDemand,
            approval_mode: ApprovalMode::Supervised,
        },
    });
    assert_eq!(harness.model.history.active_cells().count(), 0);
    let printed = harness.printed();
    assert!(
        printed.contains("Claude Code · Haiku · peer-crew"),
        "{printed}"
    );
}

/// The app's names come first, so the header reads like Claude Code's, with the model an
/// alias runs: `Claude Code · Opus 5.5 with xhigh effort · peer-crew`.
#[test]
fn a_welcome_naming_the_agent_and_effort_prints_the_header_at_once() {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::model_catalog::{CatalogEntry, ModelCatalog};

    let mut opus = CatalogEntry::new("claude-code/claude-opus-5-5".parse().unwrap());
    opus.aliases = Some(vec!["opus".into()]);
    let names = DisplayNames {
        catalog: ModelCatalog {
            models: vec![opus],
            ..ModelCatalog::default()
        },
        ..DisplayNames::default()
    };
    let mut harness = Harness::new();
    harness.state.apply(&AppEvent::Names {
        names: Box::new(names),
    });
    harness.send(AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/github/argonauts".into(),
        preset: "peer-crew".into(),
        agents: vec![],
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        default_agent: Some(WorkerRef::new("claude-code", Some("opus".into()))),
        effort: Some("xhigh".into()),
    });
    assert_eq!(harness.model.history.active_cells().count(), 0);
    let printed = harness.printed();
    assert!(
        printed.contains("Claude Code · Opus 5.5 with xhigh effort · peer-crew"),
        "{printed}"
    );
}

#[test]
fn a_queued_prompt_gets_its_own_echo() {
    let mut harness = Harness::new();
    // The composer echoed the first prompt, grey until its turn starts.
    let pending = harness
        .model
        .history
        .push_active(crate::cell::UserMessage::pending("hi"));
    for event in chat_turn(1, "hi", "Hello.") {
        harness.send(event);
    }
    assert!(harness.model.history.active(pending).is_none());
    for event in chat_turn(2, "and then?", "Then it ends.") {
        harness.send(event);
    }
    let printed = harness.printed();
    assert_eq!(printed.matches("❯ hi").count(), 1, "{printed}");
    assert_eq!(printed.matches("❯ and then?").count(), 1, "{printed}");
}

/// The input drew the keywords of the CLI the screen knew, but the echo keeps those of the CLI
/// that took the turn, as when a fallback or another conversation's agent takes it.
#[test]
fn the_echo_draws_the_keywords_of_the_cli_that_took_the_turn() {
    use argonauts_core::keywords::{Keyword, KeywordLook};

    let ultrathink = Keyword::new("ultrathink", KeywordLook::Rainbow, true);
    for (worker, kept) in [("claude-code", true), ("codex", false)] {
        let mut harness = Harness::new();
        harness.state.vendor_commands = Some(
            crate::commands::tests::claude_commands()
                .with_keywords(std::slice::from_ref(&ultrathink)),
        );
        harness.model.history.push_active(
            crate::cell::UserMessage::pending("please ultrathink")
                .with_keywords(std::slice::from_ref(&ultrathink)),
        );
        harness.send(AppEvent::Chat {
            event: ChatEvent::TurnStarted {
                turn: TurnId::numbered(1),
                text: "please ultrathink".into(),
                worker: WorkerRef::new(worker, None),
                read_only: false,
                commit: "8b41d07".into(),
                uncommitted: vec![],
                settings_sha256: None,
                continues: false,
            },
        });
        let echoed = harness
            .model
            .history
            .cells()
            .iter()
            .find_map(|cell| match cell {
                Cell::User(message) => Some(message),
                _ => None,
            })
            .expect("the echo");
        assert_eq!(echoed.keywords.is_empty(), !kept, "{worker}");
        assert!(!echoed.pending);
    }
}

#[test]
fn a_notice_before_the_turn_starts_prints_the_prompt_once() {
    let mut harness = Harness::new();
    harness
        .model
        .history
        .push_active(crate::cell::UserMessage::pending("hi"));
    harness.send(note(NoteLevel::Warn, "codex is logged out"));
    for event in chat_turn(1, "hi", "Hello.") {
        harness.send(event);
    }
    let printed = harness.printed();
    assert_eq!(printed.matches("❯ hi").count(), 1, "{printed}");
    assert!(
        printed.find("codex is logged out") < printed.find("❯ hi"),
        "{printed}"
    );
}

#[test]
fn a_whole_message_after_deltas_is_not_repeated() {
    let mut harness = Harness::new();
    let turn = TurnId::numbered(1);
    let mut events = chat_turn(1, "hi", "Hello there.");
    let finished = events.pop().expect("the turn's end");
    events.push(AppEvent::Chat {
        event: ChatEvent::Agent {
            turn,
            event: AgentEvent::Message {
                text: "Hello there.".into(),
                parent: None,
            },
        },
    });
    events.push(finished);
    for event in events {
        harness.send(event);
    }
    let printed = harness.printed();
    assert_eq!(printed.matches("Hello there.").count(), 1, "{printed}");
}

#[test]
fn status_answers_print_as_one_item() {
    use argonauts_core::api::{CardBrief, StatusSummary};

    let mut harness = Harness::new();
    let summary = StatusSummary {
        run: Some(run_id()),
        goal: vec![
            "Add a --json flag".into(),
            "G1 export --json writes JSON lines".into(),
        ],
        plan: Some("Plan 1, 2 cards".into()),
        cards: vec![CardBrief {
            card: card_id(),
            title: "Parse the flag".into(),
            state: CardState::Running,
            owner: Some(WorkerRef::new("codex", Some("gpt-5.6".into()))),
            note: "6m 12s · 38k tok".into(),
        }],
        recent: (1..=8).map(|n| format!("event {n}")).collect(),
        waiting: vec!["plan checkpoint".into()],
        ..StatusSummary::default()
    };
    harness.send(AppEvent::View {
        data: Box::new(argonauts_core::app::ViewData::Status { summary }),
    });
    assert_eq!(
        harness.printed(),
        "● Status(run-01)\n  ⎿  Goal: Add a --json flag\n     G1 export --json writes JSON lines\n     Plan: Plan 1, 2 cards\n     card-001 Parse the flag · running · GPT 5.6 on Codex · 6m 12s · 38k tok\n     event 4\n     event 5\n     event 6\n     event 7\n     event 8\n     Waiting on you: plan checkpoint"
    );
    let empty = super::status_cells(&StatusSummary::default(), &Default::default());
    assert_eq!(empty.len(), 1);
}

/// A greeting answered the way the chat agent does: it checks the run status, streams the
/// answer, then sends the report.
fn greeting_with_own_calls() -> Vec<AppEvent> {
    let turn = TurnId::numbered(1);
    let answer = "Hi! What would you like to do?";
    let agent = |event| AppEvent::Chat {
        event: ChatEvent::Agent {
            turn: turn.clone(),
            event,
        },
    };
    let call = |id: &str, name: &str, input: serde_json::Value| {
        [
            agent(AgentEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input,
                parent: None,
            }),
            agent(AgentEvent::ToolFinished {
                id: id.into(),
                ok: true,
                output: Some("ok".into()),
                parent: None,
            }),
        ]
    };
    let mut events = vec![AppEvent::Chat {
        event: ChatEvent::TurnStarted {
            turn: turn.clone(),
            text: "hi".into(),
            worker: WorkerRef::new("claude-code", Some("haiku".into())),
            read_only: true,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        },
    }];
    events.extend(call(
        "status-1",
        "mcp__argonauts_status__argonauts_status",
        serde_json::json!({}),
    ));
    events.push(agent(AgentEvent::TextDelta {
        text: "Hi! ".into(),
        parent: None,
    }));
    events.extend(call(
        "status-2",
        "mcp__argonauts_status__argonauts_status",
        serde_json::json!({}),
    ));
    events.push(agent(AgentEvent::TextDelta {
        text: "What would you like to do?".into(),
        parent: None,
    }));
    events.extend(call(
        "report-1",
        "StructuredOutput",
        serde_json::json!({ "status": "done", "answer_markdown": answer }),
    ));
    events.push(AppEvent::Chat {
        event: ChatEvent::TurnFinished {
            turn,
            reply: TurnReply::Answer {
                markdown: answer.into(),
            },
            spend: Spend::default(),
            elapsed_ms: 6_000,
        },
    });
    events
}

#[test]
fn argonauts_own_calls_stay_out_of_the_transcript() {
    for level in [Verbosity::Full, Verbosity::Normal, Verbosity::Quiet] {
        let mut harness = Harness::new();
        harness.settings.verbosity = level;
        for event in greeting_with_own_calls() {
            harness.send(event);
        }
        let printed = harness.printed();
        assert!(!printed.contains("Report"), "{level:?}: {printed}");
        assert!(
            !printed.contains("argonauts_status"),
            "{level:?}: {printed}"
        );
        // The calls don't split the answer, so it prints whole, once.
        assert_eq!(
            printed.matches("Hi! What would you like to do?").count(),
            1,
            "{level:?}: {printed}"
        );

        // Ctrl+O shows them done, the report after the answer it sent.
        let cells = harness.model.history.cells();
        let titles: Vec<String> = cells
            .iter()
            .filter_map(|cell| match cell {
                Cell::Tool(call) => Some(call.title.plain()),
                _ => None,
            })
            .collect();
        assert_eq!(titles.len(), 3, "{level:?}: {titles:?}");
        assert_eq!(titles.last().map(String::as_str), Some("Report(done)"));
        let answer = cells
            .iter()
            .position(|cell| matches!(cell, Cell::Assistant(_)))
            .expect("the answer");
        let report = cells
            .iter()
            .position(
                |cell| matches!(cell, Cell::Tool(call) if call.title.plain() == "Report(done)"),
            )
            .expect("the report");
        assert!(answer < report, "{level:?}");
        assert!(cells.iter().all(|cell| match cell {
            Cell::Tool(call) => !call.is_running(),
            _ => true,
        }));
    }
}

#[test]
fn an_own_call_still_running_when_the_turn_ends_is_kept_stopped() {
    let mut harness = Harness::new();
    let mut events = greeting_with_own_calls();
    // Drop the report's end.
    let report_end = events
        .iter()
        .position(|event| {
            matches!(event, AppEvent::Chat { event: ChatEvent::Agent { event: AgentEvent::ToolFinished { id, .. }, .. } } if id == "report-1")
        })
        .expect("the report's end");
    events.remove(report_end);
    for event in events {
        harness.send(event);
    }
    assert!(!harness.printed().contains("Report"));
    let report = harness
        .model
        .history
        .cells()
        .iter()
        .find_map(|cell| match cell {
            Cell::Tool(call) if call.title.plain() == "Report(done)" => Some(call),
            _ => None,
        })
        .expect("kept for Ctrl+O");
    assert!(!report.is_running());
}

#[test]
fn a_rotation_prints_a_line() {
    let mut harness = Harness::new();
    harness.send(started());
    harness.send(run_event(RunEventKind::Card(CardEvent::Attempt {
        attempt: 2,
        worker: WorkerRef::new("claude-code", None),
        cause: AttemptCause::Rotation,
    })));
    let printed = harness.printed();
    assert!(
        printed.contains("card-001 rotated to a fresh Claude Code session"),
        "{printed}"
    );
}

#[test]
fn a_model_you_pick_is_named_as_its_list_named_it() {
    let mut harness = Harness::new();
    let choice = |worker: &str, name: &str| argonauts_core::app::ModelChoice {
        worker: worker.parse().unwrap(),
        family: "gpt".into(),
        access: argonauts_core::registry::ModelAccess::Subscription,
        cost: argonauts_core::limits::CostEstimate::Unknown,
        quota: None,
        efforts: Vec::new(),
        strengths: Vec::new(),
        current: false,
        note: None,
        older: false,
        name: name.to_owned(),
        description: None,
        recommended: false,
        new: false,
        default_effort: None,
        effort: None,
        alias: false,
        runs: None,
    };
    harness.state.model_words.insert(
        "codex/gpt-6-sol".to_owned(),
        crate::format::choice_words(&choice("codex/gpt-6-sol", "GPT-6-Sol")),
    );
    harness.send(AppEvent::SettingChanged {
        key: "default_agent".into(),
        value: "\"codex/gpt-6-sol\"".into(),
        scope: argonauts_core::settings::SettingScope::Global,
        previous: Some("\"claude-code/claude-opus-5-5\"".into()),
        reset: false,
    });
    assert_eq!(
        harness.printed().trim_end(),
        "  ⎿  Set model to GPT-6-Sol · everywhere · was Opus 5.5"
    );
}

/// `/effort`'s All hands toggle reads as a result line, not as its key.
#[test]
fn all_hands_saved_reads_as_turned_on_or_off() {
    for (value, scope, line) in [
        (
            "true",
            argonauts_core::settings::SettingScope::ProjectLocal,
            "  ⎿  Turned All hands on · just you",
        ),
        (
            "false",
            argonauts_core::settings::SettingScope::Session,
            "  ⎿  Turned All hands off · this session",
        ),
    ] {
        let mut harness = Harness::new();
        harness.send(AppEvent::SettingChanged {
            key: "triage.all_hands".into(),
            value: value.into(),
            scope,
            previous: None,
            reset: false,
        });
        assert_eq!(harness.printed().trim_end(), line);
    }
}

/// The triage block says where the run starts in words, with no commit hash or setting key.
#[test]
fn triage_says_where_the_run_starts_in_words() {
    let mut harness = Harness::new();
    harness.send(started());
    let printed = harness.printed();
    assert!(printed.contains("● Triage(solo, checked)"), "{printed}");
    assert!(printed.contains("Starts from main"), "{printed}");
    assert!(
        printed.contains("Goal: your prompt as you wrote it"),
        "{printed}"
    );
    for plumbing in ["8b41d07", "Recorded commit", "solo.restate_first"] {
        assert!(!printed.contains(plumbing), "{plumbing} in:\n{printed}");
    }
}

/// `● Resume(<title>)` names the run as people know it.
#[test]
fn resume_names_the_run_by_its_title() {
    use argonauts_core::events::run::{ResumeCause, RunResumed};

    let mut harness = Harness::new();
    let start = started();
    harness.state.apply(&start);
    harness.send(start);
    harness.printed();
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Resumed(
        RunResumed {
            cause: ResumeCause::User,
            settings_changed: false,
            harness_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        },
    ))));
    let printed = harness.printed();
    assert!(printed.contains("● Resume(Add a --json flag)"), "{printed}");
    assert!(!printed.contains("run-01"), "{printed}");
}

/// A card's wait prints nothing, so your answer moving the run on prints nothing either, unless
/// something changed while it waited.
#[test]
fn an_answer_resumes_the_run_without_a_line() {
    use argonauts_core::events::run::{ResumeCause, RunResumed};

    let resumed = |settings_changed: bool| {
        run_event(RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::Answer,
            settings_changed,
            harness_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        })))
    };
    let mut harness = Harness::new();
    let start = started();
    harness.state.apply(&start);
    harness.send(start);
    harness.printed();
    harness.send(resumed(false));
    assert_eq!(harness.printed().trim(), "");
    harness.send(resumed(true));
    let printed = harness.printed();
    assert!(printed.contains("● Resume(Add a --json flag)"), "{printed}");
    assert!(printed.contains("After your answer"), "{printed}");
    assert!(
        printed.contains("Settings changed since it stopped"),
        "{printed}"
    );
}

/// What the merge card put up for the merge, as `RunState` keeps it.
fn merge_opened(cards: usize, combined: argonauts_core::DiffStat) -> AppEvent {
    use argonauts_core::events::card::CardKind;
    use argonauts_core::events::checkpoint::{
        CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket, FreeText, MergePacket,
        MergeRow,
    };
    use argonauts_core::names::CheckpointId;
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

    let rows = (1..=cards)
        .map(|n| MergeRow {
            card: CardId::new(format!("card-{n:03}")).unwrap(),
            title: format!("Card {n}"),
            kind: CardKind::Build,
            stat: argonauts_core::DiffStat {
                files_changed: 1,
                insertions: 3,
                deletions: 0,
            },
            owner: None,
            gates_passed: true,
            review: None,
            debt: Vec::new(),
        })
        .collect();
    let packet = MergePacket {
        target: Some("main".into()),
        branch: "argonauts/run-01/integration".into(),
        rows,
        suite_passed_on_latest_main: true,
        gates: Vec::new(),
        combined,
        conductor: Default::default(),
        diff_path: PathBuf::from("/nonexistent/run-01.diff"),
        tree_sha: "abc".into(),
        spent: Spend::default(),
        limit: None,
        partial: false,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
    };
    run_event(RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(
        CheckpointOpened {
            id: CheckpointId::numbered(1),
            kind: CheckpointKind::Merge,
            title: "Merge this run into main?".into(),
            card: None,
            packet: CheckpointPacket::Merge(packet),
            evidence_sha256: "0".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::Guidance,
            needs_you: true,
        },
    ))))
}

/// The line after a merge says what the merge card said, and the spend the mode line shows.
#[test]
fn the_finish_line_says_what_the_merge_card_said() {
    let mut harness = Harness::new();
    let fold = |harness: &mut Harness, event: AppEvent| {
        harness.state.apply(&event);
        harness.send(event);
    };
    fold(&mut harness, started());
    // More cards than the merge took, with their own diffs.
    for n in 1..=10 {
        let mut event = run_event(RunEventKind::Card(CardEvent::Diff {
            stat: argonauts_core::DiffStat {
                files_changed: 1,
                insertions: 2,
                deletions: 0,
            },
            path: PathBuf::from("/nonexistent/card.diff"),
        }));
        if let AppEvent::Run { event } = &mut event {
            event.card_id = Some(CardId::new(format!("card-{n:03}")).unwrap());
        }
        fold(&mut harness, event);
    }
    let combined = argonauts_core::DiffStat {
        files_changed: 13,
        insertions: 78,
        deletions: 1,
    };
    fold(&mut harness, merge_opened(8, combined));
    // A session reports more than the budget has counted yet.
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        "claude-code".parse().unwrap(),
        "claude-code",
        argonauts_core::names::RoleName::Owner,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.cost_usd = Some(0.26);
    let run = harness.state.runs.get_mut(&run_id()).unwrap();
    run.sessions.insert(session.key.clone(), session);
    fold(
        &mut harness,
        run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        })),
    );
    let printed = harness.printed();
    assert!(
        printed.contains("Merged the run into main (8 cards, 13 files, +78 -1)"),
        "{printed}"
    );
    let run = &harness.state.runs[&run_id()];
    let spent = format!("· {}", crate::format::usd(run.cost_usd()));
    let finished = printed
        .lines()
        .find(|line| line.contains("Run finished"))
        .unwrap();
    assert!(finished.contains(&spent), "{printed}");
    assert!(finished.contains("$0.26"), "{printed}");
}

#[test]
fn a_shell_command_runs_live_and_prints_once_it_ends() {
    use argonauts_core::app::ShellEvent;

    let shell = |event| AppEvent::Shell { event };
    let mut h = Harness::new();
    h.send(shell(ShellEvent::Started {
        id: 1,
        command: "echo hi".into(),
    }));
    assert_eq!(h.printed(), "", "it runs in the live region");
    assert_eq!(h.model.history.active_cells().count(), 1);
    h.send(shell(ShellEvent::Output {
        id: 1,
        text: "hi\n".into(),
    }));
    h.send(shell(ShellEvent::Output {
        id: 2,
        text: "another command's\n".into(),
    }));
    h.send(shell(ShellEvent::Finished {
        id: 1,
        exit_code: Some(0),
        stopped: false,
    }));
    assert_eq!(h.model.history.active_cells().count(), 0);
    assert_eq!(h.printed(), "! echo hi\n  ⎿  hi");

    h.send(shell(ShellEvent::Started {
        id: 2,
        command: "sleep 25".into(),
    }));
    h.send(shell(ShellEvent::Finished {
        id: 2,
        exit_code: None,
        stopped: true,
    }));
    assert_eq!(h.printed(), "\n! sleep 25\n  ⎿  Stopped");
}

#[test]
fn the_goal_line_counts_the_leads_answers_apart() {
    use argonauts_core::events::goal::{
        Assumption, Criterion, GoalEvent, GoalFile, GoalSource, LEAD_ANSWER,
    };

    let mut harness = Harness::new();
    harness.send(started());
    harness.send(note(
        NoteLevel::Info,
        "The lead answered 1 of the goal's questions for you. Its answers are assumptions in \
         the goal, which Ctrl+O and /status show",
    ));
    let assumption = |text: &str, basis: String| Assumption {
        text: text.into(),
        basis,
        by: None,
    };
    harness.send(run_event(RunEventKind::Goal(GoalEvent::Approved {
        file: GoalFile {
            run: run_id(),
            version: 1,
            summary: "Add a --verbose flag".into(),
            criteria: vec![Criterion::new("G1", "--verbose logs at trace")],
            in_scope: vec![],
            out_of_scope: vec![],
            assumptions: vec![
                assumption(
                    "Which log level: trace",
                    format!("{LEAD_ANSWER}: the README"),
                ),
                assumption("the flag's name: --verbose", "a detail".into()),
            ],
            answers: vec![],
            source: GoalSource::Restatements,
            path: PathBuf::from("goal.md"),
            sha256: String::new(),
        },
        by: Party::User,
    })));
    let printed = harness.printed();
    assert!(
        printed.contains("The lead answered 1 of the goal's questions for you"),
        "{printed}"
    );
    assert!(
        printed.contains("1 question answered by the lead · 1 assumption stated"),
        "{printed}"
    );
}

/// A crew run named `fix-slugify-median` that declared its workflow, ready to end.
fn crew_workflow_harness() -> Harness {
    use argonauts_core::events::run::RunNamed;
    use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::workflow::StandardPhase;

    let mut harness = Harness::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "peer-crew".into(),
        from: None,
        description: String::new(),
        phases: vec!["Fix".into(), "Merge".into()],
        parent: None,
        steps: Vec::new(),
    });
    run.named = Some(RunNamed {
        name: "fix-slugify-median".into(),
        description: "Fix slugify's dashes and median's even and empty lists".into(),
        style: RunStyle::Workflow,
        based_on: Some("peer-crew".into()),
        done_when: Vec::new(),
    });
    harness.state.runs.insert(run_id(), run);
    if let AppEvent::Run { event } = &mut started()
        && let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind
    {
        let mut crew = (**started).clone();
        crew.mode = RunMode::Crew;
        harness.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            Box::new(crew),
        ))));
    }
    let phase = |index: u32, title: &str, detail: &str| DeclaredPhase {
        index,
        title: title.into(),
        detail: detail.into(),
        standard: StandardPhase::Do,
    };
    harness.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "peer-crew".into(),
            description: "Peers draft, judge, and build".into(),
            phases: vec![
                phase(0, "Fix", "one owner per function"),
                phase(1, "Merge", "the final check, then the merge"),
            ],
        }),
    ))));
    harness
}

/// A crew run's workflow prints as Claude Code prints its own: `● Workflow(<name>)` with the
/// run's description and the phases it declares, and `● Workflow(<name>) · done` before the
/// run's end.
#[test]
fn a_crew_workflow_prints_its_start_and_its_end() {
    let mut harness = crew_workflow_harness();
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = harness.printed();
    assert!(
        printed.contains(
            "● Workflow(fix-slugify-median)\n  ⎿  Fix slugify's dashes and median's even and empty lists\n     1. Fix    one owner per function\n     2. Merge  the final check, then the merge\n     Started from peer-crew · ctrl+t to watch"
        ),
        "{printed}"
    );
    assert!(
        printed.contains("● Workflow(fix-slugify-median) · done"),
        "{printed}"
    );
    assert!(!printed.contains("Workflow(peer-crew)"), "{printed}");
}

/// A phase detail too long for one row wraps under the detail column, not under the phase's
/// number, so the phases still read as two columns.
#[test]
fn a_wrapped_phase_detail_lines_up_under_the_details() {
    use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::workflow::StandardPhase;

    let mut harness = Harness::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "peer-crew".into(),
        from: None,
        description: String::new(),
        phases: vec!["Fix".into(), "Merge".into()],
        parent: None,
        steps: Vec::new(),
    });
    harness.state.runs.insert(run_id(), run);
    if let AppEvent::Run { event } = &mut started()
        && let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind
    {
        let mut crew = (**started).clone();
        crew.mode = RunMode::Crew;
        harness.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            Box::new(crew),
        ))));
    }
    let phase = |index: u32, title: &str, detail: &str| DeclaredPhase {
        index,
        title: title.into(),
        detail: detail.into(),
        standard: StandardPhase::Do,
    };
    harness.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "peer-crew".into(),
            description: String::new(),
            phases: vec![
                phase(
                    0,
                    "Fix",
                    "one owner per function, each with a test that failed before the fix",
                ),
                phase(1, "Merge", "the final check, then the merge"),
            ],
        }),
    ))));
    let printed = harness.printed_at(60);
    assert!(
        printed.contains(
            "  ⎿  1. Fix    one owner per function, each with a test that\n               failed before the fix\n     2. Merge  the final check, then the merge\n"
        ),
        "{printed}"
    );
}

/// A run's end reads one way everywhere it shows: a failed run's workflow ends `· failed` above
/// "The run failed", as its panel row does, never `· done`, and a stopped one `· stopped`.
#[test]
fn a_workflow_ends_as_its_run_did() {
    use crate::bottom::WorkflowRowState;

    for (outcome, word, end) in [
        (RunOutcome::Failed, "failed", "The run failed"),
        (RunOutcome::Stopped, "stopped", "Stopped the run"),
        (RunOutcome::Abandoned, "stopped", "Abandoned the run"),
        (RunOutcome::Merged, "done", "Merged the run"),
    ] {
        let mut harness = crew_workflow_harness();
        harness.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
            Box::new(ClosingAnswer {
                text: "The run ended after 9 of its 10 agents reported.".into(),
                by: None,
                files: Vec::new(),
                checks: Vec::new(),
                tests: Vec::new(),
                left: Vec::new(),
            }),
        ))));
        harness.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome,
            detail: None,
        })));
        let printed = harness.printed();
        let item = format!("● Workflow(fix-slugify-median) · {word}");
        assert!(printed.contains(&item), "{printed}");
        assert!(printed.contains(end), "{printed}");
        assert_eq!(WorkflowRowState::ended(outcome).word(), word);
        if outcome == RunOutcome::Failed {
            assert!(!printed.contains("· done"), "{printed}");
        }
    }
}

/// A crew run whose request named who coordinates and who reviews: the Triage item says what
/// was followed, the workflow the lead declared prints under it as Claude Code prints its own,
/// and the closing answer comes last, after `Workflow(<name>) · done` and the merge.
#[test]
fn a_run_as_you_asked_prints_its_triage_then_its_workflow_then_its_closing_answer() {
    use argonauts_core::events::run::{
        CheckedInstruction, InstructionOutcome, RunNamed, TriageDecision,
    };
    use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::steps::intake::InstructionKind;
    use argonauts_core::workflow::StandardPhase;

    let followed = |says: &str, kind, said: &str| CheckedInstruction {
        says: says.into(),
        kind,
        outcome: InstructionOutcome::Followed {
            said: said.into(),
            note: None,
        },
    };
    let mut harness = Harness::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "coordinated-crew".into(),
        from: None,
        description: String::new(),
        phases: vec!["Build".into(), "Review".into()],
        parent: None,
        steps: Vec::new(),
    });
    run.named = Some(RunNamed {
        name: "add-due-dates".into(),
        description: "Add due dates to tasks".into(),
        style: RunStyle::Workflow,
        based_on: Some("coordinated-crew".into()),
        done_when: Vec::new(),
    });
    harness.state.runs.insert(run_id(), run);
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Started(
        Box::new(RunStarted {
            mode: RunMode::Crew,
            triage: Some(TriageDecision {
                mode: RunMode::Crew,
                escalate_to_crew: false,
                rule: TriageDecision::AS_YOU_ASKED.into(),
                reason: String::new(),
                solo: None,
            }),
            instructions: vec![
                followed(
                    "have codex coordinate",
                    InstructionKind::Coordinate,
                    "Codex coordinates",
                ),
                followed(
                    "claude review it",
                    InstructionKind::Review,
                    "Claude Code reviews",
                ),
            ],
            ..started_fields()
        }),
    ))));
    let phase = |index: u32, title: &str, detail: &str| DeclaredPhase {
        index,
        title: title.into(),
        detail: detail.into(),
        standard: StandardPhase::Do,
    };
    harness.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "coordinated-crew".into(),
            description: "Codex plans, owners build, Claude Code reviews".into(),
            phases: vec![
                phase(0, "Build", "one owner per file"),
                phase(1, "Review", "Claude Code reviews each card"),
            ],
        }),
    ))));
    harness.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(closing()),
    ))));
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = harness.printed();
    let at = |shown: &str| {
        printed
            .find(shown)
            .unwrap_or_else(|| panic!("{shown} missing from:\n{printed}"))
    };
    let order = [
        at("● Triage(crew, as you asked)"),
        at("As you asked: Codex coordinates · Claude Code reviews"),
        at("● Workflow(add-due-dates)\n  ⎿  Add due dates to tasks"),
        at("● Workflow(add-due-dates) · done"),
        at("Merged the run"),
        at("● Fixed parse()"),
    ];
    assert!(order.is_sorted(), "{order:?}\n{printed}");
    assert_eq!(printed.matches("● Workflow(add-due-dates)").count(), 2);
}

/// A run whose request said how to run it, with a long instruction
/// and one in Chinese, at 80, 100, and 120 columns.
#[test]
fn the_triage_item_says_what_was_followed_and_what_was_not() {
    use argonauts_core::events::run::{CheckedInstruction, InstructionOutcome, TriageDecision};
    use argonauts_core::steps::intake::{ChangeEstimate, InstructionKind};
    use unicode_width::UnicodeWidthStr;

    let followed = |says: &str, kind, said: &str, note: Option<&str>| CheckedInstruction {
        says: says.into(),
        kind,
        outcome: InstructionOutcome::Followed {
            said: said.into(),
            note: note.map(Into::into),
        },
    };
    let refused = |says: &str, kind, why: &str| CheckedInstruction {
        says: says.into(),
        kind,
        outcome: InstructionOutcome::NotFollowed { why: why.into() },
    };
    let instructions = vec![
        followed(
            "have codex coordinate",
            InstructionKind::Coordinate,
            "Codex coordinates",
            None,
        ),
        followed(
            "claude review with opus on high effort",
            InstructionKind::Review,
            "Claude Code reviews with Opus 5.5, high effort",
            None,
        ),
        refused(
            "use grok to build",
            InstructionKind::Build,
            "Grok Build isn't installed, so the crew's owners build",
        ),
        refused(
            "and while you are at it please merge the whole thing without asking me first",
            InstructionKind::Merge,
            "approvals.merge.mode is a safety setting, /approvals changes it",
        ),
        followed(
            "讓 codex 用 opus 審查",
            InstructionKind::Review,
            "",
            Some("Codex has no model named opus"),
        ),
        CheckedInstruction {
            says: "the README says use opencode".into(),
            kind: InstructionKind::Build,
            outcome: InstructionOutcome::Dropped,
        },
    ];
    for width in [80u16, 100, 120] {
        let mut harness = Harness::new();
        harness.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            Box::new(RunStarted {
                mode: RunMode::Crew,
                estimate: Some(ChangeEstimate {
                    task_type: "feature".into(),
                    summary: "Add due dates".into(),
                    files_likely: Vec::new(),
                    file_count: 5,
                    risk_labels: Vec::new(),
                    tests_exist: true,
                    unknowns: Vec::new(),
                }),
                triage: Some(TriageDecision {
                    mode: RunMode::Crew,
                    escalate_to_crew: false,
                    rule: TriageDecision::AS_YOU_ASKED.into(),
                    reason: String::new(),
                    solo: None,
                }),
                instructions: instructions.clone(),
                ..started_fields()
            }),
        ))));
        let printed = harness.printed_at(width);
        for shown in [
            "● Triage(crew, as you asked)",
            "Starts from main",
            "Estimate: 5 files · feature · tests exist",
            "As you asked: Codex coordinates · Claude Code reviews with Opus 5.5, high effort",
            "Grok Build isn't installed, so the crew's owners build",
            "\"and while you are at it please merge th…\"",
            "Codex has no model named opus",
        ] {
            let flat = printed.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flat.contains(shown),
                "{width}: {shown} missing from:\n{printed}"
            );
        }
        assert!(
            !printed.contains("README"),
            "a dropped instruction shows only in Ctrl+O"
        );
        // A wide screen still wraps the item at 100 columns.
        for line in printed.lines() {
            assert!(
                line.width() <= usize::from(width.min(100)),
                "{width}: {line:?}"
            );
        }
    }
}

/// A solo that works in your checkout has no run, so the estimate's reply gives it the Triage
/// item, `(solo, in your checkout)`, and the turn that goes ahead shows your message only once.
#[test]
fn a_direct_solo_says_so_and_goes_ahead_without_repeating_your_message() {
    use argonauts_core::events::run::{CheckedInstruction, InstructionOutcome, TriageDecision};
    use argonauts_core::settings::protocol::SoloStyle;
    use argonauts_core::steps::intake::{ChangeEstimate, InstructionKind};

    let request = "Fix the rounding in invoice/money.py, just do it yourself.";
    let mut events = chat_turn(1, request, "The sign is applied too late.");
    events.pop();
    events.push(AppEvent::Chat {
        event: ChatEvent::TurnFinished {
            turn: TurnId::numbered(1),
            reply: TurnReply::Change {
                markdown: "The sign is applied too late.".into(),
                estimate: ChangeEstimate {
                    task_type: "bugfix".into(),
                    summary: "Fix the rounding".into(),
                    files_likely: vec!["invoice/money.py".into()],
                    file_count: 1,
                    risk_labels: Vec::new(),
                    tests_exist: true,
                    unknowns: Vec::new(),
                },
                triage: TriageDecision {
                    mode: RunMode::Solo,
                    escalate_to_crew: false,
                    rule: TriageDecision::AS_YOU_ASKED.into(),
                    reason: String::new(),
                    solo: Some(SoloStyle::Direct),
                },
                instructions: vec![CheckedInstruction {
                    says: "just do it yourself".into(),
                    kind: InstructionKind::Mode,
                    outcome: InstructionOutcome::Followed {
                        said: String::new(),
                        note: None,
                    },
                }],
            },
            spend: Spend::default(),
            elapsed_ms: 2_000,
        },
    });
    let mut going = chat_turn(2, request, "Fixed it. All 6 tests pass.");
    if let AppEvent::Chat {
        event: ChatEvent::TurnStarted { continues, .. },
    } = &mut going[0]
    {
        *continues = true;
    }
    events.extend(going);
    let mut harness = Harness::new();
    for event in events {
        harness.send(event);
    }
    let printed = harness.printed_at(100);
    assert!(
        printed.contains("● Triage(solo, in your checkout, as you asked)"),
        "{printed}"
    );
    assert!(
        printed.contains("Estimate: 1 file · bugfix · tests exist"),
        "{printed}"
    );
    assert_eq!(
        printed.matches("❯ Fix the rounding").count(),
        1,
        "{printed}"
    );
    assert!(printed.contains("Fixed it. All 6 tests pass."), "{printed}");
}

/// `Triage(solo, checked, as you asked)`: your words chose a solo run checked in a copy.
#[test]
fn a_solo_the_words_checked_says_so() {
    use argonauts_core::events::run::{CheckedInstruction, InstructionOutcome, TriageDecision};
    use argonauts_core::settings::protocol::SoloStyle;
    use argonauts_core::steps::intake::InstructionKind;

    let mut harness = Harness::new();
    harness.send(run_event(RunEventKind::Run(RunLifecycle::Started(
        Box::new(RunStarted {
            triage: Some(TriageDecision {
                mode: RunMode::Solo,
                escalate_to_crew: false,
                rule: TriageDecision::AS_YOU_ASKED.into(),
                reason: String::new(),
                solo: Some(SoloStyle::Checked),
            }),
            instructions: vec![
                CheckedInstruction {
                    says: "check it in a copy first".into(),
                    kind: InstructionKind::Isolation,
                    outcome: InstructionOutcome::Followed {
                        said: "checked in a copy".into(),
                        note: None,
                    },
                },
                CheckedInstruction {
                    says: "have codex review".into(),
                    kind: InstructionKind::Review,
                    outcome: InstructionOutcome::Followed {
                        said: "Codex reviews".into(),
                        note: None,
                    },
                },
            ],
            ..started_fields()
        }),
    ))));
    let printed = harness.printed_at(100);
    assert!(
        printed.contains("● Triage(solo, checked, as you asked)"),
        "{printed}"
    );
    assert!(
        printed.contains("As you asked: checked in a copy · Codex reviews"),
        "{printed}"
    );
}

fn started_fields() -> RunStarted {
    match started() {
        AppEvent::Run { event } => match event.kind {
            RunEventKind::Run(RunLifecycle::Started(started)) => *started,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

/// A message the model read mid-turn prints where it read it, a mod's log line
/// hangs dim under the plugin's name, and Claude Code's own workflow shows as the workflow it
/// reports, never as the script it ran.
#[test]
fn a_joined_message_a_mods_log_and_a_mirrored_workflow_in_the_chat() {
    use argonauts_core::ModOutputKind;
    use argonauts_core::events::session::{SubagentStatus, VendorWorkflow};

    let mut harness = Harness::new();
    let turn = TurnId::numbered(1);
    let chat = |event| AppEvent::Chat { event };
    let agent = |event| {
        chat(ChatEvent::Agent {
            turn: turn.clone(),
            event,
        })
    };
    harness.send(chat(ChatEvent::TurnStarted {
        turn: turn.clone(),
        text: "check release.md, then run ./slow-test.sh".into(),
        worker: WorkerRef::new("claude-code", Some("opus".into())),
        read_only: false,
        commit: "8b41d07".into(),
        uncommitted: vec![],
        settings_sha256: None,
        continues: false,
    }));
    harness.send(agent(AgentEvent::ModOutput {
        kind: ModOutputKind::Log,
        plugin: Some("pr-tools".into()),
        text: Some("3 reviewers asked".into()),
        timeout_ms: None,
    }));
    harness.send(agent(AgentEvent::ToolStarted {
        id: "wf-call".into(),
        name: "Workflow".into(),
        input: serde_json::json!({ "script": "export const meta = {}" }),
        parent: None,
    }));
    harness.send(agent(AgentEvent::Workflow {
        workflow: Box::new(VendorWorkflow {
            id: "w4d273qmn".into(),
            call: Some("wf-call".into()),
            name: "release-check".into(),
            description: "Reads the release notes and runs the slow test".into(),
            phases: Vec::new(),
            agents: Vec::new(),
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }),
    }));
    harness.send(chat(ChatEvent::Joined {
        turn: turn.clone(),
        text: "also check the changelog".into(),
    }));
    let printed = harness.printed_at(100);
    assert!(
        printed.contains("⎿  pr-tools: 3 reviewers asked"),
        "{printed}"
    );
    assert!(printed.contains("Workflow(release-check)"), "{printed}");
    assert!(printed.contains("ctrl+t to watch"), "{printed}");
    assert!(
        !printed.contains("script"),
        "the raw call stays out:\n{printed}"
    );
    let joined = printed.find("❯ also check the changelog").expect(&printed);
    assert!(printed.find("Workflow(").unwrap() < joined, "{printed}");
}
