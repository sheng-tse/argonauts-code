//! A workflow asked for in solo, as the chat shows it: your message, the main session's line
//! saying what started and with whom, and the workflow's row with the reviewers you asked for.
//! Compared cell for cell, colours included, with `tests/golden/crew_after`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::app::{AppEvent, ChatEvent, TurnReply};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{
    CheckedInstruction, InstructionOutcome, RunLifecycle, RunMode, RunNamed, RunStarted,
};
use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{TurnId, WorkerRef, WorkflowId};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{RunId, Spend};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use time::macros::datetime;

use super::Transcript;
use crate::bridge::{Cx, Effects};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::{AppState, RunState, WorkflowState};
use crate::text::plain_text;
use crate::theme::{Glyphs, Role, Theme};

const PICTURE: &str = include_str!("../../../tests/golden/crew_after.txt");
const COLOURS: &str = include_str!("../../../tests/golden/crew_after.html");

/// The picture's widest line, the main session's, is 130 cells; at the 118 its other lines
/// are drawn for, that line wraps after "plans and".
const WIDTH: u16 = 130;

const TYPED: &str = "Use a workflow for this, with Codex as the reviewer, and choose everything \
                     else yourself. Fix the money…";
const SAID: &str = "Starting a workflow: Orchestrator, with **GPT 5.6 Luna on Codex** reviewing \
                    as you asked; **Haiku 5.5 on Claude Code** plans and builds.";

struct Screen {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
}

impl Screen {
    fn new() -> Self {
        let mut state = AppState::default();
        state.names = DisplayNames {
            harnesses: BTreeMap::from([
                ("claude-code".to_owned(), "Claude Code".to_owned()),
                ("codex".to_owned(), "Codex".to_owned()),
            ]),
            configured: BTreeMap::from([("codex".to_owned(), "gpt-5.6-luna".to_owned())]),
            ..DisplayNames::default()
        };
        // The picture shows the moment the workflow starts, before the turn's own end line.
        let settings = UiSettings {
            turn_duration: false,
            ..UiSettings::default()
        };
        Self {
            model: ViewModel::default(),
            state,
            settings,
            transcript: Transcript::with_clock(WallClock::pinned(datetime!(2026-10-09 10:42 UTC))),
        }
    }

    fn send(&mut self, event: AppEvent) {
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

    fn printed(&mut self, theme: &Theme) -> Vec<Line<'static>> {
        let ctx = RenderCtx::new(WIDTH, theme, Clock::still(Instant::now()));
        self.model.history.drain_printable(&ctx)
    }
}

fn run_id() -> RunId {
    RunId::new("run-01").expect("a valid run id")
}

fn run_event(kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-10-09 10:42 UTC),
            run_id: run_id(),
            card_id: None,
            session: None,
            source: "conductor".into(),
            kind,
        }),
    }
}

/// The run the start tool began: the orchestrator's workflow, Codex reviewing as you asked.
fn started() -> RunStarted {
    RunStarted {
        request: TYPED.into(),
        title: "Fix the money rounding".into(),
        mode: RunMode::Crew,
        forced: false,
        workflow: "orchestrator".into(),
        estimate: None,
        triage: None,
        chat_turn: Some(TurnId::numbered(1)),
        continues: None,
        repo_root: PathBuf::from("/repo"),
        base_branch: Some("main".into()),
        base_commit: "6fc02994c".into(),
        uncommitted: vec![],
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::json!({"roles": {"reviewer": {"workers": ["codex"]}}}),
        overrides: vec![],
        harness_versions: Default::default(),
        version: "0.0.1".into(),
        front_end: None,
        instructions: vec![
            CheckedInstruction {
                says: "Use a workflow for this".into(),
                kind: InstructionKind::Mode,
                outcome: InstructionOutcome::Followed {
                    said: String::new(),
                    note: None,
                },
            },
            CheckedInstruction {
                says: "with Codex as the reviewer".into(),
                kind: InstructionKind::Review,
                outcome: InstructionOutcome::Followed {
                    said: "Codex reviews".into(),
                    note: None,
                },
            },
        ],
        required_steps: Vec::new(),
    }
}

fn screen() -> Screen {
    let mut screen = Screen::new();
    let turn = TurnId::numbered(1);
    screen.send(AppEvent::Chat {
        event: ChatEvent::TurnStarted {
            turn: turn.clone(),
            text: TYPED.into(),
            worker: WorkerRef::new("claude-code", Some("haiku".into())),
            read_only: false,
            commit: String::new(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        },
    });
    screen.send(AppEvent::Chat {
        event: ChatEvent::TurnFinished {
            turn,
            reply: TurnReply::Answer {
                markdown: SAID.into(),
            },
            spend: Spend::default(),
            elapsed_ms: 4_000,
        },
    });

    let id = WorkflowId::numbered(1);
    let mut run = RunState::new(run_id());
    run.workflow = Some("orchestrator".into());
    run.workflows.push(WorkflowState {
        id: id.clone(),
        name: "orchestrator".into(),
        from: None,
        description: String::new(),
        phases: vec!["Build".into(), "Review".into(), "Merge".into()],
        parent: None,
        steps: Vec::new(),
    });
    run.named = Some(RunNamed {
        name: "fix-money-and-add-tax".into(),
        description: "Fix the money rounding and add with_tax".into(),
        style: RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    screen.state.runs.insert(run_id(), run);
    screen.send(run_event(RunEventKind::Run(RunLifecycle::Started(
        Box::new(started()),
    ))));
    let phase = |index: u32, title: &str| DeclaredPhase {
        index,
        title: title.into(),
        detail: String::new(),
        standard: StandardPhase::Do,
    };
    screen.send(run_event(RunEventKind::Workflow(WorkflowEvent::Declared(
        Box::new(WorkflowDeclared {
            id,
            name: "orchestrator".into(),
            description: String::new(),
            phases: vec![phase(0, "Build"), phase(1, "Review"), phase(2, "Merge")],
            required: Vec::new(),
        }),
    ))));
    screen
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The printed lines from your message on, without the blank rows that close the screen.
fn drawn(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let text = |line: &Line<'_>| plain_text(std::slice::from_ref(line));
    let start = lines
        .iter()
        .position(|line| text(line).starts_with(Glyphs::UNICODE.prompt))
        .unwrap_or_else(|| {
            let all: Vec<String> = lines.iter().map(text).collect();
            panic!("no message in\n{}", all.join("\n"))
        });
    let mut lines = lines[start..].to_vec();
    while lines
        .last()
        .is_some_and(|line| text(line).trim().is_empty())
    {
        lines.pop();
    }
    lines
}

#[test]
fn a_workflow_asked_for_in_solo_reads_as_the_picture_draws_it() {
    let theme = theme();
    let lines = drawn(screen().printed(&theme));
    let printed: Vec<String> = lines
        .iter()
        .map(|line| plain_text(std::slice::from_ref(line)).trim_end().to_owned())
        .collect();
    crate::picture_dump::rows("crew_after", &printed);
    assert_eq!(printed, picture_text(), "\n{}", printed.join("\n"));
}

/// The picture's lines. It draws your message's mark as `>`, where the theme draws `❯` as
/// Claude Code does; that one cell is the only difference.
fn picture_text() -> Vec<String> {
    PICTURE
        .trim_end()
        .lines()
        .map(|line| match line.strip_prefix("> ") {
            Some(message) => format!("{} {message}", Glyphs::UNICODE.prompt),
            None => line.to_owned(),
        })
        .collect()
}

/// Each cell's look in the picture: its colour by name, bold, and the band behind your message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Look {
    colour: Option<&'static str>,
    bold: bool,
    band: bool,
}

/// The picture's colours, one `Look` per cell of each line, read off its spans.
fn picture_looks() -> Vec<Vec<(char, Look)>> {
    let body = COLOURS
        .split_once("<pre")
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("</pre>"))
        .map(|(body, _)| body)
        .expect("the picture's cells");
    let plain = Look {
        colour: None,
        bold: false,
        band: false,
    };
    let mut lines = vec![Vec::new()];
    let mut rest = body;
    while !rest.is_empty() {
        let (look, text, after) = match rest.strip_prefix("<span style=\"") {
            Some(span) => {
                let (style, after) = span.split_once("\">").expect("a span's style");
                let (text, after) = after.split_once("</span>").expect("a span's end");
                (look_of(style), text, after)
            }
            None => {
                let end = rest.find("<span").unwrap_or(rest.len());
                (plain.clone(), &rest[..end], &rest[end..])
            }
        };
        let text = text
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&amp;", "&");
        for c in text.chars() {
            if c == '\n' {
                lines.push(Vec::new());
            } else {
                lines.last_mut().unwrap().push((c, look.clone()));
            }
        }
        rest = after;
    }
    lines
}

/// The picture's colour names, each with the theme role that draws it. Its accent is the
/// periwinkle the theme draws cards and working marks in.
const ROLES: [(&str, Role); 6] = [
    ("acc", Role::Card),
    ("dim", Role::Dim),
    ("faint", Role::Faint),
    ("grn", Role::Success),
    ("red", Role::Error),
    ("amb", Role::Warning),
];

fn look_of(style: &str) -> Look {
    let colour = ROLES
        .into_iter()
        .map(|(name, _)| name)
        .find(|name| style.contains(&format!("color:var(--term-{name})")));
    Look {
        colour,
        bold: style.contains("font-weight:700"),
        band: style.contains("background:var(--term-band)"),
    }
}

/// The picture's name for a drawn colour; `None` for the terminal's own foreground, which
/// the theme draws as plain or bright text.
fn colour_name(theme: &Theme, colour: Option<Color>) -> Option<&'static str> {
    let plain = [Role::Text, Role::Bright].map(|role| theme.style(role).fg);
    if plain.contains(&colour) {
        return None;
    }
    let named = ROLES
        .into_iter()
        .find(|(_, role)| theme.style(*role).fg == colour);
    named.map(|(name, _)| name).or(Some("another colour"))
}

#[test]
fn its_colours_are_the_picture_s() {
    let theme = theme();
    let band = theme.style(Role::UserBand).bg;
    let lines = drawn(screen().printed(&theme));
    let picture = picture_looks();
    for (row, (line, want)) in lines.iter().zip(&picture).enumerate() {
        let mut cells: Vec<(char, Style)> = Vec::new();
        for span in &line.spans {
            let style = line.style.patch(span.style);
            cells.extend(span.content.chars().map(|c| (c, style)));
        }
        for (column, ((c, style), (want_c, want))) in cells.iter().zip(want).enumerate() {
            // Your message's mark is the message cell's own, drawn faint on its band; the
            // picture draws `> ` dim beside the band.
            if row == 0 && column < 2 {
                continue;
            }
            assert_eq!(c, want_c, "row {row} column {column}");
            let got = Look {
                // A blank cell shows only its background.
                colour: if c.is_whitespace() {
                    want.colour
                } else {
                    colour_name(&theme, style.fg)
                },
                bold: if c.is_whitespace() {
                    want.bold
                } else {
                    style.add_modifier.contains(Modifier::BOLD)
                },
                band: band.is_some() && style.bg == band,
            };
            assert_eq!(&got, want, "row {row} column {column} {c:?}");
        }
    }
}
