//! The colour set as its picture has it: each CLI's three shapes in its hue, then the grey
//! of the rest and the states' colours, on a dark screen and beside it on a light one.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::{Glyphs, Role, Theme};
use crate::kit::{Kit, fit};
use crate::overview::golden::{self, Picture, Region};
use crate::overview::shades::{OWN_TEXTURES, shade, texture};

/// The CLIs as the picture names them, with their adapter ids.
const CLIS: [(&str, &str); 4] = [
    ("Claude Code", "claude-code"),
    ("Codex", "codex"),
    ("agy", "antigravity-cli"),
    ("OpenCode", "opencode"),
];

/// The colours that aren't a CLI's, each drawn in full.
const OTHERS: [(&str, Role); 4] = [
    ("+N more", Role::CatOther),
    ("success", Role::Success),
    ("warning", Role::Warn),
    ("error", Role::Danger),
];

/// The width of the names, and of each screen's swatches.
const NAME: usize = 13;
const SWATCH: usize = 8;

/// A CLI's first three models, two cells of each one's shape in its colour.
fn textures(theme: &Theme, cli: &str) -> Vec<Span<'static>> {
    let kit = Kit::new(theme);
    let hue = Role::for_cli(cli);
    (0..OWN_TEXTURES)
        .map(|place| Span::styled(texture(&kit, place).repeat(2), shade(&kit, hue, place)))
        .collect()
}

fn full(theme: &Theme, role: Role) -> Vec<Span<'static>> {
    vec![Span::styled("████", theme.style(role))]
}

#[test]
fn the_colour_set_matches_its_picture() {
    let picture = Picture::screen("palette", "colour_set");
    let [dark, light] = [Theme::dark(), Theme::light()].map(|mut theme| {
        theme.glyphs = Glyphs::UNICODE;
        theme
    });
    let swatches = CLIS
        .iter()
        .map(|(name, cli)| (*name, textures(&dark, cli), textures(&light, cli)))
        .chain(
            OTHERS
                .iter()
                .map(|(name, role)| (*name, full(&dark, *role), full(&light, *role))),
        );
    let rows: Vec<Vec<Span<'static>>> = swatches
        .map(|(name, on_dark, on_light)| {
            let mut row = fit(vec![Span::raw(name)], NAME);
            row.extend(fit(on_dark, SWATCH));
            row.extend(on_light);
            row
        })
        .collect();

    let area = Rect::new(0, 0, picture.width() as u16, rows.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, row) in rows.into_iter().enumerate() {
        buf.set_line(0, y as u16, &Line::from(row), area.width);
    }
    golden::assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &dark);
}
