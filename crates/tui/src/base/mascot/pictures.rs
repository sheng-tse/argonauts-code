//! The pictures the header sends where the terminal draws them: the generator's PNGs, drawn for
//! one cell size, and for any other size fitted to it, as the generator fits a mascot into its
//! cells. An ultra look has a frame every 50 ms of its cycle.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::Deserialize;

use super::frames::{self, Drawn};
use super::png::{self, Area};
use super::{Look, Mascot, WIDTH};

const LISTING: &str = include_str!("../../../assets/mascots/pictures.json");

/// The cell size in pixels the generator draws for, as `pictures.json` lists it.
pub const DRAWN_FOR: (u16, u16) = (18, 42);

#[derive(Debug, Deserialize)]
struct Listing {
    /// What the generator drew, which the tests check against what is built in.
    #[cfg(test)]
    cells: Vec<(u16, u16)>,
    /// Seconds between an ultra look's frames.
    #[cfg(test)]
    step: f64,
    /// The rows an ultra look's picture takes above the mascot's own.
    #[cfg(test)]
    above: usize,
    mascots: BTreeMap<String, Listed>,
}

#[derive(Debug, Deserialize)]
struct Listed {
    /// The drawn area in the SVG's units: x, y, width and height.
    area: [f64; 4],
    /// Seconds an ultra look's animation takes.
    cycle: f64,
    /// The frames of one cycle.
    frames: usize,
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

/// The frames of one cycle of `mascot`'s ultra looks.
pub fn frames(mascot: Mascot) -> usize {
    listing().mascots[mascot.name()].frames
}

fn drawn(mascot: Mascot) -> &'static Drawn {
    match mascot {
        Mascot::Argonaut => &frames::ARGONAUT,
        Mascot::Medusa => &frames::MEDUSA,
        Mascot::Hydra => &frames::HYDRA,
    }
}

/// The generator's own PNG of `mascot` in `look`, at `frame` of an ultra look's cycle.
fn drawn_picture(mascot: Mascot, look: Look, frame: usize) -> &'static [u8] {
    let drawn = drawn(mascot);
    let of = |frames: &'static [&'static [u8]]| frames[frame % frames.len()];
    match look {
        Look::Normal => drawn.normal,
        Look::Light => drawn.light,
        Look::Ultracode => of(drawn.ultracode),
        Look::Codex => of(drawn.codex),
    }
}

/// For each frame of `look`, the first frame that looks the same, so a frame like the one
/// before it needs nothing sent: one for a still look.
pub fn first_alike(mascot: Mascot, look: Look) -> &'static [usize] {
    static ALIKE: OnceLock<HashMap<(Mascot, Look), Vec<usize>>> = OnceLock::new();
    let all = ALIKE.get_or_init(|| {
        let mut all = HashMap::new();
        for mascot in Mascot::ALL {
            for look in Look::ALL {
                let count = if look.is_ultra() { frames(mascot) } else { 1 };
                let pictures: Vec<&[u8]> = (0..count)
                    .map(|frame| drawn_picture(mascot, look, frame))
                    .collect();
                let alike = (0..count)
                    .map(|frame| {
                        (0..frame)
                            .find(|earlier| pictures[*earlier] == pictures[frame])
                            .unwrap_or(frame)
                    })
                    .collect();
                all.insert((mascot, look), alike);
            }
        }
        all
    });
    &all[&(mascot, look)]
}

/// The PNG of `mascot` in `look` that fills its cells at `cell` pixels a cell, at `frame` of an
/// ultra look's animation. `None` for a cell too small to draw in.
pub fn picture(
    mascot: Mascot,
    look: Look,
    cell: (u16, u16),
    frame: usize,
) -> Option<Cow<'static, [u8]>> {
    let drawn = drawn_picture(mascot, look, frame);
    if cell == DRAWN_FOR {
        return Some(Cow::Borrowed(drawn));
    }
    if cell.0 == 0 || cell.1 == 0 {
        return None;
    }
    let source = png::decode(drawn)?;
    let rows = look.picture_rows();
    let box_of =
        |(width, height): (u16, u16)| (usize::from(width) * WIDTH, usize::from(height) * rows);
    let from = art_in(mascot, look, box_of(DRAWN_FOR));
    let (width, height) = box_of(cell);
    let fitted = png::fitted(
        &source,
        from,
        width,
        height,
        art_in(mascot, look, (width, height)),
    );
    Some(Cow::Owned(png::encode(&fitted)))
}

/// Where the generator puts `mascot`'s drawn area in a picture of `look` `box_size` pixels: in
/// its bottom three rows, as tall as they are, or as wide as the picture when that would be
/// too wide, and in their middle. An ultra look's picture shows the rows above too.
fn art_in(mascot: Mascot, look: Look, (box_width, box_height): (usize, usize)) -> Area {
    let [_, _, area_width, area_height] = listing().mascots[mascot.name()].area;
    let lift = box_height * look.rows_above() / look.picture_rows();
    let rows_height = box_height - lift;
    let (box_w, box_h) = (box_width as f64, rows_height as f64);
    let (mut width, mut height) = (box_h * area_width / area_height, box_h);
    if width > box_w {
        (width, height) = (box_w, box_w * area_height / area_width);
    }
    // Python rounds halves to even, and the generator's sizes came from it.
    let (width, height) = (
        (width.round_ties_even() as usize).min(box_width),
        (height.round_ties_even() as usize).min(rows_height),
    );
    Area {
        x: (box_width - width) / 2,
        y: (rows_height - height) / 2,
        width,
        height: height + lift,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_matches_what_is_built_in() {
        let listing = listing();
        assert_eq!(listing.cells, [DRAWN_FOR]);
        assert!(listing.step <= 0.05, "a frame every 50 ms or sooner");
        assert_eq!(listing.above, super::super::ULTRA_ROWS_ABOVE);
        for mascot in Mascot::ALL {
            let listed = &listing.mascots[mascot.name()];
            let drawn = drawn(mascot);
            assert_eq!(drawn.ultracode.len(), listed.frames, "{mascot:?}");
            assert_eq!(drawn.codex.len(), listed.frames, "{mascot:?}");
            assert!((listed.cycle / listed.frames as f64 - listing.step).abs() < 1e-9);
        }
    }

    #[test]
    fn every_picture_fills_its_cells() {
        for mascot in Mascot::ALL {
            for look in Look::ALL {
                let frames = if look.is_ultra() { frames(mascot) } else { 1 };
                for frame in 0..frames {
                    let bytes = picture(mascot, look, DRAWN_FOR, frame).unwrap();
                    let image = png::decode(&bytes)
                        .unwrap_or_else(|| panic!("{mascot:?} {look:?} {frame}"));
                    assert_eq!(
                        (image.width, image.height),
                        (18 * 9, 42 * look.picture_rows())
                    );
                }
            }
        }
    }

    /// The medusa stays still for part of each cycle, so frames there are the one before
    /// them; the hydra moves all the way round.
    #[test]
    fn frames_alike_point_to_the_first_of_them() {
        let medusa = first_alike(Mascot::Medusa, Look::Ultracode);
        assert_eq!(medusa.len(), 36);
        let distinct = medusa.iter().enumerate().filter(|(at, first)| at == *first);
        assert!(distinct.count() < 36);
        assert!(
            first_alike(Mascot::Hydra, Look::Codex)
                .iter()
                .enumerate()
                .all(|(at, first)| at == *first)
        );
        assert_eq!(first_alike(Mascot::Argonaut, Look::Normal), [0]);
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
        // The generator's 81 x 63 Hydra is 66 wide: the area's 90.3 x 85.8 at 63 tall.
        assert_eq!(
            art_in(Mascot::Hydra, Look::Normal, (81, 63)),
            Area {
                x: 7,
                y: 0,
                width: 66,
                height: 63
            }
        );
        // An ultra look's picture is a row taller, and shows that row over the same mascot.
        assert_eq!(
            art_in(Mascot::Argonaut, Look::Ultracode, (162, 168)),
            Area {
                x: 5,
                y: 0,
                width: 151,
                height: 168
            }
        );
    }

    /// Fitted by its own height, the mascot fills its three rows in every look, and an ultra
    /// mark rises into the row above, the blank one over the header.
    #[test]
    fn a_mascot_fills_its_rows_and_its_ultra_mark_rises_above_them() {
        let ink_rows = |look: Look| {
            let bytes = picture(Mascot::Argonaut, look, DRAWN_FOR, 0).unwrap();
            let image = png::decode(&bytes).unwrap();
            let inked = |y: usize| {
                (0..image.width).any(|x| image.pixels[(y * image.width + x) * 4 + 3] > 40)
            };
            let rows: Vec<usize> = (0..image.height).filter(|y| inked(*y)).collect();
            (rows[0], rows[rows.len() - 1])
        };
        assert_eq!(ink_rows(Look::Normal), (0, 125));
        let (top, bottom) = ink_rows(Look::Ultracode);
        assert_eq!(bottom, 42 + 125, "the same mascot under the row above");
        assert!(
            top < 42,
            "the gold shell rises into the row above, from {top}"
        );
    }
}
