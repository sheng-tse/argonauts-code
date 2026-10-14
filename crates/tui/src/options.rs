use std::path::{Path, PathBuf};

use argonauts_core::settings::ui::{ColorChoice, UiSettings};
use argonauts_core::{KeyScheme, Verbosity};

use crate::theme::{ColorEnv, ColorLevel};

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

/// The prompt history of the repo in `state_dir`, the folder [`argonauts_core::paths`] gives it.
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
