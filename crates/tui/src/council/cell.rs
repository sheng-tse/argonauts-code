//! The council's items as cells of the conversation. Each prints once as it lands, at the
//! screen's width, and the typing row redraws in the live region above the working line.

use argonauts_core::names::{MessageId, SeatId};
use ratatui::text::Line;

use super::Look;
use super::faces::Faces;
use std::sync::{Arc, Mutex};

use super::item::{Drawn, Edge, Item, blank_between};
use super::picture::{self, Picture};
use super::shapes::Shape;
use super::words::Piece;
use crate::cell::HistoryCell;
use crate::render::RenderCtx;

/// Keeps the blank rows between the council's items right: Claude Code's one blank row in the
/// transcript and block looks, and in the pictured looks only where a name or a centred line
/// needs room from text next to it, since a bubble brings its own.
#[derive(Debug, Clone, Default)]
pub struct Spacing {
    last: Option<Edge>,
}

impl Spacing {
    /// Places `item` after the last one: whether it follows it with no blank row of the
    /// conversation's own, and whether it starts with a blank row of its own.
    pub fn place(&mut self, item: &Item, look: Look) -> (bool, bool) {
        let (first, last) = item.edges();
        let placed = match (look.bubbles_bring_room(), self.last) {
            (true, Some(before)) => (true, blank_between(before, first)),
            _ => (false, false),
        };
        self.last = Some(last);
        placed
    }

    /// Something else printed in between, so the next item starts as any cell does.
    pub fn interrupt(&mut self) {
        self.last = None;
    }
}

/// An item shared between the cell that draws it and the council that changes it, so a message
/// still live shows its tapbacks and readers as they come.
pub type SharedItem = Arc<Mutex<Item>>;

/// One item of the council's conversation.
#[derive(Debug, Clone)]
pub struct TalkCell {
    item: SharedItem,
    look: Look,
    faces: Faces,
    /// Right under the item before, with no blank row of the conversation's own.
    follows: bool,
    /// A blank row of its own first.
    blank: bool,
    /// The message it draws, which can be picked after it printed.
    message: Option<MessageId>,
}

impl TalkCell {
    pub fn new(item: Item, look: Look, faces: Faces, spacing: &mut Spacing) -> Self {
        Self::shared(Arc::new(Mutex::new(item)), look, faces, spacing)
    }

    pub fn shared(item: SharedItem, look: Look, faces: Faces, spacing: &mut Spacing) -> Self {
        let (follows, blank) = match item.lock() {
            Ok(item) => spacing.place(&item, look),
            Err(_) => (false, false),
        };
        Self {
            item,
            look,
            faces,
            follows,
            blank,
            message: None,
        }
    }

    /// It draws message `id`, so its picture comes with a picked copy.
    pub fn for_message(mut self, id: MessageId) -> Self {
        self.message = Some(id);
        self
    }

    /// Its shapes in one picture for a terminal whose cells are `cell` pixels, placed from its
    /// first row. `None` where the look draws no pictures.
    pub fn picture(&self, ctx: &RenderCtx<'_>, cell: (u16, u16)) -> Option<Picture> {
        let brightness = ctx.theme.brightness;
        let Some(id) = &self.message else {
            return picture::draw(&self.shapes(ctx), cell, brightness);
        };
        // A message prints once, so its picture comes unpicked, with the picked one beside it.
        let item = self.item.lock().ok()?.clone();
        let as_picked = |picked: bool| {
            let mut item = item.clone();
            item.set_picked(picked);
            picture::draw(&self.shapes_of(&item, ctx), cell, brightness)
        };
        let mut picture = as_picked(false)?;
        picture.picked = as_picked(true).map(|ringed| (id.clone(), Box::new(ringed)));
        Some(picture)
    }

    fn drawn(&self, ctx: &RenderCtx<'_>) -> Drawn {
        match self.item.lock() {
            Ok(item) => self.drawn_of(&item, ctx),
            Err(_) => Drawn::default(),
        }
    }

    fn drawn_of(&self, item: &Item, ctx: &RenderCtx<'_>) -> Drawn {
        item.draw(self.look, usize::from(ctx.width), &self.faces, ctx.theme)
    }

    /// `item`'s pictures under its rows, measured from its first row.
    fn shapes_of(&self, item: &Item, ctx: &RenderCtx<'_>) -> Vec<Shape> {
        if !self.look.draws_pictures() {
            return Vec::new();
        }
        let above = usize::from(self.blank);
        let drawn = self.drawn_of(item, ctx);
        drawn
            .shapes
            .into_iter()
            .map(|shape| shape.moved(above))
            .collect()
    }

    /// The pictures under its rows, measured from its first row, at `width`.
    pub fn shapes(&self, ctx: &RenderCtx<'_>) -> Vec<Shape> {
        match self.item.lock() {
            Ok(item) => self.shapes_of(&item, ctx),
            Err(_) => Vec::new(),
        }
    }
}

impl HistoryCell for TalkCell {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let drawn = self.drawn(ctx);
        let mut rows = Vec::with_capacity(drawn.rows.len() + 1);
        if self.blank {
            rows.push(Line::default());
        }
        rows.extend(drawn.rows);
        rows
    }

    fn continues_previous(&self) -> bool {
        self.follows
    }

    fn picture_under(&self, ctx: &RenderCtx<'_>, cell: (u16, u16)) -> Option<Picture> {
        self.picture(ctx, cell)
    }
}

/// The live typing row: each seat writing now in its typing bubble, the lit dot moving, and why
/// it is that seat.
#[derive(Debug, Clone)]
pub struct TypingCell {
    pub writers: Vec<SeatId>,
    pub reason: Vec<Piece>,
    pub look: Look,
    pub faces: Faces,
}

/// How long each dot stays lit.
const DOT: std::time::Duration = std::time::Duration::from_millis(400);

impl TypingCell {
    fn frame(&self, ctx: &RenderCtx<'_>) -> usize {
        if ctx.theme.reduced_motion {
            return 1;
        }
        ctx.clock.frame(DOT, 3)
    }
}

impl HistoryCell for TypingCell {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        if self.writers.is_empty() {
            return Vec::new();
        }
        let (line, _) = self.look.typing(
            &self.writers,
            self.frame(ctx),
            &self.reason,
            usize::from(ctx.width),
            &self.faces,
            ctx.theme,
        );
        vec![line]
    }

    fn is_animating(&self) -> bool {
        !self.writers.is_empty()
    }
}
