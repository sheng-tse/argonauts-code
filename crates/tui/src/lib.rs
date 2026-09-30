//! Terminal interface for Argonauts, with a live full screen and a plain line renderer.
//! Both strip terminal control sequences from agent text before showing it.

mod app;
mod channel;
mod format;
mod interactive;
mod model;
mod plain;
mod sanitize;
mod terminal;
mod text;
mod theme;
mod tools;
mod view;

#[cfg(test)]
mod testing;

use std::io::IsTerminal;
use std::path::PathBuf;

use argonauts_core::settings::ui::{ColorChoice, UiSettings};
use argonauts_core::{KeyScheme, UiConfig, Verbosity};

pub use channel::{MergePrompt, UiCommand, UiEvent, ask_merge};
pub use interactive::{AppIo, run_app, run_screen_reader};
pub use plain::{PlainRenderer, run_plain, run_plain_stdio};
pub use sanitize::{clean, clean_line};
pub use terminal::run_terminal;
pub use theme::{color_allowed, color_from_env};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UiOptions {
    pub verbosity: Verbosity,
    pub keys: KeyScheme,
    pub color: bool,
    /// Ring the terminal bell when a card needs the user.
    pub bell: bool,
    /// Shown as `~` in paths.
    pub home_dir: Option<PathBuf>,
}

impl UiOptions {
    /// Options from the `[ui]` config, with color decided by the environment.
    pub fn from_config(ui: &UiConfig) -> Self {
        Self {
            verbosity: ui.verbosity,
            keys: ui.keys,
            color: color_from_env(),
            bell: true,
            home_dir: dirs::home_dir(),
        }
    }
}

impl UiOptions {
    pub fn from_settings(ui: &UiSettings) -> Self {
        let color = match ui.color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => color_from_env(),
        };
        Self {
            verbosity: ui.verbosity,
            keys: ui.keys,
            color,
            bell: ui.bell,
            home_dir: dirs::home_dir(),
        }
    }
}

impl Default for UiOptions {
    fn default() -> Self {
        Self::from_config(&UiConfig::default())
    }
}

pub fn wants_plain(requested: bool) -> bool {
    let dumb = std::env::var_os("TERM").is_some_and(|t| t == "dumb");
    requested || dumb || !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_follow_the_config() {
        let ui = UiConfig {
            verbosity: Verbosity::Quiet,
            keys: KeyScheme::Vim,
            plain: false,
        };
        let options = UiOptions::from_config(&ui);
        assert_eq!(options.verbosity, Verbosity::Quiet);
        assert_eq!(options.keys, KeyScheme::Vim);
        assert!(options.bell);
    }

    #[test]
    fn options_follow_the_new_settings() {
        let ui = UiSettings {
            verbosity: Verbosity::Full,
            keys: KeyScheme::Vim,
            color: ColorChoice::Never,
            bell: false,
            ..UiSettings::default()
        };
        let options = UiOptions::from_settings(&ui);
        assert_eq!(options.verbosity, Verbosity::Full);
        assert_eq!(options.keys, KeyScheme::Vim);
        assert!(!options.color && !options.bell);
    }

    #[test]
    fn plain_when_requested() {
        assert!(wants_plain(true));
    }
}
