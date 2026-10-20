//! What the screen sends to put the header's mascot on screen as a picture, and to swap an ultra
//! look's frames while the header shows.

use super::graphics::{self, Graphics};
use super::{FRAMES, Look, Mascot, picture};

/// A mascot sent as a picture: what it is, and the kitty protocol's ids of its frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub graphics: Graphics,
    pub mascot: Mascot,
    pub look: Look,
    pub cell: (u16, u16),
    /// One id per frame for the kitty protocol, which keeps every frame and shows one.
    ids: Vec<u32>,
    /// The frame on screen.
    pub frame: usize,
}

impl Sent {
    /// The escapes that draw `mascot` in `look` from the cursor, for a terminal of `cell`
    /// pixels a cell, naming its pictures from `first_id`. An ultra look's frames all go at
    /// once, so each swap after sends only which one shows. `None` when the cell size can't
    /// take a picture.
    pub fn first(
        graphics: Graphics,
        mascot: Mascot,
        look: Look,
        cell: (u16, u16),
        first_id: u32,
    ) -> Option<(Self, Vec<u8>)> {
        let frames = if look.is_ultra() { FRAMES } else { 1 };
        let pictures: Vec<_> = (0..frames)
            .map(|frame| picture(mascot, look, cell, frame))
            .collect::<Option<_>>()?;
        let ids: Vec<u32> = (0..frames)
            .map(|frame| first_id.saturating_add(u32::try_from(frame).unwrap_or(0)))
            .collect();
        let escapes = match (graphics, frames) {
            (Graphics::Kitty, 1) | (Graphics::Iterm, _) => {
                graphics::draw(graphics, &pictures[0], ids[0])
            }
            (Graphics::Kitty, _) => {
                let mut out: Vec<u8> = pictures
                    .iter()
                    .zip(&ids)
                    .flat_map(|(png, id)| graphics::store(png, *id))
                    .collect();
                out.extend(graphics::show(ids[0], None));
                out
            }
        };
        let sent = Sent {
            graphics,
            mascot,
            look,
            cell,
            ids,
            frame: 0,
        };
        Some((sent, escapes))
    }

    /// Like [`Self::first`], for a screen that places the picture itself as it draws: the kitty
    /// protocol keeps every frame, and none shows yet. iTerm2 keeps no pictures, so nothing goes
    /// out before [`Self::place`].
    pub fn stored(
        graphics: Graphics,
        mascot: Mascot,
        look: Look,
        cell: (u16, u16),
        first_id: u32,
    ) -> Option<(Self, Vec<u8>)> {
        let frames = if look.is_ultra() { FRAMES } else { 1 };
        let ids: Vec<u32> = (0..frames)
            .map(|frame| first_id.saturating_add(u32::try_from(frame).unwrap_or(0)))
            .collect();
        let mut escapes = Vec::new();
        for (frame, id) in ids.iter().enumerate() {
            let png = picture(mascot, look, cell, frame)?;
            if graphics == Graphics::Kitty {
                escapes.extend(graphics::store(&png, *id));
            }
        }
        let sent = Sent {
            graphics,
            mascot,
            look,
            cell,
            ids,
            frame: 0,
        };
        Some((sent, escapes))
    }

    /// The escapes that put `frame` in the mascot's cells from the cursor, a still look's one
    /// frame for any. For the kitty protocol they first take its picture off wherever it shows,
    /// so the same escapes move it.
    pub fn place(&mut self, frame: usize) -> Option<Vec<u8>> {
        let frame = if self.look.is_ultra() {
            frame % FRAMES
        } else {
            0
        };
        let escapes = match self.graphics {
            Graphics::Kitty => {
                let mut out = graphics::hide(self.ids[self.frame]);
                out.extend(graphics::show(self.ids[frame], None));
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

    /// The escapes that show `frame` in place of the one on screen, sent with the cursor where
    /// the picture starts. Nothing when it already shows, or the look doesn't move.
    pub fn show(&mut self, frame: usize) -> Option<Vec<u8>> {
        let frame = frame % FRAMES;
        if !self.look.is_ultra() || frame == self.frame {
            return None;
        }
        let escapes = match self.graphics {
            Graphics::Kitty => graphics::show(self.ids[frame], Some(self.ids[self.frame])),
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
            Graphics::Kitty => graphics::forget(self.ids.iter().copied()),
            Graphics::Iterm => Vec::new(),
        }
    }

    /// The first id after its own, for the next picture.
    pub fn next_id(&self) -> u32 {
        self.ids.last().map_or(0, |last| last.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::graphics::pictures_in;

    #[test]
    fn an_ultra_look_sends_every_frame_once_then_swaps_by_id() {
        let (mut sent, escapes) =
            Sent::first(Graphics::Kitty, Mascot::Medusa, Look::Codex, (9, 21), 100).unwrap();
        let text = String::from_utf8_lossy(&escapes);
        assert_eq!(text.matches("a=t,f=100,q=2,i=").count(), FRAMES);
        assert!(text.ends_with("\x1b_Ga=p,q=2,C=1,c=9,r=3,i=100\x1b\\"));
        assert_eq!(pictures_in(&escapes).len(), FRAMES);
        assert_eq!(sent.show(0), None);
        assert_eq!(
            String::from_utf8(sent.show(1).unwrap()).unwrap(),
            "\x1b_Ga=p,q=2,C=1,c=9,r=3,i=101\x1b\\\x1b_Ga=d,d=i,q=2,i=100\x1b\\"
        );
        assert_eq!(sent.next_id(), 118);
        assert!(String::from_utf8_lossy(&sent.forget()).contains("d=I,q=2,i=117"));
    }

    /// For a screen that draws itself again, every frame is kept first, then placed and moved
    /// by id, and taken off without being forgotten.
    #[test]
    fn a_stored_picture_is_placed_moved_and_taken_off_by_id() {
        let (mut sent, escapes) = Sent::stored(
            Graphics::Kitty,
            Mascot::Argonaut,
            Look::Ultracode,
            (9, 21),
            40,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&escapes);
        assert_eq!(text.matches("a=t,f=100,q=2,i=").count(), FRAMES);
        assert!(
            !text.contains("a=p") && !text.contains("a=T"),
            "nothing shows yet"
        );
        assert_eq!(
            String::from_utf8(sent.place(0).unwrap()).unwrap(),
            "\x1b_Ga=d,d=i,q=2,i=40\x1b\\\x1b_Ga=p,q=2,C=1,c=9,r=3,i=40\x1b\\"
        );
        assert_eq!(
            String::from_utf8(sent.place(2).unwrap()).unwrap(),
            "\x1b_Ga=d,d=i,q=2,i=40\x1b\\\x1b_Ga=p,q=2,C=1,c=9,r=3,i=42\x1b\\"
        );
        assert_eq!(
            String::from_utf8(sent.hide()).unwrap(),
            "\x1b_Ga=d,d=i,q=2,i=42\x1b\\"
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
    }

    #[test]
    fn iterm2_gets_each_frame_whole() {
        let (mut sent, escapes) =
            Sent::first(Graphics::Iterm, Mascot::Hydra, Look::Ultracode, (9, 21), 1).unwrap();
        assert_eq!(pictures_in(&escapes).len(), 1);
        let next = sent.show(5).unwrap();
        assert_eq!(
            pictures_in(&next)[0],
            picture(Mascot::Hydra, Look::Ultracode, (9, 21), 5)
                .unwrap()
                .as_ref()
        );
        assert!(sent.forget().is_empty());
    }
}
