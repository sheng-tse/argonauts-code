use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::{RunState as RunStatus, RunSummary};
use argonauts_core::app::{AppEvent, ChatEvent, ReplayTurn, TurnReply};
use argonauts_core::events::RetroEvent;
use argonauts_core::events::card::{AttemptCause, CardEvent, CardState, GateKind, GateOutcome};
use argonauts_core::events::run::{
    Party, PhaseChange, RunLifecycle, RunMode, RunOutcome, RunPhase, RunStarted,
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
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
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
        "● Triage(solo)",
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
    assert!(printed.contains("● Triage(solo)"), "{printed}");
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
