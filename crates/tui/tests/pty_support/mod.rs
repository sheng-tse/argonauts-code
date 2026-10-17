//! What the pty tests share: the test driver binary in a pseudo-terminal, scripts of app events,
//! builders for the events themselves, and checks on what the screen shows.

// Each test file uses its own part of these helpers.
#![allow(dead_code, unused_imports)]

mod conversation;
mod driver;
mod run;
mod screen;
mod script;
mod views;

pub use argonauts_testkit::{Key, Screen};

/// The mark items and agents start with: Claude Code's `⏺` on macOS, `●` elsewhere.
pub const BULLET: &str = if cfg!(target_os = "macos") {
    "⏺"
} else {
    "●"
};
pub use conversation::*;
pub use driver::{COLS, Launch, ROWS, TestDriver, WAIT, command_type};
pub use run::*;
pub use screen::*;
pub use script::Script;
pub use views::*;

/// A question works only the arrows for a moment after it shows, so a key meant for something
/// else never answers it. Waits that out, as someone reading the question would.
pub fn past_the_question_guard() {
    std::thread::sleep(
        argonauts_tui::bottom::QUESTION_GUARD + std::time::Duration::from_millis(100),
    );
}
