//! Screens drawn with logos as pictures, for their goldens. A placeholder cell's character is
//! one no font draws, so a picture of such a screen writes each logo's two cells as `◧◨`, in
//! the colour that names the logo's picture to the terminal, such as `#4c4f02` for Codex's.

use ratatui::buffer::Buffer;
use ratatui::text::Line;

use super::LOGO_WIDTH;
use crate::base::mascot::placeholder;
use crate::base::theme::Theme;

/// What a picture writes for each of a logo's cells, left to right.
pub const STAND_INS: [&str; LOGO_WIDTH as usize] = ["◧", "◨"];

/// `theme`, drawing logos as pictures, as in a terminal that holds them.
pub fn pictured(theme: &Theme) -> Theme {
    let mut theme = theme.clone();
    theme.logo_pictures = true;
    theme
}

/// `buf` with each logo's placeholder cells written as its picture writes them, in their own
/// colour. Fails on a placeholder cell that isn't a logo's.
pub fn with_stand_ins(buf: &Buffer) -> Buffer {
    let mut shown = buf.clone();
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let symbol = buf[(x, y)].symbol();
            if !symbol.starts_with(placeholder::PLACEHOLDER) {
                continue;
            }
            let column = (0..LOGO_WIDTH)
                .find(|column| placeholder::cell(0, *column).as_deref() == Some(symbol))
                .unwrap_or_else(|| panic!("({x}, {y}): {symbol:?} is no logo's cell"));
            shown[(x, y)].set_symbol(STAND_INS[usize::from(column)]);
        }
    }
    shown
}

/// How many logos `buf` draws as pictures.
pub fn count(buf: &Buffer) -> usize {
    let first = placeholder::cell(0, 0).unwrap_or_default();
    buf.content()
        .iter()
        .filter(|cell| cell.symbol() == first)
        .count()
}

/// `lines` with each logo's placeholder cells written as its picture writes them, as
/// [`with_stand_ins`] does for a buffer.
pub fn lines_with_stand_ins(lines: &[Line<'static>]) -> Vec<Line<'static>> {
    let cells: Vec<(String, &str)> = (0..LOGO_WIDTH)
        .filter_map(|column| placeholder::cell(0, column))
        .zip(STAND_INS)
        .collect();
    let mut shown = lines.to_vec();
    for span in shown.iter_mut().flat_map(|line| line.spans.iter_mut()) {
        if span.content.contains(placeholder::PLACEHOLDER) {
            let content = (cells.iter())
                .fold(span.content.to_string(), |content, (cell, written)| {
                    content.replace(cell.as_str(), written)
                });
            span.content = content.into();
        }
    }
    shown
}

/// The logos `line` draws as pictures, in order, each by the mark its picture shows and the
/// number it wears, if any: `codex`, or `claude-code 2`. Fails on a placeholder that names no
/// logo.
pub fn logos_drawn(line: &Line<'_>) -> Vec<String> {
    let mut drawn = Vec::new();
    for span in &line.spans {
        if !span.content.starts_with(placeholder::PLACEHOLDER) {
            continue;
        }
        let Some(ratatui::style::Color::Rgb(red, green, blue)) = span.style.fg else {
            panic!(
                "{:?}: a picture's cells with no picture's colour",
                span.content
            );
        };
        let id = u32::from_be_bytes([0, red, green, blue]);
        let (mark, number) = argonauts_core::look::logos::LogoMark::of_picture(id)
            .unwrap_or_else(|| panic!("{id:#x} is no logo's picture"));
        let name = format!("{mark:?}").to_lowercase();
        drawn.push(match number {
            Some(number) => format!("{name} {number}"),
            None => name,
        });
    }
    drawn
}

/// A drawn line's text as the council's pictures write it: each logo's picture as `▐▌`, and a
/// numbered logo's as `▐` and its number, `▐1`. Fails on a placeholder that names no logo.
pub fn council_text(line: &Line<'_>) -> String {
    let mut text = String::new();
    for span in &line.spans {
        if !span.content.starts_with(placeholder::PLACEHOLDER) {
            text.push_str(&span.content);
            continue;
        }
        let Some(ratatui::style::Color::Rgb(red, green, blue)) = span.style.fg else {
            panic!(
                "{:?}: a picture's cells with no picture's colour",
                span.content
            );
        };
        let id = u32::from_be_bytes([0, red, green, blue]);
        match super::numbered::number_of(id) {
            Some(number) => text.push_str(&format!("▐{number}")),
            None => text.push_str("▐▌"),
        }
    }
    text
}
