//! Argonauts Code's terminal interface, built to look and feel like Claude Code. Finished
//! transcript items print once into the terminal's own scrollback, and only the live region at the
//! bottom redraws. Overlays take the alternate screen and give the conversation back where it was.
//!
//! The app controller talks to it over two channels: [`AppEvent`](argonauts_core::app::AppEvent)s
//! come down, [`UiCommand`](argonauts_core::app::UiCommand)s go up.

// The parts the screen is built from.
pub mod base;
pub mod bottom;
pub mod cells;
pub mod council;
pub mod frame;
pub mod panels;
pub mod world;

// The app around them: the loop, the terminal, the screens and what connects them to the app
// controller.
pub mod a11y;
pub mod app;
pub mod bridge;
pub mod config;
pub mod mods;
pub mod questions;
pub mod runtime;
pub mod terminal;
pub mod views;

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

pub use a11y::run_screen_reader;
pub use app::model::ViewModel;
pub use app::options::{UiOptions, history_file_in};
pub use app::{Flow, Host, Ui, UiAction, run};
pub use base::draw::render::{Clock, RenderCtx};
pub use base::theme::Theme;
pub use cells::cell::history::{CellId, History};
pub use cells::cell::{Cell, HistoryCell};
pub use runtime::{AppIo, TerminalGone, run_app, run_with, stop_signal};
pub use terminal::InlineTerminal;
