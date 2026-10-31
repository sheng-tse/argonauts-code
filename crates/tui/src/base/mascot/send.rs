//! What the screen sends to put the header's mascot on screen as a picture, and to swap an ultra
//! look's frames while the header shows.
//!
//! With the kitty protocol the terminal keeps each picture by id, so a swap sends only which
//! one shows. The first frame goes with the header and the next one with it, and each later
//! frame a step before it is due, so the header never waits for a whole cycle's pictures. A
//! frame that looks like the one on screen sends nothing. iTerm2 keeps no pictures, so it gets
//! each one whole, 18 a cycle.

use super::graphics::{self, Graphics};
use super::pictures::first_alike;
use super::{ITERM_FRAMES, Look, Mascot, picture};

/// A mascot sent as a picture: what it is, and the kitty protocol's ids of its frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub graphics: Graphics,
    pub mascot: Mascot,
    pub look: Look,
    pub cell: (u16, u16),
    /// The id of each frame's picture; frames that look alike share one.
    ids: Vec<u32>,
    /// The ids the terminal keeps a picture for.
    kept: Vec<u32>,
    /// The frame whose picture is on screen.
    frame: usize,
}

impl Sent {
    fn new(
        graphics: Graphics,
        mascot: Mascot,
        look: Look,
        cell: (u16, u16),
        first_id: u32,
    ) -> Self {
        let alike = first_alike(mascot, look);
        let mut ids = Vec::with_capacity(alike.len());
        let mut next = first_id;
        for (frame, first) in alike.iter().enumerate() {
            if *first == frame {
                ids.push(next);
                next = next.saturating_add(1);
            } else {
                ids.push(ids[*first]);
            }
        }
        Sent {
            graphics,
            mascot,
            look,
            cell,
            ids,
            kept: Vec::new(),
            frame: 0,
        }
    }

    /// The escapes that draw `mascot` in `look` from the cursor, for a terminal of `cell`
    /// pixels a cell, naming its pictures from `first_id`. An ultra look's first frame goes
    /// with the next one, and later frames as they come due. `None` when the cell size can't
    /// take a picture.
    pub fn first(
        graphics: Graphics,
        mascot: Mascot,
        look: Look,
        cell: (u16, u16),
        first_id: u32,
    ) -> Option<(Self, Vec<u8>)> {
        let mut sent = Self::new(graphics, mascot, look, cell, first_id);
        let escapes = match graphics {
            Graphics::Kitty if look.is_ultra() => {
                let mut out = sent.keep(0)?;
                out.extend(graphics::show(sent.ids[0], None));
                out.extend(sent.keep_next(0));
                out
            }
            Graphics::Kitty | Graphics::Iterm => {
                let first = picture(mascot, look, cell, 0)?;
                if graphics == Graphics::Kitty {
                    sent.kept.push(sent.ids[0]);
                }
                graphics::draw(graphics, &first, sent.ids[0])
            }
        };
        Some((sent, escapes))
    }

    /// Like [`Self::first`], for a screen that places the picture itself as it draws: the kitty
    /// protocol keeps the first frames, and none shows yet. iTerm2 keeps no pictures, so nothing
    /// goes out before [`Self::place`].
    pub fn stored(
        graphics: Graphics,
        mascot: Mascot,
        look: Look,
        cell: (u16, u16),
        first_id: u32,
    ) -> Option<(Self, Vec<u8>)> {
        let mut sent = Self::new(graphics, mascot, look, cell, first_id);
        picture(mascot, look, cell, 0)?;
        let mut escapes = Vec::new();
        if graphics == Graphics::Kitty {
            escapes.extend(sent.keep(0)?);
            escapes.extend(sent.keep_next(0));
        }
        Some((sent, escapes))
    }

    /// Which picture shows at `frame` of the clock, as [`Self::place`] would put it: the same
    /// id for frames that look alike, so a caller can tell a placement would change nothing.
    pub fn due_picture(&self, frame: usize) -> u32 {
        self.ids[self.due(frame)]
    }

    /// The frame whose picture shows at `frame` of the clock: one a still look, the first of
    /// those alike, and for iTerm2 the nearest of its 18 a cycle.
    fn due(&self, frame: usize) -> usize {
        if !self.look.is_ultra() {
            return 0;
        }
        let count = self.ids.len().max(1);
        let frame = frame % count;
        let frame = match self.graphics {
            Graphics::Kitty => frame,
            Graphics::Iterm => {
                let step = frame * ITERM_FRAMES / count;
                (2 * step * count + ITERM_FRAMES) / (2 * ITERM_FRAMES) % count
            }
        };
        first_alike(self.mascot, self.look)[frame]
    }

    /// The escapes that keep `frame`'s picture in the terminal, nothing where it has it.
    fn keep(&mut self, frame: usize) -> Option<Vec<u8>> {
        let id = self.ids[frame];
        if self.graphics != Graphics::Kitty || self.kept.contains(&id) {
            return Some(Vec::new());
        }
        let png = picture(self.mascot, self.look, self.cell, frame)?;
        self.kept.push(id);
        Some(graphics::store(&png, id))
    }

    /// The escapes that keep the picture of the next frame after `frame` that looks different,
    /// so it is there when it comes due.
    fn keep_next(&mut self, frame: usize) -> Vec<u8> {
        let count = self.ids.len();
        let next = (1..count)
            .map(|ahead| (frame + ahead) % count)
            .find(|next| self.ids[*next] != self.ids[frame]);
        next.and_then(|next| self.keep(next)).unwrap_or_default()
    }

    /// The escapes that put `frame` in the mascot's cells from the cursor, a still look's one
    /// frame for any. For the kitty protocol they first take its picture off wherever it shows,
    /// so the same escapes move it.
    pub fn place(&mut self, frame: usize) -> Option<Vec<u8>> {
        let frame = self.due(frame);
        let escapes = match self.graphics {
            Graphics::Kitty => {
                let mut out = self.keep(frame)?;
                out.extend(graphics::hide(self.ids[self.frame]));
                out.extend(graphics::show(self.ids[frame], None));
                out.extend(self.keep_next(frame));
                out
            }
            Graphics::Iterm => {
                let png = picture(self.mascot, self.look, self.cell, frame)?;
                graphics::draw(Graphics::Iterm, &png, 0)
            }
        };
        self.frame = frame;
        Some(escapes)
    }

    /// The escapes that take its picture off the screen, keeping it. iTerm2's sits in the cells
    /// it was drawn in, which only writing them again clears.
    pub fn hide(&self) -> Vec<u8> {
        match self.graphics {
            Graphics::Kitty => graphics::hide(self.ids[self.frame]),
            Graphics::Iterm => Vec::new(),
        }
    }

    /// The escapes that show `frame` of the clock in place of the one on screen, sent with the
    /// cursor where the picture starts. Nothing when the picture due looks like the one that
    /// shows, or the look doesn't move.
    pub fn show(&mut self, frame: usize) -> Option<Vec<u8>> {
        let frame = self.due(frame);
        if !self.look.is_ultra() || self.ids[frame] == self.ids[self.frame] {
            return None;
        }
        let escapes = match self.graphics {
            Graphics::Kitty => {
                let mut out = self.keep(frame)?;
                out.extend(graphics::show(self.ids[frame], Some(self.ids[self.frame])));
                out.extend(self.keep_next(frame));
                out
            }
            Graphics::Iterm => {
                let png = picture(self.mascot, self.look, self.cell, frame)?;
                graphics::draw(Graphics::Iterm, &png, 0)
            }
        };
        self.frame = frame;
        Some(escapes)
    }

    /// The escapes that free its pictures once another takes its place.
    pub fn forget(&self) -> Vec<u8> {
        match self.graphics {
            Graphics::Kitty => graphics::forget(self.kept.iter().copied()),
            Graphics::Iterm => Vec::new(),
        }
    }

    /// The first id after its own, for the next picture.
    pub fn next_id(&self) -> u32 {
        self.ids
            .iter()
            .max()
            .map_or(0, |last| last.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::mascot::graphics::pictures_in;

    /// How many pictures the escapes keep in the terminal.
    fn kept_in(escapes: &[u8]) -> usize {
        String::from_utf8_lossy(escapes)
            .matches("a=t,f=100,q=2,i=")
            .count()
    }

    #[test]
    fn an_ultra_look_sends_its_first_frames_with_the_header_and_the_rest_as_they_come() {
        let (mut sent, escapes) =
            Sent::first(Graphics::Kitty, Mascot::Hydra, Look::Codex, (18, 42), 100).unwrap();
        let text = String::from_utf8_lossy(&escapes);
        // The frame that shows and the one after it, no more, so the header never waits.
        assert_eq!(kept_in(&escapes), 2);
        assert!(text.contains("\x1b_Ga=p,q=2,C=1,c=9,r=3,i=100\x1b\\"));
        assert!(escapes.len() < 40_000, "{} bytes", escapes.len());
        assert_eq!(sent.show(0), None);
        let next = sent.show(1).unwrap();
        let text = String::from_utf8_lossy(&next);
        assert!(text.starts_with("\x1b_Ga=p,q=2,C=1,c=9,r=3,i=101\x1b\\\x1b_Ga=d,d=i,q=2,i=100"));
        assert_eq!(kept_in(&next), 1, "the frame after it, ahead of time");
        // Over a cycle every picture goes once, and the next cycle only swaps them.
        let frames = Mascot::Hydra.frames();
        let first_cycle: usize = (2..frames)
            .chain([0])
            .map(|frame| kept_in(&sent.show(frame).unwrap()))
            .sum();
        assert_eq!(2 + 1 + first_cycle, frames);
        for frame in 1..frames {
            assert_eq!(kept_in(&sent.show(frame).unwrap()), 0, "frame {frame}");
        }
        assert_eq!(sent.next_id(), 100 + 72);
        assert!(String::from_utf8_lossy(&sent.forget()).contains("d=I,q=2,i=171"));
    }

    /// Where the medusa stands still nothing goes out, and its frames alike share a picture.
    #[test]
    fn a_frame_like_the_one_on_screen_sends_nothing() {
        let (mut sent, _) = Sent::first(
            Graphics::Kitty,
            Mascot::Medusa,
            Look::Ultracode,
            (18, 42),
            1,
        )
        .unwrap();
        let frames = Mascot::Medusa.frames();
        let sends = (1..frames)
            .filter(|frame| sent.show(*frame).is_some())
            .count();
        let alike = first_alike(Mascot::Medusa, Look::Ultracode);
        let changes = (1..frames)
            .filter(|frame| alike[*frame] != alike[frame - 1])
            .count();
        assert_eq!(sends, changes);
        assert!(sends < frames - 1, "the still part sends nothing");
        let distinct = alike.iter().enumerate().filter(|(at, first)| at == *first);
        assert_eq!(sent.next_id(), 1 + distinct.count() as u32);
    }

    /// For a screen that draws itself again, the first frames are kept first, then placed and
    /// moved by id, and taken off without being forgotten.
    #[test]
    fn a_stored_picture_is_placed_moved_and_taken_off_by_id() {
        let (mut sent, escapes) =
            Sent::stored(Graphics::Kitty, Mascot::Hydra, Look::Ultracode, (9, 21), 40).unwrap();
        let text = String::from_utf8_lossy(&escapes);
        assert_eq!(kept_in(&escapes), 2);
        assert!(
            !text.contains("a=p") && !text.contains("a=T"),
            "nothing shows yet"
        );
        assert_eq!(
            String::from_utf8(sent.place(0).unwrap()).unwrap(),
            "\x1b_Ga=d,d=i,q=2,i=40\x1b\\\x1b_Ga=p,q=2,C=1,c=9,r=3,i=40\x1b\\"
        );
        let moved = sent.place(1).unwrap();
        assert!(
            String::from_utf8_lossy(&moved)
                .starts_with("\x1b_Ga=d,d=i,q=2,i=40\x1b\\\x1b_Ga=p,q=2,C=1,c=9,r=3,i=41\x1b\\")
        );
        assert_eq!(
            String::from_utf8(sent.hide()).unwrap(),
            "\x1b_Ga=d,d=i,q=2,i=41\x1b\\"
        );
        // A still look has one frame, whatever the clock says.
        let (mut still, _) =
            Sent::stored(Graphics::Kitty, Mascot::Medusa, Look::Normal, (9, 21), 7).unwrap();
        assert!(String::from_utf8_lossy(&still.place(5).unwrap()).ends_with("i=7\x1b\\"));
        // iTerm2 keeps nothing, so each placing draws the frame whole.
        let (mut iterm, stored) =
            Sent::stored(Graphics::Iterm, Mascot::Hydra, Look::Normal, (9, 21), 1).unwrap();
        assert!(stored.is_empty());
        assert_eq!(pictures_in(&iterm.place(0).unwrap()).len(), 1);
        assert!(iterm.hide().is_empty());
    }

    #[test]
    fn a_still_look_sends_one_picture_and_never_swaps() {
        let (mut sent, escapes) =
            Sent::first(Graphics::Kitty, Mascot::Argonaut, Look::Normal, (9, 21), 7).unwrap();
        assert_eq!(pictures_in(&escapes).len(), 1);
        assert!(
            String::from_utf8_lossy(&escapes).starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=9,r=3,i=7,")
        );
        assert_eq!(sent.show(3), None);
        assert_eq!(sent.next_id(), 8);
    }

    /// iTerm2 takes each picture whole, so it gets 18 a cycle, each the frame due then.
    #[test]
    fn iterm2_gets_18_pictures_a_cycle() {
        let (mut sent, escapes) =
            Sent::first(Graphics::Iterm, Mascot::Hydra, Look::Ultracode, (18, 42), 1).unwrap();
        assert_eq!(pictures_in(&escapes).len(), 1);
        let frames = Mascot::Hydra.frames();
        let sent_in_a_cycle: Vec<Vec<u8>> =
            (1..=frames).filter_map(|frame| sent.show(frame)).collect();
        assert_eq!(sent_in_a_cycle.len(), ITERM_FRAMES);
        // The hydra's 72 frames take every fourth.
        assert_eq!(
            pictures_in(&sent_in_a_cycle[0])[0],
            picture(Mascot::Hydra, Look::Ultracode, (18, 42), 4)
                .unwrap()
                .as_ref()
        );
        assert!(sent.forget().is_empty());
    }
}
