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
