//! Pictures of screens, one character per terminal cell with its colours, and a comparison of
//! what the panel drew with them, cell by cell. A picture is a `.txt` of its characters and an
//! `.html` of the same characters in coloured spans, in `pictures/`.
//!
//! Colours in a picture are names, such as `--term-dim` or `--v-claude`; each stands for the
//! theme roles that draw it. A colour written as its value, such as `#b4472a`, is that colour
//! exactly. Text in the terminal's own colour may be drawn in the theme's normal or bright
//! text. A blank cell's text colour and weight can't be seen, so only its fill is compared.

use std::ops::Range;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use crate::theme::{ColorLevel, Glyphs, Role, Theme};

/// One cell of a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureCell {
    pub ch: char,
    /// The colour name of its text, or `None` for the terminal's own.
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
}

#[derive(Debug, Clone)]
pub struct Picture {
    pub name: String,
    pub rows: Vec<Vec<PictureCell>>,
    /// Roles a colour name stands for in this picture alone, beside the ones it always does.
    also: Vec<(&'static str, Role)>,
}

impl Picture {
    /// `pictures/<name>.txt` and its `.html`, which must hold the same characters.
    pub fn load(name: &str) -> Picture {
        Self::read(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/overview/pictures"),
            name,
        )
    }

    /// A whole screen's picture in the crate's `golden/`, of which a test compares the rows a
    /// panel draws.
    pub fn screen(name: &str) -> Picture {
        Self::read(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden"),
            name,
        )
    }

    fn read(dir: &std::path::Path, name: &str) -> Picture {
        let text = std::fs::read_to_string(dir.join(format!("{name}.txt")))
            .unwrap_or_else(|err| panic!("{name}.txt: {err}"));
        let html = std::fs::read_to_string(dir.join(format!("{name}.html")))
            .unwrap_or_else(|err| panic!("{name}.html: {err}"));
        let rows = parse_html(&html);
        let picture = Picture {
            name: name.to_owned(),
            rows,
            also: Vec::new(),
        };
        let from_text: Vec<&str> = text.lines().collect();
        assert_eq!(
            picture.text(),
            from_text,
            "{name}: the .txt and the .html disagree"
        );
        picture
    }

    /// A picture of `rows` made in a test, where a file would say no more.
    pub fn of(name: &str, rows: Vec<Vec<PictureCell>>) -> Picture {
        Picture {
            name: name.to_owned(),
            rows,
            also: Vec::new(),
        }
    }

    pub fn text(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect())
            .collect()
    }

    pub fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// In this picture `name` stands for `role` too, as where the page draws a model in a
    /// colour it gives the same value as that model's own.
    pub fn standing_for(mut self, name: &'static str, role: Role) -> Picture {
        self.also.push((name, role));
        self
    }

    /// The colours a cell named `name` may be drawn in.
    fn colors(&self, name: &str, theme: &Theme) -> Vec<Color> {
        if let Some(exact) = exact_color(name) {
            return vec![exact];
        }
        let also = self.also.iter().filter(|(known, _)| *known == name);
        roles(name)
            .iter()
            .chain(also.map(|(_, role)| role))
            .map(|role| theme.color(*role).unwrap_or(Color::Reset))
            .collect()
    }
}

/// `#b4472a` as the colour it is.
fn exact_color(name: &str) -> Option<Color> {
    let hex = u32::from_str_radix(name.strip_prefix('#')?, 16).ok()?;
    Some(Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
}

/// The theme pictures are drawn in: dark, in full colour, with Unicode glyphs.
pub fn theme() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// Where in a buffer a picture's rows and columns are compared.
#[derive(Debug, Clone)]
pub struct Region {
    /// The picture's rows to compare.
    pub rows: Range<usize>,
    /// The picture's columns to compare.
    pub columns: Range<usize>,
    /// The buffer cell the picture's top left corner falls on.
    pub at: (u16, u16),
}

impl Region {
    /// The whole picture, its top left corner on `at`.
    pub fn whole(picture: &Picture, at: (u16, u16)) -> Region {
        Region {
            rows: 0..picture.rows.len(),
            columns: 0..picture.width(),
            at,
        }
    }
}

/// Every cell in `region` where `buf` is different from the picture, in characters, colours or
/// weight, one line each.
pub fn differences(picture: &Picture, buf: &Buffer, region: &Region, theme: &Theme) -> Vec<String> {
    let mut found = Vec::new();
    for row in region.rows.clone() {
        let cells = &picture.rows[row];
        for column in region.columns.clone() {
            let blank = PictureCell {
                ch: ' ',
                fg: None,
                bg: None,
                bold: false,
            };
            let want = cells.get(column).unwrap_or(&blank);
            let x = region.at.0 + column as u16;
            let y = region.at.1 + (row - region.rows.start) as u16;
            let Some(got) = buf.cell((x, y)) else {
                found.push(format!("row {row} column {column}: outside the buffer"));
                continue;
            };
            let symbol = got.symbol();
            let ch = symbol.chars().next().unwrap_or(' ');
            let place = format!("row {row} column {column}");
            if ch != want.ch {
                found.push(format!(
                    "{place}: {:?} where the picture has {:?}",
                    ch, want.ch
                ));
                continue;
            }
            if !matches_bg(picture, want.bg.as_deref(), got.bg, theme) {
                found.push(format!(
                    "{place} {:?}: fill {:?} where the picture has {:?}",
                    ch, got.bg, want.bg
                ));
            }
            if ch == ' ' {
                continue;
            }
            if !matches_fg(picture, want.fg.as_deref(), got.fg, theme) {
                found.push(format!(
                    "{place} {:?}: colour {:?} where the picture has {:?}",
                    ch, got.fg, want.fg
                ));
            }
            let bold = got.modifier.contains(Modifier::BOLD);
            if bold != want.bold {
                found.push(format!(
                    "{place} {:?}: bold {bold} where the picture has {}",
                    ch, want.bold
                ));
            }
        }
    }
    found
}

/// Fails with the cells that are different, the first few in full.
pub fn assert_matches(picture: &Picture, buf: &Buffer, region: &Region, theme: &Theme) {
    let whole = region.rows == (0..picture.rows.len()) && region.columns.start == 0;
    let name = if whole {
        picture.name.clone()
    } else {
        format!(
            "{}-rows-{}-{}",
            picture.name, region.rows.start, region.rows.end
        )
    };
    let at = (
        usize::from(region.at.0) + region.columns.start,
        usize::from(region.at.1),
    );
    let size = (region.columns.len(), region.rows.len());
    crate::picture_dump::rows(&name, &crate::picture_dump::cut(buf, at, size));
    let found = differences(picture, buf, region, theme);
    if found.is_empty() {
        return;
    }
    let drawn: Vec<String> = region
        .rows
        .clone()
        .map(|row| {
            let y = region.at.1 + (row - region.rows.start) as u16;
            region
                .columns
                .clone()
                .map(|column| {
                    buf.cell((region.at.0 + column as u16, y))
                        .map_or(" ", |cell| cell.symbol())
                        .to_owned()
                })
                .collect()
        })
        .collect();
    panic!(
        "{}: {} cells are different\n{}\n\ndrawn:\n{}\n\npicture:\n{}",
        picture.name,
        found.len(),
        found
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        drawn.join("\n"),
        picture.text()[region.rows.clone()].join("\n")
    );
}

/// The roles a picture's colour name stands for.
fn roles(name: &str) -> &'static [Role] {
    match name {
        "--term-fg" => &[Role::Text, Role::Bright],
        "--term-dim" => &[Role::Dim],
        "--term-faint" => &[
            Role::Faint,
            Role::ModBorder,
            Role::Rule,
            Role::BarTrack,
            Role::Denominator,
        ],
        "--term-acc" => &[Role::Question, Role::Accent],
        "--term-grn" => &[Role::Success],
        "--term-amb" => &[Role::Warn],
        "--term-red" => &[Role::Danger],
        // A picture may name a CLI's colour by its hue, which is the same colour on the page.
        "--term-org" => &[Role::AgentOrange, Role::CliClaude],
        "--term-blu" => &[Role::AgentBlue, Role::CliCodex],
        "--term-cyn" => &[Role::AgentCyan, Role::CliAgy],
        "--term-pur" => &[Role::Ultracode, Role::AgentPurple],
        "--term-bg" => &[Role::BadgeText],
        "--term-band" => &[Role::UserBand],
        "--v-claude" => &[Role::CliClaude],
        "--v-codex" => &[Role::CliCodex],
        "--v-agy" => &[Role::CliAgy],
        _ => &[],
    }
}

fn matches_fg(picture: &Picture, want: Option<&str>, got: Color, theme: &Theme) -> bool {
    let colors: Vec<Color> = match want {
        None => vec![Color::Reset],
        Some(name) => picture.colors(name, theme),
    };
    // The terminal's own colour is the theme's normal or bright text.
    let own = [Role::Text, Role::Bright].map(|role| theme.color(role).unwrap_or(Color::Reset));
    colors.contains(&got) || ((want.is_none() || want == Some("--term-fg")) && own.contains(&got))
}

fn matches_bg(picture: &Picture, want: Option<&str>, got: Color, theme: &Theme) -> bool {
    match want {
        None => got == Color::Reset,
        Some(name) => picture.colors(name, theme).contains(&got),
    }
}

/// The cells of a picture's `.html`: spans with a style, and text between them.
fn parse_html(html: &str) -> Vec<Vec<PictureCell>> {
    let start = html
        .find("<pre")
        .and_then(|at| html[at..].find('>').map(|end| at + end + 1));
    let body = &html[start.unwrap_or(0)..];
    let body = &body[..body.find("</pre>").unwrap_or(body.len())];
    let mut rows = vec![Vec::new()];
    let mut rest = body;
    while !rest.is_empty() {
        let (text, style, after) = if let Some(open) = rest.strip_prefix("<span") {
            let tag_end = open.find('>').expect("a span's tag ends");
            let style = attribute(&open[..tag_end], "style");
            let inner = &open[tag_end + 1..];
            let close = inner.find("</span>").expect("a span ends");
            (&inner[..close], style, &inner[close + "</span>".len()..])
        } else {
            let next = rest.find("<span").unwrap_or(rest.len());
            (&rest[..next], String::new(), &rest[next..])
        };
        let (fg, bg, bold) = parse_style(&style);
        for ch in unescape(text).chars() {
            if ch == '\n' {
                rows.push(Vec::new());
                continue;
            }
            rows.last_mut().unwrap().push(PictureCell {
                ch,
                fg: fg.clone(),
                bg: bg.clone(),
                bold,
            });
        }
        rest = after;
    }
    rows
}

fn attribute(tag: &str, name: &str) -> String {
    let key = format!("{name}=\"");
    tag.find(&key)
        .map(|at| {
            let value = &tag[at + key.len()..];
            value[..value.find('"').unwrap_or(value.len())].to_owned()
        })
        .unwrap_or_default()
}

/// `color:var(--term-dim);background:var(--term-band);font-weight:700`.
fn parse_style(style: &str) -> (Option<String>, Option<String>, bool) {
    let mut fg = None;
    let mut bg = None;
    let mut bold = false;
    for part in style.split(';') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let name = value
            .strip_prefix("var(")
            .and_then(|name| name.strip_suffix(')'))
            .or_else(|| value.starts_with('#').then_some(value))
            .map(str::to_owned);
        match key.trim() {
            "color" => fg = name,
            "background" => bg = name,
            "font-weight" => bold = value == "700" || value == "bold",
            _ => {}
        }
    }
    (fg, bg, bold)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_reads_the_same_from_its_text_and_its_colours() {
        let picture = Picture::load("status_clis");
        assert_eq!(picture.width(), 118);
        let first = &picture.rows[8][2];
        assert_eq!(first.ch, '◆');
        assert_eq!(first.fg.as_deref(), Some("--term-acc"));
        // `who you&#x27;re` reads as one apostrophe in one cell.
        assert!(picture.text()[8].contains("who you're signed in as"));
    }

    /// A buffer painted from the picture's own cells matches it, and one changed cell is one
    /// difference.
    #[test]
    fn the_comparison_finds_each_cell_that_is_different() {
        let picture = Picture::load("limits_after");
        let theme = theme();
        let color = |name: Option<&str>| {
            name.and_then(|name| roles(name).first())
                .and_then(|role| theme.color(*role))
                .unwrap_or(Color::Reset)
        };
        let area = ratatui::layout::Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        for (y, row) in picture.rows.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let drawn = &mut buf[(x as u16 + 1, y as u16)];
                drawn.set_char(cell.ch);
                drawn.fg = color(cell.fg.as_deref());
                drawn.bg = color(cell.bg.as_deref());
                if cell.bold {
                    drawn.modifier.insert(Modifier::BOLD);
                }
            }
        }
        let whole = Region::whole(&picture, (1, 0));
        assert_matches(&picture, &buf, &whole, &theme);
        buf[(3, 1)].fg = Color::Reset;
        let found = differences(&picture, &buf, &whole, &theme);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].starts_with("row 1 column 2 '◆': colour"),
            "{found:#?}"
        );
    }
}
