//! Pictures of screens, one character per terminal cell with its colours, and a comparison of
//! what the panel drew with them, cell by cell. A picture is a `.txt` of its characters and an
//! `.html` of the same characters in coloured spans, in the crate's `tests/golden/`.
//!
//! Colours in a picture are names, such as `--term-dim` or `--v-claude`; each stands for the
//! theme roles that draw it. A colour written as its value, such as `#6a9ee6`, is that colour
//! exactly. Text in the terminal's own colour may be drawn in the theme's normal or bright
//! text. A blank cell's text colour and weight can't be seen, so only its fill is compared.

use std::ops::Range;
use std::path::PathBuf;

use argonauts_testkit::pictures::picture;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use crate::base::theme::{ColorLevel, Glyphs, Role, Theme};

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
    /// `tests/golden/overview/<name>.txt` and its `.html`, which must hold the same characters.
    pub fn load(name: &str) -> Picture {
        Self::screen("overview", name)
    }

    /// The picture `name` of a screen in `area`, a folder of the crate's `tests/golden/`, of
    /// which a test compares the rows a panel draws.
    pub fn screen(area: &str, name: &str) -> Picture {
        Self::read(&golden_folder(area), name)
    }

    fn read(dir: &std::path::Path, name: &str) -> Picture {
        let (text, html) = picture::read(dir, name);
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

/// A folder of the crate's pictures, named for the screens it holds.
pub(crate) fn golden_folder(area: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(area)
}

/// `#6a9ee6` as the colour it is.
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

/// The screen a golden drew, as text, into the folder `ARGONAUTS_PICTURES_OUT` names, for
/// someone to set beside its picture. Nothing is written without it.
pub fn keep(name: &str, buf: &Buffer) {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") else {
        return;
    };
    let rows: Vec<String> = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    // A picture of two screens is drawn twice: the second goes beside the first.
    let dir = std::path::Path::new(&dir);
    let file = (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.txt")),
            n => dir.join(format!("{name}.{n}.txt")),
        })
        .find(|file| !file.exists())
        .unwrap_or_else(|| dir.join(format!("{name}.txt")));
    if let Err(err) = std::fs::write(&file, rows.join("\n") + "\n") {
        panic!("{}: {err}", file.display());
    }
    if let Some(stem) = file.file_stem().and_then(|stem| stem.to_str()) {
        crate::testing::pictures::picture_dump::screen_page_in(dir, stem, buf, &theme());
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
    crate::testing::pictures::picture_dump::rows(
        &name,
        &crate::testing::pictures::picture_dump::cut(buf, at, size),
    );
    crate::testing::pictures::picture_dump::buffer_page(&name, buf, at, size, theme);
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
        "--term-amb" => &[Role::Warn],
        "--term-red" => &[Role::Danger],
        // A picture may name a CLI's colour by its hue, which is the same colour on the page.
        "--term-grn" => &[Role::Success, Role::AgentGreen],
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

/// The cells of a picture's first grid.
fn parse_html(html: &str) -> Vec<Vec<PictureCell>> {
    cells_of(&picture::grid(html))
}

fn cells_of(grid: &[Vec<picture::Cell>]) -> Vec<Vec<PictureCell>> {
    grid.iter()
        .map(|row| {
            row.iter()
                .map(|cell| PictureCell {
                    ch: cell.ch,
                    fg: colour_name(cell.color.as_deref()),
                    bg: colour_name(cell.background.as_deref()),
                    bold: cell.bold,
                })
                .collect()
        })
        .collect()
}

/// `var(--term-dim)` as `--term-dim`, and `#6a9ee6` as itself. Any other colour is one no
/// picture here names.
fn colour_name(value: Option<&str>) -> Option<String> {
    let value = value?;
    value
        .strip_prefix("var(")
        .and_then(|name| name.strip_suffix(')'))
        .or_else(|| value.starts_with('#').then_some(value))
        .map(str::to_owned)
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
        buf[(3, 1)].fg = color(picture.rows[1][2].fg.as_deref());

        // The same one cell changed in the picture's page instead.
        let (_, html) = picture::read(&golden_folder("overview"), "limits_after");
        let changed = |edit: &dyn Fn(&mut picture::Cell)| {
            let page = picture::edited(&html, |grid| edit(&mut grid[1][2]));
            let picture = Picture::of("limits_after", parse_html(&page));
            differences(&picture, &buf, &whole, &theme)
        };
        assert!(changed(&|_| {}).is_empty());
        let recoloured = changed(&|cell| cell.color = Some("var(--term-red)".to_owned()));
        assert_eq!(recoloured.len(), 1, "{recoloured:#?}");
        let respelled = changed(&|cell| cell.ch = '◇');
        assert_eq!(respelled.len(), 1, "{respelled:#?}");
        let bolder = changed(&|cell| cell.bold ^= true);
        assert_eq!(bolder.len(), 1, "{bolder:#?}");
        let filled = changed(&|cell| cell.background = Some("var(--term-band)".to_owned()));
        assert_eq!(filled.len(), 1, "{filled:#?}");
    }
}
