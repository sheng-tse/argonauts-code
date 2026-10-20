//! Argonauts Code's terminal interface, built to look and feel like Claude Code. Finished
//! transcript items print once into the terminal's own scrollback, and only the live region at the
//! bottom redraws. Overlays take the alternate screen and give the conversation back where it was.
//!
//! The app controller talks to it over two channels: [`AppEvent`](argonauts_core::app::AppEvent)s
//! come down, [`UiCommand`](argonauts_core::app::UiCommand)s go up.

// The inline screen: cells, the live bottom region, the terminal, and the loop around them.
pub mod app;
pub mod bottom;
pub mod cell;
pub mod format;
pub mod history;
pub mod keywords;
pub mod kit;
pub mod markdown;
pub mod mascot;
pub mod model;
pub mod render;
pub mod terminal;
pub mod text;
pub mod theme;

// What connects the screen to the app controller.
pub mod a11y;
pub mod bridge;
pub mod commands;
pub mod keymap;
pub mod mentions;
pub mod mods;
pub mod overview;
pub mod prompt_history;
pub mod questions;
pub mod runtime;
pub mod stability;
pub mod state;
pub mod views;

mod options;
#[cfg(test)]
mod picture_dump;

pub use a11y::run_screen_reader;
pub use app::{Flow, Host, Ui, UiAction, run};
pub use cell::{Cell, HistoryCell};
pub use history::{CellId, History};
pub use model::ViewModel;
pub use options::{UiOptions, history_file_in};
pub use render::{Clock, RenderCtx};
pub use runtime::{AppIo, TerminalGone, run_app, run_with, stop_signal};
pub use terminal::InlineTerminal;
pub use theme::Theme;
