//! The Context tab for a main session whose CLI says how full it is and nothing of what fills
//! it, against its pictures cell for cell and colour for colour: Qwen3 Coder 30B on OpenCode at
//! 41k of its 64k window with a run's three sessions under it, and the same session full.
//!
//! The pictures draw a run whose first two sessions have ended, each row saying when. The key
//! line is left out here and every other cell is compared.

use std::ops::Range;

use argonauts_core::RunId;
use argonauts_core::Spend;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::model_catalog::CatalogEntry;
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::views::{
    ContextRow, ContextView, EndedHow, ServerCap, SessionLeft, WindowFigures,
};
use ratatui::buffer::Buffer;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::golden_tests::panel;
use crate::bridge::tools::Places;
use crate::overview::cards::cli_role;
use crate::overview::golden::{Picture, Region, differences, theme};
use crate::state::{AppState, LastStep, RunState};

const QWEN: &str = "opencode/ollama/qwen3-coder:30b-64k";
const GEMMA: &str = "ollama/gemma3:12b-32k";
/// Ollama's figures: the 64k tag's cap and the model's own most, as `/api/show` gives them, and
/// the 32k tag's cap.
const QWEN_WINDOW: u64 = 65_536;
const QWEN_MOST: u64 = 262_144;
const GEMMA_WINDOW: u64 = 32_768;
/// Where a 64k session starts afresh, at 90% of its window.
const QWEN_AFRESH: u64 = 58_982;
/// What the main session holds: 41k of its 64k.
const USED: u64 = 41 * 1_024;
/// The picture's green for OpenCode, compared as whatever colour the theme gives OpenCode.
const OPENCODE_GREEN: &str = "#4ADE80";

fn now() -> OffsetDateTime {
    datetime!(2026-10-19 20:30 UTC)
}

fn run() -> RunId {
    RunId::new("run-20261019-002419-395c").expect("a run id")
}

fn model(id: &str) -> ModelRef {
    id.parse().expect("a model id")
}

fn row(
    run: Option<RunId>,
    label: &str,
    worker: &str,
    used: u64,
    window: u64,
    cap: Option<u64>,
    calls: u32,
) -> ContextRow {
    ContextRow {
        run,
        session: SessionKey::numbered(1),
        label: label.to_owned(),
        worker: model(worker),
        used,
        window: Some(window),
        cap,
        tool_calls: calls,
        spent: Spend::default(),
        rotations: 0,
        estimated: false,
        counts: false,
        categories: Vec::new(),
        recorded: None,
        ended: None,
    }
}

/// A session of the run that ran to its end at `at`.
fn ended(mut row: ContextRow, at: OffsetDateTime) -> ContextRow {
    row.ended = Some(SessionLeft {
        at,
        how: EndedHow::Done,
        fallback_for: None,
    });
    row
}

/// The main session at `used`, with the run's sessions: two that ran to their end, which
/// start afresh nowhere, and one at work.
fn view(used: u64) -> ContextView {
    let crew = Some(run());
    ContextView {
        rows: vec![
            row(None, "conversation", QWEN, used, QWEN_WINDOW, None, 0),
            ended(
                row(
                    crew.clone(),
                    "build:slug",
                    QWEN,
                    15_729,
                    QWEN_WINDOW,
                    None,
                    24,
                ),
                datetime!(2026-10-19 20:26 UTC),
            ),
            ended(
                row(
                    crew.clone(),
                    "review:slug",
                    GEMMA,
                    7_420,
                    GEMMA_WINDOW,
                    None,
                    0,
                ),
                datetime!(2026-10-19 20:27 UTC),
            ),
            row(
                crew,
                "build:unicode",
                QWEN,
                9_551,
                QWEN_WINDOW,
                Some(QWEN_AFRESH),
                6,
            ),
        ],
        local: vec!["ollama".to_owned()],
        finished: Vec::new(),
        main_window: Some(WindowFigures {
            model: Some(QWEN_MOST),
            server: Some(ServerCap {
                name: "Ollama".to_owned(),
                tokens: QWEN_WINDOW,
            }),
            cli: Some(QWEN_WINDOW),
        }),
    }
}

/// OpenCode's name for its model, as its own list gives it.
fn names() -> DisplayNames {
    let mut names = DisplayNames::default();
    let entry: CatalogEntry = serde_json::from_value(serde_json::json!({
        "id": QWEN,
        "name": "Qwen3 Coder 30B (64k)",
    }))
    .expect("a catalog entry");
    names.catalog.models.push(entry);
    names
}

/// The screen's state as the main session's last step left it, `counted` before now.
fn state(counted: Duration) -> AppState {
    let mut state = AppState::default();
    state.names = names();
    state.conversation.worker = Some(model(QWEN));
    state.conversation.last_step = LastStep {
        at: Some(now() - counted),
        cached: 0,
        output: 212,
    };
    let mut run_state = RunState::new(run());
    run_state.title = "Slug fixes".to_owned();
    state.runs.insert(run(), run_state);
    state
}

/// The picture with OpenCode's green standing for the colour the theme draws OpenCode in.
fn picture(name: &str) -> Picture {
    let mut picture = Picture::load(name);
    for cell in picture.rows.iter_mut().flatten() {
        if cell.fg.as_deref() == Some(OPENCODE_GREEN) {
            cell.fg = Some("opencode".to_owned());
        }
    }
    picture.standing_for("opencode", cli_role("opencode"))
}

/// Fails with every cell of `regions`, each picture rows and columns, that the panel drew
/// differently. The picture's row `r` falls on the buffer's row `r + shift`.
fn assert_matches(
    picture: &Picture,
    buf: &Buffer,
    shift: i32,
    regions: &[(Range<usize>, Range<usize>)],
) {
    assert_shifted(
        picture,
        buf,
        shift,
        &regions
            .iter()
            .map(|(rows, columns)| (rows.clone(), columns.clone(), 0))
            .collect::<Vec<_>>(),
    );
}

/// [`assert_matches`] with each region's columns moved `dx` cells right on the buffer.
fn assert_shifted(
    picture: &Picture,
    buf: &Buffer,
    shift: i32,
    regions: &[(Range<usize>, Range<usize>, u16)],
) {
    let found: Vec<String> = regions
        .iter()
        .flat_map(|(rows, columns, dx)| {
            let top = i32::try_from(rows.start).unwrap_or(0) + shift;
            let region = Region {
                rows: rows.clone(),
                columns: columns.clone(),
                at: (*dx, u16::try_from(top).expect("the region is on the panel")),
            };
            differences(picture, buf, &region, &theme())
        })
        .collect();
    if found.is_empty() {
        return;
    }
    let text = |y: u16| -> String {
        (0..118)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(" ", |cell| cell.symbol())
                    .to_owned()
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    let drawn: Vec<String> = (0..buf.area.height).map(text).collect();
    panic!(
        "{}: {} cells are different\n{}\n\ndrawn:\n{}",
        picture.name,
        found.len(),
        found
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        drawn.join("\n"),
    );
}

/// Writes the buffer's rows from `top` for the picture's rows `rows`, where
/// `ARGONAUTS_PICTURE_DUMP` asks for them.
fn dump(picture: &Picture, buf: &Buffer, top: usize, rows: Range<usize>) {
    let name = format!("{}-rows-{}-{}", picture.name, rows.start, rows.end);
    let drawn = crate::picture_dump::cut(buf, (0, top), (118, rows.len()));
    crate::picture_dump::rows(&name, &drawn);
}

/// Every column of each row in `rows`.
fn whole(rows: Range<usize>) -> (Range<usize>, Range<usize>) {
    (rows, 0..118)
}

#[test]
fn a_session_that_says_only_how_full_matches_its_picture() {
    let picture = picture("context_qwen_after");
    let state = state(Duration::minutes(2));
    let buf = panel(&state, view(USED), now(), Places::default());
    dump(&picture, &buf, 0, 2..40);
    // From the panel's rule to its key line: the rows above are the conversation, and the rows
    // below are the prompt after esc.
    assert_eq!(buf.area.height, 38, "the panel is as tall as its picture");
    assert_matches(
        &picture,
        &buf,
        -2,
        &[
            // The rule, the tabs and the row under them that counts the sessions.
            whole(2..5),
            // The main session, from the title to the line that says why nothing is listed,
            // then the crew's header and the column heads.
            whole(5..30),
            // The sessions that ended, each with when it did, then the one at work, with where
            // it starts afresh.
            whole(30..39),
        ],
    );
}

#[test]
fn a_full_window_matches_its_picture() {
    let picture = picture("context_qwen_full");
    let state = state(Duration::ZERO);
    let buf = panel(&state, view(QWEN_WINDOW), now(), Places::default());
    dump(&picture, &buf, 4, 0..14);
    assert_matches(&picture, &buf, 4, &[whole(0..14)]);
}

/// After esc, the agent panel under the prompt: the session at work measures its context
/// against its window, in the window's divisor, as the tab does.
#[test]
fn the_agent_row_measures_against_the_window() {
    use argonauts_core::names::RoleName;
    use argonauts_core::settings::ui::UiSettings;
    use argonauts_core::views::ContextUse;
    use ratatui::layout::Rect;

    use crate::bottom::panel_lines;
    use crate::bridge::{Cx, Effects, panel as agents};
    use crate::model::ViewModel;
    use crate::render::{Clock, RenderCtx};
    use crate::state::{SessionState, SessionStatus};

    let picture = picture("context_qwen_after");
    let wall = OffsetDateTime::now_utc();
    let started = wall - Duration::seconds(130);
    let mut state = state(Duration::minutes(2));
    let worker = model(QWEN);
    let mut unicode = SessionState::new(
        SessionKey::numbered(3),
        worker.clone(),
        &worker.agent,
        RoleName::Worker,
        started,
    );
    unicode.label = Some("build:unicode".to_owned());
    unicode.status = SessionStatus::Working;
    unicode.tokens_out = 1_200;
    unicode.context = Some(ContextUse {
        used: 9_551,
        window: Some(QWEN_WINDOW),
        cap: Some(QWEN_AFRESH),
    });
    unicode.streaming = "Running python3 -m unittest".to_owned();
    unicode.workflow_step = Some(argonauts_core::workflow::StepRef {
        workflow: argonauts_core::names::WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    });
    let run_state = state.runs.get_mut(&run()).expect("the run");
    run_state.session_order.push(unicode.key.clone());
    run_state.sessions.insert(unicode.key.clone(), unicode);

    let mut model = ViewModel::default();
    let mut settings = UiSettings::default();
    settings.panel.agents = true;
    let mut out = Effects::default();
    let now = std::time::Instant::now();
    agents::sync(&mut Cx {
        model: &mut model,
        state: &state,
        settings: &settings,
        now,
        out: &mut out,
    });
    let theme = theme();
    let ctx = RenderCtx::new(118, &theme, Clock::still(now));
    let lines = panel_lines(&model.bottom.workflows, &model.bottom.agents, &ctx);
    let mut buf = Buffer::empty(Rect::new(
        0,
        0,
        118,
        u16::try_from(lines.len()).unwrap_or(0),
    ));
    for (y, line) in lines.iter().enumerate() {
        buf.set_line(0, u16::try_from(y).unwrap_or(0), line, 118);
    }
    // `● main` on the first row, then the run's, then the session's on the third: its mark
    // and label where the picture has them, and its notes flush with the right edge, as the
    // panel sets the widest notes of its rows. What it does is the panel's own, in the
    // terminal's colour where the picture dims it.
    let notes = 62..95;
    let dx = 118 - 1 - u16::try_from(notes.len()).unwrap_or(0) - 62;
    dump(&picture, &buf, 0, 48..51);
    assert_shifted(
        &picture,
        &buf,
        -48,
        &[(48..49, 0..4, 0), (50..51, 0..21, 0), (50..51, notes, dx)],
    );
}
