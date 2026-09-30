//! Colors and text styles, with a monochrome mode for `NO_COLOR` and dumb terminals.

use std::ffi::OsStr;

use ratatui::style::{Color, Modifier, Style};

use crate::model::State;
use crate::tools::DiffSign;

// Okabe-Ito colors, which stay distinct for common kinds of color blindness, as xterm-256 indexes.
const BLUE: Color = Color::Indexed(74);
const ORANGE: Color = Color::Indexed(214);
const GREEN: Color = Color::Indexed(36);
const VERMILION: Color = Color::Indexed(166);
const GREY: Color = Color::Indexed(245);

/// Follows no-color.org. Any non-empty `NO_COLOR` turns color off, and so does `TERM=dumb`.
pub fn color_allowed(no_color: Option<&OsStr>, term: Option<&OsStr>) -> bool {
    let no_color_set = no_color.is_some_and(|v| !v.is_empty());
    let dumb = term.is_some_and(|t| t == "dumb");
    !(no_color_set || dumb)
}

pub fn color_from_env() -> bool {
    color_allowed(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    color: bool,
}

impl Theme {
    pub(crate) fn new(color: bool) -> Self {
        Self { color }
    }

    fn fg(&self, color: Color) -> Style {
        if self.color {
            Style::new().fg(color)
        } else {
            Style::new()
        }
    }

    pub(crate) fn state(&self, state: State) -> Style {
        match state {
            State::NeedsYou | State::Warning => self.fg(ORANGE).add_modifier(Modifier::BOLD),
            State::Working => self.fg(BLUE),
            State::Waiting | State::Stopped | State::Info => self.fg(GREY),
            State::Done => self.fg(GREEN),
            State::Failed => self.fg(VERMILION).add_modifier(Modifier::BOLD),
        }
    }

    pub(crate) fn diff(&self, sign: DiffSign) -> Style {
        match sign {
            DiffSign::Added => self.fg(GREEN),
            DiffSign::Removed => self.fg(VERMILION),
        }
    }

    pub(crate) fn plain(&self) -> Style {
        Style::new()
    }

    pub(crate) fn bold(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    pub(crate) fn dim(&self) -> Style {
        if self.color {
            Style::new().fg(GREY)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        }
    }

    pub(crate) fn key(&self) -> Style {
        self.fg(ORANGE).add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_and_dumb_terminals_turn_color_off() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert!(color_allowed(None, os("xterm-256color")));
        assert!(color_allowed(os(""), os("xterm")));
        assert!(!color_allowed(os("1"), os("xterm")));
        assert!(!color_allowed(None, os("dumb")));
    }

    #[test]
    fn monochrome_theme_sets_no_colors() {
        let theme = Theme::new(false);
        let states = [
            State::NeedsYou,
            State::Working,
            State::Waiting,
            State::Done,
            State::Failed,
            State::Stopped,
            State::Warning,
            State::Info,
        ];
        for state in states {
            assert_eq!(theme.state(state).fg, None, "{state:?}");
        }
        assert_eq!(theme.diff(DiffSign::Added).fg, None);
        assert_eq!(theme.dim().fg, None);
        assert_eq!(theme.key().fg, None);
    }

    #[test]
    fn color_theme_marks_states() {
        let theme = Theme::new(true);
        assert_eq!(theme.state(State::Failed).fg, Some(VERMILION));
        assert_eq!(theme.state(State::Done).fg, Some(GREEN));
        assert_eq!(theme.diff(DiffSign::Removed).fg, Some(VERMILION));
    }
}
