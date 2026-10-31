//! Council as a group conversation inside Claude Code's shell, as Messages draws one: the seats'
//! bubbles on the left with each seat's name dim above and its logo at the tail, yours on the
//! right in blue with "Read by" under it, typing bubbles while a seat writes, replies that quote
//! what they answer, tapbacks ✔ ✘ ? with the logo of the seat that cast each, and Messages' own
//! lines centred.
//!
//! Where the terminal draws pictures under its text (Ghostty and kitty), each bubble is a
//! picture under the words, so selecting and searching still show on them. Elsewhere the talk
//! is a transcript with everything else kept. `ui.bubbles` and `ui.council_view` choose
//! otherwise ([`Look::choose`]).
//!
//! An [`Item`] is what the conversation holds; each look draws it into rows and, for the
//! pictured look, the [`shapes`] under them. [`cell`] prints items into the conversation.

pub mod draw;
pub mod screens;
pub mod talk;
#[cfg(test)]
pub(crate) mod tests;
pub mod work;

use argonauts_core::base::names::SeatId;
use argonauts_core::settings::ui::{Bubbles, CouncilView};
use ratatui::text::Line;

pub use super::base::draw::shapes::Shape;
pub use talk::cell::{SharedItem, Spacing, TalkCell, TypingCell};
pub use talk::conversation::{Conversation, Said, When};
pub use talk::faces::{Face, Faces};
pub use talk::item::{Body, Drawn, Edge, Header, Item, Quote, SeatSays, YouSay};
pub use talk::words::{Mark, Piece, Tone};

use crate::base::theme::Theme;

/// The input's placeholder while a council talks.
pub const PLACEHOLDER: &str =
    "Write to the council, @ a seat, or shift+↑  to pick a message or task";

/// The tip under the working line as a council starts, after the line's own "Tip: ".
pub const TIP: &str = "write to every seat, @ one, or shift+↑  to pick a message or a task";

/// The working line's word while the seats talk, and while they work.
pub const TALKING: &str = "Parleying";
pub const WORKING: &str = "Rowing";
/// The working line once the talk ends and the note keeper writes the plan.
pub const WRITING_PLAN: &str = "Writing the plan";

/// How the talk is drawn on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Look {
    /// Bubbles as pictures under the words.
    Pictures,
    /// The words where the bubbles would put them, with nothing behind them.
    Plain,
    /// Bubbles of block cells.
    Blocks,
    /// Claude Code's transcript.
    Transcript,
}

impl Look {
    /// The look `ui.council_view` and `ui.bubbles` ask for, on a terminal that does or doesn't
    /// draw pictures under its text.
    pub fn choose(view: CouncilView, bubbles: Bubbles, under_text: bool) -> Self {
        match (view, bubbles) {
            (CouncilView::Transcript, _) => Look::Transcript,
            (CouncilView::Messages, Bubbles::Blocks) => Look::Blocks,
            (CouncilView::Messages, Bubbles::Plain) => Look::Plain,
            (CouncilView::Messages, Bubbles::Auto) if under_text => Look::Pictures,
            (CouncilView::Messages, Bubbles::Auto) => Look::Transcript,
        }
    }

    /// Whether each item brings its own room above and below, as pictured bubbles do, so no
    /// blank row is added between two.
    pub fn bubbles_bring_room(self) -> bool {
        matches!(self, Look::Pictures | Look::Plain)
    }

    pub fn draws_pictures(self) -> bool {
        self == Look::Pictures
    }

    /// The typing row: who writes now and why, with the shapes under it in the pictured look.
    pub fn typing(
        self,
        writers: &[SeatId],
        frame: usize,
        reason: &[Piece],
        width: usize,
        faces: &Faces,
        theme: &Theme,
    ) -> (Line<'static>, Vec<Shape>) {
        match self {
            Look::Pictures | Look::Plain => {
                let (line, shapes) =
                    draw::bubbles::typing(writers, frame, reason, width, faces, theme);
                (
                    line,
                    if self == Look::Pictures {
                        shapes
                    } else {
                        Vec::new()
                    },
                )
            }
            Look::Blocks => (
                draw::blocks::typing(writers, frame, reason, width, faces, theme),
                Vec::new(),
            ),
            Look::Transcript => (
                draw::transcript::typing(writers, frame, reason, width, faces, theme),
                Vec::new(),
            ),
        }
    }
}
