//! The pictures the header sends where the terminal draws them: the generator's PNGs for the cell
//! sizes it draws for, and for any other size the largest of them fitted to it, as the generator
//! fits a mascot into its cells.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use super::png::{self, Area};
use super::{FRAMES, Look, Mascot, ROWS, WIDTH};

const LISTING: &str = include_str!("../../assets/mascots/pictures.json");

/// The cell sizes in pixels the generator draws for, in `pictures.json`'s order.
pub const DRAWN_FOR: [(u16, u16); 2] = [(9, 21), (18, 42)];

macro_rules! picture {
    ($($part:literal),+) => {
        include_bytes!(concat!("../../assets/mascots/pictures/", $($part),+)).as_slice()
    };
}

/// A still look at each size the generator draws for.
macro_rules! still {
    ($stem:literal) => {
        [picture!($stem, "-9x21.png"), picture!($stem, "-18x42.png")]
    };
}

macro_rules! frames_at {
    ($stem:literal, $size:literal) => {
        [
            picture!($stem, $size, "-00.png"),
            picture!($stem, $size, "-01.png"),
            picture!($stem, $size, "-02.png"),
            picture!($stem, $size, "-03.png"),
            picture!($stem, $size, "-04.png"),
            picture!($stem, $size, "-05.png"),
            picture!($stem, $size, "-06.png"),
            picture!($stem, $size, "-07.png"),
            picture!($stem, $size, "-08.png"),
            picture!($stem, $size, "-09.png"),
            picture!($stem, $size, "-10.png"),
            picture!($stem, $size, "-11.png"),
            picture!($stem, $size, "-12.png"),
            picture!($stem, $size, "-13.png"),
            picture!($stem, $size, "-14.png"),
            picture!($stem, $size, "-15.png"),
            picture!($stem, $size, "-16.png"),
            picture!($stem, $size, "-17.png"),
        ]
    };
}

/// An ultra look's frames at each size the generator draws for.
macro_rules! frames {
    ($stem:literal) => {
        [frames_at!($stem, "-9x21"), frames_at!($stem, "-18x42")]
    };
}

type Stills = [&'static [u8]; DRAWN_FOR.len()];
type Frames = [[&'static [u8]; FRAMES]; DRAWN_FOR.len()];

/// One mascot's pictures in every look.
struct Drawn {
    normal: Stills,
    light: Stills,
    ultracode: Frames,
    codex: Frames,
}

static ARGONAUT: Drawn = Drawn {
    normal: still!("a-captain-argonaut"),
    light: still!("a-captain-argonaut-light"),
    ultracode: frames!("a-captain-argonaut-ultracode"),
    codex: frames!("a-captain-argonaut-codex"),
};

static MEDUSA: Drawn = Drawn {
    normal: still!("a-orchestrator-medusa"),
    light: still!("a-orchestrator-medusa-light"),
    ultracode: frames!("a-orchestrator-medusa-ultracode"),
    codex: frames!("a-orchestrator-medusa-codex"),
};

static HYDRA: Drawn = Drawn {
    normal: still!("a-council-hydra"),
    light: still!("a-council-hydra-light"),
    ultracode: frames!("a-council-hydra-ultracode"),
    codex: frames!("a-council-hydra-codex"),
};

#[derive(Debug, Deserialize)]
struct Listing {
    /// What the generator drew, which the tests check against what is built in.
    #[cfg(test)]
    cells: Vec<(u16, u16)>,
    #[cfg(test)]
    frames: usize,
    mascots: BTreeMap<String, Listed>,
}

#[derive(Debug, Deserialize)]
struct Listed {
    /// The drawn area in the SVG's units: x, y, width and height.
    area: [f64; 4],
    /// Seconds an ultra look's animation takes.
    cycle: f64,
}

fn listing() -> &'static Listing {
    static LISTED: OnceLock<Listing> = OnceLock::new();
    LISTED.get_or_init(|| {
        serde_json::from_str(LISTING).expect("pictures.json is the generator's own")
    })
}

/// Seconds `mascot`'s ultra animation takes, one cycle of its frames.
pub fn cycle(mascot: Mascot) -> f64 {
    listing().mascots[mascot.name()].cycle
}

/// The PNG of `mascot` in `look` that fills its cells at `cell` pixels a cell, at `frame` of an
/// ultra look's animation. `None` for a cell too small to draw in.
pub fn picture(
    mascot: Mascot,
    look: Look,
    cell: (u16, u16),
    frame: usize,
) -> Option<Cow<'static, [u8]>> {
    let drawn = match mascot {
        Mascot::Argonaut => &ARGONAUT,
        Mascot::Medusa => &MEDUSA,
        Mascot::Hydra => &HYDRA,
    };
    let at = |size: usize| match look {
        Look::Normal => drawn.normal[size],
        Look::Light => drawn.light[size],
        Look::Ultracode => drawn.ultracode[size][frame % FRAMES],
        Look::Codex => drawn.codex[size][frame % FRAMES],
    };
    if let Some(size) = DRAWN_FOR.iter().position(|drawn| *drawn == cell) {
        return Some(Cow::Borrowed(at(size)));
    }
    if cell.0 == 0 || cell.1 == 0 {
        return None;
    }
    let largest = DRAWN_FOR.len() - 1;
    let source = png::decode(at(largest))?;
    let box_of =
        |(width, height): (u16, u16)| (usize::from(width) * WIDTH, usize::from(height) * ROWS);
    let from = art_in(mascot, box_of(DRAWN_FOR[largest]));
    let (width, height) = box_of(cell);
    let fitted = png::fitted(
        &source,
        from,
        width,
        height,
        art_in(mascot, (width, height)),
    );
    Some(Cow::Owned(png::encode(&fitted)))
}

/// Where the generator puts `mascot`'s drawn area in a picture `box_size` pixels: as tall as the
/// picture, or as wide when that would be too wide, and in the middle.
fn art_in(mascot: Mascot, (box_width, box_height): (usize, usize)) -> Area {
    let [_, _, area_width, area_height] = listing().mascots[mascot.name()].area;
    let (box_w, box_h) = (box_width as f64, box_height as f64);
    let (mut width, mut height) = (box_h * area_width / area_height, box_h);
    if width > box_w {
        (width, height) = (box_w, box_w * area_height / area_width);
    }
    // Python rounds halves to even, and the generator's sizes came from it.
    let (width, height) = (
        (width.round_ties_even() as usize).min(box_width),
        (height.round_ties_even() as usize).min(box_height),
    );
    Area {
        x: (box_width - width) / 2,
        y: (box_height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_matches_what_is_built_in() {
        let listing = listing();
        assert_eq!(listing.cells, DRAWN_FOR);
        assert_eq!(listing.frames, FRAMES);
        for mascot in Mascot::ALL {
            assert!(listing.mascots.contains_key(mascot.name()), "{mascot:?}");
        }
    }

    #[test]
    fn every_picture_fills_its_cells_at_each_size_drawn_for() {
        for mascot in Mascot::ALL {
            for look in Look::ALL {
                for (size, cell) in DRAWN_FOR.iter().enumerate() {
                    let frames = if look.is_ultra() { FRAMES } else { 1 };
                    for frame in 0..frames {
                        let bytes = picture(mascot, look, *cell, frame).unwrap();
                        let image = png::decode(&bytes)
                            .unwrap_or_else(|| panic!("{mascot:?} {look:?} {size} {frame}"));
                        assert_eq!(
                            (image.width, image.height),
                            (usize::from(cell.0) * 9, usize::from(cell.1) * 3)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn another_cell_size_gets_the_largest_fitted_to_it() {
        let bytes = picture(Mascot::Argonaut, Look::Normal, (10, 20), 0).unwrap();
        let image = png::decode(&bytes).unwrap();
        assert_eq!((image.width, image.height), (90, 60));
        // The mascot sits in the middle: its middle column has ink, the outer ones have none.
        let alpha = |x: usize, y: usize| image.pixels[(y * image.width + x) * 4 + 3];
        assert!((0..60).any(|y| alpha(45, y) > 0));
        assert!((0..60).all(|y| alpha(0, y) == 0 && alpha(89, y) == 0));
        assert_eq!(picture(Mascot::Argonaut, Look::Normal, (0, 20), 0), None);
    }

    #[test]
    fn the_area_sits_where_the_generator_puts_it() {
        // The generator's 81 x 63 Hydra is 66 wide: the area's 97.52 x 93.02 at 63 tall.
        assert_eq!(
            art_in(Mascot::Hydra, (81, 63)),
            Area {
                x: 7,
                y: 0,
                width: 66,
                height: 63
            }
        );
    }
}
