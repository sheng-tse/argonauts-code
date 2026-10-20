//! The welcome header with each mode's mascot in block cells, as `golden/mascot_headers` draws
//! it: the three mascots in amber, with Claude Code's ultracode on, and with Codex at its ultra
//! effort, each beside the header's three lines. The picture's words after the folder, such as
//! `(Argonaut, normal)`, say which header it is and are not drawn.
//!
//! The Hydra's cells there are the ones drawn before its final drawing; the header draws the
//! ones redrawn after it, so its nine cells are checked against the generator's grid instead,
//! and the cells that changed are named here.

use std::time::Instant;

use argonauts_core::app::UltraColour;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::overview::golden::{Picture, Region, assert_matches, theme};
use crate::render::Clock;

const WIDTH: u16 = 80;
/// Where the folder ends in each header's third row, and the words about it begin.
const FOLDER_END: usize = TEXT_COL + "~/github/argonauts".len();

fn header(workflow: &str, ultra: Option<UltraColour>) -> SessionHeader {
    SessionHeader {
        app: "Argonauts Code".into(),
        version: "0.0.1".into(),
        main_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        workflow: workflow.into(),
        ultra,
        folder: "~/github/argonauts".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        way_back: None,
    }
}

/// The header's three rows beside the mascot, drawn with a still clock.
fn drawn(header: &SessionHeader) -> Buffer {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let lines = header.lines(&ctx);
    let mut buf = Buffer::empty(Rect::new(0, 0, WIDTH, 3));
    for (y, line) in lines[1..4].iter().enumerate() {
        buf.set_line(0, y as u16, line, WIDTH);
    }
    buf
}

#[test]
fn each_mode_draws_its_mascot_beside_the_header_as_its_picture_does() {
    let picture = Picture::screen("mascot_headers");
    let theme = theme();
    let looks = [None, Some(UltraColour::Purple), Some(UltraColour::Blue)];
    let modes = ["captain", "orchestrator", "council"];
    for (look_at, ultra) in looks.into_iter().enumerate() {
        for (mode_at, mode) in modes.into_iter().enumerate() {
            let top = (look_at * 3 + mode_at) * 4;
            let buf = drawn(&header(mode, ultra));
            let hydra = mode == "council";
            // The text column on, and the mascot unless it was redrawn since.
            let from = if hydra { TEXT_COL - 1 } else { 0 };
            let lines = Region {
                rows: top..top + 2,
                columns: from..usize::from(WIDTH),
                at: (0, 0),
            };
            assert_matches(&picture, &buf, &lines, &theme);
            let folder = Region {
                rows: top + 2..top + 3,
                columns: from..FOLDER_END,
                at: (0, 2),
            };
            assert_matches(&picture, &buf, &folder, &theme);
            if hydra {
                let changed: Vec<(usize, usize, char, char)> = (0..mascot::ROWS)
                    .flat_map(|row| {
                        let (buf, picture) = (&buf, &picture);
                        (MARK_COL..MARK_COL + mascot::WIDTH).filter_map(move |column| {
                            let got = buf[(column as u16, row as u16)].symbol().chars().next();
                            let want = picture.rows[top + row][column].ch;
                            let got = got.unwrap_or(' ');
                            (got != want).then_some((row, column, got, want))
                        })
                    })
                    .collect();
                assert_eq!(changed, REDRAWN_HYDRA, "{mode} {ultra:?}");
            }
        }
    }
}

/// The Hydra's cells that its final drawing changed, by row and column of the header, each as
/// the header draws it and as the picture has it: the overwhelmed head slumped from the top
/// rows to the lower ones, and the tail curls up at the right.
const REDRAWN_HYDRA: [(usize, usize, char, char); 10] = [
    (0, 1, ' ', '▗'),
    (0, 2, ' ', '▙'),
    (1, 1, '▗', ' '),
    (1, 2, '▙', '▜'),
    (1, 3, ' ', '▖'),
    (2, 2, '▀', ' '),
    (2, 3, '█', '▜'),
    (2, 7, '█', '▛'),
    (2, 8, '▄', ' '),
    (2, 9, '▘', ' '),
];
