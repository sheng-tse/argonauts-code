//! The Starting lines of the fork picture, drawn as the main session's reply is drawn, on macOS
//! at the picture's width of 118 columns, and compared cell for cell with `tests/golden/fork_start`:
//! the text, and which cells are bold, as the picture's colours mark them.

use std::time::Instant;

use argonauts_testkit::pictures::picture;
use argonauts_tui::base::theme::Glyphs;
use argonauts_tui::cells::cell::{AssistantMessage, HistoryCell};
use argonauts_tui::{Clock, RenderCtx, Theme};
use ratatui::style::Modifier;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

const WIDTH: u16 = 118;
const TXT: &str = include_str!("golden/fork_start.txt");
const HTML: &str = include_str!("golden/fork_start.html");

/// The lines the app builds for the two starts, which its own test checks against the same
/// picture: a fork on the main session's CLI, and agy, which starts from the brief.
const FORKED: &str = "Starting a workflow: Orchestrator, **Opus 5.5 on Claude Code**, carrying \
                      this conversation; **GPT 6 Luna on Codex** reviews as you asked, and \
                      **Haiku 5.5 on Claude Code** builds.";
const FROM_THE_BRIEF: &str = "Starting a workflow: Orchestrator, **Gemini 3.6 Pro on agy**, \
                              starting from the brief, since agy can't carry a Claude Code \
                              conversation; **GPT 6 Luna on Codex** reviews as you asked, and \
                              **Haiku 5.5 on Claude Code** builds.";

/// One cell of a row: its text, and whether it is bold.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    text: String,
    bold: bool,
}

fn drawn(line: &Line<'_>) -> Vec<Cell> {
    let mut cells = Vec::new();
    for span in &line.spans {
        let bold = span.style.add_modifier.contains(Modifier::BOLD);
        for ch in span.content.chars() {
            push(&mut cells, ch, bold);
        }
    }
    trimmed(cells)
}

/// A row of the picture: only which cells are bold matters here.
fn pictured(row: &[picture::Cell]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for cell in row {
        push(&mut cells, cell.ch, cell.bold);
    }
    trimmed(cells)
}

fn push(cells: &mut Vec<Cell>, ch: char, bold: bool) {
    let text = ch.to_string();
    let width = text.width();
    cells.push(Cell { text, bold });
    // A wide character's second cell holds nothing of its own.
    for _ in 1..width {
        cells.push(Cell {
            text: String::new(),
            bold,
        });
    }
}

/// Without the blanks at the end, which the picture pads to its width.
fn trimmed(mut cells: Vec<Cell>) -> Vec<Cell> {
    while cells.last().is_some_and(|cell| cell.text == " ") {
        cells.pop();
    }
    cells
}

#[test]
fn the_fork_starting_lines_are_their_picture() {
    assert_starts_drawn(TXT, &picture::grid(HTML));
}

/// The comparison fails once one cell of the picture is changed: its character or its weight.
#[test]
fn a_picture_changed_in_one_cell_is_no_longer_the_starts() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let grid = picture::grid(HTML);
    let row = TXT
        .lines()
        .position(|row| row.starts_with(STARTS[0]))
        .expect("the first start's row");
    // The last bold letter, after the words that find the row.
    let column = grid[row]
        .iter()
        .rposition(|cell| cell.bold && cell.ch.is_alphanumeric())
        .expect("a bold word");
    let fails = |txt: &str, grid: &picture::Grid| {
        catch_unwind(AssertUnwindSafe(|| assert_starts_drawn(txt, grid))).is_err()
    };
    let mut bolder = grid.clone();
    bolder[row][column].bold = false;
    assert!(fails(TXT, &bolder), "a weight changed");
    let mut respelled = grid.clone();
    respelled[row][column].ch = '#';
    let mut rows: Vec<String> = TXT.lines().map(str::to_owned).collect();
    let mut chars: Vec<char> = rows[row].chars().collect();
    chars[column] = '#';
    rows[row] = chars.into_iter().collect();
    assert!(fails(&rows.join("\n"), &respelled), "a character changed");
}

/// How each start's first row begins in the picture.
const STARTS: [&str; 2] = [
    "⏺ Starting a workflow: Orchestrator, Opus 5.5",
    "⏺ Starting a workflow: Orchestrator, Gemini 3.6 Pro",
];

/// The two starts drawn are the picture's rows that begin as [`STARTS`] says, in their text and
/// in which cells are bold.
fn assert_starts_drawn(txt: &str, html: &picture::Grid) {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::MACOS;
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let text: Vec<&str> = txt.lines().collect();
    for (line, starts) in [(FORKED, STARTS[0]), (FROM_THE_BRIEF, STARTS[1])] {
        let at = text
            .iter()
            .position(|row| row.starts_with(starts))
            .unwrap_or_else(|| panic!("no row starts {starts:?}"));
        let rows = AssistantMessage::new(line).lines(&ctx);
        assert_eq!(rows.len(), 2, "{rows:?}");
        for (offset, row) in rows.iter().enumerate() {
            let want = pictured(&html[at + offset]);
            let got = drawn(row);
            let plain = |cells: &[Cell]| {
                cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            };
            assert_eq!(
                plain(&got),
                text[at + offset].trim_end(),
                "row {}",
                at + offset
            );
            assert_eq!(plain(&got), plain(&want), "row {}", at + offset);
            for (column, (got, want)) in got.iter().zip(&want).enumerate() {
                assert_eq!(
                    got.bold,
                    want.bold,
                    "row {} column {column}: {:?}",
                    at + offset,
                    got.text
                );
            }
        }
    }
}
