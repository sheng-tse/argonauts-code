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

mod blocks;
mod bubbles;
pub mod cell;
pub mod checks;
pub mod conversation;
pub mod edits;
pub mod faces;
pub mod item;
pub mod pick;
pub mod picture;
pub mod place;
pub mod plan;
pub mod reply;
mod row;
pub mod session_view;
pub mod shapes;
pub mod task_header;
pub mod thread;
mod transcript;
pub mod views;
pub mod words;

#[cfg(test)]
mod approved_tests;
#[cfg(test)]
pub(crate) mod golden_tests;
#[cfg(test)]
pub(crate) mod story;
#[cfg(test)]
mod work_rows_golden_tests;

use argonauts_core::names::SeatId;
use argonauts_core::settings::ui::{Bubbles, CouncilView};
use ratatui::text::Line;

pub use cell::{SharedItem, Spacing, TalkCell, TypingCell};
pub use conversation::{Conversation, Said, When};
pub use faces::{Face, Faces};
pub use item::{Body, Drawn, Edge, Header, Item, Quote, SeatSays, YouSay};
pub use shapes::Shape;
pub use words::{Mark, Piece, Tone};

use crate::theme::Theme;

/// The input's placeholder while a council talks.
pub const PLACEHOLDER: &str =
    "Write to the council, @ a seat, or shift+↑  to pick a message or task";

/// The tip under the working line as a council starts, after the line's own "Tip: ".
pub const TIP: &str = "write to every seat, @ one, or shift+↑  to pick a message or a task";

/// The working line's word while the seats talk, and while they work.
pub const TALKING: &str = "Parleying";
pub const WORKING: &str = "Rowing";

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
                let (line, shapes) = bubbles::typing(writers, frame, reason, width, faces, theme);
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
                blocks::typing(writers, frame, reason, width, faces, theme),
                Vec::new(),
            ),
            Look::Transcript => (
                transcript::typing(writers, frame, reason, width, faces, theme),
                Vec::new(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_setting_picks_its_look() {
        use Bubbles::*;
        use CouncilView::*;
        assert_eq!(Look::choose(Messages, Auto, true), Look::Pictures);
        assert_eq!(Look::choose(Messages, Auto, false), Look::Transcript);
        assert_eq!(Look::choose(Messages, Blocks, false), Look::Blocks);
        assert_eq!(Look::choose(Messages, Plain, true), Look::Plain);
        assert_eq!(Look::choose(Transcript, Auto, true), Look::Transcript);
    }
}
