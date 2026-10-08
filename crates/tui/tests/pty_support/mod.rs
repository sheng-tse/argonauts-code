//! What the pty tests share: the harness binary in a pseudo-terminal, scripts of app events,
//! builders for the events themselves, and checks on what the screen shows.

// Each test file uses its own part of these helpers.
#![allow(dead_code, unused_imports)]

mod chat;
mod harness;
mod run;
mod screen;
mod script;
mod views;

pub use argonauts_testkit::{Key, Screen};
pub use chat::*;
pub use harness::{COLS, Harness, Launch, ROWS, WAIT, command_type};
pub use run::*;
pub use screen::*;
pub use script::Script;
pub use views::*;

/// A card that took the keys by itself works only the arrows for a moment, so a key meant for
/// something else never answers it. Waits that out, as someone reading the card would.
pub fn past_the_card_guard() {
    std::thread::sleep(argonauts_tui::bottom::CARD_GUARD + std::time::Duration::from_millis(100));
}
