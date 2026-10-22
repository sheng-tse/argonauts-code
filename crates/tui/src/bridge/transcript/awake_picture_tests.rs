//! What `/awake` says, drawn from its picture at the picture's width of 118 columns.

use argonauts_core::app::AppEvent;

use super::live_rows_tests::Screen;
use super::picture_check::assert_matches;

const WIDTH: u16 = 118;

#[test]
fn awake_says_what_it_holds_and_what_a_closed_lid_still_does() {
    let mut screen = Screen::new();
    screen.app(AppEvent::Switched {
        text: "Keeping the computer and its screen awake until you quit.".into(),
        undo: Some("/awake off lets them sleep again.".into()),
        caveat: Some("On battery, closing the lid still puts the Mac to sleep.".into()),
    });
    let lines = screen.lines(WIDTH);
    assert_matches(
        "awake",
        &lines,
        include_str!("../../../tests/golden/transcript/awake.txt"),
        include_str!("../../../tests/golden/transcript/awake.html"),
        &Screen::theme(),
        &[],
    );
}
