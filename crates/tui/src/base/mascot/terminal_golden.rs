//! The pictures the header sends, checked against the picture of the header in Ghostty with
//! Iosevka at 17 px (cells of 9 x 21 pixels, and 18 x 42 on a Retina screen), which drew 18
//! frames a cycle. The art is the same at a frame every 50 ms: each frame at a moment that page
//! drew is the pixels of its place in the strip there, and at 9 x 21 pixels, where the header
//! fits the 18 x 42 pictures to the cells, as close as fitting allows. The page's own font is
//! left out of the copy kept here.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::graphics::{Graphics, pictures_in};
use super::png::{self, Rgba};
use super::{Look, Mascot, Sent, picture};

/// The frames a cycle the page's strips step through.
const PAGE_FRAMES: usize = 18;
/// How far a fitted picture's channels may be from the page's, on average, out of 255.
const FITTED_WITHIN: f64 = 1.0;

const PAGE: &str = include_str!("../../../tests/golden/mascot/mascot_terminal.html");

/// Each picture on the page: how it is labelled, and its PNG.
fn pictures_on_page() -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    for (open, label_key) in [
        ("<img src=\"", "alt=\""),
        ("<div role=\"img\"", "aria-label=\""),
    ] {
        for part in PAGE.split(open).skip(1) {
            let data = part
                .split_once("data:image/png;base64,")
                .map(|(_, rest)| rest.split(['"', ')']).next().unwrap_or_default().to_owned())
                .unwrap_or_default();
            let label = part
                .split_once(label_key)
                .and_then(|(_, rest)| rest.split_once('"'))
                .map(|(label, _)| label.to_owned())
                .unwrap_or_default();
            found.push((label, STANDARD.decode(data).expect("a picture's PNG")));
        }
    }
    found
}

fn frame_of(strip: &Rgba, at: usize, width: usize) -> Rgba {
    let mut frame = Rgba::transparent(width, strip.height);
    for y in 0..strip.height {
        let from = (y * strip.width + at * width) * 4;
        let to = y * width * 4;
        frame.pixels[to..to + width * 4].copy_from_slice(&strip.pixels[from..from + width * 4]);
    }
    frame
}

/// The average distance of the two pictures' channels, out of 255.
fn distance(one: &Rgba, other: &Rgba) -> f64 {
    assert_eq!((one.width, one.height), (other.width, other.height));
    let total: u64 = one
        .pixels
        .iter()
        .zip(&other.pixels)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    total as f64 / one.pixels.len() as f64
}

/// Whether `ours` is the page's picture: the very pixels at the size drawn for, and as close
/// as fitting allows at another.
fn is_the_pages(ours: &Rgba, page: &Rgba, cell: (u16, u16)) -> Result<(), String> {
    if cell == super::pictures::DRAWN_FOR {
        return (ours == page)
            .then_some(())
            .ok_or_else(|| "other pixels".to_owned());
    }
    let distance = distance(ours, page);
    (distance <= FITTED_WITHIN)
        .then_some(())
        .ok_or_else(|| format!("{distance:.2} apart"))
}

#[test]
fn the_header_sends_the_pictures_ghostty_draws() {
    let pictures = pictures_on_page();
    assert_eq!(pictures.len(), 11, "the page's pictures");
    let mut compared = 0;
    for (label, page_png) in pictures {
        let parts: Vec<&str> = label.split(", ").collect();
        let mascot = match parts[0] {
            "Argonaut" => Mascot::Argonaut,
            "Medusa" => Mascot::Medusa,
            "Hydra" => Mascot::Hydra,
            other => panic!("{other}"),
        };
        let look = match parts[1] {
            "normal" => Look::Normal,
            "light" => Look::Light,
            "ultracode" => Look::Ultracode,
            "codex" => Look::Codex,
            other => panic!("{other}"),
        };
        let cell = if parts.get(2) == Some(&"2x") {
            (18, 42)
        } else {
            (9, 21)
        };
        let page = png::decode(&page_png).expect("the page's picture");
        let (_, escapes) = Sent::first(Graphics::Kitty, mascot, look, cell, 1).unwrap();
        let sent = pictures_in(&escapes);
        let ours =
            |frame: usize| png::decode(&picture(mascot, look, cell, frame).unwrap()).unwrap();
        if !look.is_ultra() {
            assert_eq!(sent.len(), 1, "{label}");
            assert_eq!(png::decode(&sent[0]).unwrap(), ours(0), "{label}");
            is_the_pages(&ours(0), &page, cell).unwrap_or_else(|why| panic!("{label}: {why}"));
            compared += 1;
            continue;
        }
        let width = page.width / PAGE_FRAMES;
        assert_eq!(
            (width, page.height),
            (usize::from(cell.0) * 9, usize::from(cell.1) * 3)
        );
        let frames = mascot.frames();
        // The moments the page drew that a frame every 50 ms draws too.
        for at in (0..PAGE_FRAMES).filter(|at| at * frames % PAGE_FRAMES == 0) {
            let frame = at * frames / PAGE_FRAMES;
            is_the_pages(&ours(frame), &frame_of(&page, at, width), cell)
                .unwrap_or_else(|why| panic!("{label}: frame {frame} at the page's {at}: {why}"));
            compared += 1;
        }
    }
    // The stills, every frame of the medusa's and the hydra's strips, and the argonaut's two.
    assert_eq!(compared, 4 + 4 * PAGE_FRAMES + 2 * 2 + PAGE_FRAMES);
}
