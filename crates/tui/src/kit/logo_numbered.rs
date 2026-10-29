//! Numbered logos: a seat whose model sits more than once wears its logo with its number in a
//! circle at the lower right, since one logo can't tell two seats of a model apart. Each is a
//! picture of its own, made from the logo's when a screen first draws it and kept with the
//! plain logos from that frame on, so a session makes only the few its councils seat. A font
//! size change that calls for another [`LogoSize`] makes them all again for the new cells.

use super::{Brightness, LOGO_WIDTH, LogoMark, LogoSize, mark_png, one_colour, takes_logos};
use crate::mascot::{placeholder, png};

/// Seats of one model are numbered up to this, as far as the circled numbers go in pictures.
pub const MOST: u8 = LogoMark::MOST_NUMBERED;

/// The id of `mark`'s picture numbered `number`, in the ink for a screen as bright as `on`.
pub fn id(mark: LogoMark, number: u8, on: Brightness) -> u32 {
    mark.picture_id(Some(number), on == Brightness::Light && one_colour(mark))
}

/// The number a numbered logo's picture `id` carries, or `None` for any other picture.
#[cfg(test)]
pub fn number_of(id: u32) -> Option<u8> {
    mark_of(id).map(|(_, number)| number)
}

/// The logo and number a numbered logo's picture `id` shows, or `None` for any other picture.
#[cfg(test)]
pub fn mark_of(id: u32) -> Option<(LogoMark, u8)> {
    match LogoMark::of_picture(id)? {
        (mark, Some(number)) => Some((mark, number)),
        (_, None) => None,
    }
}

/// Notes that a screen drew `mark` numbered `number`, so its picture goes out before the next
/// frame.
pub fn want(mark: LogoMark, number: u8) {
    if (1..=MOST).contains(&number) {
        drawn::want(mark, number);
    }
}

/// The escapes that send each numbered logo drawn and not sent yet, in every ink its logo
/// ships in, for cells `cell` pixels in size, each with the virtual placement its placeholders
/// show. Empty while there are none, or where the terminal doesn't take the logos.
pub fn unsent(cell: Option<(u16, u16)>) -> Vec<u8> {
    send(cell, false)
}

/// The escapes that send every numbered logo drawn so far, as [`unsent`] does, for cells `cell`
/// pixels in size.
pub fn all_drawn(cell: Option<(u16, u16)>) -> Vec<u8> {
    send(cell, true)
}

fn send(cell: Option<(u16, u16)>, all: bool) -> Vec<u8> {
    if !takes_logos() {
        return Vec::new();
    }
    let size = LogoSize::for_terminal(cell);
    let mut out = Vec::new();
    for (mark, number) in drawn::take(all) {
        let inks: &[Brightness] = if one_colour(mark) {
            &[Brightness::Dark, Brightness::Light]
        } else {
            &[Brightness::Dark]
        };
        for &on in inks {
            if let Some(picture) = picture(mark, number, size, on) {
                out.extend(placeholder::send(
                    &picture,
                    id(mark, number, on),
                    (LOGO_WIDTH, 1),
                ));
            }
        }
    }
    out
}

/// Each digit as three columns of five rows, the top row first, a set bit a lit cell.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// `mark`'s two-cell picture at `size`, inked for a screen as bright as `on`, with a disc at its
/// lower right that `number` is cut out of, so the screen shows through the digit as it does
/// through `①`. The disc takes the screen's text colour.
pub fn picture(mark: LogoMark, number: u8, size: LogoSize, on: Brightness) -> Option<Vec<u8>> {
    let digit = DIGITS.get(usize::from(number))?;
    let mut image = png::decode(mark_png(mark, size, on))?;
    let (width, height) = (image.width, image.height);
    // The pictures are drawn for cells 42 pixels tall; a larger size scales the disc with it.
    let scale = height as f32 / 42.0;
    let radius = 7.5 * scale;
    let (cx, cy) = (width as f32 - 8.0 * scale, height as f32 - 9.0 * scale);
    let ink: [u8; 3] = match on {
        Brightness::Dark => [0xf2, 0xf2, 0xf2],
        Brightness::Light => [0x24, 0x24, 0x24],
    };
    let dot = 2.0 * scale;
    let (left, top) = (cx - 1.5 * dot, cy - 2.5 * dot);
    for y in 0..height {
        for x in 0..width {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let cover = (radius + 0.5 - distance).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let at = (y * width + x) * 4;
            let lit = {
                let (column, row) = ((px - left) / dot, (py - top) / dot);
                (0.0..3.0).contains(&column)
                    && (0.0..5.0).contains(&row)
                    && digit[row as usize] & (0b100 >> column as u8) != 0
            };
            if lit {
                image.pixels[at..at + 4].copy_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            blend(&mut image.pixels[at..at + 4], ink, cover);
        }
    }
    Some(png::encode(&image))
}

/// Lays `ink` at `cover` over the straight-alpha pixel `under`.
fn blend(under: &mut [u8], ink: [u8; 3], cover: f32) {
    let below = f32::from(under[3]) / 255.0;
    let alpha = cover + below * (1.0 - cover);
    if alpha <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let mixed = (f32::from(ink[channel]) * cover
            + f32::from(under[channel]) * below * (1.0 - cover))
            / alpha;
        under[channel] = mixed.round().clamp(0.0, 255.0) as u8;
    }
    under[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// The numbered logos drawn so far in the process, and whether each went out.
mod drawn {
    use super::LogoMark;
    use list::with;

    pub fn want(mark: LogoMark, number: u8) {
        with(|drawn| {
            if !drawn.iter().any(|(m, n, _)| *m == mark && *n == number) {
                drawn.push((mark, number, false));
            }
        });
    }

    /// The ones not sent yet, or with `all` every one, each counted as sent from now on.
    pub fn take(all: bool) -> Vec<(LogoMark, u8)> {
        with(|drawn| {
            let mut taken = Vec::new();
            for (mark, number, sent) in drawn.iter_mut().filter(|(_, _, sent)| all || !*sent) {
                *sent = true;
                taken.push((*mark, *number));
            }
            taken
        })
        .unwrap_or_default()
    }

    /// One list for the whole process, since the pictures go to its one terminal.
    #[cfg(not(test))]
    mod list {
        use std::sync::Mutex;

        use super::LogoMark;

        static DRAWN: Mutex<Vec<(LogoMark, u8, bool)>> = Mutex::new(Vec::new());

        pub fn with<R>(change: impl FnOnce(&mut Vec<(LogoMark, u8, bool)>) -> R) -> Option<R> {
            DRAWN.lock().ok().map(|mut drawn| change(&mut drawn))
        }
    }

    /// Unit tests run in parallel threads, so each keeps its own list.
    #[cfg(test)]
    mod list {
        use std::cell::RefCell;

        use super::LogoMark;

        thread_local! {
            static DRAWN: RefCell<Vec<(LogoMark, u8, bool)>> = const { RefCell::new(Vec::new()) };
        }

        pub fn with<R>(change: impl FnOnce(&mut Vec<(LogoMark, u8, bool)>) -> R) -> Option<R> {
            Some(DRAWN.with(|drawn| change(&mut drawn.borrow_mut())))
        }
    }
}
