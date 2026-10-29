//! The line a start says once after a background update failed, drawn from its picture at
//! the picture's width of 118 columns.

use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;
use argonauts_core::log::COULDNT_UPDATE_TO;

use super::live_rows_tests::Screen;
use super::picture_check::assert_matches;

const WIDTH: u16 = 118;

#[test]
fn a_failed_update_warns_with_its_mark_and_offers_the_command_as_code() {
    let mut screen = Screen::new();
    screen.app(AppEvent::Notice {
        level: NoteLevel::Warn,
        text: format!(
            "{COULDNT_UPDATE_TO}0.1.1: the download stopped at 61% · argonauts self-update"
        ),
    });
    let lines = screen.lines(WIDTH);
    assert_matches(
        "update_failed",
        &lines,
        include_str!("../../../tests/golden/transcript/update_failed.txt"),
        include_str!("../../../tests/golden/transcript/update_failed.html"),
        &Screen::theme(),
        &[],
    );
}
