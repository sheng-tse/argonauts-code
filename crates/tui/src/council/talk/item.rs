//! What the council's conversation holds, as plain data each look draws its own way: a header,
//! centred lines, the seats' messages and yours.

use argonauts_core::base::names::SeatId;
use ratatui::text::Line;

use super::super::work::plan::{Columns, PlanTask, TaskWork, who_does_what};
use super::super::{
    Look,
    draw::{blocks, bubbles, transcript},
};
use super::faces::Faces;
use super::words::{Mark, Piece, Tone};
use crate::base::draw::shapes::Shape;
use crate::base::theme::Theme;

/// One thing in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// The council's own header: its seats' badges, its name and what it is.
    Header(Header),
    /// A line centred across the screen, as Messages writes its own: a time, who moved, what
    /// was agreed.
    Line(Vec<Piece>),
    /// Messages from one seat in a row: the name over the first, the tail on the last.
    Seat(Vec<SeatSays>),
    You(YouSay),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub seats: Vec<SeatId>,
    pub title: String,
    pub subtitle: String,
}

/// A seat's message as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatSays {
    pub seat: SeatId,
    pub body: Body,
    /// Rows drawn on an inset inside the bubble, under its words: a proposal, a plan.
    pub inset: Option<Inset>,
    /// Dim words right after the name: " · keeps the notes".
    pub note: Option<String>,
    /// Dim words at the name row's right end: what the seat read and ran.
    pub right: Option<String>,
    pub reply: Option<Quote>,
    pub taps: Vec<(SeatId, Mark)>,
    /// Picked with shift+↑ or from its run row: the bubble gets a ring.
    pub picked: bool,
    /// Dim words at the first row's right end, on the one picked task: "enter to open".
    pub hint: Option<String>,
    /// The fewest cells the words take, so tasks line up one under another.
    pub min_width: usize,
    /// A dim line under the bubble.
    pub below: Option<Vec<Piece>>,
    /// Held back before any seat read it: the bubble faded, its note amber.
    pub held: bool,
}

impl SeatSays {
    pub fn new(seat: SeatId, body: Body) -> Self {
        Self {
            seat,
            body,
            inset: None,
            note: None,
            right: None,
            reply: None,
            taps: Vec::new(),
            picked: false,
            hint: None,
            min_width: 0,
            below: None,
            held: false,
        }
    }
}

/// A message's words: paragraphs to wrap, of which the first `shown` show and the rest fold,
/// or rows laid out already, such as a task's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Words {
        paragraphs: Vec<Vec<Piece>>,
        shown: Option<usize>,
    },
    Rows(Vec<Vec<Piece>>),
    /// A task's task while the work runs.
    Task(TaskWork),
}

/// What an inset task holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inset {
    Rows(Vec<Vec<Piece>>),
    /// The note keeper's proposal: its rows, then your ways, which bind, wrapped way by way to
    /// the bubble under "Yours, binding: ".
    Proposal {
        rows: Vec<Vec<Piece>>,
        yours: Vec<Vec<Piece>>,
    },
    /// Who does what, with the rule of thumb the note keeper went by.
    Plan {
        tasks: Vec<PlanTask>,
        columns: Columns,
        rule: Option<String>,
    },
}

impl Inset {
    /// Its rows in a bubble whose words take at most `room` cells.
    pub fn rows(&self, room: usize, faces: &Faces, theme: &Theme) -> Vec<Vec<Piece>> {
        match self {
            Inset::Rows(rows) => rows.clone(),
            Inset::Proposal { rows, yours } => {
                let mut all = rows.clone();
                all.extend(binding(yours, room, faces, theme));
                all
            }
            Inset::Plan {
                tasks,
                columns,
                rule,
            } => who_does_what(
                tasks,
                *columns,
                rule.as_deref(),
                room.min(PLAN_ROOM),
                faces,
                theme,
            ),
        }
    }
}

/// The widest the plan's task gets.
const PLAN_ROOM: usize = 85;

/// Your ways as the proposal lists them: "❯  Yours, binding: " and then each way, " · " between
/// two on a row. A way that doesn't fit starts the next row, under the first way.
fn binding(yours: &[Vec<Piece>], room: usize, faces: &Faces, theme: &Theme) -> Vec<Vec<Piece>> {
    const HANG: &str = "   ";
    let Some((first, rest)) = yours.split_first() else {
        return Vec::new();
    };
    let mut rows = vec![vec![
        Piece::tone("❯  ", Tone::Yours),
        Piece::tone("Yours, binding: ", Tone::Yours),
    ]];
    let mut used = super::words::width(&rows[0], faces, theme);
    for (i, way) in std::iter::once(first).chain(rest).enumerate() {
        let cells = super::words::width(way, faces, theme);
        let row = rows.last_mut().expect("a row to fill");
        if i > 0 && used + 3 + cells <= room {
            row.push(Piece::text(" · "));
            used += 3;
        } else if i > 0 {
            rows.push(vec![Piece::text(HANG)]);
            used = HANG.len();
        }
        let row = rows.last_mut().expect("a row to fill");
        row.extend(way.iter().cloned());
        used += cells;
    }
    rows
}

/// A message's task rows in a bubble whose words take at most `room` cells.
pub fn inset_rows(says: &SeatSays, room: usize, faces: &Faces, theme: &Theme) -> Vec<Vec<Piece>> {
    says.inset
        .as_ref()
        .map_or_else(Vec::new, |inset| inset.rows(room, faces, theme))
}

impl Default for Body {
    fn default() -> Self {
        Body::Rows(Vec::new())
    }
}

impl Body {
    pub fn words(paragraphs: Vec<Vec<Piece>>) -> Self {
        Body::Words {
            paragraphs,
            shown: None,
        }
    }
}

/// The words a reply answers, faded above it, and how many replies that message has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// The seat whose message it answers, or `None` for one of yours.
    pub seat: Option<SeatId>,
    pub words: String,
    pub replies: u32,
}

/// One of your messages.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct YouSay {
    pub paragraphs: Vec<Vec<Piece>>,
    pub reply: Option<Quote>,
    /// "Read by" and the badges of the seats that read it, then anything said after them.
    pub read_by: Vec<SeatId>,
    pub read_after: Vec<Piece>,
    pub picked: bool,
}

/// Where an item's first and last rows leave the eye, which decides the blank rows between two
/// items when bubbles bring their own room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Blank,
    Text,
    /// A seat's name over its bubble.
    Name,
    /// A centred line.
    Centred,
}

impl Edge {
    fn has_text(self) -> bool {
        self != Edge::Blank
    }
}

/// An item drawn: its rows, the pictures under them, and its edges.
#[derive(Debug, Clone, Default)]
pub struct Drawn {
    pub rows: Vec<Line<'static>>,
    pub shapes: Vec<Shape>,
    pub first: Option<Edge>,
    pub last: Option<Edge>,
}

impl Item {
    /// Rings it as the one picked, or takes the ring off; only a message or task can be.
    pub fn set_picked(&mut self, picked: bool) {
        match self {
            Item::Seat(run) => {
                for says in run {
                    says.picked = picked;
                }
            }
            Item::You(you) => you.picked = picked,
            _ => {}
        }
    }

    /// A task's box drawn again as its work stands, its ring and words kept.
    pub fn set_work(&mut self, work: TaskWork) {
        if let Item::Seat(run) = self
            && let Some(says) = run.first_mut()
        {
            says.body = Body::Task(work);
        }
    }

    /// The dim words at the right end of its first row, such as "enter to open".
    pub fn set_hint(&mut self, hint: Option<String>) {
        if let Item::Seat(run) = self
            && let Some(says) = run.first_mut()
        {
            says.hint = hint;
        }
    }

    pub fn draw(&self, look: Look, width: usize, faces: &Faces, theme: &Theme) -> Drawn {
        match look {
            Look::Pictures | Look::Plain => bubbles::draw(self, width, faces, theme),
            Look::Blocks => blocks::draw(self, width, faces, theme),
            Look::Transcript => transcript::draw(self, width, faces, theme),
        }
    }

    /// Its edges in the bubble looks, which don't depend on the width.
    pub fn edges(&self) -> (Edge, Edge) {
        match self {
            Item::Header(_) => (Edge::Text, Edge::Text),
            Item::Line(_) => (Edge::Centred, Edge::Centred),
            Item::Seat(run) => {
                let last = match run.last().and_then(|says| says.below.as_ref()) {
                    Some(_) => Edge::Text,
                    None => Edge::Blank,
                };
                (Edge::Name, last)
            }
            Item::You(you) => (
                if you.reply.is_some() {
                    Edge::Text
                } else {
                    Edge::Blank
                },
                if you.read_by.is_empty() && you.read_after.is_empty() {
                    Edge::Blank
                } else {
                    Edge::Text
                },
            ),
        }
    }
}

/// Whether a blank row sits between an item that ends at `last` and the next, which starts at
/// `first`, when bubbles bring their own room: a name or a centred line keeps one blank row
/// from text above it, and a centred line from text under it.
pub fn blank_between(last: Edge, first: Edge) -> bool {
    (matches!(first, Edge::Name | Edge::Centred) && last.has_text())
        || (last == Edge::Centred && first.has_text())
}
