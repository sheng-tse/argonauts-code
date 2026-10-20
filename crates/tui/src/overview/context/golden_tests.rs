//! The Context tab against its pictures, cell for cell and colour for colour: a 1M main
//! session at 75% with two crew agents at work, the same session at 94%, and a main session
//! after a finished run, its nine sessions listed as each one ended.
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

/// The picture after the run spells counts as Claude Code does in the main session's blocks,
/// and spend under a cent as `$0.00`, which every screen now writes `<$0.01`.
const COUNTS_AFTER_RUN: &[Respelled] = &[
    respelled(8, "/1m tokens", "/1M tokens", None),
    respelled(19, "939.2k", "939k", Some("939.2k")),
    respelled(19, "972.2k", "972k", Some("972.2k")),
    respelled(22, "1m tokens", "1M tokens", None),
    respelled(22, "939.2k", "939k", None),
    respelled(55, "$0.00", "<$0.01", Some("$0.00")),
    respelled(58, "$0.00", "<$0.01", Some("$0.00")),
    respelled(64, "~$0.00", "~<$0.01", Some("~$0.00")),
    respelled(67, "~$0.00", "~<$0.01", Some("~$0.00")),
    respelled(70, "~$0.00", "~<$0.01", Some("~$0.00")),
    respelled(73, "$0.00", "<$0.01", Some("$0.00")),
    respelled(76, "~$0.00", "~<$0.01", Some("~$0.00")),
    respelled(79, "$0.00", "<$0.01", Some("$0.00")),
];

/// What fills each crew agent's session lines up in one column a part across the crew, as
/// wide as the part's widest entry, where the picture lets each part run on after the one
/// before. With the four parts too wide for the row, MCP tools, the smallest, has no column,
/// so system takes 54, MCP server instructions 71 and messages 103, and the one row with MCP
/// tools and no MCP server instructions puts it in that column: each part of the picture moved
/// to its column, its colours kept.
const LINED_UP: &[Moved] = &[
    moved(56, "\u{26c1} messages 4.9k", Some(103)),
    moved(56, "\u{26c1} MCP server instructions 273", Some(71)),
    moved(59, "\u{26c1} messages 3.6k", Some(103)),
    moved(59, "\u{26c1} MCP server instructions 273", Some(71)),
    moved(74, "\u{26c1} messages 5.3k", Some(103)),
    moved(74, "\u{26c1} MCP tools 167", Some(71)),
    moved(80, "\u{26c1} messages 4.2k", Some(103)),
    moved(80, "\u{26c1} MCP server instructions 273", Some(71)),
    // A tool's bar ends where every tool's bar ends, in the column the tab's pictures with
    // several tools end it at, three cells before this one's lone bar.
    Moved {
        from: 62,
        ..moved(45, "\u{2501}\u{2501}\u{2501}  ", None)
    },
];

/// The picture's rows of messages by kind, from row 45, in the order the tab lists them,
/// largest first: the picture keeps the order of the picture it was drawn from, with new
/// numbers. Each is the picture's row of that kind.
const KINDS_LARGEST_FIRST: [usize; 6] = [47, 49, 48, 45, 46, 50];

/// The picture with its message kinds largest first, each row's left half moved whole.
fn kinds_largest_first(mut picture: Picture) -> Picture {
    let left = 0..62;
    let kinds: Vec<Vec<PictureCell>> = KINDS_LARGEST_FIRST
        .iter()
        .map(|&row| picture.rows[row][left.clone()].to_vec())
        .collect();
    for (row, cells) in (45..).zip(kinds) {
        picture.rows[row].splice(left.clone(), cells);
    }
    picture
}

/// A part of a picture's row moved to another column, or taken out.
struct Moved {
    row: usize,
    text: &'static str,
    /// The column it is looked for from.
    from: usize,
    to: Option<usize>,
}

const fn moved(row: usize, text: &'static str, to: Option<usize>) -> Moved {
    Moved {
        row,
        text,
        from: 0,
        to,
    }
}

/// The picture with each part moved, in the order listed: the cells it leaves are blank, and
/// the cells it lands on must be.
fn lined_up(mut picture: Picture, parts: &[Moved]) -> Picture {
    let blank = PictureCell {
        ch: ' ',
        fg: None,
        bg: None,
        bold: false,
    };
    for part in parts {
        let cells = &mut picture.rows[part.row];
        let at = found(cells, part.text, part.from)
            .unwrap_or_else(|| panic!("row {} has no {:?}", part.row, part.text));
        let len = part.text.chars().count();
        let taken: Vec<PictureCell> = cells[at..at + len].to_vec();
        cells[at..at + len].fill(blank.clone());
        if let Some(to) = part.to {
            assert!(
                cells[to..to + len].iter().all(|cell| cell.ch == ' '),
                "row {} has no room for {:?} at {to}",
                part.row,
                part.text
            );
            cells.splice(to..to + len, taken);
        }
    }
    picture
}

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

/// The picture with the tabs' arrows on its key line `row` a space apart, where it writes them
/// together: Ghostty with Iosevka draws both arrows two cells wide, so the panel spaces them.
fn arrows_apart(mut picture: Picture, row: usize) -> Picture {
    let cells = &mut picture.rows[row];
    let at = found(cells, "\u{2190}\u{2192}", 0)
        .unwrap_or_else(|| panic!("row {row} has no arrows together"));
    let mut blank = cells[at].clone();
    blank.ch = ' ';
    cells.insert(at + 1, blank);
    if cells.last().is_some_and(|cell| cell.ch == ' ') {
        cells.pop();
    }
    picture
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
        if new > old {
            // A longer count flush right takes the blank cells before it.
            assert_eq!(
                count.blanks_before,
                Some(count.from),
                "{:?} is longer than {:?} and not flush right",
                count.to,
                count.from
            );
            let grow = new - old;
            assert!(
                at >= grow && cells[at - grow..at].iter().all(|cell| cell.ch == ' '),
                "row {} has no room before {:?}",
                count.row,
                count.from
            );
            let style = cells[at].clone();
            let spelled = count.to.chars().map(|ch| PictureCell {
                ch,
                ..style.clone()
            });
            cells.splice(at - grow..at + old, spelled);
            continue;
        }
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
pub(super) fn panel(
    state: &AppState,
    view: ContextView,
    now: OffsetDateTime,
    places: Places,
) -> Buffer {
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
    let picture = arrows_apart(
        respell(Picture::load("context_tab"), COUNTS_AT_THREE_QUARTERS),
        75,
    );
    let buf = drawn(&example::at_three_quarters());
    // From the panel's rule to its key line: the rows above are the conversation, and the
    // rows below are the prompt after Esc.
    assert_panel_matches(&picture, &buf, 4..76);
}

#[test]
fn the_context_tab_near_full_matches_its_picture() {
    let picture = arrows_apart(
        respell(Picture::load("context_near_full"), COUNTS_NEAR_FULL),
        78,
    );
    let buf = drawn(&example::near_full());
    assert_panel_matches(&picture, &buf, 4..79);
}

/// After the orchestrator run: the main session counted as its answer ended, and the run's
/// nine sessions listed in the order they started, each as it left its window.
#[test]
fn the_context_tab_after_a_run_matches_its_picture() {
    let picture = respell(Picture::load("context_after"), COUNTS_AFTER_RUN);
    let picture = kinds_largest_first(lined_up(picture, LINED_UP));
    let example = example::after_run();
    let mut state = AppState::default();
    state.names = example.names.clone();
    state.conversation.context = Some(example.count.clone());
    state.conversation.worker = Some(example.view.rows[0].worker.clone());
    // Haiku 5.5 is the third of Claude Code's models by tokens in all, and GPT 6 Luna the
    // second of Codex's, so each draws in that shade of its CLI's colour.
    let mut usage = crate::overview::sample::usage();
    usage.all_time_models = [
        ("claude-code", "claude-opus-5-5", 9_400_000),
        ("claude-code", "claude-sonnet-5-5", 2_100_000),
        ("claude-code", "claude-haiku-5-5", 310_000),
        ("codex", "gpt-6-astra", 3_200_000),
        ("codex", "gpt-6-luna", 140_000),
    ]
    .map(|(agent, model, tokens)| ModelTokens {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        tokens,
    })
    .to_vec();
    state.usage = Some(Arc::new(usage));
    let mut run = RunState::new(example::finished_run());
    run.title = example::AFTER_RUN_TITLE.to_owned();
    state.runs.insert(example::finished_run(), run);
    let buf = panel(
        &state,
        example.view.clone(),
        example.now,
        example.places.clone(),
    );
    // From the panel's rule to its key line: the rows above are the conversation.
    assert_panel_matches(&picture, &buf, 4..83);
}

/// A resumed conversation before a turn of it runs again, its session as the record left it:
/// the council session's one turn, which left 25,517 of 1M tokens and spent $0.0040, drawn
/// under the transcript of `resumed_after`.
#[test]
fn a_resumed_conversations_context_matches_its_picture() {
    let mut picture = arrows_apart(
        respell(
            Picture::screen("resumed_after"),
            &[respelled(45, "/1m", "/1M", None)],
        ),
        49,
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
        ended: None,
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
        ContextView::of(vec![row]),
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
