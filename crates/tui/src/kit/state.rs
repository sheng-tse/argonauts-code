//! One glyph and one color for each state, on every screen. `[ui.states.<state>]` changes a
//! glyph here, and its color through the state's theme role.

use ratatui::text::Span;

use super::Kit;
use crate::text::str_width;
use crate::theme::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Working,
    /// Waiting on other work.
    Waiting,
    /// Waiting on a timer, such as a plan usage reset.
    Timer,
    NeedsYou,
    /// Done, merged, or passed.
    Done,
    /// Failed, crashed, or out.
    Failed,
    /// Stopped or paused.
    Stopped,
}

impl State {
    pub const ALL: [State; 7] = [
        State::Working,
        State::Waiting,
        State::Timer,
        State::NeedsYou,
        State::Done,
        State::Failed,
        State::Stopped,
    ];

    /// The key under `ui.states`.
    pub fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting",
            State::Timer => "timer",
            State::NeedsYou => "needs-you",
            State::Done => "done",
            State::Failed => "failed",
            State::Stopped => "stopped",
        }
    }

    pub fn from_name(name: &str) -> Option<State> {
        State::ALL.into_iter().find(|state| state.name() == name)
    }

    pub fn glyph(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (State::Working, false) => "◐",
            (State::Waiting, false) => "○",
            (State::Timer, false) => "✢",
            (State::NeedsYou, false) => "◆",
            (State::Done, false) => "✔",
            (State::Failed, false) => "✗",
            (State::Stopped, false) => "■",
            (State::Working, true) => ">",
            (State::Waiting, true) => "o",
            (State::Timer, true) => "+",
            (State::NeedsYou, true) => "?",
            (State::Done, true) => "*",
            (State::Failed, true) => "x",
            (State::Stopped, true) => "=",
        }
    }

    pub fn role(self) -> Role {
        match self {
            State::Working => Role::StateWorking,
            State::Waiting => Role::StateWaiting,
            State::Timer => Role::StateTimer,
            State::NeedsYou => Role::StateNeedsYou,
            State::Done => Role::StateDone,
            State::Failed => Role::StateFailed,
            State::Stopped => Role::StateStopped,
        }
    }

    /// The word that goes near the glyph, so color is never the only signal.
    pub fn word(self) -> &'static str {
        match self {
            State::Working => "running",
            State::Waiting => "waiting",
            State::Timer => "waiting",
            State::NeedsYou => "needs you",
            State::Done => "done",
            State::Failed => "failed",
            State::Stopped => "stopped",
        }
    }
}

/// The state glyphs in use, with any `ui.states.<state>.glyph` overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct States {
    glyphs: [Option<String>; 7],
}

impl States {
    /// Draws `state` as `glyph`. A glyph must be one cell, or rows would shift.
    pub fn set_glyph(&mut self, state: State, glyph: &str) -> Result<(), String> {
        if str_width(glyph) != 1 || glyph.chars().any(char::is_control) {
            return Err(format!("{glyph:?} isn't one cell wide"));
        }
        self.glyphs[Self::slot(state)] = Some(glyph.to_owned());
        Ok(())
    }

    pub fn glyph(&self, state: State, ascii: bool) -> &str {
        self.glyphs[Self::slot(state)]
            .as_deref()
            .unwrap_or_else(|| state.glyph(ascii))
    }

    /// The glyph in its state's color.
    pub fn span(&self, kit: &Kit<'_>, state: State) -> Span<'static> {
        let ascii = kit.glyphs.the_mod.is_ascii();
        kit.span(self.glyph(state, ascii).to_owned(), state.role())
    }

    fn slot(state: State) -> usize {
        state as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn each_state_has_one_glyph_one_role_and_a_word() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let states = States::default();
        for state in State::ALL {
            assert_eq!(State::from_name(state.name()), Some(state));
            assert_eq!(str_width(state.glyph(false)), 1, "{state:?}");
            assert!(state.glyph(true).is_ascii());
            assert!(!state.word().is_empty());
            let span = states.span(&kit, state);
            assert_eq!(span.style, theme.style(state.role()));
        }
        assert_eq!(theme.style(State::Done.role()), theme.style(Role::Success));
        assert_eq!(
            theme.style(State::Working.role()),
            theme.style(Role::Question)
        );
    }

    #[test]
    fn a_glyph_override_must_be_one_cell() {
        let mut states = States::default();
        assert!(states.set_glyph(State::Done, "✓").is_ok());
        assert_eq!(states.glyph(State::Done, false), "✓");
        assert!(states.set_glyph(State::Failed, "xx").is_err());
        assert!(states.set_glyph(State::Failed, "修").is_err());
        assert_eq!(states.glyph(State::Failed, false), "✗");
    }
}
