//! One glyph and one color for each state, in every view. A view asks here
//! what a row's or a session's state is and how it draws, rather than pairing a glyph with a
//! color itself, so working is `◐` in the working color in the workflow view, the docked
//! agent, the opened agent, and the row at the bottom alike.
//!
//! The glyphs and the color roles are the kit's ([`crate::mods::State`]), so a theme that
//! moves a state's role moves it everywhere at once.

use argonauts_core::app::views::RowStatus;
use argonauts_core::runs::events::session::SubagentStatus;
use ratatui::text::Span;

use crate::base::kit::State;
use crate::base::theme::{Role, Theme};
use crate::frame::state::SessionStatus;

/// The state's glyph for this theme's glyph set, and the role it draws in.
pub fn mark(state: State, theme: &Theme) -> (&'static str, Role) {
    (state.glyph(theme.glyphs.is_ascii()), state.role())
}

/// The glyph in its state's color.
pub fn span(state: State, theme: &Theme) -> Span<'static> {
    let (glyph, role) = mark(state, theme);
    Span::styled(glyph, theme.style(role))
}

/// A row of a workflow. Queued and skipped rows wait, as one not started does.
pub fn of_row(status: RowStatus) -> State {
    match status {
        RowStatus::Running => State::Working,
        RowStatus::NeedsYou => State::NeedsYou,
        RowStatus::Done => State::Done,
        RowStatus::Failed => State::Failed,
        RowStatus::Paused => State::Stopped,
        RowStatus::Waiting | RowStatus::Queued | RowStatus::Skipped => State::Waiting,
    }
}

/// A session the screen follows. One that sleeps waits on a timer, such as a usage reset.
pub fn of_session(status: SessionStatus) -> State {
    match status {
        SessionStatus::Starting | SessionStatus::Working => State::Working,
        SessionStatus::Waiting => State::Waiting,
        SessionStatus::Sleeping => State::Timer,
        SessionStatus::Done => State::Done,
        SessionStatus::Failed => State::Failed,
        SessionStatus::Stopped => State::Stopped,
    }
}

/// An agent's own subagent, as its CLI reports it.
pub fn of_subagent(status: SubagentStatus) -> State {
    match status {
        SubagentStatus::Running => State::Working,
        SubagentStatus::Done => State::Done,
        SubagentStatus::Failed => State::Failed,
        SubagentStatus::Started | SubagentStatus::Unknown => State::Waiting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::text::str_width;
    use crate::base::theme::Glyphs;

    #[test]
    fn every_state_is_one_cell_in_its_own_role_in_both_glyph_sets() {
        for glyphs in [Glyphs::UNICODE, Glyphs::ASCII] {
            let mut theme = Theme::dark();
            theme.glyphs = glyphs;
            for state in State::ALL {
                let (glyph, role) = mark(state, &theme);
                assert_eq!(str_width(glyph), 1, "{state:?}");
                assert_eq!(role, state.role());
                assert_eq!(glyph.is_ascii(), theme.glyphs.is_ascii(), "{state:?}");
            }
        }
    }

    #[test]
    fn rows_sessions_and_subagents_agree_on_what_working_and_done_are() {
        assert_eq!(of_row(RowStatus::Running), State::Working);
        assert_eq!(of_session(SessionStatus::Working), State::Working);
        assert_eq!(of_session(SessionStatus::Starting), State::Working);
        assert_eq!(of_subagent(SubagentStatus::Running), State::Working);
        assert_eq!(of_row(RowStatus::Done), State::Done);
        assert_eq!(of_session(SessionStatus::Done), State::Done);
        assert_eq!(of_subagent(SubagentStatus::Done), State::Done);
        assert_eq!(of_row(RowStatus::Queued), State::Waiting);
        assert_eq!(of_session(SessionStatus::Sleeping), State::Timer);
        assert_eq!(of_session(SessionStatus::Stopped), State::Stopped);
    }
}
