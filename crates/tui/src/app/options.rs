use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::settings::ui::{ColorChoice, UiSettings};
use argonauts_core::{KeyScheme, Verbosity};

use crate::base::theme::{ColorEnv, ColorLevel};

/// How the screen starts. The app builds it with [`UiOptions::from_settings`]; every `[ui]`
/// value can change later through `AppEvent::UiSettings`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct UiOptions {
    pub verbosity: Verbosity,
    pub keys: KeyScheme,
    pub color: bool,
    /// Ring the terminal bell when a question needs the user.
    pub bell: bool,
    /// Shown as `~` in paths.
    pub home_dir: Option<PathBuf>,
    /// The whole `[ui]` section in effect at start.
    pub settings: UiSettings,
    /// `--screen-reader`, `ui.screen_reader`, or `ARGONAUTS_SCREEN_READER=1`.
    pub screen_reader: bool,
    /// This repo's prompt history, `~/.argonauts/state/<repo>/history.jsonl`, which the app
    /// sets. `None` keeps history in memory only, as every test does.
    pub history_file: Option<PathBuf>,
    /// `workflows.ultracode_trigger` at start, which the app sets: the word `ultracode` in a
    /// prompt asks for a workflow.
    pub ultracode_trigger: bool,
    /// The first start's greeting, which the app sets on that start alone. The screen prints
    /// it before anything else.
    pub greeting: Option<crate::cells::welcome::Greeting>,
    /// How long a first Ctrl+C, Ctrl+D or Esc waits for the second: [`QUIT_WINDOW`] for people.
    /// The pty tests' driver widens it, since a busy test machine can stall longer than that
    /// between two keys it sends.
    ///
    /// [`QUIT_WINDOW`]: crate::app::QUIT_WINDOW
    pub double_press: Duration,
    /// `behaviors.view = "world"`, which the app sets: a terminal no variable names is asked
    /// whether it takes the kitty protocol's pictures, and gets the world if it answers.
    pub ask_for_pictures: bool,
}

impl UiOptions {
    pub fn from_settings(ui: &UiSettings) -> Self {
        let color = match ui.color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => ColorLevel::detect(&ColorEnv::from_process()) != ColorLevel::None,
        };
        Self {
            verbosity: ui.verbosity,
            keys: ui.keys,
            color,
            bell: ui.bell,
            home_dir: dirs::home_dir(),
            screen_reader: ui.screen_reader || screen_reader_from_env(),
            settings: ui.clone(),
            history_file: None,
            ultracode_trigger: true,
            greeting: None,
            double_press: crate::app::QUIT_WINDOW,
            ask_for_pictures: false,
        }
    }
}

/// The `[ui]` section's own defaults, such as `full` verbosity.
impl Default for UiOptions {
    fn default() -> Self {
        Self::from_settings(&UiSettings::default())
    }
}

fn screen_reader_from_env() -> bool {
    std::env::var_os("ARGONAUTS_SCREEN_READER").is_some_and(|v| v == "1")
}

/// The prompt history of the repo in `state_dir`, the folder [`argonauts_core::base::paths`] gives
/// it.
pub fn history_file_in(state_dir: &Path) -> PathBuf {
    state_dir.join(HISTORY_FILE)
}

const HISTORY_FILE: &str = "history.jsonl";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_ui_sections() {
        let options = UiOptions::default();
        assert_eq!(options.verbosity, Verbosity::Full);
        assert_eq!(options.settings, UiSettings::default());
        assert_eq!(
            options.history_file, None,
            "only the app keeps history on disk"
        );
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
        assert_eq!(options.settings, ui);
    }
}
