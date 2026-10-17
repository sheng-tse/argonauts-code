//! The Context tab against its pictures, cell for cell and colour for colour: a 1M main
//! session at 75% with two crew agents at work, and the same session at 94%.
//!
//! Every screen writes tokens in one format, Argonauts Code's own (`1M`, `800k`), and that format
//! stands over the pictures, which spell counts as Claude Code does (`1m`, `799.6k`). So each
//! picture is compared after the counts in [`COUNTS_AT_THREE_QUARTERS`] and [`COUNTS_NEAR_FULL`]
//! are respelled. A respelled count that is shorter leaves blank cells where its column says.
//! Each model is compared in the shade of its CLI's colour that the pictures draw it in.

use std::sync::Arc;
use std::time::Instant;

use argonauts_core::Spend;
use argonauts_core::app::AppMode;
use argonauts_core::names::{ModelRef, SessionKey, TurnId};
use argonauts_core::overview::ModelTokens;
use argonauts_core::views::{ContextRow, ContextView, RecordedContext};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use time::OffsetDateTime;
use time::macros::datetime;

use super::example::{self, Example};
use crate::bridge::tools::Places;
use crate::overview::golden::{Picture, PictureCell, Region, differences, theme};
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::state::{AppState, ContextGrowth, RunState};
use crate::views::{View, ViewCtx};

/// One count respelled on one row of a picture.
struct Respelled {
    /// The picture's row, from 0.
    row: usize,
    from: &'static str,
    to: &'static str,
    /// Where the cells a shorter count frees go: before this text, which is the count itself
    /// or is looked for after the count and then before it. Text before the count is flush
    /// right with it, and text after it starts the next column. `None` puts them at the row's
    /// end, as for text that runs on from the count.
    blanks_before: Option<&'static str>,
}

const fn respelled(
    row: usize,
    from: &'static str,
    to: &'static str,
    blanks_before: Option<&'static str>,
) -> Respelled {
    Respelled {
        row,
        from,
        to,
        blanks_before,
    }
}

const COUNTS_AT_THREE_QUARTERS: &[Respelled] = &[
    respelled(6, "799.6k", "800k", Some("3 sessions")),
    respelled(8, "/1m tokens", "/1M tokens", None),
    respelled(22, "1m tokens", "1M tokens", None),
    respelled(24, "741.6k", "742k", None),
    respelled(44, "528.6k", "529k", Some("10 tools")),
    respelled(45, "528.6k", "529k", Some("528.6k")),
    respelled(45, "180.2k", "180k", Some("180.2k")),
    respelled(46, "104.3k", "104k", Some("104.3k")),
    respelled(46, "160.4k", "160k", Some("160.4k")),
    respelled(56, "138.2k", "138k", Some("together:")),
    respelled(58, "180.2k", "180k", Some("save ~90.1k")),
    respelled(62, "160.4k", "160k", Some("save ~48.1k")),
    respelled(73, "244.8k", "245k", Some("Codex says")),
];

const COUNTS_NEAR_FULL: &[Respelled] = &[
    respelled(6, "1.2m", "1.2M", None),
    respelled(8, "944.6k", "945k", Some("944.6k")),
    respelled(8, "/1m tokens", "/1M tokens", None),
    respelled(18, "914.6k", "915k", Some("914.6k")),
    respelled(18, "914.6k", "915k", Some("914.6k")),
    respelled(22, "1m tokens", "1M tokens", None),
    respelled(24, "944.6k", "945k", Some("931.5k")),
    respelled(24, "931.5k", "932k", None),
    respelled(44, "914.6k", "915k", Some("914.6k")),
    respelled(44, "690.4k", "690k", Some("10 tools")),
    respelled(45, "690.4k", "690k", Some("690.4k")),
    respelled(45, "120.8k", "121k", Some("\u{2501}")),
    respelled(45, "241.6k", "242k", Some("241.6k")),
    respelled(46, "121.4k", "121k", Some("121.4k")),
    respelled(56, "180.5k", "180k", Some("together:")),
    respelled(61, "241.6k", "242k", Some("save ~120.8k")),
    respelled(61, "120.8k", "121k", Some("save ~120.8k")),
    respelled(69, "212.4k", "212k", Some("2 agents")),
    respelled(72, "171.4k", "171k", Some("171.4k")),
    respelled(73, "152.7k", "153k", None),
    respelled(76, "244.8k", "245k", Some("Codex says")),
];

/// Where a row's cells first spell `text`, at or after `from`.
fn found(cells: &[PictureCell], text: &str, from: usize) -> Option<usize> {
    let wanted: Vec<char> = text.chars().collect();
    (from..=cells.len().saturating_sub(wanted.len())).find(|&at| {
        cells[at..]
            .iter()
            .zip(&wanted)
            .all(|(cell, ch)| cell.ch == *ch)
    })
}

/// The picture with each count respelled. A count already respelled on a row isn't found
/// again, so a row's counts are listed left to right.
fn respell(mut picture: Picture, counts: &[Respelled]) -> Picture {
    let blank = PictureCell {
        ch: ' ',
        fg: None,
        bg: None,
        bold: false,
    };
    for count in counts {
        let cells = &mut picture.rows[count.row];
        let at = found(cells, count.from, 0)
            .unwrap_or_else(|| panic!("row {} has no {:?}", count.row, count.from));
        let (old, new) = (count.from.chars().count(), count.to.chars().count());
        assert!(new <= old, "{:?} is longer than {:?}", count.to, count.from);
        let blanks_at = count.blanks_before.map(|text| {
            if text == count.from {
                return at;
            }
            found(cells, text, at + old)
                .or_else(|| found(cells, text, 0))
                .unwrap_or_else(|| panic!("row {} has no {text:?}", count.row))
        });
        let style = cells[at].clone();
        let spelled = count.to.chars().map(|ch| PictureCell {
            ch,
            ..style.clone()
        });
        cells.splice(at..at + old, spelled);
        let blanks = std::iter::repeat_n(blank.clone(), old - new);
        match blanks_at {
            // Flush right: what is before the count moves right with it.
            Some(before) if before <= at => {
                cells.splice(before..before, blanks);
            }
            // The count's text runs on, and what is set at a column after it stays there.
            Some(after) => {
                let after = after - (old - new);
                cells.splice(after..after, blanks);
            }
            None => {}
        }
    }
    picture
}

/// The screen's state while the pictures' answer runs: the main session's count, what its
/// turns add, and the run the crew works in.
fn state(example: &Example) -> AppState {
    let mut state = AppState::default();
    state.names = example.names.clone();
    state.usage = Some(Arc::new(crate::overview::sample::usage()));
    state.conversation.context = Some(example.count.clone());
    state.conversation.growth = ContextGrowth::of(example::PER_TURN);
    state.conversation.worker = Some(example.view.rows[0].worker.clone());
    state.mode = Some(AppMode::Answering {
        turn: TurnId::numbered(1),
    });
    let mut run = RunState::new(example::run());
    run.title = example::RUN_TITLE.to_owned();
    state.runs.insert(example::run(), run);
    state
}

/// The panel as it opens on `example`, on a screen 118 cells wide and tall enough for all of
/// it.
fn drawn(example: &Example) -> Buffer {
    panel(
        &state(example),
        example.view.clone(),
        example.now,
        example.places.clone(),
    )
}

/// The Context tab drawn from `view` at `now`.
fn panel(state: &AppState, view: ContextView, now: OffsetDateTime, places: Places) -> Buffer {
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(Tab::Context, UsageWatch::default())
        .at(now)
        .showing_paths_from(places)
        .with_context(view);
    let height = view
        .dock_height(Size::new(118, 120), &ctx)
        .expect("the panel docks");
    let area = Rect::new(0, 0, 118, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// Fails with every cell of the picture's `rows` that the panel drew differently.
fn assert_panel_matches(picture: &Picture, buf: &Buffer, rows: std::ops::Range<usize>) {
    assert_eq!(
        usize::from(buf.area.height),
        rows.len(),
        "the panel is as tall as its picture"
    );
    let region = Region {
        rows: rows.clone(),
        columns: 0..118,
        at: (0, 0),
    };
    let name = format!("{}-rows-{}-{}", picture.name, rows.start, rows.end);
    let panel = crate::picture_dump::cut(buf, (0, 0), (118, rows.len()));
    crate::picture_dump::rows(&name, &panel);
    let found = differences(picture, buf, &region, &theme());
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
        "{}: {} cells are different\n{}\n\ndrawn:\n{}\n\npicture:\n{}",
        picture.name,
        found.len(),
        found
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        drawn.join("\n"),
        picture.text()[rows].join("\n")
    );
}

#[test]
fn the_context_tab_matches_its_picture() {
    let picture = respell(Picture::load("context_tab"), COUNTS_AT_THREE_QUARTERS);
    let buf = drawn(&example::at_three_quarters());
    // From the panel's rule to its key line: the rows above are the conversation, and the
    // rows below are the prompt after Esc.
    assert_panel_matches(&picture, &buf, 4..76);
}

#[test]
fn the_context_tab_near_full_matches_its_picture() {
    let picture = respell(Picture::load("context_near_full"), COUNTS_NEAR_FULL);
    let buf = drawn(&example::near_full());
    assert_panel_matches(&picture, &buf, 4..79);
}

/// A resumed conversation before a turn of it runs again, its session as the record left it:
/// the council session's one turn, which left 25,517 of 1M tokens and spent $0.0040, drawn
/// under the transcript of `resumed_after`.
#[test]
fn a_resumed_conversations_context_matches_its_picture() {
    let mut picture = respell(
        Picture::screen("resumed_after"),
        &[respelled(45, "/1m", "/1M", None)],
    );
    // The picture's tab row has no Config tab. The panel's is the one the Context tab's own
    // picture draws: Config after Status, under the bold Settings title.
    let tabs = Picture::load("context_tab");
    let at = tabs
        .text()
        .iter()
        .position(|row| row.contains(" Settings   Overview"))
        .expect("the Context tab's picture has its tab row");
    let mut tab_row = tabs.rows[at].clone();
    let width = picture.rows[42].len();
    tab_row.resize(
        width,
        PictureCell {
            ch: ' ',
            fg: None,
            bg: None,
            bold: false,
        },
    );
    picture.rows[42] = tab_row;
    let worker: ModelRef = "claude-code/claude-haiku-5-5".parse().expect("a model");
    let row = ContextRow {
        run: None,
        session: SessionKey::numbered(1),
        label: "conversation".to_owned(),
        worker: worker.clone(),
        used: 25_517,
        window: Some(1_000_000),
        cap: None,
        tool_calls: 2,
        spent: Spend {
            input_tokens: 73_708,
            output_tokens: 916,
            cached_tokens: 58_770,
            cost_usd: 0.004_032_7,
            cost_partial: false,
            cost_estimated: false,
        },
        rotations: 0,
        estimated: false,
        counts: false,
        categories: Vec::new(),
        recorded: Some(RecordedContext {
            at: datetime!(2026-10-14 01:52 UTC),
            turns: 1,
        }),
    };
    let mut state = AppState::default();
    state.names = crate::overview::sample::names();
    state.conversation.worker = Some(worker);
    // Haiku 5.5 has run the fewest tokens of the three Claude Code models here, so it draws in
    // the third shade of Claude Code's colour.
    let mut usage = crate::overview::sample::usage();
    usage.all_time_models = [
        ("claude-opus-5-5", 9_400_000),
        ("claude-sonnet-5-5", 2_100_000),
        ("claude-haiku-5-5", 310_000),
    ]
    .map(|(model, tokens)| ModelTokens {
        agent: "claude-code".to_owned(),
        model: Some(model.to_owned()),
        tokens,
    })
    .to_vec();
    state.usage = Some(Arc::new(usage));
    let buf = panel(
        &state,
        ContextView { rows: vec![row] },
        datetime!(2026-10-15 00:35 UTC),
        Places::default(),
    );
    // From the panel's rule to its key line: the rows above are the conversation.
    assert_panel_matches(&picture, &buf, 41..50);
}

/// A count that is respelled moves nothing but itself and the blanks its column takes.
#[test]
fn a_respelled_count_keeps_its_column() {
    let cell = |ch| PictureCell {
        ch,
        fg: None,
        bg: None,
        bold: false,
    };
    let row = |text: &str| -> Vec<PictureCell> { text.chars().map(cell).collect() };
    let picture = |text: &str| Picture::of("made up", vec![row(text)]);
    let after = |text: &str, count: Respelled| respell(picture(text), &[count]).text().remove(0);
    assert_eq!(
        after(
            "Bash  10 tools  528.6k",
            respelled(0, "528.6k", "529k", Some("10 tools"))
        ),
        "Bash    10 tools  529k"
    );
    assert_eq!(
        after(
            "using 180.2k tokens   save ~90k",
            respelled(0, "180.2k", "180k", Some("save"))
        ),
        "using 180k tokens     save ~90k"
    );
    assert_eq!(
        after("741.6k from cache", respelled(0, "741.6k", "742k", None)),
        "742k from cache"
    );
}
