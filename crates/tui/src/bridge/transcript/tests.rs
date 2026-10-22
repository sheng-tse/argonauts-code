use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::{RunState as RunStatus, RunSummary};
use argonauts_core::app::{AppEvent, ConversationEvent, TurnReply};
use argonauts_core::events::RunReviewEvent;
use argonauts_core::events::run::{
    ChangedFile, CheckLine, ClosingAnswer, Party, PhaseChange, RunLifecycle, RunMode, RunOutcome,
    RunPhase, RunStarted, WaitReason,
};
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskEvent, TaskState, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::log::RUN_REVIEW_FAILED;
use argonauts_core::names::{ModelRef, TurnId};
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::{AgentEvent, Note, NoteLevel, PhaseStatus, RunId, Spend, TaskId, Verbosity};
use time::macros::datetime;

use super::{ConversationTurns, Transcript};
use crate::bridge::{Cx, Effects};
use crate::cell::{Cell, HistoryCell, ToolKind};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::{AppState, Replay};
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

struct TestDriver {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
}

impl TestDriver {
    fn new() -> Self {
        let mut transcript =
            Transcript::with_clock(WallClock::pinned(datetime!(2026-09-30 10:42 UTC)));
        transcript.conversation = ConversationTurns::with_tool_kinds(|_, name| match name {
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

fn task_id() -> TaskId {
    TaskId::new("task-001").expect("a valid task id")
}

fn run_event(kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-09-30 10:00 UTC),
            run_id: run_id(),
            task_id: Some(task_id()),
            session: None,
            source: "argonauts-code".into(),
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
            mode: RunMode::Captain,
            forced: false,
            workflow: "council".into(),
            estimate: None,
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: PathBuf::from("/repo"),
            base_branch: Some("main".into()),
            base_commit: "8b41d07aa".into(),
            uncommitted: vec![],
            uncommitted_included: false,
            settings_sha256: String::new(),
            settings: serde_json::Value::Null,
            overrides: vec![],
            cli_versions: Default::default(),
            version: "0.0.1".into(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
            starts_from: None,
        },
    ))))
}

fn check(name: &str, passed: bool) -> AppEvent {
    run_event(RunEventKind::Check(CheckOutcome {
        name: name.into(),
        kind: CheckKind::Command,
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

fn checks_passed() -> AppEvent {
    run_event(RunEventKind::Task(TaskEvent::State {
        from: Some(TaskState::Checking),
        to: TaskState::InReview,
        reason: String::new(),
    }))
}

fn paused() -> AppEvent {
    run_event(RunEventKind::Run(RunLifecycle::Paused {
        sessions_stopped: 1,
        committed: vec![task_id()],
    }))
}

fn conversation_turn(n: u32, question: &str, answer: &str) -> Vec<AppEvent> {
    let turn = TurnId::numbered(n);
    let conversation = |event| AppEvent::Conversation { event };
    vec![
        conversation(ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: question.into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
            read_only: true,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        }),
        conversation(ConversationEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::ToolStarted {
                id: "tool-1".into(),
                name: "Read".into(),
                input: serde_json::json!({ "file_path": "src/cli.rs" }),
                parent: None,
            },
        }),
        conversation(ConversationEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::ToolFinished {
                id: "tool-1".into(),
                ok: true,
                output: None,
                parent: None,
            },
        }),
        conversation(ConversationEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::TextDelta {
                text: answer.into(),
                parent: None,
            },
        }),
        conversation(ConversationEvent::TurnFinished {
            turn,
            reply: TurnReply::Answer {
                markdown: answer.into(),
            },
            spend: Spend::default(),
            elapsed_ms: 3_000,
        }),
    ]
}

/// A main agent that also writes its structured answer out as JSON at the end of its text
/// streams its words, and the JSON never shows, fenced or not, however the deltas split it.
#[test]
fn a_json_object_that_ends_the_main_agents_text_never_shows() {
    let words = "Export writes CSV, one row per task.";
    for tail in [
        "\n\n```json\n{\"answer_markdown\": \"Export writes CSV, one row per task.\"}\n```",
        "\n\n{\"answer_markdown\": \"Export writes CSV, one row per task.\"}",
    ] {
        let mut driver = TestDriver::new();
        let mut events = conversation_turn(1, "what does export do?", words);
        // The streamed text comes in pieces of a few characters, the JSON among them.
        let streamed: Vec<char> = format!("{words}{tail}").chars().collect();
        let deltas: Vec<AppEvent> = streamed
            .chunks(7)
            .map(|piece| AppEvent::Conversation {
                event: ConversationEvent::Agent {
                    turn: TurnId::numbered(1),
                    event: AgentEvent::TextDelta {
                        text: piece.iter().collect(),
                        parent: None,
                    },
                },
            })
            .collect();
        events.splice(3..4, deltas);
        for event in events {
            driver.send(event);
        }
        let printed = driver.printed();
        assert!(printed.contains(words), "{printed}");
        assert!(!printed.contains("answer_markdown"), "{printed}");
        assert!(!printed.contains("```"), "{printed}");
    }
}

#[test]
fn an_attached_runs_history_only_joins_ctrl_o() {
    let mut driver = TestDriver::new();
    driver.send(AppEvent::Attached {
        summary: RunSummary {
            run: run_id(),
            title: "Add a --json flag".into(),
            mode: RunMode::Crew,
            workflow: "council".into(),
            state: RunStatus::Waiting,
            stopped_at: "plan approval, waiting for you".into(),
            updated: datetime!(2026-09-30 10:00 UTC),
            spent: Spend::default(),
            sessions_to_resume: 2,
            waiting: vec![],
            live: false,
            remind: false,
            name: None,
            shape_known: true,
        },
        started_here: false,
    });
    driver.send(started());
    for n in 0..400 {
        driver.send(note(NoteLevel::Info, &format!("replayed note {n}")));
        driver.send(check("tests", true));
        driver.send(checks_passed());
    }
    for event in conversation_turn(1, "what does export do?", "It writes CSV.") {
        driver.send(event);
    }
    driver.send(paused());
    let kept = driver.model.history.cells().len();
    assert!(kept > 800, "every replayed item stays for Ctrl+O: {kept}");
    assert_eq!(driver.printed(), "", "nothing prints while the run replays");

    driver.send(AppEvent::ReplayDone {
        run: run_id(),
        seq: 1_200,
    });
    assert_eq!(
        driver.printed(),
        "",
        "what the run did before is on screen already, so nothing sums it up"
    );

    // Live events print again.
    driver.send(paused());
    assert!(driver.printed().contains("You paused the run"));
}

/// A pause is told as whoever asked for it: the network that stayed down, your limit, or you.
#[test]
fn a_pause_says_who_asked_for_it() {
    let asked = |by: Party| {
        run_event(RunEventKind::Run(RunLifecycle::PauseRequested {
            by,
            now: true,
        }))
    };
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(asked(Party::Network));
    driver.send(paused());
    let printed = driver.printed();
    assert!(
        printed.contains("The run paused while the network was down · /resume"),
        "{printed}"
    );
    assert!(!printed.contains("You paused"), "{printed}");

    driver.send(asked(Party::Rules));
    driver.send(paused());
    assert!(
        driver
            .printed()
            .contains("The run paused at your limit · /resume")
    );

    driver.send(asked(Party::User));
    driver.send(paused());
    assert!(driver.printed().contains("You paused the run · /resume"));
}

/// A run that nothing can carry on says it stopped and how to carry it on, and never that it
/// tries again by itself. An older run that waited on a timer instead shows the wait it made.
#[test]
fn a_run_with_nobody_to_fall_back_to_says_it_stopped() {
    let waiting = |until| {
        run_event(RunEventKind::Run(RunLifecycle::Waiting {
            reason: WaitReason::NoFallback {
                detail: "nobody to fall back to as the main agent".into(),
                until,
                stop: None,
            },
            waiting_on: Vec::new(),
        }))
    };
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(waiting(None));
    let stopped = driver.printed();
    assert!(
        stopped.contains("The run stopped · nobody to fall back to as the main agent"),
        "{stopped}"
    );
    assert!(stopped.contains("/resume carries the run on"), "{stopped}");
    assert!(!stopped.contains("tries again"), "{stopped}");
    assert!(!stopped.contains("every agent is out"), "{stopped}");

    let mut older = TestDriver::new();
    older.send(started());
    older.send(waiting(Some(datetime!(2026-09-30 10:47 UTC))));
    let waited = older.printed();
    assert!(
        waited
            .contains("The run waits · nothing could take a job the run needs · it tries again at"),
        "{waited}"
    );
    assert!(!waited.contains("The run stopped"), "{waited}");
}

fn running_summary() -> RunSummary {
    RunSummary {
        run: run_id(),
        title: "Add a --json flag".into(),
        mode: RunMode::Crew,
        workflow: "council".into(),
        state: RunStatus::Running,
        stopped_at: "working".into(),
        updated: datetime!(2026-09-30 10:00 UTC),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: vec![],
        live: true,
        remind: false,
        name: None,
        shape_known: true,
    }
}

#[test]
fn a_run_started_here_prints_no_header() {
    let mut driver = TestDriver::new();
    driver.send(AppEvent::Attached {
        summary: running_summary(),
        started_here: true,
    });
    driver.send(AppEvent::ReplayDone {
        run: run_id(),
        seq: 0,
    });
    // The prompt that started it is right above, so a `● <title>` row would only repeat it.
    assert_eq!(driver.printed().trim(), "");

    // Nor does a run opened again: what it did is on screen already, drawn from its record.
    driver.send(AppEvent::Attached {
        summary: running_summary(),
        started_here: false,
    });
    driver.send(AppEvent::ReplayDone {
        run: run_id(),
        seq: 0,
    });
    assert_eq!(driver.printed().trim(), "");
}

#[test]
fn an_empty_checks_run_is_not_a_failed_check() {
    let mut driver = TestDriver::new();
    // What the engine records when `checks.run` is empty: skipped, and no command ran.
    driver.send(run_event(RunEventKind::Check(CheckOutcome {
        name: "checks.run".into(),
        kind: CheckKind::Command,
        passed: true,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary: "No command checks set · /config checks.run".into(),
        output_tail: String::new(),
        violations: vec![],
        sandboxed: false,
        skipped: true,
    })));
    driver.send(checks_passed());
    let printed = driver.printed();
    assert!(
        printed.contains("built-in checks)\n  ⎿  No command checks set · /config checks.run"),
        "{printed}"
    );
    assert!(!printed.contains("failed"), "{printed}");
}

fn run_review_phase(status: PhaseStatus, summary: Option<&str>) -> AppEvent {
    run_event(RunEventKind::Phase(PhaseChange {
        phase: RunPhase::RunReview,
        status,
        summary: summary.map(Into::into),
    }))
}

#[test]
fn a_failed_run_review_is_said_once_and_not_called_saved() {
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(run_review_phase(PhaseStatus::Started, None));
    driver.send(run_event(RunEventKind::RunReview(RunReviewEvent::Scored {
        task: task_id(),
        metric: "p_pass_checks_first_try".into(),
        predicted: 0.8,
        outcome: true,
        brier: 0.04,
    })));
    driver.send(run_review_phase(PhaseStatus::Failed, Some(SCHEMA_MISS)));
    driver.send(note(NoteLevel::Warn, RUN_REVIEW_FAILED));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = driver.printed();
    assert_eq!(
        printed.matches("The run review failed").count(),
        1,
        "{printed}"
    );
    assert!(printed.contains("Run finished"), "{printed}");
    assert!(!printed.contains("run review saved"), "{printed}");
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
        decided: Vec::new(),
    }
}

fn ended_with_closing(verbosity: Verbosity) -> String {
    let mut driver = TestDriver::new();
    driver.settings.verbosity = verbosity;
    driver.send(started());
    driver.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(closing()),
    ))));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    driver.printed()
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

/// What a run on autopilot decided in your place is listed under its closing answer, each with
/// whose call it was, and never as a question.
#[test]
fn a_closing_answer_lists_what_autopilot_decided() {
    use argonauts_core::events::run::DecidedAlone;

    let mut answer = closing();
    answer.left.clear();
    answer.decided = vec![DecidedAlone {
        what: "negative amounts round half toward +∞, as apply_discount already does".to_owned(),
        whose: "the worker's call".to_owned(),
    }];
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(answer),
    ))));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = driver.printed_at(100);
    let lines: Vec<&str> = printed.lines().map(str::trim_end).collect();
    let at = lines
        .iter()
        .position(|line| line.contains("Decided on autopilot: negative amounts round half"))
        .expect(&printed);
    assert_eq!(
        lines[at],
        "  - Decided on autopilot: negative amounts round half toward +∞, as apply_discount \
         already does",
        "{printed}"
    );
    assert_eq!(
        lines[at + 1],
        "    (the worker's call; ask for a change if you want otherwise)",
        "{printed}"
    );
    assert!(!printed.contains('?'), "nothing is asked: {printed}");
}

const SCHEMA_MISS: &str =
    "step: output didn't match the schema: [\"Additional properties are not allowed\"]";

/// A schema miss is plumbing: the main view says the run review failed in plain words, and the
/// reason waits for Ctrl+O.
#[test]
fn a_failed_run_review_keeps_its_raw_reason_for_ctrl_o() {
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(run_review_phase(PhaseStatus::Started, None));
    driver.send(run_review_phase(PhaseStatus::Failed, Some(SCHEMA_MISS)));
    driver.send(note(NoteLevel::Warn, RUN_REVIEW_FAILED));
    let printed = driver.printed();
    assert!(
        printed.contains("● The run review failed. The run's result stands. (ctrl+o to expand)"),
        "{printed}"
    );
    assert!(!printed.contains("schema"), "{printed}");

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now())).with_expanded(true);
    let expanded: String = driver
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
/// validator said of the main agent's design, wait for Ctrl+O.
#[test]
fn a_notes_detail_waits_for_ctrl_o() {
    let mut driver = TestDriver::new();
    driver.send(started());
    let incomplete = Note::with_detail(
        NoteLevel::Warn,
        "The main agent's design came back incomplete, so orchestrator runs as written.",
        "output didn't match the schema:\n\"based_on\" is a required property",
    );
    driver.send(note(NoteLevel::Warn, &incomplete.text));
    let printed = driver.printed();
    assert!(
        printed.contains(
            "● The main agent's design came back incomplete, so orchestrator runs as written."
        ),
        "{printed}"
    );
    assert!(!printed.contains("schema"), "{printed}");
    assert!(!printed.contains("required property"), "{printed}");

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let drawn = |ctx: &RenderCtx<'_>| -> String {
        driver
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

/// Another run's failed run review is said again, though its note is word for word the same.
#[test]
fn each_runs_failed_run_review_is_said() {
    let mut driver = TestDriver::new();
    for _ in 0..2 {
        driver.send(started());
        driver.send(run_review_phase(PhaseStatus::Failed, None));
        driver.send(note(NoteLevel::Warn, RUN_REVIEW_FAILED));
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        })));
    }
    let printed = driver.printed();
    assert_eq!(
        printed.matches("The run review failed").count(),
        2,
        "{printed}"
    );
    assert!(!printed.contains("ctrl+o"), "{printed}");
}

#[test]
fn warning_notice_shows_once() {
    let mut driver = TestDriver::new();
    let warning = "An always-on agent is running · approvals stay yours";
    driver.send(AppEvent::Notice {
        level: NoteLevel::Warn,
        text: warning.into(),
    });
    driver.send(note(NoteLevel::Warn, warning));
    driver.send(AppEvent::Notice {
        level: NoteLevel::Warn,
        text: format!("  {warning}  "),
    });
    driver.send(AppEvent::Notice {
        level: NoteLevel::Info,
        text: "Saved.".into(),
    });
    driver.send(AppEvent::Notice {
        level: NoteLevel::Info,
        text: "Saved.".into(),
    });
    let printed = driver.printed();
    assert_eq!(printed.matches(warning).count(), 1, "{printed}");
    assert_eq!(printed.matches("Saved.").count(), 2, "{printed}");
}

#[test]
fn a_fallback_without_a_retry_names_who_steps_in_plainly_and_why_dimly() {
    let mut driver = TestDriver::new();
    let line = format!(
        "Fell back to Claude Code as reviewer · {}",
        argonauts_core::log::NO_SECOND_TRY
    );
    driver.send(note(NoteLevel::Warn, &line));
    let Some(Cell::Notice(notice)) = driver.model.history.cells().last() else {
        panic!("the fallback is a notice");
    };
    assert_eq!(notice.text, "Fell back to Claude Code as reviewer");
    assert_eq!(
        notice.decided.as_deref(),
        Some(argonauts_core::log::NO_SECOND_TRY)
    );
    assert!(notice.plain_words);
    assert!(driver.printed_at(120).contains(&line));
}

/// The run the fallback picture tells of: `build:due-flag` building on Sonnet 5.5, and
/// `review:due-tests` done by Sonnet 5.5 in Opus 5.5's place.
fn due_dates_run(driver: &mut TestDriver) {
    use argonauts_core::model_catalog::ModelCatalog;
    use argonauts_core::names::{RoleName, SessionKey};

    use crate::state::{RunState, SessionState, SessionStatus, TaskRow};

    let catalog = serde_json::json!({ "model": [
        { "id": "claude-code/claude-opus-5-5", "name": "Opus 5.5", "aliases": ["opus"] },
        { "id": "claude-code/claude-sonnet-5-5", "name": "Sonnet 5.5", "aliases": ["sonnet"] },
        { "id": "claude-code/claude-haiku-5-5", "name": "Haiku 5.5", "aliases": ["haiku"] },
    ]});
    let catalog: ModelCatalog = serde_json::from_value(catalog).expect("a catalog");
    driver.state.names.catalog = catalog;

    let sonnet: ModelRef = "claude-code/sonnet".parse().expect("a model");
    let mut run = RunState::new(run_id());
    let mut build = TaskRow::new(task_id(), "The --due flag");
    build.item = Some("due-flag".into());
    build.worker = Some(sonnet.clone());
    build.state = Some(TaskState::Running);
    run.tasks.insert(task_id(), build);
    let key = SessionKey::numbered(2);
    let mut review = SessionState::new(
        key.clone(),
        sonnet,
        "claude-code",
        RoleName::Reviewer,
        datetime!(2026-09-30 13:58 UTC),
    );
    review.label = Some("review:due-tests".into());
    review.fallback_for = Some("claude-code/opus".parse().expect("a model"));
    review.status = SessionStatus::Done;
    run.sessions.insert(key.clone(), review);
    run.session_order.push(key);
    driver.state.runs.insert(run_id(), run);
}

/// The lines a model's outage, its CLI's, and their returns put in the conversation, as the
/// picture of fallbacks that stay on one CLI has them: every row the conversation itself
/// writes there, in order. The picture's session rows (`● review:due-tests · …` and
/// `↷ review:due-tests · …`) belong to the workflow's own rows.
#[test]
fn an_outage_that_stays_on_one_cli_reads_as_its_picture() {
    use argonauts_core::events::AgentStatusEvent;
    use argonauts_core::events::agent_status::FallbackEvent;
    use argonauts_core::limits::{
        AgentHealth, HealthState, ModelHealth, ModelState, TEST_TASK_SUCCEEDED, Trouble,
    };
    use argonauts_core::names::AgentId;

    let claude = AgentId("claude-code".into());
    let model = |name: &str, state: ModelState, trouble: Option<Trouble>| {
        let health = ModelHealth {
            state,
            trouble,
            reason: trouble.map(|_| "2 errors in a row".to_owned()),
            next_test_at: trouble.map(|_| datetime!(2026-09-30 14:03 UTC)),
            ..ModelHealth::healthy(&ModelRef::new("claude-code", Some(name.to_owned())))
        };
        run_event(RunEventKind::AgentStatus(AgentStatusEvent::Model(health)))
    };
    let whole = |state: HealthState, reason: &str| {
        run_event(RunEventKind::AgentStatus(AgentStatusEvent::Health(
            AgentHealth {
                agent_id: claude.clone(),
                state,
                since: datetime!(2026-09-30 10:00 UTC),
                reason: Some(reason.to_owned()),
                next_test_at: None,
                disabled: None,
            },
        )))
    };
    let mut driver = TestDriver::new();
    driver.send(started());
    due_dates_run(&mut driver);
    // The state hears each event before the conversation does, as in the app.
    let mut hear = |event: AppEvent| {
        driver.state.apply(&event);
        driver.send(event);
    };

    // Opus 5.5 is overloaded: another Claude Code model takes the review, and Opus goes out.
    hear(note(
        NoteLevel::Warn,
        "Still overloaded · fell back to Sonnet 5.5 on Claude Code as reviewer\n\
         same_cli is on, so Codex isn't asked",
    ));
    hear(model("opus", ModelState::Out, Some(Trouble::Overload)));
    // Sonnet 5.5 fails too, which takes Claude Code down, and the build task waits for it.
    hear(model(
        "sonnet",
        ModelState::Out,
        Some(Trouble::ServerErrors),
    ));
    hear(whole(HealthState::Down, "two of its models are out"));
    hear(run_event(RunEventKind::Fallback(FallbackEvent::Waiting {
        task: task_id(),
        agent_id: claude.clone(),
        until: Some(datetime!(2026-09-30 14:05 UTC)),
        kept_off: Some(AgentId("codex".into())),
    })));
    // Claude Code's test task passes on Sonnet 5.5, then Opus 5.5's own does.
    hear(whole(HealthState::Testing, "under test"));
    hear(model("sonnet", ModelState::Healthy, None));
    hear(whole(HealthState::Healthy, TEST_TASK_SUCCEEDED));
    hear(model("opus", ModelState::Testing, None));
    hear(model("opus", ModelState::Healthy, None));

    let printed = driver.printed_at(120);
    let rows: Vec<&str> = printed.lines().map(str::trim_end).collect();
    let drawn = rows.join("\n");
    let picture = include_str!("../../../tests/golden/transcript/fallback_same_cli.txt");
    assert_eq!(drawn.trim(), picture.trim(), "{drawn}");
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
                    phase: argonauts_core::events::run::RunPhase::Understand,
                },
                text: "The goal is settled.".into(),
                model: ModelRef::new("claude-code", Some("haiku".into())),
                spend: Spend::default(),
            },
        },
        AppEvent::Recap {
            text: "Work goes on.".into(),
            model: None,
        },
        check("tests", false),
        checks_passed(),
        paused(),
    ];
    events.extend(conversation_turn(1, "hi", "Hello."));
    events
}

#[test]
fn quiet_shows_milestones_only() {
    let mut quiet = TestDriver::new();
    quiet.settings.verbosity = Verbosity::Quiet;
    for event in chatter() {
        quiet.send(event);
    }
    let printed = quiet.printed();
    for shown in ["Checks(tests)", "You paused the run", "❯ hi", "Hello."] {
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
        let mut driver = TestDriver::new();
        driver.settings.verbosity = level;
        for event in chatter() {
            driver.send(event);
        }
        let printed = driver.printed();
        for shown in ["Worktree ready", "narrator:", "recap:", call] {
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

fn welcome_with(agent: &str, chosen: &str) -> AppEvent {
    AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/github/argonauts".into(),
        workflow: chosen.into(),
        agents: vec![],
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: Some(ModelRef::new(agent, None)),
        effort: None,
    }
}

#[test]
fn a_repeated_welcome_prints_one_header() {
    let mut driver = TestDriver::new();
    driver.send(welcome_with("claude-code", "council"));
    driver.send(welcome_with("claude-code", "council"));
    driver.send(note(NoteLevel::Warn, "codex is logged out"));
    let cells = driver.model.history.cells();
    assert!(
        matches!(
            cells,
            [crate::cell::Cell::Header(_), crate::cell::Cell::Notice(_)]
        ),
        "{cells:?}"
    );
}

fn headers(driver: &TestDriver) -> usize {
    driver
        .model
        .history
        .cells()
        .iter()
        .filter(|cell| matches!(cell, Cell::Header(_)))
        .count()
}

/// An aside, such as the no-git notice at the start, takes Claude Code's `※` where other
/// notices take the bullet.
#[test]
fn an_aside_takes_the_reference_mark() {
    let mut driver = TestDriver::new();
    driver.send(notice(NoteLevel::Aside, "This folder has no git."));
    driver.send(notice(NoteLevel::Info, "Saved."));
    let printed = driver.printed();
    assert!(printed.contains("※ This folder has no git."), "{printed}");
    assert!(printed.contains("● Saved."), "{printed}");
}

/// A quiet line, such as a newer version of a CLI being out, has no mark at all: its words
/// start where a marked line's do, as the versions picture draws it.
#[test]
fn a_quiet_line_has_no_mark() {
    let mut driver = TestDriver::new();
    driver.send(notice(
        NoteLevel::Quiet,
        "Claude Code 2.1.297 is out · claude update · shown once for this version",
    ));
    let printed = driver.printed();
    let picture = include_str!("../../../tests/golden/overview/versions.txt");
    let drawn = picture.lines().nth(9).unwrap();
    assert!(
        printed.lines().any(|line| line.trim_end() == drawn),
        "{printed}"
    );
    assert!(!printed.contains('●'), "{printed}");
}

/// Once the setup saves, the header changes where it stands instead of printing a second one
/// under the setup's answers. A warning it repeats shows once.
#[test]
fn the_header_changes_in_place_when_the_setup_saves() {
    let mut driver = TestDriver::new();
    driver.send(welcome_with("claude-code", "council"));
    driver.send(notice(NoteLevel::Warn, "a typo in your settings"));
    driver.printed();
    driver.send(welcome_with("antigravity-cli", "captain"));
    driver.send(notice(NoteLevel::Warn, "a typo in your settings"));

    assert_eq!(driver.printed(), "", "nothing prints under the old header");
    let redrawn = driver
        .model
        .history
        .take_header_redrawn()
        .expect("the header, redrawn");
    let redrawn = plain_text(&redrawn);
    assert!(redrawn.contains("agy · Captain"), "{redrawn}");
    assert!(!redrawn.contains("Claude Code"), "{redrawn}");
    assert_eq!(headers(&driver), 1, "the transcript keeps one header");
}

/// After the conversation starts, the header still changes where it stands rather than
/// printing again under the conversation.
#[test]
fn a_header_after_the_conversation_starts_never_prints_twice() {
    let mut driver = TestDriver::new();
    driver.send(welcome_with("claude-code", "council"));
    driver.send(AppEvent::Report {
        title: "Doctor".into(),
        lines: vec!["git 2.50.1".into()],
    });
    driver.printed();
    driver.send(welcome_with("codex", "captain"));
    let printed = driver.printed();
    assert!(!printed.contains("Codex · Captain"), "{printed}");
    let redrawn = driver.model.history.take_header_redrawn();
    assert!(
        redrawn.is_some_and(|lines| plain_text(&lines).contains("Codex · Captain")),
        "the header on screen changes"
    );
    assert_eq!(headers(&driver), 1);

    // The same header again changes nothing.
    driver.send(welcome_with("codex", "captain"));
    driver.printed();
    assert_eq!(driver.model.history.take_header_redrawn(), None);
}

#[test]
fn the_header_waits_for_the_main_agent() {
    use argonauts_core::app::HeaderAndBottomLine;
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::ui::NarratorMode;

    let mut driver = TestDriver::new();
    driver.send(AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/github/argonauts".into(),
        workflow: "council".into(),
        agents: vec![],
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: None,
        effort: None,
    });
    assert!(driver.model.history.cells().is_empty());
    assert_eq!(driver.model.history.active_cells().count(), 1);
    driver.send(AppEvent::Status {
        status: HeaderAndBottomLine {
            title: "argonauts".into(),
            repo: "~/github/argonauts".into(),
            workflow: "council".into(),
            ultracode: false,
            ultra: None,
            run: None,
            main_agent: ModelRef::new("claude-code", Some("haiku".into())),
            plan_chips: vec![],
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::OnDemand,
            approval_mode: ApprovalMode::Supervised,
            awake: None,
        },
    });
    assert_eq!(driver.model.history.active_cells().count(), 0);
    let printed = driver.printed();
    assert!(
        printed.contains("Claude Code · Haiku · Council"),
        "{printed}"
    );
}

/// The app's names come first, so the header reads like Claude Code's, with the model an
/// alias runs: `Claude Code · Opus 5.5 with xhigh effort · Council`.
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
    let mut driver = TestDriver::new();
    driver.state.apply(&AppEvent::Names {
        names: Box::new(names),
    });
    driver.send(AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/github/argonauts".into(),
        workflow: "council".into(),
        agents: vec![],
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: Some(ModelRef::new("claude-code", Some("opus".into()))),
        effort: Some("xhigh".into()),
    });
    assert_eq!(driver.model.history.active_cells().count(), 0);
    let printed = driver.printed();
    assert!(
        printed.contains("Claude Code · Opus 5.5 with xhigh effort · Council"),
        "{printed}"
    );
}

#[test]
fn a_queued_prompt_gets_its_own_echo() {
    let mut driver = TestDriver::new();
    // The composer echoed the first prompt, grey until its turn starts.
    let pending = driver
        .model
        .history
        .push_active(crate::cell::UserMessage::pending("hi"));
    for event in conversation_turn(1, "hi", "Hello.") {
        driver.send(event);
    }
    assert!(driver.model.history.active(pending).is_none());
    for event in conversation_turn(2, "and then?", "Then it ends.") {
        driver.send(event);
    }
    let printed = driver.printed();
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
        let mut driver = TestDriver::new();
        driver.state.cli_commands = Some(
            crate::commands::tests::claude_commands()
                .with_keywords(std::slice::from_ref(&ultrathink)),
        );
        driver.model.history.push_active(
            crate::cell::UserMessage::pending("please ultrathink")
                .with_keywords(std::slice::from_ref(&ultrathink)),
        );
        driver.send(AppEvent::Conversation {
            event: ConversationEvent::TurnStarted {
                turn: TurnId::numbered(1),
                text: "please ultrathink".into(),
                worker: ModelRef::new(worker, None),
                read_only: false,
                commit: "8b41d07".into(),
                uncommitted: vec![],
                settings_sha256: None,
                continues: false,
            },
        });
        let echoed = driver
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
    let mut driver = TestDriver::new();
    driver
        .model
        .history
        .push_active(crate::cell::UserMessage::pending("hi"));
    driver.send(note(NoteLevel::Warn, "codex is logged out"));
    for event in conversation_turn(1, "hi", "Hello.") {
        driver.send(event);
    }
    let printed = driver.printed();
    assert_eq!(printed.matches("❯ hi").count(), 1, "{printed}");
    assert!(
        printed.find("codex is logged out") < printed.find("❯ hi"),
        "{printed}"
    );
}

#[test]
fn a_whole_message_after_deltas_is_not_repeated() {
    let mut driver = TestDriver::new();
    let turn = TurnId::numbered(1);
    let mut events = conversation_turn(1, "hi", "Hello there.");
    let finished = events.pop().expect("the turn's end");
    events.push(AppEvent::Conversation {
        event: ConversationEvent::Agent {
            turn,
            event: AgentEvent::Message {
                text: "Hello there.".into(),
                parent: None,
            },
        },
    });
    events.push(finished);
    for event in events {
        driver.send(event);
    }
    let printed = driver.printed();
    assert_eq!(printed.matches("Hello there.").count(), 1, "{printed}");
}

#[test]
fn status_answers_print_as_one_item() {
    use argonauts_core::api::{StatusSummary, TaskBrief};

    let mut driver = TestDriver::new();
    let summary = StatusSummary {
        run: Some(run_id()),
        goal: vec![
            "Add a --json flag".into(),
            "G1 export --json writes JSON lines".into(),
        ],
        plan: Some("Plan 1, 2 tasks".into()),
        tasks: vec![TaskBrief {
            task: task_id(),
            title: "Parse the flag".into(),
            state: TaskState::Running,
            worker: Some(ModelRef::new("codex", Some("gpt-5.6".into()))),
            note: "6m 12s · 38k tok".into(),
        }],
        recent: (1..=8).map(|n| format!("event {n}")).collect(),
        waiting: vec!["plan approval".into()],
        ..StatusSummary::default()
    };
    driver.send(AppEvent::View {
        data: Box::new(argonauts_core::app::ViewData::Status { summary }),
    });
    assert_eq!(
        driver.printed(),
        "● Status(run-01)\n  ⎿  Goal: Add a --json flag\n     G1 export --json writes JSON lines\n     Plan: Plan 1, 2 tasks\n     task-001 Parse the flag · running · GPT 5.6 on Codex · 6m 12s · 38k tok\n     event 4\n     event 5\n     event 6\n     event 7\n     event 8\n     Waiting on you: plan approval"
    );
    let empty = super::status_cells(&StatusSummary::default(), &Default::default());
    assert_eq!(empty.len(), 1);
}

/// A greeting answered the way the main agent does: it checks the run status, streams the
/// answer, then sends the report.
fn greeting_with_own_calls() -> Vec<AppEvent> {
    let turn = TurnId::numbered(1);
    let answer = "Hi! What would you like to do?";
    let agent = |event| AppEvent::Conversation {
        event: ConversationEvent::Agent {
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
    let mut events = vec![AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: "hi".into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
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
    events.push(AppEvent::Conversation {
        event: ConversationEvent::TurnFinished {
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
        let mut driver = TestDriver::new();
        driver.settings.verbosity = level;
        for event in greeting_with_own_calls() {
            driver.send(event);
        }
        let printed = driver.printed();
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
        let cells = driver.model.history.cells();
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
    let mut driver = TestDriver::new();
    let mut events = greeting_with_own_calls();
    // Drop the report's end.
    let report_end = events
        .iter()
        .position(|event| {
            matches!(event, AppEvent::Conversation { event: ConversationEvent::Agent { event: AgentEvent::ToolFinished { id, .. }, .. } } if id == "report-1")
        })
        .expect("the report's end");
    events.remove(report_end);
    for event in events {
        driver.send(event);
    }
    assert!(!driver.printed().contains("Report"));
    let report = driver
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
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(run_event(RunEventKind::Task(TaskEvent::Try {
        try_number: 2,
        worker: ModelRef::new("claude-code", None),
        cause: TryCause::Rotation,
        told: Vec::new(),
    })));
    let printed = driver.printed();
    assert!(
        printed.contains("task-001 rotated to a fresh Claude Code session"),
        "{printed}"
    );
}

#[test]
fn a_restart_prints_a_line() {
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(run_event(RunEventKind::Task(TaskEvent::Try {
        try_number: 2,
        worker: ModelRef::new("claude-code", None),
        cause: TryCause::Restarted,
        told: Vec::new(),
    })));
    let printed = driver.printed();
    assert!(
        printed.contains("task-001 restarted in a new Claude Code session"),
        "{printed}"
    );
    assert!(printed.contains("its work so far is kept"), "{printed}");
}

#[test]
fn a_model_you_pick_is_named_as_its_list_named_it() {
    let mut driver = TestDriver::new();
    let choice = |worker: &str, name: &str| argonauts_core::app::ModelChoice {
        worker: worker.parse().unwrap(),
        provider: "openai".into(),
        access: argonauts_core::registry::ModelAccess::Subscription,
        cost: argonauts_core::limits::CostEstimate::Unknown,
        plan_usage: None,
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
        listed: None,
    };
    driver.state.model_words.insert(
        "codex/gpt-6-sol".to_owned(),
        crate::format::choice_words(&choice("codex/gpt-6-sol", "GPT-6-Sol")),
    );
    driver.send(AppEvent::SettingChanged {
        key: "crew.main".into(),
        value: "\"codex/gpt-6-sol\"".into(),
        scope: argonauts_core::settings::SettingScope::Global,
        previous: Some("\"claude-code/claude-opus-5-5\"".into()),
        reset: false,
    });
    // What the switch carried hangs under it, with no row between.
    driver.send(AppEvent::Continued {
        text: "Carried over: a summary of 14 exchanges, the last 6 in full".into(),
    });
    assert_eq!(
        driver.printed().trim_end(),
        "  ⎿  Set model to GPT-6-Sol on Codex · everywhere · was Opus 5.5 on Claude Code\n     \
         Carried over: a summary of 14 exchanges, the last 6 in full"
    );
}

/// A CLI's default reads as the model it runs when the line names the CLI anyway.
#[test]
fn a_default_left_for_another_cli_is_named_by_its_model() {
    let mut driver = TestDriver::new();
    driver.state.model_words.insert(
        "claude-code".to_owned(),
        "Claude Code's default (Opus 5.5)".to_owned(),
    );
    driver
        .state
        .model_words
        .insert("codex/gpt-6-sol".to_owned(), "GPT-6-Sol".to_owned());
    driver.send(AppEvent::SettingChanged {
        key: "crew.main".into(),
        value: "\"codex/gpt-6-sol\"".into(),
        scope: argonauts_core::settings::SettingScope::Global,
        previous: Some("\"claude-code\"".into()),
        reset: false,
    });
    assert_eq!(
        driver.printed().trim_end(),
        "  ⎿  Set model to GPT-6-Sol on Codex · everywhere · was Opus 5.5 on Claude Code"
    );
}

/// The summary a main model on another CLI starts from shows folded: its first lines, and the
/// rest behind Ctrl+O.
#[test]
fn a_folded_summary_keeps_the_rest_for_ctrl_o() {
    let mut driver = TestDriver::new();
    driver.send(AppEvent::Folded {
        title: "Summary for GPT-6.1-Sol · written by Opus 5.5 · 212 words".into(),
        lines: vec!["Asked: due dates.".into(), "Done: the store.".into()],
        more: vec!["Open: the overdue tests.".into()],
    });
    let printed = driver.printed();
    assert!(
        printed.contains(
            "● Summary for GPT-6.1-Sol · written by Opus 5.5 · 212 words (ctrl+o to expand)\n  ⎿  \
             Asked: due dates.\n     Done: the store."
        ),
        "{printed}"
    );
    assert!(!printed.contains("overdue"), "{printed}");

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now())).with_expanded(true);
    let expanded: String = driver
        .model
        .history
        .cells()
        .iter()
        .map(|cell| plain_text(&cell.lines(&ctx)))
        .collect();
    assert!(expanded.contains("Open: the overdue tests."), "{expanded}");
}

/// A model, an effort or a workflow set while an answer runs waits for the next request, and
/// the result line says so.
#[test]
fn a_change_made_mid_answer_says_it_applies_at_the_next_request() {
    use argonauts_core::settings::SettingScope;

    let changed = |key: &str, value: &str| AppEvent::SettingChanged {
        key: key.into(),
        value: value.into(),
        scope: SettingScope::Global,
        previous: None,
        reset: false,
    };
    let mut driver = TestDriver::new();
    driver.send(changed("workflow", "\"council\""));
    assert_eq!(
        driver.printed().trim(),
        "● Set workflow = council · everywhere"
    );

    driver.model.bottom.working = Some(crate::bottom::WorkingLine::new("Rowing", Instant::now()));
    driver.send(changed("workflow", "\"captain\""));
    assert_eq!(
        driver.printed().trim(),
        "● Set workflow = captain · everywhere · applies at the next request"
    );
    driver.send(changed("roles.main.effort", "\"high\""));
    assert_eq!(
        driver.printed().trim(),
        "⎿  Set effort to high · everywhere · applies at the next request"
    );
    driver.send(changed("ui.theme", "\"dark\""));
    assert_eq!(
        driver.printed().trim(),
        "● Set ui.theme = dark · everywhere",
        "a look applies at once"
    );
}

/// `/effort`'s Ultracode toggle reads as a result line, not as its key.
#[test]
fn ultracode_saved_reads_as_turned_on_or_off() {
    for (value, scope, line) in [
        (
            "true",
            argonauts_core::settings::SettingScope::ProjectLocal,
            "  ⎿  Turned Ultracode on · just you",
        ),
        (
            "false",
            argonauts_core::settings::SettingScope::Session,
            "  ⎿  Turned Ultracode off · this session",
        ),
    ] {
        let mut driver = TestDriver::new();
        driver.send(AppEvent::SettingChanged {
            key: "ultracode".into(),
            value: value.into(),
            scope,
            previous: None,
            reset: false,
        });
        assert_eq!(driver.printed().trim_end(), line);
    }
}

/// A run's start prints no row of its own and no plumbing. It says only what you
/// would otherwise never hear, such as the files it left out.
#[test]
fn a_run_starts_without_a_row_of_its_own() {
    let mut driver = TestDriver::new();
    driver.send(started());
    let printed = driver.printed();
    for gone in [
        "Starts from",
        "Goal: your prompt as you wrote it",
        "8b41d07",
        "run-01",
    ] {
        assert!(!printed.contains(gone), "{gone} in:\n{printed}");
    }

    let mut driver = TestDriver::new();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
        Box::new(RunStarted {
            uncommitted: vec!["notes.txt".into(), "draft.md".into()],
            ..started_fields()
        }),
    ))));
    let printed = driver.printed();
    assert!(
        printed.contains("2 uncommitted files left out"),
        "{printed}"
    );
}

/// `● Resume(<title>)` names the run as people know it.
#[test]
fn resume_names_the_run_by_its_title() {
    use argonauts_core::events::run::{ResumeCause, RunResumed};

    let mut driver = TestDriver::new();
    let start = started();
    driver.state.apply(&start);
    driver.send(start);
    driver.printed();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Resumed(
        RunResumed {
            cause: ResumeCause::User,
            settings_changed: false,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        },
    ))));
    let printed = driver.printed();
    assert!(printed.contains("● Resume(Add a --json flag)"), "{printed}");
    assert!(!printed.contains("run-01"), "{printed}");
}

/// A question's wait prints nothing, so your answer moving the run on prints nothing either, unless
/// something changed while it waited.
#[test]
fn an_answer_resumes_the_run_without_a_line() {
    use argonauts_core::events::run::{ResumeCause, RunResumed};

    let resumed = |settings_changed: bool| {
        run_event(RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::Answer,
            settings_changed,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        })))
    };
    let mut driver = TestDriver::new();
    let start = started();
    driver.state.apply(&start);
    driver.send(start);
    driver.printed();
    driver.send(resumed(false));
    assert_eq!(driver.printed().trim(), "");
    driver.send(resumed(true));
    let printed = driver.printed();
    assert!(printed.contains("● Resume(Add a --json flag)"), "{printed}");
    assert!(printed.contains("After your answer"), "{printed}");
    assert!(
        printed.contains("Settings changed since it stopped"),
        "{printed}"
    );
}

/// What the merge question put up for the merge, as `RunState` keeps it.
fn merge_opened(tasks: usize, combined: argonauts_core::DiffStat) -> AppEvent {
    use argonauts_core::events::approval::{
        ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, MergePacket,
        MergeRow,
    };
    use argonauts_core::events::task::TaskKind;
    use argonauts_core::names::ApprovalId;
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

    let rows = (1..=tasks)
        .map(|n| MergeRow {
            task: TaskId::new(format!("task-{n:03}")).unwrap(),
            title: format!("Task {n}"),
            kind: TaskKind::Build,
            stat: argonauts_core::DiffStat {
                files_changed: 1,
                insertions: 3,
                deletions: 0,
            },
            worker: None,
            checks_passed: true,
            review: None,
            debt: Vec::new(),
            questions: Vec::new(),
        })
        .collect();
    let packet = MergePacket {
        target: Some("main".into()),
        branch: "argonauts/run-01/merged".into(),
        rows,
        suite_passed_on_latest_main: true,
        checks: Vec::new(),
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
        both_changed: Vec::new(),
        asks_which_branch: false,
        again: None,
    };
    run_event(RunEventKind::Approval(ApprovalEvent::Opened(Box::new(
        ApprovalOpened {
            id: ApprovalId::numbered(1),
            kind: ApprovalKind::Merge,
            title: "Merge this run into main?".into(),
            task: None,
            packet: ApprovalPacket::Merge(packet),
            evidence_sha256: "0".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::Guidance,
            needs_you: true,
        },
    ))))
}

/// The line after a merge says what the merge question said, and the spend the bottom line shows.
#[test]
fn the_finish_line_says_what_the_merge_question_said() {
    let mut driver = TestDriver::new();
    let fold = |driver: &mut TestDriver, event: AppEvent| {
        driver.state.apply(&event);
        driver.send(event);
    };
    fold(&mut driver, started());
    // More tasks than the merge took, with their own diffs.
    for n in 1..=10 {
        let mut event = run_event(RunEventKind::Task(TaskEvent::Diff {
            stat: argonauts_core::DiffStat {
                files_changed: 1,
                insertions: 2,
                deletions: 0,
            },
            path: PathBuf::from("/nonexistent/task.diff"),
        }));
        if let AppEvent::Run { event } = &mut event {
            event.task_id = Some(TaskId::new(format!("task-{n:03}")).unwrap());
        }
        fold(&mut driver, event);
    }
    let combined = argonauts_core::DiffStat {
        files_changed: 13,
        insertions: 78,
        deletions: 1,
    };
    fold(&mut driver, merge_opened(8, combined));
    // A session reports more than the limit has counted yet.
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        "claude-code".parse().unwrap(),
        "claude-code",
        argonauts_core::names::RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.cost_usd = Some(0.26);
    let run = driver.state.runs.get_mut(&run_id()).unwrap();
    run.sessions.insert(session.key.clone(), session);
    fold(
        &mut driver,
        run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        })),
    );
    let printed = driver.printed();
    assert!(
        printed.contains("Merged the run into main (8 tasks, 13 files, +78 -1)"),
        "{printed}"
    );
    let run = &driver.state.runs[&run_id()];
    let spent = format!("· {}", crate::format::usd(run.cost_usd()));
    let finished = printed
        .lines()
        .find(|line| line.contains("Run finished"))
        .unwrap();
    assert!(finished.contains(&spent), "{printed}");
    assert!(finished.contains("$0.26"), "{printed}");
}

/// The last two rows of a run in a folder without git, as the picture of that folder has
/// them: where the work went, and that the run's copies are gone.
#[test]
fn a_run_written_into_a_folder_without_git_ends_as_its_picture_does() {
    use argonauts_core::events::task::MergeEvent;
    use argonauts_core::log::COPIES_REMOVED;

    let picture = include_str!("../../../tests/golden/no_git_fixes.txt");
    let at = picture
        .lines()
        .position(|row| row.starts_with("● Written into your folder"))
        .expect("the picture has the closing rows");
    let closing: Vec<&str> = picture.lines().skip(at).take(2).collect();

    let mut driver = TestDriver::new();
    let fold = |driver: &mut TestDriver, event: AppEvent| {
        driver.state.apply(&event);
        driver.send(event);
    };
    let later = |mut event: AppEvent| {
        if let AppEvent::Run { event } = &mut event {
            event.ts = datetime!(2026-09-30 10:06:20 UTC);
        }
        event
    };
    fold(&mut driver, started());
    let combined = argonauts_core::DiffStat {
        files_changed: 4,
        insertions: 52,
        deletions: 6,
    };
    fold(&mut driver, merge_opened(2, combined));
    fold(
        &mut driver,
        run_event(RunEventKind::Merge(MergeEvent::RunMerged {
            target: argonauts_core::hidden_git::BRANCH.to_owned(),
            sha: "3fa2c1d".to_owned(),
            fast_forward: true,
        })),
    );
    fold(
        &mut driver,
        run_event(RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!("{COPIES_REMOVED}base, 2 tasks, merge"),
        })),
    );
    fold(
        &mut driver,
        later(run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        }))),
    );
    let printed = driver.printed_at(118);
    let rows: Vec<&str> = printed.lines().map(str::trim_end).collect();
    let first = rows
        .iter()
        .position(|row| *row == closing[0])
        .unwrap_or_else(|| panic!("{printed}"));
    // The picture leaves out what every run's last row says in the transcript's own way: how
    // long it took, spelled as on the line under it, and what it spent.
    let last = closing[1].replace("6m20s", "6m 20s · $0.00");
    assert_eq!(rows[first + 1], last, "{printed}");
}

#[test]
fn a_shell_command_runs_live_and_prints_once_it_ends() {
    use argonauts_core::app::ShellEvent;

    let shell = |event| AppEvent::Shell { event };
    let mut h = TestDriver::new();
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
fn the_goal_line_counts_the_main_agents_answers_apart() {
    use argonauts_core::events::goal::{
        Assumption, Criterion, GoalEvent, GoalFile, GoalSource, MAIN_AGENT_ANSWER,
    };

    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(note(
        NoteLevel::Info,
        "The main agent answered 1 of the goal's questions for you. Its answers are assumptions in \
         the goal, which Ctrl+O and /status show",
    ));
    let assumption = |text: &str, basis: String| Assumption {
        text: text.into(),
        basis,
        by: None,
    };
    driver.send(run_event(RunEventKind::Goal(GoalEvent::Approved {
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
                    format!("{MAIN_AGENT_ANSWER}: the README"),
                ),
                assumption("the flag's name: --verbose", "a detail".into()),
            ],
            answers: vec![],
            source: GoalSource::Readings,
            path: PathBuf::from("goal.md"),
            sha256: String::new(),
        },
        by: Party::User,
    })));
    let printed = driver.printed();
    assert!(
        printed.contains("The main agent answered 1 of the goal's questions for you"),
        "{printed}"
    );
    assert!(
        printed.contains("1 question answered by the main agent · 1 assumption stated"),
        "{printed}"
    );
}

/// A crew run named `fix-slugify-median` that declared its workflow, ready to end.
fn crew_workflow_driver() -> TestDriver {
    use argonauts_core::events::run::RunNamed;
    use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::workflow::StandardPhase;

    let mut driver = TestDriver::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "council".into(),
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
        based_on: Some("council".into()),
        done_when: Vec::new(),
    });
    driver.state.runs.insert(run_id(), run);
    if let AppEvent::Run { event } = &mut started()
        && let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind
    {
        let mut crew = (**started).clone();
        crew.mode = RunMode::Crew;
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            Box::new(crew),
        ))));
    }
    let phase = |index: u32, title: &str, detail: &str| DeclaredPhase {
        index,
        name: title.into(),
        detail: detail.into(),
        standard: StandardPhase::Do,
    };
    driver.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "council".into(),
            description: "Peers draft, judge, and build".into(),
            phases: vec![
                phase(0, "Fix", "one worker per function"),
                phase(1, "Merge", "the final check, then the merge"),
            ],
            required: Vec::new(),
        }),
    ))));
    driver
}

/// A crew run's workflow prints `● Workflow(<name>)` with
/// what it started from and how to open it, its phases on one line, and `● Workflow(<name>)
/// · done` before the run's end.
#[test]
fn a_crew_workflow_prints_its_start_and_its_end() {
    let mut driver = crew_workflow_driver();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = driver.printed();
    let call = "● Workflow(Fix slugify median · Council)\n";
    assert!(
        printed.contains(&format!(
            "{call}  ⎿  Running in background · /workflows to monitor and save"
        )),
        "{printed}"
    );
    assert!(
        printed.contains(&format!("{call}  ⎿  ✔ Completed in ")),
        "{printed}"
    );
    assert!(!printed.contains("Workflow(council)"), "{printed}");
}

/// The start says where to watch the workflow, as Claude Code's does. Its phases and who does
/// them are the workflow view's to show.
#[test]
fn the_start_says_where_to_watch_it_and_leaves_the_phases_to_the_view() {
    use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::workflow::StandardPhase;

    let mut driver = TestDriver::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "council".into(),
        from: None,
        description: String::new(),
        phases: vec!["Fix".into(), "Merge".into()],
        parent: None,
        steps: Vec::new(),
    });
    driver.state.runs.insert(run_id(), run);
    if let AppEvent::Run { event } = &mut started()
        && let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind
    {
        let mut crew = (**started).clone();
        crew.mode = RunMode::Crew;
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            Box::new(crew),
        ))));
    }
    let phase = |index: u32, title: &str, detail: &str| DeclaredPhase {
        index,
        name: title.into(),
        detail: detail.into(),
        standard: StandardPhase::Do,
    };
    driver.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "council".into(),
            description: String::new(),
            phases: vec![
                phase(
                    0,
                    "Fix",
                    "one worker per function, each with a test that failed before the fix",
                ),
                phase(1, "Merge", "the final check, then the merge"),
            ],
            required: Vec::new(),
        }),
    ))));
    let printed = driver.printed_at(60);
    assert!(
        printed.ends_with("  ⎿  Running in background · /workflows to monitor and save"),
        "{printed}"
    );
    assert!(!printed.contains("Fix · Merge"), "{printed}");
    assert!(!printed.contains("each with a test"), "{printed}");
}

/// A run's end reads one way everywhere it shows: a stopped run's workflow reads `✘ Stopped in`,
/// as its panel row is stopped, never completed. A failed run is one block, "The run failed".
#[test]
fn a_workflow_ends_as_its_run_did() {
    use crate::bottom::WorkflowRowState;

    for (outcome, word, ended, end) in [
        (
            RunOutcome::Failed,
            "failed",
            "✘ Failed in",
            "The run failed",
        ),
        (
            RunOutcome::Stopped,
            "stopped",
            "✘ Stopped in",
            "Stopped the run",
        ),
        (
            RunOutcome::Abandoned,
            "stopped",
            "✘ Stopped in",
            "Abandoned the run",
        ),
        (
            RunOutcome::Merged,
            "done",
            "✔ Completed in",
            "Merged the run",
        ),
    ] {
        let mut driver = crew_workflow_driver();
        driver.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
            Box::new(ClosingAnswer {
                text: "The run ended after 9 of its 10 agents reported.".into(),
                by: None,
                files: Vec::new(),
                checks: Vec::new(),
                tests: Vec::new(),
                left: Vec::new(),
                decided: Vec::new(),
            }),
        ))));
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome,
            detail: None,
        })));
        let printed = driver.printed();
        let item = format!("  ⎿  {ended} ");
        // A failed run is one block, "The run failed", with no row for its workflow.
        assert_eq!(
            printed.contains(&item),
            outcome != RunOutcome::Failed,
            "{printed}"
        );
        assert!(printed.contains(end), "{printed}");
        assert_eq!(WorkflowRowState::ended(outcome).word(), word);
        if outcome != RunOutcome::Merged {
            assert!(!printed.contains("Completed in"), "{printed}");
        }
    }
}

/// A failed run is one block: "The run ended" and the failure in one plain line, and never
/// the failure again in the run's last word.
#[test]
fn a_failed_run_says_what_failed_once() {
    let mut driver = crew_workflow_driver();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(ClosingAnswer {
            text: "The run failed before any agent reported.".into(),
            by: None,
            files: Vec::new(),
            checks: Vec::new(),
            tests: Vec::new(),
            left: Vec::new(),
            decided: Vec::new(),
        }),
    ))));
    let git = "{\"message\":\"The following paths are ignored by one of your .gitignore files:\\n.argonauts\"}";
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: Some(git.into()),
    })));
    let printed = driver.printed();
    assert_eq!(printed.matches("The run ended · ").count(), 1, "{printed}");
    // The run's last word says no more than the line.
    assert!(!printed.contains("The run failed"), "{printed}");
    assert_eq!(
        printed.matches("are ignored by one of your").count(),
        1,
        "{printed}"
    );
    assert!(!printed.contains("Run ended"), "{printed}");
    assert!(!printed.contains("\\n.argonauts"), "{printed}");
    // git's whole text waits behind `r`, as the line says.
    let words = printed.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(words.matches("· r shows it all").count(), 1, "{printed}");
    let raw = driver
        .model
        .raw_failure
        .as_ref()
        .expect("the raw text is kept");
    assert_eq!(raw.text, git);
    // The session's own line about the failure, with no message of yours, leaves `r` to it.
    let mut told = conversation_turn(1, "", "The council workflow didn't start. Want me to look?");
    if let AppEvent::Conversation {
        event: ConversationEvent::TurnStarted { continues, .. },
    } = &mut told[0]
    {
        *continues = true;
    }
    for event in told {
        driver.send(event);
    }
    assert!(driver.model.raw_failure.is_some());
    // Once your next message starts a turn, `r` is a letter again.
    for event in conversation_turn(2, "retry with Sonnet", "Retrying.") {
        driver.send(event);
    }
    assert_eq!(driver.model.raw_failure, None);
}

/// A run that failed with nothing more to say than its one line hints at no raw text.
#[test]
fn a_failed_run_that_said_it_all_has_no_raw_hint() {
    let mut driver = crew_workflow_driver();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: Some("the merge was refused".into()),
    })));
    let printed = driver.printed();
    assert!(printed.contains("the merge was refused"), "{printed}");
    assert!(!printed.contains("r shows"), "{printed}");
    assert_eq!(driver.model.raw_failure, None);
}

/// A run that ended without merging says why on its line, what held it under that, and that
/// its work waits for `/resume`. In a folder without git it has no branch to keep.
#[test]
fn a_run_that_ended_without_merging_says_why_and_how_to_carry_it_on() {
    let detail = "Nothing was merged: build:fix-the-money stopped before its checks passed\n\
                  it was blocked on a question";
    let finished = || {
        run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Failed,
            detail: Some(detail.into()),
        }))
    };
    let mut driver = crew_workflow_driver();
    driver.send(finished());
    let printed = driver.printed_at(118);
    assert!(
        printed.contains(
            "● The run ended · Nothing was merged: build:fix-the-money stopped before its checks \
             passed\n  ⎿  it was blocked on a question · the branch is kept · /resume carries it on"
        ),
        "{printed}"
    );
    // Both lines are all it said, so nothing waits behind `r`.
    assert_eq!(driver.model.raw_failure, None);

    let mut driver = TestDriver::new();
    if let AppEvent::Run { event } = &mut started()
        && let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind
    {
        started.base_branch = Some(argonauts_core::hidden_git::BRANCH.into());
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            started.clone(),
        ))));
    }
    driver.send(finished());
    let printed = driver.printed_at(118);
    assert!(
        printed.contains("⎿  it was blocked on a question · the work is kept · /resume"),
        "{printed}"
    );
    assert!(!printed.contains("branch"), "{printed}");
}

/// A crew run whose request named who orchestrates and who reviews: one quiet item says what
/// was followed, the workflow the main agent declared prints under it as Claude Code prints its
/// own, and the closing answer comes last, after `Workflow(<name>) · done` and the merge.
#[test]
fn a_run_as_you_asked_says_so_then_prints_its_workflow_then_its_closing_answer() {
    use argonauts_core::events::run::{
        CaptainOrCrewDecision, CheckedInstruction, InstructionOutcome, RunNamed,
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
    let mut driver = TestDriver::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "orchestrator".into(),
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
        based_on: Some("orchestrator".into()),
        done_when: Vec::new(),
    });
    driver.state.runs.insert(run_id(), run);
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
        Box::new(RunStarted {
            mode: RunMode::Crew,
            captain_or_crew: Some(CaptainOrCrewDecision {
                mode: RunMode::Crew,
                move_to_crew: false,
                rule: CaptainOrCrewDecision::AS_YOU_ASKED.into(),
                reason: String::new(),
                captain: None,
            }),
            instructions: vec![
                followed(
                    "have codex orchestrate",
                    InstructionKind::Orchestrate,
                    "Codex orchestrates",
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
        name: title.into(),
        detail: detail.into(),
        standard: StandardPhase::Do,
    };
    driver.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "orchestrator".into(),
            description: "Codex plans, workers build, Claude Code reviews".into(),
            phases: vec![
                phase(0, "Build", "one worker per file"),
                phase(1, "Review", "Claude Code reviews each task"),
            ],
            required: Vec::new(),
        }),
    ))));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(closing()),
    ))));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = driver.printed();
    let at = |shown: &str| {
        printed
            .find(shown)
            .unwrap_or_else(|| panic!("{shown} missing from:\n{printed}"))
    };
    let order = [
        at("As you asked: Codex orchestrates · Claude Code reviews"),
        at("● Workflow(Add due dates · orchestrator workflow)\n  ⎿  Running in background"),
        at("● Workflow(Add due dates · orchestrator workflow)\n  ⎿  ✔ Completed in"),
        at("Merged the run"),
        at("● Fixed parse()"),
    ];
    assert!(order.is_sorted(), "{order:?}\n{printed}");
    assert_eq!(printed.matches("● Workflow(Add due dates · ").count(), 2);
}

/// A run whose request said how to run it, with a long instruction
/// and one in Chinese, at 80, 100, and 120 columns.
#[test]
fn a_run_s_start_says_what_was_followed_and_what_was_not() {
    use argonauts_core::events::run::{
        CaptainOrCrewDecision, CheckedInstruction, InstructionOutcome,
    };
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
            "have codex orchestrate",
            InstructionKind::Orchestrate,
            "Codex orchestrates",
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
            "Grok Build isn't installed, so the crew's workers build",
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
        let mut driver = TestDriver::new();
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
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
                captain_or_crew: Some(CaptainOrCrewDecision {
                    mode: RunMode::Crew,
                    move_to_crew: false,
                    rule: CaptainOrCrewDecision::AS_YOU_ASKED.into(),
                    reason: String::new(),
                    captain: None,
                }),
                instructions: instructions.clone(),
                ..started_fields()
            }),
        ))));
        let printed = driver.printed_at(width);
        for shown in [
            "As you asked: Codex orchestrates · Claude Code reviews with Opus 5.5, high effort",
            "Grok Build isn't installed, so the crew's workers build",
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
        for line in printed.lines() {
            assert!(line.width() <= usize::from(width), "{width}: {line:?}");
        }
    }
}

/// A Captain that works in your checkout has no run and no row of its own. The reply says what your
/// words asked when there is something to say, and the turn that goes ahead shows your message
/// only once.
#[test]
fn a_direct_captain_says_so_and_goes_ahead_without_repeating_your_message() {
    use argonauts_core::events::run::{
        CaptainOrCrewDecision, CheckedInstruction, InstructionOutcome,
    };
    use argonauts_core::settings::protocol::CaptainKind;
    use argonauts_core::steps::intake::{ChangeEstimate, InstructionKind};

    let request = "Fix the rounding in invoice/money.py, just do it yourself.";
    let mut events = conversation_turn(1, request, "The sign is applied too late.");
    events.pop();
    events.push(AppEvent::Conversation {
        event: ConversationEvent::TurnFinished {
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
                captain_or_crew: CaptainOrCrewDecision {
                    mode: RunMode::Captain,
                    move_to_crew: false,
                    rule: CaptainOrCrewDecision::AS_YOU_ASKED.into(),
                    reason: String::new(),
                    captain: Some(CaptainKind::Direct),
                },
                instructions: vec![CheckedInstruction {
                    says: "just do it yourself".into(),
                    kind: InstructionKind::Mode,
                    outcome: InstructionOutcome::Followed {
                        said: "the main agent does it".into(),
                        note: None,
                    },
                }],
            },
            spend: Spend::default(),
            elapsed_ms: 2_000,
        },
    });
    let mut going = conversation_turn(2, request, "Fixed it. All 6 tests pass.");
    if let AppEvent::Conversation {
        event: ConversationEvent::TurnStarted { continues, .. },
    } = &mut going[0]
    {
        *continues = true;
    }
    events.extend(going);
    let mut driver = TestDriver::new();
    for event in events {
        driver.send(event);
    }
    let printed = driver.printed_at(100);
    assert!(!printed.contains("Estimate:"), "{printed}");
    assert!(
        printed.contains("As you asked: the main agent does it"),
        "{printed}"
    );
    assert_eq!(
        printed.matches("❯ Fix the rounding").count(),
        1,
        "{printed}"
    );
    assert!(printed.contains("Fixed it. All 6 tests pass."), "{printed}");
}

/// Your words chose a Captain run checked in a worktree, and its start says what was followed.
#[test]
fn a_captain_the_words_checked_says_so() {
    use argonauts_core::events::run::{
        CaptainOrCrewDecision, CheckedInstruction, InstructionOutcome,
    };
    use argonauts_core::settings::protocol::CaptainKind;
    use argonauts_core::steps::intake::InstructionKind;

    let mut driver = TestDriver::new();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
        Box::new(RunStarted {
            captain_or_crew: Some(CaptainOrCrewDecision {
                mode: RunMode::Captain,
                move_to_crew: false,
                rule: CaptainOrCrewDecision::AS_YOU_ASKED.into(),
                reason: String::new(),
                captain: Some(CaptainKind::Checked),
            }),
            instructions: vec![
                CheckedInstruction {
                    says: "check it in a copy first".into(),
                    kind: InstructionKind::Isolation,
                    outcome: InstructionOutcome::Followed {
                        said: "checked in a worktree".into(),
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
    let printed = driver.printed_at(100);
    assert!(
        printed.contains("As you asked: checked in a worktree · Codex reviews"),
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
fn a_joined_message_a_mods_log_and_a_mirrored_workflow_in_the_conversation() {
    use argonauts_core::PluginOutputKind;
    use argonauts_core::events::session::{CliWorkflowRun, SubagentStatus};

    let mut driver = TestDriver::new();
    let turn = TurnId::numbered(1);
    let conversation = |event| AppEvent::Conversation { event };
    let agent = |event| {
        conversation(ConversationEvent::Agent {
            turn: turn.clone(),
            event,
        })
    };
    driver.send(conversation(ConversationEvent::TurnStarted {
        turn: turn.clone(),
        text: "check release.md, then run ./slow-test.sh".into(),
        worker: ModelRef::new("claude-code", Some("opus".into())),
        read_only: false,
        commit: "8b41d07".into(),
        uncommitted: vec![],
        settings_sha256: None,
        continues: false,
    }));
    driver.send(agent(AgentEvent::PluginOutput {
        kind: PluginOutputKind::Log,
        plugin: Some("pr-tools".into()),
        text: Some("3 reviewers asked".into()),
        timeout_ms: None,
    }));
    driver.send(agent(AgentEvent::ToolStarted {
        id: "wf-call".into(),
        name: "Workflow".into(),
        input: serde_json::json!({ "script": "export const meta = {}" }),
        parent: None,
    }));
    driver.send(agent(AgentEvent::Workflow {
        workflow: Box::new(CliWorkflowRun {
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
    driver.send(conversation(ConversationEvent::Joined {
        turn: turn.clone(),
        text: "also check the changelog".into(),
    }));
    let printed = driver.printed_at(100);
    assert!(
        printed.contains("⎿  pr-tools: 3 reviewers asked"),
        "{printed}"
    );
    assert!(
        printed.contains(
            "● Workflow(release-check · Reads the release notes and runs the slow test)\n  \
             ⎿  Running in background · /workflows to monitor and save"
        ),
        "{printed}"
    );
    assert!(
        !printed.contains("script"),
        "the raw call stays out:\n{printed}"
    );
    let joined = printed.find("❯ also check the changelog").expect(&printed);
    assert!(printed.find("Workflow(").unwrap() < joined, "{printed}");
}

/// Esc that stops the main agent's turn alone leaves its line when the turn stops, with the
/// turn still open. What the main agent says once its subagents are done
/// prints below, and a turn that ends with nothing more doesn't get the line twice.
#[test]
fn a_turn_stopped_alone_leaves_the_interrupted_line_once() {
    const LINE: &str = "⎿  Interrupted · What should Argonauts Code do instead?";
    let turn = TurnId::numbered(1);
    let conversation = |event| AppEvent::Conversation { event };
    let said = |event| {
        conversation(ConversationEvent::Agent {
            turn: TurnId::numbered(1),
            event,
        })
    };
    let writes = |text: &str| {
        said(AgentEvent::TextDelta {
            text: text.into(),
            parent: None,
        })
    };
    let stopped = || {
        said(AgentEvent::Error {
            category: argonauts_core::ErrorCategory::Interrupted,
            message: "interrupted".into(),
            retryable: false,
        })
    };
    let started = || {
        conversation(ConversationEvent::TurnStarted {
            turn: TurnId::numbered(1),
            text: "count in the background".into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
            read_only: true,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        })
    };

    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(writes("The count runs on its own while I"));
    driver.send(stopped());
    let printed = driver.printed();
    assert!(
        printed.ends_with(&format!("● The count runs on its own while I\n  {LINE}")),
        "{printed}"
    );
    driver.send(writes("The count reached four hundred."));
    driver.send(conversation(ConversationEvent::TurnFinished {
        turn: turn.clone(),
        reply: TurnReply::Answer {
            markdown: "The count reached four hundred.".into(),
        },
        spend: Spend::default(),
        elapsed_ms: 3_000,
    }));
    let printed = driver.printed();
    assert!(
        printed.contains("● The count reached four hundred."),
        "{printed}"
    );
    assert!(!printed.contains("Interrupted"), "{printed}");

    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(writes("The count runs on its own while I"));
    driver.send(stopped());
    driver.send(conversation(ConversationEvent::TurnCancelled { turn }));
    let printed = driver.printed();
    assert_eq!(printed.matches("Interrupted").count(), 1, "{printed}");
}

/// A turn a watchdog stopped ends with one line under it that says what it saw, and no
/// Interrupted line, since nobody pressed Esc.
#[test]
fn a_turn_a_watchdog_stopped_says_what_it_saw() {
    let turn = TurnId::numbered(1);
    let conversation = |event| AppEvent::Conversation { event };
    let mut driver = TestDriver::new();
    driver.send(conversation(ConversationEvent::TurnStarted {
        turn: turn.clone(),
        text: "fix slugify".into(),
        worker: ModelRef::new("opencode", Some("ollama/qwen3-coder:30b-64k".into())),
        read_only: false,
        commit: "8b41d07".into(),
        uncommitted: vec![],
        settings_sha256: None,
        continues: false,
    }));
    driver.send(conversation(ConversationEvent::Agent {
        turn: turn.clone(),
        event: AgentEvent::TextDelta {
            text: "Let me hand in the report.".into(),
            parent: None,
        },
    }));
    let why = "Stopped: Qwen3 Coder 30B repeated the same execute call 12 times. Its edits stay \
               in your checkout.";
    driver.send(conversation(ConversationEvent::TurnStopped {
        turn,
        why: why.into(),
    }));
    let printed = driver.printed();
    assert!(
        printed.ends_with(
            "● Let me hand in the report.\n  ⎿  Stopped: Qwen3 Coder 30B repeated the same \
             execute call 12 times. Its edits\n     stay in your checkout."
        ),
        "{printed}"
    );
    assert!(!printed.contains("Interrupted"), "{printed}");
    assert!(!printed.contains("couldn't answer"), "{printed}");
}

/// A crew run named `switch-reads` whose one step runs another workflow's steps, and whose own
/// phases say only that: Work, then Merge. Benchmark is a step you require.
fn handed_over_driver() -> (TestDriver, argonauts_core::names::WorkflowId) {
    use argonauts_core::events::run::RunNamed;
    use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::workflow::StandardPhase;

    let mut driver = TestDriver::new();
    let id = WorkflowId::numbered(1);
    let mut run = crate::state::RunState::new(run_id());
    run.workflows.push(crate::state::WorkflowState {
        id: id.clone(),
        name: "script".into(),
        from: None,
        description: String::new(),
        phases: vec!["Work".into(), "Merge".into()],
        parent: None,
        steps: Vec::new(),
    });
    run.reported.expect_from(&id);
    run.named = Some(RunNamed {
        name: "switch-reads".into(),
        description: "Move reads to the new table".into(),
        style: RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    driver.state.runs.insert(run_id(), run);
    if let AppEvent::Run { event } = &mut started()
        && let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind
    {
        let mut crew = (**started).clone();
        crew.mode = RunMode::Crew;
        driver.send(run_event(RunEventKind::Run(RunLifecycle::Started(
            Box::new(crew),
        ))));
    }
    let phase = |index: u32, title: &str| DeclaredPhase {
        index,
        name: title.into(),
        detail: String::new(),
        standard: StandardPhase::Do,
    };
    driver.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id: id.clone(),
            name: "script".into(),
            description: "The steps of the workflow in use, then the merge approval".into(),
            phases: vec![phase(0, "Work"), phase(1, "Merge")],
            required: vec!["benchmark".into()],
        }),
    ))));
    (driver, id)
}

/// What the workflow the Work step runs reports: its phases in its order, and an agent at work.
fn report_of(id: &argonauts_core::names::WorkflowId, phases: &[&str]) -> RunEventKind {
    use argonauts_core::events::session::{
        CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
    };
    use argonauts_core::events::workflow::{WorkflowEvent, WorkflowReport};
    use argonauts_core::workflow::StepRef;

    let agents = phases
        .first()
        .map(|_| CliAgent {
            index: 1,
            label: "plan:orchestrator".into(),
            phase: Some(1),
            model: None,
            state: CliAgentState::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        })
        .into_iter()
        .collect();
    RunEventKind::Workflow(WorkflowEvent::Reported(Box::new(WorkflowReport {
        step: StepRef {
            workflow: id.clone(),
            phase: 0,
            path: vec![0],
        },
        workflow: CliWorkflowRun {
            id: "wf-steps".into(),
            call: None,
            name: "orchestrator".into(),
            description: String::new(),
            phases: phases
                .iter()
                .zip(1..)
                .map(|(title, index)| CliPhase {
                    index,
                    title: (*title).into(),
                    detail: String::new(),
                })
                .collect(),
            agents,
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        },
        sessions: Vec::new(),
        size_asked: None,
    })))
}

/// The run's start item waits until the workflow its step runs reports, so nothing shows of a
/// workflow whose script doesn't exist yet. It prints once, however often the workflow reports.
#[test]
fn a_workflow_a_step_runs_prints_once_it_reports() {
    let (mut driver, id) = handed_over_driver();
    assert!(
        !driver.printed().contains("Workflow("),
        "{}",
        driver.printed()
    );

    // Nothing reported yet: still waiting.
    driver.send(run_event(report_of(&id, &[])));
    assert!(
        !driver.printed().contains("Workflow("),
        "{}",
        driver.printed()
    );

    let mut titles: Vec<String> = (1..=10).map(|n| format!("Shelf {n}")).collect();
    titles.insert(3, "Benchmark".into());
    titles.push("Shelf 1".into());
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    driver.send(run_event(report_of(&id, &titles)));
    driver.send(run_event(report_of(&id, &titles)));
    let printed = driver.printed_at(100);
    assert_eq!(
        printed.matches("Workflow(Switch reads)").count(),
        1,
        "{printed}"
    );
    assert!(
        printed.contains(
            "● Workflow(Switch reads)\n  ⎿  Running in background · /workflows to monitor and save"
        ),
        "{printed}"
    );
    assert!(!printed.contains("Shelf 1"), "{printed}");
}

/// On Claude Code's own path no script reports: the task's session reports Claude Code's own
/// workflow, and its first report with phases prints the start item then, not at the run's end.
#[test]
fn a_workflow_claude_code_runs_for_the_task_prints_once_its_phases_are_known() {
    use argonauts_core::AgentEvent;
    use argonauts_core::names::{RoleName, SessionKey};

    let (mut driver, id) = handed_over_driver();
    let host = SessionKey::numbered(1);
    let mut session = crate::state::SessionState::new(
        host.clone(),
        "claude-code/haiku".parse().unwrap(),
        "claude-code",
        RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.task = Some(task_id());
    let run = driver.state.runs.get_mut(&run_id()).unwrap();
    run.session_order.push(host.clone());
    run.sessions.insert(host.clone(), session);
    let reported = |phases: &[&str]| {
        let RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Reported(
            report,
        )) = report_of(&id, phases)
        else {
            unreachable!()
        };
        let mut event = run_event(RunEventKind::Agent(AgentEvent::Workflow {
            workflow: Box::new(report.workflow),
        }));
        if let AppEvent::Run { event } = &mut event {
            event.session = Some(host.clone());
        }
        event
    };
    driver.send(reported(&[]));
    assert!(
        !driver.printed().contains("Workflow("),
        "{}",
        driver.printed()
    );
    driver.send(reported(&["Build", "Review", "Final"]));
    driver.send(reported(&["Build", "Review", "Final"]));
    let printed = driver.printed_at(100);
    assert_eq!(
        printed.matches("Workflow(Switch reads · Captain)").count(),
        1,
        "{printed}"
    );
}

/// The size your request asked for, which the orchestrator keeps to, is the first line under
/// the workflow's row.
#[test]
fn the_size_you_asked_for_is_said_under_the_workflows_row() {
    use argonauts_core::events::workflow::WorkflowEvent;

    let (mut driver, id) = handed_over_driver();
    let mut reported = report_of(&id, &["Shelf 1"]);
    if let RunEventKind::Workflow(WorkflowEvent::Reported(report)) = &mut reported {
        report.size_asked = Some("a small workflow, 5 agents at most".into());
    }
    driver.send(run_event(reported));
    let printed = driver.printed_at(100);
    assert!(
        printed.contains(
            "● Workflow(Switch reads)\n  ⎿  Running in background · /workflows to monitor and save\n     \
             As you asked: a small workflow, 5 agents at most"
        ),
        "{printed}"
    );
}

/// A workflow of one phase stands in for Work as well. A run that ends before its workflow
/// reports anything still prints its start, as declared, ahead of its end.
#[test]
fn a_workflow_of_one_phase_or_none_reported_still_prints_its_start() {
    let (mut driver, id) = handed_over_driver();
    driver.send(run_event(report_of(&id, &["Sort the boxes"])));
    let printed = driver.printed_at(100);
    assert!(
        printed.contains("● Workflow(Switch reads)\n  ⎿  Running in background"),
        "{printed}"
    );

    let (mut driver, _) = handed_over_driver();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: None,
    })));
    let printed = driver.printed_at(100);
    let start = printed.find("● Workflow(Switch reads)\n").expect(&printed);
    // A failed run is one block: no second row for its workflow.
    assert_eq!(
        printed.matches("Workflow(Switch reads)").count(),
        1,
        "{printed}"
    );
    let end = printed.find("The run failed").expect(&printed);
    assert!(start < end, "{printed}");
}

#[test]
fn an_edit_in_the_conversation_shows_the_file_numbers_its_result_reported() {
    let mut driver = TestDriver::new();
    driver.transcript.conversation = ConversationTurns::with_tool_kinds(|_, name| match name {
        "Edit" => ToolKind::Edit,
        _ => ToolKind::Other,
    });
    let turn = TurnId::numbered(1);
    let agent = |event| AppEvent::Conversation {
        event: ConversationEvent::Agent {
            turn: turn.clone(),
            event,
        },
    };
    driver.send(AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: "fix the count".into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
            read_only: false,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        },
    });
    driver.printed();
    driver.send(agent(AgentEvent::ToolStarted {
        id: "edit-1".into(),
        name: "Edit".into(),
        input: serde_json::json!({"file_path": "src/lib.rs", "old_string": "let count = 0;",
                                  "new_string": "let mut count = 0;"}),
        parent: None,
    }));
    driver.send(agent(AgentEvent::FilePatch {
        id: "edit-1".into(),
        path: "src/lib.rs".into(),
        hunks: "@@ -11,3 +11,3 @@\n fn total() -> u32 {\n-    let count = 0;\n+    let mut count = 0;\n     count\n"
            .into(),
        parent: None,
    }));
    driver.send(agent(AgentEvent::ToolFinished {
        id: "edit-1".into(),
        ok: true,
        output: Some("The file src/lib.rs has been updated.".into()),
        parent: None,
    }));
    assert_eq!(
        driver.printed(),
        "\n● Update(src/lib.rs)\n  ⎿  Added 1 line, removed 1 line\n      11  fn total() -> u32 {\n      12 -    let count = 0;\n      12 +    let mut count = 0;\n      13      count"
    );
}

/// With a second workflow on screen, the first's events still print, and its closing answer
/// still ends it in the conversation.
#[test]
fn a_run_off_screen_still_ends_with_its_closing_answer() {
    let mut driver = TestDriver::new();
    driver.send(started());
    let second = RunId::new("run-02").expect("a valid run id");
    driver.send(AppEvent::Attached {
        summary: RunSummary {
            run: second.clone(),
            title: "Fix the median".into(),
            ..running_summary()
        },
        started_here: true,
    });
    driver.send(AppEvent::ReplayDone {
        run: second,
        seq: 0,
    });
    driver.printed();
    driver.send(run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(
        Box::new(closing()),
    ))));
    driver.send(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    let printed = driver.printed();
    assert!(printed.contains("Merged the run"), "{printed}");
    assert!(printed.contains("● Fixed parse()"), "{printed}");
}

/// The run brought on screen may still be replaying its history when the run beside it ends.
/// The replay stays quiet, and the ending and its recap print as they come.
#[test]
fn a_run_off_screen_prints_its_ending_while_the_one_on_screen_replays() {
    // Through the state, which tells the replay's events from the rest.
    fn send(driver: &mut TestDriver, event: AppEvent) {
        driver.state.apply(&event);
        driver.send(event);
    }
    let mut driver = TestDriver::new();
    send(&mut driver, started());
    let second = RunId::new("run-02").expect("a valid run id");
    send(
        &mut driver,
        AppEvent::Attached {
            summary: RunSummary {
                run: second.clone(),
                title: "Fix the median".into(),
                ..running_summary()
            },
            started_here: false,
        },
    );
    driver.printed();
    let replayed = |kind| match run_event(kind) {
        AppEvent::Run { mut event } => {
            event.run_id = second.clone();
            AppEvent::Run { event }
        }
        _ => unreachable!(),
    };
    send(
        &mut driver,
        replayed(RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: "An old warning".into(),
        })),
    );
    send(
        &mut driver,
        run_event(RunEventKind::Run(RunLifecycle::ClosingAnswer(Box::new(
            closing(),
        )))),
    );
    send(
        &mut driver,
        run_event(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        })),
    );
    send(
        &mut driver,
        AppEvent::Recap {
            text: "The json flag is in.".into(),
            model: None,
        },
    );
    let printed = driver.printed();
    assert!(printed.contains("● Fixed parse()"), "{printed}");
    assert!(printed.contains("Merged the run"), "{printed}");
    assert!(printed.contains("The json flag is in."), "{printed}");
    assert!(!printed.contains("An old warning"), "{printed}");
}

/// An answer that missed its format is asked for again, and the one it ends with prints once:
/// what hadn't printed yet goes, and what had printed isn't printed again.
#[test]
fn an_answer_asked_for_again_prints_once() {
    let turn = TurnId::numbered(1);
    let conversation = |event| AppEvent::Conversation { event };
    let writes = |text: &str| {
        conversation(ConversationEvent::Agent {
            turn: TurnId::numbered(1),
            event: AgentEvent::TextDelta {
                text: text.into(),
                parent: None,
            },
        })
    };
    let started = || {
        conversation(ConversationEvent::TurnStarted {
            turn: TurnId::numbered(1),
            text: "what does the limit do".into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
            read_only: true,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        })
    };
    let finished = || {
        conversation(ConversationEvent::TurnFinished {
            turn: TurnId::numbered(1),
            reply: TurnReply::Answer {
                markdown: "It caps each client at 10 requests a second.".into(),
            },
            spend: Spend::default(),
            elapsed_ms: 2_000,
        })
    };
    const ANSWER: &str = "It caps each client at 10 requests a second.";

    // Nothing printed yet, so the first try goes and the reply prints once.
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(writes("It caps each client at ten"));
    driver.send(conversation(ConversationEvent::AnswerRetried {
        turn: turn.clone(),
    }));
    driver.send(writes(ANSWER));
    driver.send(finished());
    let printed = driver.printed();
    assert_eq!(printed.matches(ANSWER).count(), 1, "{printed}");
    assert!(!printed.contains("at ten"), "{printed}");

    // Its rows printed already, so they stay and the same answer isn't printed again.
    let mut driver = TestDriver::new();
    driver.send(started());
    driver.send(writes(&format!("{ANSWER}\n\n")));
    let mut printed = driver.printed();
    assert!(printed.contains(ANSWER), "{printed}");
    driver.send(conversation(ConversationEvent::AnswerRetried { turn }));
    driver.send(writes(ANSWER));
    driver.send(finished());
    printed.push_str(&driver.printed());
    assert_eq!(printed.matches(ANSWER).count(), 1, "{printed}");
    assert!(printed.contains("✻ Sailed for 2s"), "{printed}");
}
