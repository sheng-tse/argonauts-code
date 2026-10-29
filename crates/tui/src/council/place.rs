//! Putting the bubbles' pictures under the words. The kitty graphics protocol draws a picture
//! under the text when its z-index is below INT32_MIN/2, and under any cell with a background
//! of its own too, so selection and search highlights still show on the words. Ghostty and kitty
//! do so; WezTerm takes kitty pictures but not under text, and nothing passes them through tmux
//! or screen, so there the talk is a transcript.
//!
//! [`Pictures`] words every escape a bubble's picture takes through the screen's own picture
//! delivery, [`graphics`]. The inline screen puts each picture once, under rows the terminal
//! then scrolls; the full screen draws its own scrollback, so it puts each picture again
//! wherever its rows show in each frame, cut to the rows in view.

use std::ops::Range;

use argonauts_core::names::MessageId;

use super::picture::Picture;
use crate::mascot::graphics::{self, Crop, Graphics, Placement, UNDER_BACKGROUNDS};

/// The first picture id the council's bubbles use, apart from the mascot's and the world's.
pub const FIRST_ID: u32 = 0x4342_0000;

/// A bubble shows through one placement of its picture, which moves as its rows do.
const PLACEMENT: u32 = 1;

/// Whether the terminal named by `var` draws kitty pictures under its text: Ghostty and kitty,
/// straight to the terminal.
pub fn under_text(var: impl Fn(&str) -> Option<String>) -> bool {
    let wezterm = var("WEZTERM_PANE").is_some_and(|pane| !pane.is_empty())
        || var("TERM_PROGRAM").as_deref() == Some("WezTerm")
        || var("TERM").as_deref() == Some("wezterm");
    Graphics::detect(&var) == Some(Graphics::Kitty) && !wezterm
}

/// The bubbles' pictures this screen put and the ids they go by.
#[derive(Debug, Default)]
pub struct Pictures {
    next: u32,
    /// The full screen's pictures, put again each frame where their rows show.
    kept: Vec<Kept>,
    /// The screen width and the pixels a cell the kept pictures were drawn for.
    drawn_for: (u16, (u16, u16)),
    /// Pictures in the terminal that go with the next frame.
    gone: Vec<u32>,
}

#[derive(Debug)]
struct Kept {
    /// The printed rows of its cell.
    rows: Range<usize>,
    plain: Image,
    /// The same message picked, ringed.
    picked: Option<(MessageId, Image)>,
    /// Where the last frame put it, if it showed, and whether picked.
    shown: Option<(bool, Shown)>,
}

/// A picture as the terminal holds it.
#[derive(Debug)]
struct Image {
    picture: Picture,
    id: u32,
    /// Its image data is in the terminal.
    sent: bool,
}

/// Escapes to send with the cursor at a screen cell, or wherever it is.
pub type Placing = (Option<(u16, u16)>, Vec<u8>);

/// A picture put at a screen cell, cut to the source rows `crop` in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    at: (u16, u16),
    crop: (u32, u32),
}

impl Pictures {
    fn take_id(&mut self) -> u32 {
        let id = FIRST_ID.wrapping_add(self.next);
        self.next = self.next.wrapping_add(1) % 0x1_0000;
        id
    }

    fn image(&mut self, picture: Picture) -> Image {
        Image {
            picture,
            id: self.take_id(),
            sent: false,
        }
    }

    /// The escapes that put `picture` under the words with the cursor at the cell where it
    /// starts, on rows the terminal scrolls from now on: the inline screen's.
    pub fn printed(&mut self, picture: &Picture) -> Vec<u8> {
        let id = self.take_id();
        let mut out = graphics::store(&picture.png, id);
        out.extend(under_words(picture, id, None));
        out
    }

    /// Keeps `picture` for the full screen, under its cell's printed `rows` on a screen `width`
    /// cells wide whose cells are `cell` pixels. Pictures drawn for another width or cell size
    /// go with the next frame, as their rows wrap again or no longer fit them.
    pub fn keep(&mut self, rows: Range<usize>, mut picture: Picture, width: u16, cell: (u16, u16)) {
        if (width, cell) != self.drawn_for {
            self.drawn_for = (width, cell);
            self.forget_all();
        }
        let picked = picture
            .picked
            .take()
            .map(|(message, ringed)| (message, self.image(*ringed)));
        let plain = self.image(picture);
        self.kept.push(Kept {
            rows,
            plain,
            picked,
            shown: None,
        });
    }

    /// What the full screen sends after drawing a frame `width` cells wide whose cells are
    /// `cell` pixels, each with the screen cell to send it at, or none: every kept picture
    /// whose rows show put where they show now, cut to the rows in view, ringed when its
    /// message is `picked`, and taken off where they don't show. `rows_at` says where printed
    /// rows showed: the screen row of the first, and the screen rows of them in view.
    pub fn frame(
        &mut self,
        width: u16,
        cell: (u16, u16),
        picked: Option<&MessageId>,
        rows_at: impl Fn(usize, usize) -> Option<(i32, Range<u16>)>,
    ) -> Vec<Placing> {
        let mut out = Vec::new();
        if (width, cell) != self.drawn_for {
            self.forget_all();
        }
        let gone = graphics::forget(self.gone.drain(..));
        if !gone.is_empty() {
            out.push((None, gone));
        }
        let cell_height = u32::from(cell.1.max(1));
        for kept in &mut self.kept {
            let ringed = kept
                .picked
                .as_ref()
                .is_some_and(|(message, _)| Some(message) == picked);
            let image = kept.image(ringed);
            let rows = &kept.rows;
            let shown = rows_at(rows.start, rows.len())
                .and_then(|(first, view)| image.showing(first, view, rows.len(), cell_height))
                .map(|shown| (ringed, shown));
            if shown == kept.shown {
                continue;
            }
            // The other look, or the place it showed at, goes first.
            if let Some((was, _)) = kept.shown
                && (shown.is_none() || was != ringed)
            {
                out.push((None, graphics::remove(kept.image(was).id, PLACEMENT)));
            }
            if let Some((_, at)) = shown {
                let image = kept.image_mut(ringed);
                let mut escapes = Vec::new();
                if !std::mem::replace(&mut image.sent, true) {
                    escapes.extend(graphics::store(&image.picture.png, image.id));
                }
                let (from, rows) = at.crop;
                let crop = Crop {
                    x: 0,
                    y: from,
                    width: u32::from(image.picture.size.0),
                    height: rows,
                };
                escapes.extend(under_words(&image.picture, image.id, Some(crop)));
                out.push((Some(at.at), escapes));
            }
            kept.shown = shown;
        }
        out
    }

    /// The escapes that take every picture out of the terminal before another program has it
    /// or the app ends. Each goes out again as it next shows.
    pub fn let_go(&mut self) -> Vec<u8> {
        let mut ids: Vec<u32> = self.gone.drain(..).collect();
        for kept in &mut self.kept {
            for image in kept.images_mut() {
                if std::mem::take(&mut image.sent) {
                    ids.push(image.id);
                }
            }
            kept.shown = None;
        }
        graphics::forget(ids)
    }

    /// Drops every kept picture, as the screen starts over; they leave the terminal with the
    /// next frame.
    pub fn forget_all(&mut self) {
        for mut kept in std::mem::take(&mut self.kept) {
            let sent = kept
                .images_mut()
                .filter(|image| image.sent)
                .map(|image| image.id);
            self.gone.extend(sent);
        }
    }
}

impl Kept {
    fn image(&self, ringed: bool) -> &Image {
        match &self.picked {
            Some((_, image)) if ringed => image,
            _ => &self.plain,
        }
    }

    fn image_mut(&mut self, ringed: bool) -> &mut Image {
        match &mut self.picked {
            Some((_, image)) if ringed => image,
            _ => &mut self.plain,
        }
    }

    fn images_mut(&mut self) -> impl Iterator<Item = &mut Image> {
        std::iter::once(&mut self.plain).chain(self.picked.as_mut().map(|(_, image)| image))
    }
}

impl Image {
    /// Where its picture goes when the first of its cell's `rows` sits on screen row `first`
    /// and the screen rows `view` of them show, cut at the edges of the view.
    fn showing(
        &self,
        first: i32,
        view: Range<u16>,
        rows: usize,
        cell_height: u32,
    ) -> Option<Shown> {
        let rows = i32::try_from(rows).ok()?;
        let top = first + i32::from(self.picture.row);
        let height = u32::from(self.picture.size.1);
        let from = i32::from(view.start).max(top);
        // Cut at the bottom only where the view cuts the cell, so a tail that hangs below the
        // cell's last row still shows.
        let cut_below = i32::from(view.end) < first + rows;
        let skipped = u32::try_from(from - top).ok()? * cell_height;
        let room = if cut_below {
            u32::try_from(i32::from(view.end) - from).ok()? * cell_height
        } else {
            height
        };
        let rows_shown = height.saturating_sub(skipped).min(room);
        (rows_shown > 0).then_some(Shown {
            at: (self.picture.col, u16::try_from(from).ok()?),
            crop: (skipped, rows_shown),
        })
    }
}

/// The escape that shows picture `id` under the words from the cell at the cursor, which
/// stays there, `picture`'s offset into that cell and cut to `crop`.
fn under_words(picture: &Picture, id: u32, crop: Option<Crop>) -> Vec<u8> {
    graphics::place(&Placement {
        offset: picture.offset,
        crop,
        z: UNDER_BACKGROUNDS,
        ..Placement::new(id, PLACEMENT)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn only_ghostty_and_kitty_draw_under_the_text() {
        assert!(under_text(env(&[("TERM_PROGRAM", "ghostty")])));
        assert!(under_text(env(&[("TERM", "xterm-kitty")])));
        assert!(!under_text(env(&[("TERM_PROGRAM", "WezTerm")])));
        assert!(!under_text(env(&[
            ("TERM_PROGRAM", "ghostty"),
            ("TMUX", "/tmp/t,1,0")
        ])));
        assert!(!under_text(env(&[("TERM_PROGRAM", "Apple_Terminal")])));
    }

    #[test]
    fn a_printed_picture_is_stored_then_put_under_the_words_at_the_cursor() {
        let picture = Picture {
            png: vec![7; 5000],
            col: 3,
            row: 0,
            offset: (4, 2),
            size: (10, 10),
            picked: None,
        };
        let mut pictures = Pictures::default();
        let out = String::from_utf8(pictures.printed(&picture)).expect("text");
        // A payload longer than one chunk goes in more than one.
        let stored = format!("\x1b_Ga=t,f=100,q=2,i={FIRST_ID},m=1;");
        assert!(out.starts_with(&stored), "{out:?}");
        assert!(out.contains("\x1b_Gm=0;"), "{out:?}");
        let put = format!("\x1b_Ga=p,q=2,C=1,i={FIRST_ID},p=1,X=4,Y=2,z=-1073741825\x1b\\");
        assert!(out.ends_with(&put), "{out:?}");
        let next = String::from_utf8(pictures.printed(&picture)).expect("text");
        assert!(next.contains(&format!("i={}", FIRST_ID + 1)), "{next:?}");
    }
}
