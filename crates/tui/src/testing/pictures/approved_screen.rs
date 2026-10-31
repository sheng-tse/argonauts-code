//! A drawn buffer as the screen `approved/` drawings are held to, for tests that draw a panel
//! and compare it with its approved drawing.

use argonauts_testkit::approved::{self, Drawing, Palette, differences_at, report};
use ratatui::buffer::Buffer;

use crate::base::stability::grid::on_screen;
use crate::base::theme::Theme;
use crate::testing::pictures::golden;

/// The theme the drawings were made in, with logos drawn as pictures.
pub(crate) fn theme() -> Theme {
    let mut theme = golden::theme();
    theme.logo_pictures = true;
    theme
}

/// Panics with the differences where `buf`, its first row at row `top` of a screen
/// `cols` × `rows`, is different from the drawing `file` under `approved/`. Its logos count as
/// shown, since the app sends their pictures beside the buffer.
pub(crate) fn assert_matches(file: &str, buf: &Buffer, (cols, rows): (u16, u16), top: u16) {
    approved::assert_matches(file, &on_screen(buf, (cols, rows), top));
}

/// [`assert_matches`] for the drawing `file` from its row `from` down, for a panel that
/// draws only the rows under the ones another part of the screen draws.
pub(crate) fn assert_rows_match(
    file: &str,
    from: usize,
    buf: &Buffer,
    (cols, rows): (u16, u16),
    top: u16,
) {
    let path = approved::dir().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{file}: {err}"));
    let drawing = Drawing::parse(
        file.rsplit('/').next().unwrap_or(file),
        &rows_from(&text, from),
    )
    .unwrap_or_else(|err| panic!("{file}: {err:#}"));
    let drawn = on_screen(buf, (cols, rows), top);
    let placement = drawing.place(&drawn);
    let found = differences_at(
        &drawing,
        &placement,
        &drawn,
        &Palette::TOKYONIGHT_MOON,
        &|_, _| false,
    );
    if !found.is_empty() {
        panic!("{}", report(file, &drawing, &drawn, &placement, &found));
    }
}

/// A drawing's file with its first `from` rows of text and of paint left out.
fn rows_from(text: &str, from: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let at = |what: &str| lines.iter().position(|line| *line == what);
    let (Some(text_at), Some(paint_at)) = (at("text"), at("paint")) else {
        return text.to_owned();
    };
    let height = paint_at - text_at - 1;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines[..=text_at] {
        out.push(
            match line
                .strip_prefix("size ")
                .and_then(|size| size.split_once('x'))
            {
                Some((width, _)) => format!("size {width}x{}", height - from),
                None => (*line).to_owned(),
            },
        );
    }
    out.extend(
        lines[text_at + 1 + from..=paint_at]
            .iter()
            .map(|line| line.to_string()),
    );
    out.extend(
        lines[paint_at + 1 + from..]
            .iter()
            .map(|line| line.to_string()),
    );
    out.join("\n")
}
