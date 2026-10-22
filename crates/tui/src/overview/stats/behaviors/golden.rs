//! The Behaviors sub-tab drawn from its pictures' example numbers at their width, 118 cells, and
//! compared with them cell for cell, colours included. A picture holds the rows above the map,
//! the legend beside it and the rows under it, with `[picture]` where the map's picture goes;
//! the braille picture holds every row as text.
//!
//! The panel draws its frame as on the tab's other sub-tabs, so three of its rows sit elsewhere
//! than a picture puts them, and each is compared at its own place: the sub-tabs one row lower,
//! under a blank row, and one cell further left; the key line on the panel's last row, one cell
//! further in; and the tab row, whose open tab is Overview on the grey band while the sub-tabs
//! have the keys where the pictures fill Stats in the accent, by its characters alone. Rows a
//! picture draws otherwise say what the screen draws there and why, and fail once they match.

use std::time::Instant;

use argonauts_core::app::ViewData;
use argonauts_core::behavior_map::{BehaviorMap, GRID, MapRequest, Placing};
use argonauts_core::overview::{ModelTokens, OverviewPeriod};
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use time::macros::datetime;

use super::sample::Sample;
use super::{LEGEND_GAP, MAP_COLUMNS, MAP_INSET, MAP_ROWS};
use crate::overview::golden::{self, Picture, PictureCell, Region, parse_parts};
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::state::AppState;
use crate::views::map::{MapCells, MapView};
use crate::views::{View, ViewCtx};

const WIDTH: u16 = 118;
/// The body's first row: under the rule, the tabs, a blank, the sub-tabs and a blank.
const BODY: u16 = 5;
/// The map's first row and the legend's first column.
const MAP_TOP: u16 = BODY + 2;
const LEGEND_X: u16 = (1 + MAP_INSET + MAP_COLUMNS + LEGEND_GAP) as u16;

/// Each model's colour on the page, by its group.
const MODELS: [(&str, &str, &str, &str); 5] = [
    ("Haiku 5.5", "claude-code", "claude-haiku-5-5", "#e08a66"),
    ("GPT 6 Luna", "codex", "gpt-6-luna", "#6aa6ff"),
    ("GPT 5.6 Luna", "codex", "gpt-5.6-luna", "#3fc4f0"),
    ("Haiku 4.5", "claude-code", "claude-haiku-4-5", "#f3b27a"),
    (
        "Gemini 3.8 Flash",
        "antigravity-cli",
        "gemini-3.8-flash",
        "#36cfe6",
    ),
];
const SIZES: [usize; 5] = [406, 66, 49, 22, 19];

/// GPT 6 Luna's behaviors and Haiku 5.5's, with each one's count and the others'.
const LUNA: [(&str, usize, usize); 5] = [
    ("checked git status or diff and read git history", 4, 2),
    ("checked git status or diff and ran a shell command", 3, 4),
    ("ran the test suite", 8, 19),
    ("returned its structured answer", 34, 135),
    ("edited a source file", 3, 23),
];
const HAIKU: [(&str, usize, usize); 6] = [
    ("wrote a workflow script", 13, 0),
    ("gave a structured answer that didn't fit", 20, 1),
    ("asked another agent to review or judge", 8, 0),
    ("edited a test file", 13, 1),
    ("searched code by pattern", 25, 3),
    ("read a source file", 53, 9),
];

/// A picture's parts: the rows above the map, the legend, the rows under it; or one part.
fn load(name: &str) -> Vec<Vec<Vec<PictureCell>>> {
    load_in("behaviors", name)
}

/// As [`load`], from the pictures of the screen area `area`.
fn load_in(area: &str, name: &str) -> Vec<Vec<Vec<PictureCell>>> {
    let (text, html) = argonauts_testkit::picture::read(&golden::golden_folder(area), name);
    let parts = parse_parts(&html);
    let texts: Vec<&str> = text.split("\n\n[picture]\n\n").collect();
    assert_eq!(parts.len(), texts.len(), "{name}: its parts");
    for (part, text) in parts.iter().zip(&texts) {
        let rows: Vec<String> = part
            .iter()
            .map(|r| r.iter().map(|c| c.ch).collect())
            .collect();
        assert_eq!(
            rows,
            text.lines().collect::<Vec<_>>(),
            "{name}: the .txt and the .html disagree"
        );
    }
    parts
}

/// How one picture row is compared with the screen.
enum How {
    /// Cell for cell, the picture's column `c` at the screen's `c + shift`.
    Cells(i16),
    /// By its characters alone, for the reason given.
    Text(i16, &'static str),
    /// The screen draws this instead, for the reason given.
    Ours(String, &'static str),
}

struct Check<'a> {
    row: &'a [PictureCell],
    at: (u16, u16),
    how: How,
}

/// The row moved `shift` cells, as the screen draws it.
fn shifted(row: &[PictureCell], shift: i16) -> Vec<PictureCell> {
    let blank = PictureCell {
        ch: ' ',
        fg: None,
        bg: None,
        bold: false,
    };
    match shift {
        0 => row.to_vec(),
        s if s > 0 => std::iter::repeat_n(blank, s as usize)
            .chain(row.iter().cloned())
            .collect(),
        s => row.iter().skip((-s) as usize).cloned().collect(),
    }
}

fn text_at(buf: &Buffer, (x, y): (u16, u16)) -> String {
    (x..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn compare(name: &str, buf: &Buffer, checks: Vec<Check<'_>>) {
    golden::keep(name, buf);
    let theme = golden::theme();
    let mut found = Vec::new();
    for check in checks {
        let words: String = check.row.iter().map(|c| c.ch).collect();
        let (x, y) = check.at;
        match check.how {
            How::Cells(shift) => {
                let row = shifted(check.row, shift);
                let width = row.len();
                let picture = Picture::of(name, vec![row]);
                let region = Region {
                    rows: 0..1,
                    columns: 0..width.min(usize::from(buf.area.width - x)),
                    at: (x, y),
                };
                for difference in golden::differences(&picture, buf, &region, &theme) {
                    found.push(format!("screen row {y}, {words:?}: {difference}"));
                }
            }
            How::Text(shift, why) => {
                let wanted: String = shifted(check.row, shift).iter().map(|c| c.ch).collect();
                let got = text_at(buf, (x, y));
                if got != wanted.trim_end() {
                    found.push(format!(
                        "screen row {y}, compared by its text since {why}:\n  picture {wanted:?}\n  drawn   {got:?}"
                    ));
                }
            }
            How::Ours(ours, why) => {
                let got = text_at(buf, (x, y));
                if got != ours {
                    found.push(format!(
                        "screen row {y}:\n  ours  {ours:?}\n  drawn {got:?}"
                    ));
                }
                assert_ne!(words.trim_end(), ours, "{name}: row {y} matches now: {why}");
            }
        }
    }
    if !found.is_empty() {
        let screen: Vec<String> = (0..buf.area.height).map(|y| text_at(buf, (0, y))).collect();
        panic!(
            "{name}: {} differences\n{}\n\nscreen:\n{}",
            found.len(),
            found.join("\n"),
            screen.join("\n")
        );
    }
}

/// The tabs, the sub-tabs, the line under them and its blank row.
fn frame<'a>(top: &'a [Vec<PictureCell>], line: Option<(String, &'static str)>) -> Vec<Check<'a>> {
    let mut checks = vec![
        Check {
            row: &top[0],
            at: (0, 0),
            how: How::Cells(0),
        },
        Check {
            row: &top[1],
            at: (0, 1),
            how: How::Text(
                0,
                "the panel fills the open tab, Overview, on the grey band while the keys are under it",
            ),
        },
        Check {
            row: &top[2],
            at: (0, 3),
            how: How::Cells(-1),
        },
        Check {
            row: &top[4],
            at: (0, BODY + 1),
            how: How::Cells(0),
        },
    ];
    checks.push(Check {
        row: &top[3],
        at: (0, BODY),
        how: match line {
            Some((ours, why)) => How::Ours(ours, why),
            None => How::Cells(0),
        },
    });
    checks
}

/// The legend beside the map, row by row from the map's top, and the map's cells blank under
/// the picture.
fn legend<'a>(legend: &'a [Vec<PictureCell>], blank: &'a [PictureCell]) -> Vec<Check<'a>> {
    let mut checks: Vec<Check<'a>> = legend
        .iter()
        .enumerate()
        .map(|(at, row)| Check {
            row,
            at: (LEGEND_X, MAP_TOP + at as u16),
            how: How::Cells(0),
        })
        .collect();
    for row in 0..MAP_ROWS as u16 {
        checks.push(Check {
            row: blank,
            at: (0, MAP_TOP + row),
            how: How::Cells(0),
        });
    }
    checks
}

/// The rows under the map, the last one, the keys, on the panel's last row.
fn under<'a>(
    bottom: &'a [Vec<PictureCell>],
    height: u16,
    ours: &[(usize, String, &'static str)],
) -> Vec<Check<'a>> {
    let last = bottom.len() - 1;
    bottom
        .iter()
        .enumerate()
        .map(|(at, row)| {
            let y = if at == last {
                height - 1
            } else {
                MAP_TOP + MAP_ROWS as u16 + at as u16
            };
            let how = match ours.iter().find(|(row, ..)| *row == at) {
                Some((_, words, why)) => How::Ours(words.clone(), why),
                None if at == last => How::Cells(1),
                None => How::Cells(0),
            };
            Check {
                row,
                at: (0, y),
                how,
            }
        })
        .collect()
}

fn blank_map_row() -> Vec<PictureCell> {
    vec![
        PictureCell {
            ch: ' ',
            fg: None,
            bg: None,
            bold: false,
        };
        1 + MAP_INSET + MAP_COLUMNS
    ]
}

fn models() -> argonauts_core::overview::OverviewData {
    let mut data = crate::overview::home::sample::overview();
    data.period = OverviewPeriod::All;
    // The places among each CLI's models that give the page's shades.
    let tokens = [
        ("claude-code", "claude-haiku-5-5", 900),
        ("claude-code", "claude-opus-5-5", 800),
        ("claude-code", "claude-haiku-4-5", 700),
        ("codex", "gpt-6-luna", 900),
        ("codex", "gpt-6", 800),
        ("codex", "gpt-5.6", 700),
        ("codex", "gpt-5.6-luna", 600),
        ("antigravity-cli", "gemini-3.8-flash", 900),
    ];
    data.all_time_models = tokens
        .iter()
        .map(|(agent, model, tokens)| ModelTokens {
            agent: (*agent).to_owned(),
            model: Some((*model).to_owned()),
            tokens: *tokens,
        })
        .collect();
    data
}

fn groups(request: MapRequest) -> Sample {
    MODELS
        .iter()
        .fold(Sample::new(request), |sample, (name, agent, model, _)| {
            sample.model(name, agent, model)
        })
}

/// Every group at its size, Luna's and Haiku's behaviors at their counts, the rest filler.
fn label_map() -> BehaviorMap {
    let mut sample = groups(MapRequest::default());
    let place = |n: usize| (0.1 + 0.07 * n as f32, 0.2 + 0.05 * n as f32);
    for (n, (behavior, mine, others)) in LUNA.iter().enumerate() {
        sample =
            sample
                .actions(1, behavior, *mine, place(n))
                .actions(0, behavior, *others, place(n));
    }
    for (n, (behavior, mine, others)) in HAIKU.iter().enumerate() {
        sample = sample.actions(0, behavior, *mine, place(n + 5)).actions(
            2,
            behavior,
            *others,
            place(n + 5),
        );
    }
    for (group, size) in SIZES.iter().enumerate() {
        let short = size - sample.map.groups[group].actions;
        sample = sample.actions(group, "listed a folder", short, (0.9, 0.9));
    }
    sample.rows(1, &LUNA).rows(0, &HAIKU).map
}

fn state() -> AppState {
    let mut state = AppState::default();
    state.names = crate::overview::sample::names();
    state
}

/// The panel on Behaviors with `map`, after `keys` on the map, `height` rows tall.
fn draw(map: &BehaviorMap, keys: &[KeyCode], height: u16, pictures: bool) -> Buffer {
    draw_pointing(map, keys, None, height, pictures)
}

/// Like [`draw`], with the mouse resting at `pointer` on the map.
fn draw_pointing(
    map: &BehaviorMap,
    keys: &[KeyCode],
    pointer: Option<(f32, f32)>,
    height: u16,
    pictures: bool,
) -> Buffer {
    drawn(map, keys, pointer, height, pictures).1
}

/// The panel as [`draw_pointing`] leaves it, and what it drew.
fn drawn(
    map: &BehaviorMap,
    keys: &[KeyCode],
    pointer: Option<(f32, f32)>,
    height: u16,
    pictures: bool,
) -> (OverviewView, Buffer) {
    drawn_in(&state(), map, keys, pointer, height, pictures)
}

/// As [`drawn`], with what the app has told the screen in `state`.
fn drawn_in(
    state: &AppState,
    map: &BehaviorMap,
    keys: &[KeyCode],
    pointer: Option<(f32, f32)>,
    height: u16,
    pictures: bool,
) -> (OverviewView, Buffer) {
    let mut theme = golden::theme();
    theme.mascot_pictures = pictures;
    let mut view =
        OverviewView::new(Tab::Overview, UsageWatch::default()).at(datetime!(2026-10-20 08:40 -4));
    if pictures {
        view = view.with_map_picture();
    }
    let press = |view: &mut OverviewView, code: KeyCode| {
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        view.handle_key(code.into(), &ctx);
    };
    assert!(view.accept(&ViewData::Overview {
        overview: Box::new(models())
    }));
    for code in [KeyCode::Down, KeyCode::Right, KeyCode::Right] {
        press(&mut view, code);
    }
    view.accept(&ViewData::Behaviors {
        map: Box::new(map.clone()),
    });
    press(&mut view, KeyCode::Down);
    for code in keys {
        press(&mut view, *code);
    }
    if pointer.is_some() {
        assert!(view.point_at_map(pointer), "the mouse rests near a dot");
    }
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, WIDTH, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    (view, buf)
}

fn height_for(bottom: &[Vec<PictureCell>]) -> u16 {
    MAP_TOP + MAP_ROWS as u16 + bottom.len() as u16
}

/// A picture of the map with the picture beside its legend, after `keys`.
fn map_screen(
    name: &str,
    map: &BehaviorMap,
    keys: &[KeyCode],
    line: Option<(String, &'static str)>,
    ours: &[(usize, String, &'static str)],
) {
    map_screen_in("behaviors", name, map, keys, line, ours);
}

/// As [`map_screen`], with the picture from the screen area `area`.
fn map_screen_in(
    area: &str,
    name: &str,
    map: &BehaviorMap,
    keys: &[KeyCode],
    line: Option<(String, &'static str)>,
    ours: &[(usize, String, &'static str)],
) {
    let parts = load_in(area, name);
    let (top, legend_rows, bottom) = (&parts[0], &parts[1], &parts[2]);
    let height = height_for(bottom);
    let buf = draw(map, keys, height, true);
    let blank = blank_map_row();
    let mut checks = frame(top, line);
    checks.extend(legend(legend_rows, &blank));
    checks.extend(under(bottom, height, ours));
    compare(name, &buf, checks);
}

const NEWER_LINE: &str = "the line under the tabs reads as the opening pictures have it; this picture keeps an \
     earlier wording, with the period and without the projects";

fn opening_line() -> String {
    "  What your agents did, from 562 actions in 19 runs across 6 projects · by model · Argonauts' labels"
        .to_owned()
}

#[test]
fn the_map_opens_on_every_models_dots_with_nothing_compared() {
    map_screen("open_plain", &label_map(), &[], None, &[]);
}

#[test]
fn right_compares_a_model_with_the_rest() {
    map_screen(
        "open_compared",
        &label_map(),
        &[KeyCode::Right, KeyCode::Right],
        None,
        &[],
    );
}

#[test]
fn down_lights_lunas_first_behavior() {
    let keys = [KeyCode::Right, KeyCode::Right, KeyCode::Down];
    map_screen(
        "lit_luna",
        &label_map(),
        &keys,
        Some((opening_line(), NEWER_LINE)),
        &[],
    );
}

#[test]
fn down_lights_haikus_first_behavior() {
    let keys = [KeyCode::Right, KeyCode::Down];
    map_screen(
        "lit_haiku",
        &label_map(),
        &keys,
        Some((opening_line(), NEWER_LINE)),
        &[],
    );
}

#[test]
fn the_first_visit_offers_the_deeper_map() {
    let mut map = label_map();
    map.offer_deeper = true;
    map_screen(
        "first_visit",
        &map,
        &[KeyCode::Right, KeyCode::Right],
        None,
        &[],
    );
}

/// Haiku's deeper behaviors in the model's words, each with its label, two marked new.
fn deep_map() -> BehaviorMap {
    let mut map = label_map();
    map.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B on Ollama".to_owned(),
    };
    let rows = [
        (
            "Plan rate limiting changes",
            "returned its structured answer",
            true,
        ),
        (
            "Initiate due date feature development workflow",
            "wrote a workflow script",
            false,
        ),
        (
            "Fix token clamping issue",
            "returned its structured answer",
            true,
        ),
        (
            "Run tests and verify parse behavior",
            "ran the test suite and ran a Python script",
            false,
        ),
        (
            "Read bucket implementation file",
            "read a source file",
            false,
        ),
        (
            "Write and modify token bucket implementation",
            "ran the test suite",
            false,
        ),
    ];
    let haiku: Vec<usize> = map.in_group(0).map(|(at, _)| at).take(14).collect();
    map.comparisons[0].rows = rows
        .iter()
        .map(
            |(summary, label, new)| argonauts_core::behavior_map::BehaviorRow {
                behavior: (*summary).to_owned(),
                label: Some((*label).to_owned()),
                mine: 14,
                others: 0,
                lit: haiku.clone(),
                new: *new,
            },
        )
        .collect();
    map
}

#[test]
fn the_deeper_map_lights_haikus_first_summary() {
    map_screen(
        "deep_lit_haiku",
        &deep_map(),
        &[KeyCode::Right, KeyCode::Down],
        None,
        &[],
    );
}

#[test]
fn enter_on_a_deeper_behavior_rings_its_actions() {
    let mut map = deep_map();
    let actions = [
        (
            "Read external task's implementation",
            "build:per-api-key",
            "run-20261017-071701-3f92",
            "read git history",
        ),
        (
            "Outline task implementation plan",
            "plan:due",
            "run-20261014-055153-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline implementation phases and details",
            "plan:orchestrator",
            "run-20261017-070940-0a1b",
            "returned its structured answer",
        ),
        (
            "Generate task metadata and phase description",
            "plan:council",
            "run-20261020-122145-0a1b",
            "returned its structured answer",
        ),
        (
            "Document task name and description",
            "plan:orchestrator",
            "run-20261014-055235-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline project plan and build phases for task management",
            "plan:orchestrator",
            "run-20261017-070202-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline the tests",
            "plan:due",
            "run-20261014-055300-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline the review",
            "plan:due",
            "run-20261014-055400-0a1b",
            "returned its structured answer",
        ),
    ];
    let haiku: Vec<usize> = map
        .in_group(0)
        .map(|(at, _)| at)
        .take(actions.len())
        .collect();
    for (at, (summary, agent, run, label)) in haiku.iter().zip(actions) {
        let action = &mut map.actions[*at];
        action.summary = Some(summary.to_owned());
        action.behavior = summary.to_owned();
        action.agent = agent.to_owned();
        action.run = Some(run.to_owned());
        action.label = label.to_owned();
    }
    let row = &mut map.comparisons[0].rows[0];
    row.behavior = actions[0].0.to_owned();
    row.lit = haiku;
    let line = (
        "  Deeper map · 562 actions summarized by Qwen3 Coder 30B on Ollama · by model".to_owned(),
        "the picture's line names what it shows; the screen keeps the map's own line",
    );
    let keys = [KeyCode::Right, KeyCode::Down, KeyCode::Enter];
    map_screen("deep_examples", &map, &keys, Some(line), &[]);
}

/// GPT 6 Luna's nine git history reads, newest first, and the other models' nine.
fn examples_map(card: bool) -> BehaviorMap {
    let commands = [
        (
            "git status --porcelain=v1 && git show HEAD:ratelimit/bucket.py",
            "run-20261020-123036-4e50",
        ),
        (
            "git show --stat --oneline ef3f187 && git show ef3f187 -- ratelimit/bucket.py",
            "run-20261020-123036-4e50",
        ),
        (
            "\"git status --short && git diff -- ratelimit/bucket.py ratelimit/__init__.py\"",
            "run-20261020-123036-4e50",
        ),
        (
            "\"git status --short && git log -1 --oneline && sed -n '1,240p' ratelimit/bucket.py\"",
            "run-20261020-123036-4e50",
        ),
        (
            "tmpdir=$(mktemp -d) && cp -R ratelimit tests \"$tmpdir\"/ && git -C \"$tmpdir\" init",
            "run-20261017-071701-3f92",
        ),
    ];
    let mut sample = groups(MapRequest::default())
        .actions(1, "read git history", 9, (0.6, 0.8))
        .actions(0, "read git history", 9, (0.6, 0.8));
    for (group, size) in SIZES.iter().enumerate() {
        let short = size - sample.map.groups[group].actions;
        sample = sample.actions(group, "listed a folder", short, (0.9, 0.9));
    }
    let mut map = sample.rows(1, &[("read git history", 9, 9)]).map;
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).take(9).collect();
    for (n, at) in luna.iter().enumerate() {
        let action = &mut map.actions[*at];
        action.at = datetime!(2026-10-20 12:40 UTC) - time::Duration::minutes(n as i64 * 4);
        if let Some((command, run)) = commands.get(n) {
            action.input = (*command).to_owned();
            action.run = Some((*run).to_owned());
        }
    }
    let second = &mut map.actions[luna[1]];
    second.at = datetime!(2026-10-20 12:32 UTC) + time::Duration::minutes(4);
    if card {
        second.input = "git show --stat --oneline ef3f187 && git show ef3f187".to_owned();
        second.run_shows = Some(
            "ef3f187 is cap-bucket-refills' commit. This task started without it, so it read the change \
             from history and copied it in, and review sent the copy back for its scope."
                .to_owned(),
        );
        second.at = datetime!(2026-10-20 12:32 UTC);
    }
    map
}

#[test]
fn enter_lists_lunas_git_history_reads_and_opens_the_seconds_card() {
    let parts = load("examples_card");
    let rows = &parts[0];
    // The list: its heading, a blank, five reads; then the keys under them.
    let keys = [
        KeyCode::Right,
        KeyCode::Right,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Down,
    ];
    let height = 32;
    let buf = draw(&examples_map(false), &keys, height, true);
    let heading = MAP_TOP + MAP_ROWS as u16 + 3;
    let mut checks = vec![Check {
        row: &rows[0],
        at: (0, heading),
        how: How::Ours(
            "  GPT 6 Luna · read git history · 9 times".to_owned(),
            "the list names the behavior as its row does; the picture's grouped the label with the ones \
             that join it to another command",
        ),
    }];
    for (at, row) in rows.iter().enumerate().take(7).skip(1) {
        checks.push(Check {
            row,
            at: (0, heading + at as u16),
            how: How::Cells(0),
        });
    }
    checks.push(Check {
        row: &rows[8],
        at: (0, height - 1),
        how: How::Cells(1),
    });
    compare("examples_card", &buf, checks);
    // Enter on the second: its card, the picture's rows from the twelfth.
    let mut keys = keys.to_vec();
    keys.push(KeyCode::Enter);
    let height = 20;
    let buf = draw(&examples_map(true), &keys, height, true);
    let card = &rows[12..];
    let last = card.len() - 1;
    let checks = card
        .iter()
        .enumerate()
        .map(|(at, row)| Check {
            row,
            at: (0, if at == last { height - 1 } else { BODY + at as u16 }),
            how: match at {
                1 => How::Ours(
                    "  run-20261020-123036-4e50 · council project · spec-first · 8:32 AM · its action 3 of 6"
                        .to_owned(),
                    "a card names the run's project, the folder it ran in with or without git",
                ),
                _ if at == last => How::Cells(1),
                _ => How::Cells(0),
            },
        })
        .collect();
    compare("examples_card", &buf, checks);
}

/// The braille picture's own dots as actions, each in the group whose colour its cell has,
/// and its shading as the comparison's difference.
fn braille_map(rows: &[Vec<PictureCell>]) -> BehaviorMap {
    let mut sample = groups(MapRequest::default());
    let mut grid = vec![0.0f32; GRID * GRID];
    let bits = [
        (0, 0, 0x01),
        (1, 0, 0x02),
        (2, 0, 0x04),
        (0, 1, 0x08),
        (1, 1, 0x10),
        (2, 1, 0x20),
        (3, 0, 0x40),
        (3, 1, 0x80),
    ];
    for (row, line) in rows.iter().enumerate() {
        for (column, cell) in line.iter().skip(2).take(MAP_COLUMNS).enumerate() {
            if let Some(bg) = &cell.bg {
                let x = ((column as f32 + 0.5) / MAP_COLUMNS as f32 * GRID as f32) as usize;
                let y = ((1.0 - (row as f32 + 0.5) / MAP_ROWS as f32) * GRID as f32) as usize;
                grid[y.min(GRID - 1) * GRID + x.min(GRID - 1)] =
                    if bg == "#4a2a33" { 1.0 } else { -1.0 };
            }
            let code = u32::from(cell.ch).wrapping_sub(0x2800);
            let Some(group) = MODELS.iter().position(|m| Some(m.3) == cell.fg.as_deref()) else {
                continue;
            };
            for (dr, dc, bit) in bits {
                if code & bit != 0 && code <= 0xff {
                    let x = (column * 2 + dc) as f32 + 0.5;
                    let y = (row * 4 + dr) as f32 + 0.5;
                    let place = (
                        x / (MAP_COLUMNS * 2) as f32,
                        1.0 - y / (MAP_ROWS * 4) as f32,
                    );
                    sample = sample.at(group, "listed a folder", place);
                }
            }
        }
    }
    for (group, size) in SIZES.iter().enumerate() {
        let short = size.saturating_sub(sample.map.groups[group].actions);
        sample = sample.again(group, short);
    }
    let mut map = sample.rows(1, &LUNA).map;
    map.comparisons[1].overlay = Some(grid);
    map
}

#[test]
fn without_pictures_the_map_is_braille_its_shading_the_comparison() {
    let parts = load("braille");
    let rows = &parts[0];
    let height = 31;
    let map = braille_map(&rows[5..18]);
    let buf = draw(&map, &[KeyCode::Right, KeyCode::Right], height, false);
    let last = rows.len() - 1;
    let mut checks = frame(&rows[..5], Some((opening_line(), NEWER_LINE)));
    for (at, row) in rows.iter().enumerate().skip(5) {
        let y = if at == last {
            height - 1
        } else {
            at as u16 + 2
        };
        let how = match at {
            23 => How::Ours(
                "    checked git status or diff and read git history         4 (6.1%)   others   2 (0.4%)".to_owned(),
                "with the keys on the map no row is selected; the picture marked the first",
            ),
            _ if at == last => How::Ours(
                "   ←  →  compare a model · ↓  into the list, which lights each behavior · g group · p period · esc back"
                    .to_owned(),
                "the key line is the map's own; the picture's is an earlier one",
            ),
            _ => How::Cells(0),
        };
        checks.push(Check {
            row,
            at: (0, y),
            how,
        });
    }
    compare("braille", &buf, checks);
}

#[test]
fn the_mouse_rings_the_dot_it_rests_near_and_says_what_it_is() {
    let mut map = deep_map();
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).collect();
    let near = &mut map.actions[luna[0]];
    near.summary = Some("Reviewed git history and previous implementations".to_owned());
    near.behavior = "Reviewed git history and previous implementations".to_owned();
    near.label = "read git history".to_owned();
    near.agent = "tie-breaker-2".to_owned();
    (near.x, near.y) = (0.95, 0.06);
    let pointer = (0.954, 0.057);
    let parts = load("mouse");
    let (top, legend_rows, bottom) = (&parts[0], &parts[1], &parts[2]);
    let height = height_for(bottom);
    let buf = draw_pointing(
        &map,
        &[KeyCode::Right, KeyCode::Right],
        Some(pointer),
        height,
        true,
    );
    let line = (
        "  Deeper map · 562 actions summarized by Qwen3 Coder 30B on Ollama · by model".to_owned(),
        "the picture's line names what it shows; the screen keeps the map's own line",
    );
    let blank = blank_map_row();
    let mut checks = frame(top, Some(line));
    checks.extend(legend(legend_rows, &blank));
    checks.extend(under(bottom, height, &[]));
    compare("mouse", &buf, checks);
}

/// The cells the panel leaves for the picture are the ones the pictures leave, and the braille
/// map takes the same cells; the picture's key changes as a behavior lights, and only then.
#[test]
fn the_map_says_where_its_cells_are_for_the_picture_and_the_mouse() {
    let map = label_map();
    let height = 40;
    let (view, _) = drawn(&map, &[], None, height, true);
    let cells = view.map_cells().expect("the map shows");
    let area = Rect::new(
        (1 + MAP_INSET) as u16,
        MAP_TOP,
        MAP_COLUMNS as u16,
        MAP_ROWS as u16,
    );
    assert_eq!(cells.area, area);
    let (again, _) = drawn(&map, &[], None, height, true);
    assert_eq!(again.map_cells(), Some(cells));
    let (lit, _) = drawn(&map, &[KeyCode::Right, KeyCode::Down], None, height, true);
    let lit = lit.map_cells().expect("the map shows");
    assert_eq!(lit.area, area);
    assert_ne!(lit.picture, cells.picture);
    assert!(lit.picture.is_some());
    let (braille, _) = drawn(&map, &[], None, height, false);
    assert_eq!(
        braille.map_cells(),
        Some(MapCells {
            area,
            picture: None
        })
    );
    // A card has the map put away.
    let (card, _) = drawn(
        &map,
        &[
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Enter,
        ],
        None,
        height,
        true,
    );
    assert_eq!(card.map_cells(), None);
}

#[test]
fn while_the_deeper_map_is_made_the_tab_says_how_far_as_its_picture_shows() {
    let parts = load_in("overview", "choose_running");
    let (top, legend_rows, bottom) = (&parts[0], &parts[1], &parts[2]);
    let mut state = state();
    state.deep.running = Some(argonauts_core::deep_map::DeepProgress {
        done: 312,
        total: 562,
        seconds_left: Some(50),
        summarizer: "Qwen3 Coder 30B on Ollama".to_owned(),
    });
    // The line under the sub-tabs, a blank row and the progress, then the panel's blank row.
    let map_top = BODY + 4;
    // The picture ends above the panel's own last rows, the blank and the keys.
    let height = map_top + MAP_ROWS as u16 + bottom.len() as u16 + 2;
    let keys = [KeyCode::Right, KeyCode::Right];
    let (_, buf) = drawn_in(&state, &label_map(), &keys, None, height, true);
    let mut checks = frame(&top[..5], None);
    for (at, row) in top.iter().enumerate().skip(5) {
        checks.push(Check {
            row,
            at: (0, BODY + at as u16 - 3),
            how: How::Cells(0),
        });
    }
    let blank = blank_map_row();
    for (at, row) in legend_rows.iter().enumerate() {
        checks.push(Check {
            row,
            at: (LEGEND_X, map_top + at as u16),
            how: How::Cells(0),
        });
    }
    for row in 0..MAP_ROWS as u16 {
        checks.push(Check {
            row: &blank,
            at: (0, map_top + row),
            how: How::Cells(0),
        });
    }
    for (at, row) in bottom.iter().enumerate() {
        checks.push(Check {
            row,
            at: (0, map_top + MAP_ROWS as u16 + at as u16),
            how: How::Cells(0),
        });
    }
    compare("choose_running", &buf, checks);
}

#[test]
fn d_on_the_first_visit_asks_in_the_tab_as_its_picture_shows_and_starts_on_the_answers() {
    use argonauts_core::app::{UiCommand, ViewRequest};

    use crate::views::ViewOutcome;

    let mut map = label_map();
    map.offer_deeper = true;
    let state = state();
    let theme = golden::theme();
    let (mut view, _) = drawn_in(&state, &map, &[], None, 40, true);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    assert!(matches!(
        view.handle_key(KeyCode::Char('d').into(), &ctx),
        ViewOutcome::Request(ViewRequest::DeepOffer)
    ));
    let offer = crate::questions::deep_map::sample::page_offer();
    assert!(view.accept(&ViewData::DeepOffer {
        offer: Box::new(offer)
    }));
    let area = Rect::new(0, 0, WIDTH, 40);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    // The first question, under the sub-tabs where the map was.
    let parts = load_in("overview", "first_visit_questions");
    let checks = parts[0]
        .iter()
        .take(10)
        .enumerate()
        .map(|(at, row)| Check {
            row,
            at: (0, BODY + at as u16),
            how: How::Cells(0),
        })
        .collect();
    compare("first_visit_questions", &buf, checks);
    for _ in 0..2 {
        assert!(matches!(
            view.handle_key(KeyCode::Enter.into(), &ctx),
            ViewOutcome::Consumed
        ));
    }
    assert!(matches!(
        view.handle_key(KeyCode::Enter.into(), &ctx),
        ViewOutcome::Send(UiCommand::DeepStart { .. })
    ));
    // The map the update makes is the one the tab asks for next.
    let next = view.handle_key(KeyCode::Char('p').into(), &ctx);
    assert!(
        matches!(
            next,
            ViewOutcome::Request(ViewRequest::Behaviors(MapRequest {
                map: Some(argonauts_core::settings::behaviors::BehaviorMap::Deeper),
                ..
            }))
        ),
        "{next:?}"
    );
}

#[test]
fn when_the_deeper_map_is_made_its_list_marks_what_labels_only_group_as_its_picture_shows() {
    let mut map = deep_map();
    let rows = [
        (
            "Accept task completion",
            "returned its structured answer",
            true,
        ),
        (
            "Reject change due to negative input handling failure",
            "returned its structured answer",
            true,
        ),
        (
            "Located key code elements and references",
            "checked git status or diff and ran a shell command",
            false,
        ),
        ("Run full unit test suite", "ran the test suite", false),
        (
            "Check current repository status and file changes",
            "checked git status or diff and read git history",
            false,
        ),
        (
            "Confirm task completion",
            "returned its structured answer",
            true,
        ),
    ];
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).take(14).collect();
    map.comparisons[1].rows = rows
        .iter()
        .map(
            |(summary, label, new)| argonauts_core::behavior_map::BehaviorRow {
                behavior: (*summary).to_owned(),
                label: Some((*label).to_owned()),
                mine: 14,
                others: 0,
                lit: luna.clone(),
                new: *new,
            },
        )
        .collect();
    map_screen_in(
        "overview",
        "choose_done",
        &map,
        &[KeyCode::Right, KeyCode::Right, KeyCode::Down],
        None,
        &[],
    );
}
