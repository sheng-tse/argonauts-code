//! What `/awake` says, drawn from its picture at the picture's width of 118 columns.

use argonauts_core::app::AppEvent;

use super::live_rows_tests::Screen;
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 118;

#[test]
fn awake_says_what_it_holds_and_what_a_closed_lid_still_does() {
    let mut screen = Screen::new();
    screen.app(AppEvent::Switched {
        text: "Keeping the computer and its screen awake until you quit.".into(),
        undo: Some("/awake again lets them sleep.".into()),
        caveat: Some("On battery, closing the lid still puts it to sleep.".into()),
    });
    let lines = screen.lines(WIDTH);
    assert_matches(
        "awake",
        &lines,
        include_str!("../../../../tests/golden/transcript/awake.txt"),
        include_str!("../../../../tests/golden/transcript/awake.html"),
        &Screen::theme(),
    );
}

#[test]
fn awake_again_says_what_still_holds_while_agents_work() {
    let mut screen = Screen::new();
    screen.app(AppEvent::Switched {
        text: "The computer and its screen may sleep again. While agents work, the computer \
               still stays awake."
            .into(),
        undo: Some("/awake turns it on.".into()),
        caveat: None,
    });
    let lines = screen.lines(WIDTH);
    assert_matches(
        "awake_off",
        &lines,
        include_str!("../../../../tests/golden/transcript/awake_off.txt"),
        include_str!("../../../../tests/golden/transcript/awake_off.html"),
        &Screen::theme(),
    );
}
