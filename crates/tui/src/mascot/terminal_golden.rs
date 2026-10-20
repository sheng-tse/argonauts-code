//! The pictures the header sends, checked against the picture of the header in Ghostty with
//! Iosevka at 17 px (cells of 9 x 21 pixels, and 18 x 42 on a Retina screen): each still the
//! very bytes drawn there, and each frame of an ultra look the pixels of its place in the strip
//! that page steps through. The page's own font is left out of the copy kept here.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::graphics::{Graphics, pictures_in};
use super::png::{self, Rgba};
use super::{FRAMES, Look, Mascot, Sent};

const PAGE: &str = include_str!("../../golden/mascot_terminal.html");

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

#[test]
fn the_header_sends_the_pictures_ghostty_draws() {
    let pictures = pictures_on_page();
    assert_eq!(pictures.len(), 11, "the page's pictures");
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
        let (_, escapes) = Sent::first(Graphics::Kitty, mascot, look, cell, 1).unwrap();
        let sent = pictures_in(&escapes);
        if !look.is_ultra() {
            assert_eq!(sent, [page_png], "{label}: the same bytes");
            continue;
        }
        assert_eq!(sent.len(), FRAMES, "{label}");
        let strip = png::decode(&page_png).expect("the page's strip of frames");
        let width = strip.width / FRAMES;
        assert_eq!(
            (width, strip.height),
            (usize::from(cell.0) * 9, usize::from(cell.1) * 3)
        );
        for (at, frame) in sent.iter().enumerate() {
            let frame = png::decode(frame).unwrap();
            assert!(
                frame == frame_of(&strip, at, width),
                "{label}: frame {at} is not the page's"
            );
        }
    }
}
