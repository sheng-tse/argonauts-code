//! A run on autopilot as the conversation shows it: the worker's row counting what it decided
//! in your place, and the closing answer listing each decision with whose call it was. Compared
//! cell for cell, colours included, with `tests/golden/autopilot`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::app::AppEvent;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{
    ClosingAnswer, DecidedAlone, RunLifecycle, RunMode, RunOutcome, RunStarted,
};
use argonauts_core::events::session::{EndReason, SessionEnd, SessionEvent, SessionStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{AgentId, ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::{AgentEvent, RunId, Spend, TaskId};
use ratatui::text::Line;
use serde_json::json;
use time::macros::datetime;

use super::Transcript;
use super::picture_check::{Unlike, assert_matches};
use crate::bridge::agents::AgentBlocks;
use crate::bridge::{Cx, Effects};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::theme::{Glyphs, Theme};

const PICTURE: &str = include_str!("../../../tests/golden/autopilot.txt");
const COLOURS: &str = include_str!("../../../tests/golden/autopilot.html");

/// The picture's widest row, the decision, is 97 cells.
const WIDTH: u16 = 100;

const WORKER: &str = "claude-code/claude-haiku-5-5";
const SESSION: &str = "s-0001";
const ASSUMED: &str = "negative amounts round half toward +∞, as apply_discount already does";
const ANSWER: &str = "Merged into main (1 task, 4 files, +45 -5). 14 tests pass.";

/// The picture with the two things every agent's row on screen has and the picture leaves
/// out: the second cell after `⎿`, as Claude Code draws it, and `report ✓` for a session that
/// ended with its report. Put in, the rest of the row is compared in place.
fn as_every_row_reads(picture: &str) -> String {
    picture.replacen("  ⎿ ", "  ⎿  ", 1).replacen(
        "tool uses · decided",
        "tool uses · report ✓ · decided",
        1,
    )
}

struct Screen {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
    agents: AgentBlocks,
    seq: u64,
}

impl Screen {
    fn new() -> Self {
        let mut state = AppState::default();
        state.names = DisplayNames {
            clis: BTreeMap::from([("claude-code".to_owned(), "Claude Code".to_owned())]),
            ..DisplayNames::default()
        };
        // The picture is the rows the run leaves, without the turn's own end line.
        let settings = UiSettings {
            turn_duration: false,
            ..UiSettings::default()
        };
        Self {
            model: ViewModel::default(),
            state,
            settings,
            transcript: Transcript::with_clock(WallClock::pinned(datetime!(2026-10-10 08:19 UTC))),
            agents: AgentBlocks::default(),
            seq: 0,
        }
    }

    /// The state folds the event first, then each part of the bridge draws from it.
    fn send(&mut self, session: bool, kind: RunEventKind) {
        self.seq += 1;
        let event = AppEvent::Run {
            event: Box::new(RunEvent {
                seq: self.seq,
                ts: datetime!(2026-10-10 08:19 UTC) + time::Duration::seconds(self.seq as i64),
                run_id: run(),
                task_id: session.then(|| TaskId::numbered(1)),
                session: session.then(key),
                source: "test".into(),
                kind,
            }),
        };
        self.state.apply(&event);
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.agents.on_event(&event, &mut cx);
        self.transcript.on_event(&event, &mut cx);
    }

    fn printed(&mut self, theme: &Theme) -> Vec<Line<'static>> {
        let ctx = RenderCtx::new(WIDTH, theme, Clock::still(Instant::now()));
        self.model.history.drain_printable(&ctx)
    }
}

fn run() -> RunId {
    RunId::new("run-01").expect("a run id")
}

fn key() -> SessionKey {
    SessionKey::parse(SESSION).expect("a session key")
}

fn worker() -> ModelRef {
    WORKER.parse().expect("a model")
}

fn run_started() -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: "Add due dates to invoices".into(),
        title: "Add due dates".into(),
        mode: RunMode::Crew,
        forced: false,
        workflow: "orchestrator".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: PathBuf::from("/work/captain-crew"),
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
    })))
}

fn session_started() -> RunEventKind {
    RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
        agent_id: AgentId("claude-code".to_owned()),
        worker: worker(),
        provider: "test".to_owned(),
        kind: RunnerKind::Cli,
        role: RoleName::Worker,
        step: None,
        access: Access::Build,
        lifetime: SessionLifetime::Task,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
        label: Some("build:due-dates".to_owned()),
        asked: None,
    })))
}

/// The worker's session ends after seven tool calls and fourteen seconds, with a report whose
/// concerns say the one thing it decided where it would have asked.
fn session_ended() -> RunEventKind {
    RunEventKind::Session(SessionEvent::Ended(Box::new(SessionEnd {
        cli_session: None,
        exit_code: Some(0),
        signal: None,
        duration_ms: 14_000,
        reason: EndReason::Completed,
        spend: Spend::default(),
        tool_calls: 7,
        result_text: None,
        structured: Some(json!({
            "status": "done",
            "summary": "Invoices carry a due date.",
            "tests": ["python3 -m unittest: 14 passed"],
            "concerns": [format!("Assumed: {ASSUMED}.")],
        })),
        last_message: None,
        raw_log: None,
    })))
}

fn closing_answer() -> RunEventKind {
    RunEventKind::Run(RunLifecycle::ClosingAnswer(Box::new(ClosingAnswer {
        text: ANSWER.to_owned(),
        by: Some(worker()),
        files: Vec::new(),
        checks: Vec::new(),
        tests: Vec::new(),
        left: Vec::new(),
        decided: vec![DecidedAlone {
            what: ASSUMED.to_owned(),
            whose: "the worker's call".to_owned(),
        }],
    })))
}

/// The worker's row and the run's closing answer, as the run on autopilot leaves them.
fn autopilot_run() -> Screen {
    let mut screen = Screen::new();
    screen.send(false, run_started());
    screen.send(
        false,
        RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: argonauts_core::events::run::Party::User,
        }),
    );
    screen.send(true, session_started());
    for n in 1..=7 {
        let id = format!("t{n}");
        screen.send(
            true,
            RunEventKind::Agent(AgentEvent::ToolStarted {
                id: id.clone(),
                name: "Read".to_owned(),
                input: json!({"file_path": format!("/work/captain-crew/invoice/f{n}.py")}),
                parent: None,
            }),
        );
        screen.send(
            true,
            RunEventKind::Agent(AgentEvent::ToolFinished {
                id,
                ok: true,
                output: Some("ok".to_owned()),
                parent: None,
            }),
        );
    }
    screen.send(true, session_ended());
    screen
}

/// The picture is the two blocks the run leaves for you to review: the worker's finished row
/// and the closing answer. The worker's calls in its row, between its header and its Done
/// line, and the run's own finish line between the two blocks are the screen's other rows,
/// which the picture leaves out.
#[test]
fn autopilot_the_workers_row_counts_its_decisions_and_the_answer_lists_them() {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let mut screen = autopilot_run();
    let worked = screen.printed(&theme);
    let header = worked
        .iter()
        .position(|line| line.to_string().starts_with("● build:due-dates"))
        .expect("the worker's row");
    let mut lines = vec![worked[header].clone(), worked[worked.len() - 1].clone()];
    screen.send(false, closing_answer());
    screen.send(
        false,
        RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        }),
    );
    let ended = screen.printed(&theme);
    let answer = ended
        .iter()
        .position(|line| line.to_string().contains(ANSWER))
        .expect("the closing answer");
    lines.push(Line::default());
    lines.extend_from_slice(&ended[answer..]);
    assert_matches(
        "autopilot",
        &lines,
        &as_every_row_reads(PICTURE),
        &as_every_row_reads(COLOURS),
        &theme,
        &[
            Unlike {
                row: 0,
                cols: (2, 16),
                why: "an agent's label reads in the text's colour on every row, as the \
                      council's rows draw it; the picture draws this one purple",
            },
            Unlike {
                row: 0,
                cols: (20, 28),
                why: "an agent's model reads in its shade, as the council's rows draw it, \
                      where the picture draws it dim",
            },
            Unlike {
                row: 1,
                cols: (2, 2),
                why: "the worker's calls print in its row above its Done line, which so \
                      continues the row with no ⎿ of its own",
            },
        ],
    );
    let screen: String = ended.iter().map(|line| format!("{line}\n")).collect();
    assert!(!screen.contains('?'), "nothing is asked: {screen}");
}

/// Off autopilot the report comes to you, so what its agent assumed is counted as assumed.
#[test]
fn off_autopilot_the_row_counts_what_was_assumed() {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let mut screen = Screen::new();
    screen.send(false, run_started());
    screen.send(true, session_started());
    screen.send(true, session_ended());
    let rows: Vec<String> = screen.printed(&theme).iter().map(Line::to_string).collect();
    assert_eq!(
        rows.last().map(String::as_str),
        Some("  ⎿  Done · 0 tool uses · report ✓ · assumed 1 thing · 14s"),
        "{rows:?}"
    );
}
