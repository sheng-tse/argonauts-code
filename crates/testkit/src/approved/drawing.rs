//! A drawing's file, as the approved pictures are exported: a header, a character and a key
//! for each cell, and what each key paints. [`Drawing::parse`] reads one; [`drawing_text`]
//! writes a part of a screen as one.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, anyhow, bail, ensure};

use super::drawn::Drawn;
use super::paint::{
    Palette, Terminal, blank, logo_half, logo_id, logo_of, shows_fill, shows_glyph,
};
use super::version;
use crate::screen::{Cell, Colour, Grid, Placeholder, Rgb};

/// What a drawing's text holds in a cell a logo or a picture covers.
pub const COVERED: char = '\u{FFFC}';

/// The key of a blank on the terminal's own background, which every drawing gives.
pub(crate) const BLANK: char = '.';
const BLANK_WORDS: &str = "blank";

/// The key of a cell whose character alone is drawn.
const ANY: char = '?';
const ANY_WORDS: &str = "the character alone";

/// The note on a key whose text colour is the terminal's own.
const OWN_NOTE: &str = "the terminal's own";

/// The rectangle of the screen a drawing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub top: u16,
    pub left: u16,
    pub rows: u16,
    pub cols: u16,
}

impl Region {
    pub fn whole(cols: u16, rows: u16) -> Region {
        Region {
            top: 0,
            left: 0,
            rows,
            cols,
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        (self.top..self.top + self.rows).contains(&row)
            && (self.left..self.left + self.cols).contains(&col)
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bottom = self.top + self.rows.saturating_sub(1);
        let right = self.left + self.cols.saturating_sub(1);
        write!(f, "rows {}-{bottom}, cols {}-{right}", self.top, self.left)
    }
}

/// Where a drawing's first cell sits on its screen, counted from 0: `None` where the drawing
/// doesn't say, so the comparison finds it by its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct At {
    pub row: Option<u16>,
    pub col: Option<u16>,
}

/// A colour by one of its names, such as a theme role's, and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: Option<String>,
    pub rgb: Rgb,
}

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{} {name}", self.rgb),
            None => write!(f, "{}", self.rgb),
        }
    }
}

/// What a key paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paint {
    /// A blank on the terminal's own background.
    Blank,
    /// A blank on this background.
    Fill(Rgb),
    /// A character in this colour, on this background or else the terminal's own.
    Ink {
        ink: Rgb,
        fill: Option<Rgb>,
        bold: bool,
        italic: bool,
    },
    /// A cell of a logo's picture, which covers two cells, numbered for a seat.
    Logo { stem: String, number: Option<u8> },
    /// A cell of any other picture, by its name.
    Picture(String),
    /// A cell whose character alone is compared, in whatever colours.
    Character,
}

impl fmt::Display for Paint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Paint::Blank => f.write_str(BLANK_WORDS),
            Paint::Fill(fill) => write!(f, "bg={fill}"),
            Paint::Ink {
                ink,
                fill,
                bold,
                italic,
            } => {
                write!(f, "fg={ink}")?;
                if let Some(fill) = fill {
                    write!(f, " bg={fill}")?;
                }
                if *bold {
                    f.write_str(" bold")?;
                }
                if *italic {
                    f.write_str(" italic")?;
                }
                Ok(())
            }
            Paint::Logo { stem, number: None } => write!(f, "logo={stem}"),
            Paint::Logo {
                stem,
                number: Some(number),
            } => write!(f, "logo={stem}-{number}"),
            Paint::Picture(name) => write!(f, "picture={name}"),
            Paint::Character => f.write_str(ANY_WORDS),
        }
    }
}

/// A key's paint, and the names its line gives the paint's colours after a `;`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    paint: Paint,
    notes: String,
}

impl Key {
    /// The names the notes give the key's colours: the text colour's plain, the background's
    /// after `on`, and a blank's background's plain.
    fn named(&self) -> Vec<Named> {
        let (ink, fill) = match self.paint {
            Paint::Ink { ink, fill, .. } => (Some(ink), fill),
            Paint::Fill(fill) => (None, Some(fill)),
            _ => return Vec::new(),
        };
        self.notes
            .split(',')
            .map(str::trim)
            .filter(|note| !note.is_empty() && *note != OWN_NOTE)
            .filter_map(|note| {
                let (name, rgb) = match note.strip_prefix("on ") {
                    Some(name) => (name, fill?),
                    None => (note, ink.or(fill)?),
                };
                Some(Named {
                    name: Some(name.to_owned()),
                    rgb,
                })
            })
            .collect()
    }
}

/// One cell of a drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub text: char,
    pub key: char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawing {
    /// What the drawing shows, in one line.
    pub title: String,
    /// The terminal size it is drawn for.
    pub cols: u16,
    pub rows: u16,
    pub at: At,
    /// Its own size, in cells.
    pub width: u16,
    pub height: u16,
    /// The terminal's own colours, which its unstyled cells are painted in.
    pub terminal: Terminal,
    /// Its rows of text as the file writes them, for a report.
    lines: Vec<String>,
    cells: Vec<Wanted>,
    keys: BTreeMap<char, Key>,
}

impl Drawing {
    pub fn load(path: &Path) -> anyhow::Result<Drawing> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the drawing {}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
        Drawing::parse(name, &text)
    }

    /// `name` is the file's name, `<state>@<cols>x<rows>.txt`, whose size its `screen` line
    /// must repeat.
    pub fn parse(name: &str, text: &str) -> anyhow::Result<Drawing> {
        let named_size =
            size_of(name).ok_or_else(|| anyhow!("{name} isn't named <state>@<cols>x<rows>.txt"))?;
        let mut lines = text.lines().enumerate().map(|(at, line)| (at + 1, line));
        let mut next = |what: &str| {
            lines
                .next()
                .ok_or_else(|| anyhow!("the drawing ends before {what}"))
        };
        let (_, title) = next("its description")?;
        let mut header = Header::default();
        loop {
            let (number, line) = next("its text")?;
            if line == "text" {
                break;
            }
            header
                .read(line)
                .with_context(|| format!("line {number}, {line:?}"))?;
        }
        let shape = header.shape(named_size)?;
        let (width, height) = (usize::from(shape.width), usize::from(shape.height));

        let mut shown = Vec::with_capacity(height);
        let mut cells = Vec::with_capacity(width * height);
        for _ in 0..height {
            let (number, line) = next("its last row of text")?;
            let row = text_row(line, width).with_context(|| format!("line {number}"))?;
            cells.extend(row.into_iter().map(|text| Wanted { text, key: BLANK }));
            shown.push(line.to_owned());
        }
        expect_line(next("its paint")?, "paint")?;
        for row in 0..height {
            let (number, line) = next("its last row of paint")?;
            let keys: Vec<char> = line.chars().collect();
            ensure!(
                keys.len() <= width,
                "line {number}: {} keys for {width} cells",
                keys.len()
            );
            for (cell, key) in cells[row * width..].iter_mut().zip(keys) {
                cell.key = key;
            }
        }
        version::as_own(&mut shown, &mut cells, width, version::OWN);
        expect_line(next("its key")?, "key")?;
        let mut keys = BTreeMap::new();
        for (number, line) in lines {
            let (key, paint) = key_line(line).with_context(|| format!("line {number}"))?;
            ensure!(
                keys.insert(key, paint).is_none(),
                "line {number}: the key {key:?} is given twice"
            );
        }
        let drawing = Drawing {
            title: title.to_owned(),
            cols: named_size.0,
            rows: named_size.1,
            at: shape.at,
            width: shape.width,
            height: shape.height,
            terminal: shape.terminal,
            lines: shown,
            cells,
            keys,
        };
        drawing.check_keys()?;
        Ok(drawing)
    }

    /// That every cell's key is given and fits its character, and every key given is painted.
    fn check_keys(&self) -> anyhow::Result<()> {
        ensure!(
            self.keys.contains_key(&BLANK),
            "the key gives no `{BLANK} {BLANK_WORDS}`"
        );
        for (at, cell) in self.cells.iter().enumerate() {
            let width = usize::from(self.width);
            let (row, col) = (at / width, at % width);
            let key = self.keys.get(&cell.key).ok_or_else(|| {
                anyhow!(
                    "row {row}, col {col}: the key {:?} is painted but never given",
                    cell.key
                )
            })?;
            let fits = match key.paint {
                Paint::Logo { .. } | Paint::Picture(_) => cell.text == COVERED,
                Paint::Blank | Paint::Fill(_) => cell.text == ' ',
                Paint::Ink { .. } => cell.text != ' ' && cell.text != COVERED,
                Paint::Character => cell.text != COVERED,
            };
            ensure!(
                fits,
                "row {row}, col {col}: {:?} can't be painted {}",
                cell.text,
                key.paint
            );
        }
        let unpainted: Vec<char> = self
            .keys
            .keys()
            .copied()
            .filter(|&key| key != BLANK && !self.cells.iter().any(|cell| cell.key == key))
            .collect();
        ensure!(
            unpainted.is_empty(),
            "no cell is painted with the keys {unpainted:?}"
        );
        Ok(())
    }

    /// The drawing's cell at its own `row` and `col`, counted from its first cell, and its
    /// paint.
    pub(crate) fn wanted(&self, row: u16, col: u16) -> Option<(Wanted, &Paint)> {
        if row >= self.height || col >= self.width {
            return None;
        }
        let cell = *self
            .cells
            .get(usize::from(row) * usize::from(self.width) + usize::from(col))?;
        let key = self.keys.get(&cell.key)?;
        Some((cell, &key.paint))
    }

    /// Which half of its logo a logo cell is: logos sit in pairs of cells from the first of a
    /// run.
    pub(crate) fn logo_half(&self, row: u16, col: u16) -> usize {
        let paint = self.wanted(row, col).map(|(_, paint)| paint);
        let mut start = col;
        while start > 0 && self.wanted(row, start - 1).map(|(_, paint)| paint) == paint {
            start -= 1;
        }
        usize::from(col - start) % 2
    }

    /// The drawing's own cells as a grid, painted as its keys say, its logos' placeholders
    /// showing their pictures, so the screen laws read it as they read a screen.
    pub fn grid(&self) -> Grid {
        let mut cells = Vec::with_capacity(usize::from(self.width) * usize::from(self.height));
        for row in 0..self.height {
            for col in 0..self.width {
                cells.push(match self.wanted(row, col) {
                    Some((wanted, paint)) => {
                        painted_cell(wanted.text, paint, || self.logo_half(row, col))
                    }
                    None => Cell::default(),
                });
            }
        }
        Grid::from_cells(self.width, self.height, cells)
    }

    /// The drawing's own row `row` of text, as its file writes it.
    pub(crate) fn line(&self, row: u16) -> Option<&str> {
        self.lines.get(usize::from(row)).map(String::as_str)
    }

    /// Every colour the drawing's key names, for writing a screen beside it in the same words.
    pub fn names(&self) -> Vec<Named> {
        self.keys.values().flat_map(Key::named).collect()
    }

    /// The first name the drawing gives `rgb`.
    pub(crate) fn name_of(&self, rgb: Rgb) -> Option<String> {
        self.names()
            .into_iter()
            .find(|named| named.rgb == rgb)
            .and_then(|named| named.name)
    }
}

/// A drawing's cell as a screen's: `text` painted `paint`, a logo's placeholder for the half of
/// it `half` says, and any other picture's under no id.
fn painted_cell(text: char, paint: &Paint, half: impl Fn() -> usize) -> Cell {
    let placeholder = |id: u32| Cell {
        text: logo_half(half()),
        ink: Colour::Rgb(Rgb::from_id(id)),
        picture: Some(Placeholder { id, shows: true }),
        ..Cell::default()
    };
    match *paint {
        Paint::Blank => Cell::default(),
        Paint::Fill(fill) => Cell {
            background: Colour::Rgb(fill),
            ..Cell::default()
        },
        Paint::Ink {
            ink,
            fill,
            bold,
            italic,
        } => Cell {
            text: text.to_string(),
            ink: Colour::Rgb(ink),
            background: fill.map_or(Colour::Own, Colour::Rgb),
            bold,
            italic,
            ..Cell::default()
        },
        Paint::Logo { ref stem, number } => placeholder(logo_id(stem, number).unwrap_or(0)),
        Paint::Picture(_) => placeholder(0),
        Paint::Character => Cell::plain(&text.to_string()),
    }
}

/// The header's lines between the description and `text`, each given once.
#[derive(Default)]
struct Header {
    screen: Option<(u16, u16)>,
    at: Option<At>,
    size: Option<(u16, u16)>,
    terminal: Option<Terminal>,
}

/// What the header says of the drawing's size and place.
struct Shape {
    at: At,
    width: u16,
    height: u16,
    terminal: Terminal,
}

impl Header {
    fn read(&mut self, line: &str) -> anyhow::Result<()> {
        let (word, value) = line.split_once(' ').unwrap_or((line, ""));
        let fresh = match word {
            "screen" => self.screen.replace(cells_of(value)?).is_none(),
            "at" => self.at.replace(at_of(value)?).is_none(),
            "size" => self.size.replace(cells_of(value)?).is_none(),
            "terminal" => self.terminal.replace(terminal_of(value)?).is_none(),
            _ => bail!("expected screen, at, size or terminal before `text`"),
        };
        ensure!(fresh, "{word} is given twice");
        Ok(())
    }

    /// The drawing's place and size, which must fit the screen its file is named for.
    fn shape(self, (cols, rows): (u16, u16)) -> anyhow::Result<Shape> {
        let missing = |word: &str| anyhow!("the header gives no {word} line");
        let screen = self.screen.ok_or_else(|| missing("screen"))?;
        let at = self.at.ok_or_else(|| missing("at"))?;
        let (width, height) = self.size.ok_or_else(|| missing("size"))?;
        let terminal = self.terminal.ok_or_else(|| missing("terminal"))?;
        ensure!(
            screen == (cols, rows),
            "its file is named for a {cols}x{rows} screen, but it says screen {}x{}",
            screen.0,
            screen.1
        );
        let fits = |at: Option<u16>, size: u16, room: u16| {
            size <= room && at.is_none_or(|at| u32::from(at) + u32::from(size) <= u32::from(room))
        };
        ensure!(
            fits(at.col, width, cols) && fits(at.row, height, rows),
            "a {width}x{height} drawing{} doesn't fit the {cols}x{rows} screen",
            at_words(at)
        );
        Ok(Shape {
            at,
            width,
            height,
            terminal,
        })
    }
}

fn at_words(at: At) -> String {
    let part = |at: Option<u16>| at.map_or_else(|| "?".to_owned(), |at| at.to_string());
    format!(" at {},{}", part(at.row), part(at.col))
}

/// `<cols>x<rows>`, neither 0.
fn cells_of(value: &str) -> anyhow::Result<(u16, u16)> {
    let parsed = value.split_once('x').and_then(|(cols, rows)| {
        let size: (u16, u16) = (cols.parse().ok()?, rows.parse().ok()?);
        (size.0 > 0 && size.1 > 0).then_some(size)
    });
    parsed.ok_or_else(|| anyhow!("{value:?} isn't <cols>x<rows>"))
}

/// `<row>,<col>`, either `?`.
fn at_of(value: &str) -> anyhow::Result<At> {
    let part = |part: &str| -> anyhow::Result<Option<u16>> {
        if part == "?" {
            return Ok(None);
        }
        part.parse()
            .map(Some)
            .map_err(|_| anyhow!("{part:?} isn't a number or ?"))
    };
    let (row, col) = value
        .split_once(',')
        .ok_or_else(|| anyhow!("{value:?} isn't <row>,<col>"))?;
    Ok(At {
        row: part(row)?,
        col: part(col)?,
    })
}

/// `fg=#rrggbb bg=#rrggbb`.
fn terminal_of(value: &str) -> anyhow::Result<Terminal> {
    let mut words = value.split(' ');
    let mut colour = |name: &str| -> anyhow::Result<Rgb> {
        let word = words.next().unwrap_or_default();
        let hex = word
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or_else(|| anyhow!("expected fg=#rrggbb bg=#rrggbb"))?;
        colour_of(hex)
    };
    let terminal = Terminal {
        text: colour("fg")?,
        background: colour("bg")?,
    };
    ensure!(words.next().is_none(), "expected fg=#rrggbb bg=#rrggbb");
    Ok(terminal)
}

fn colour_of(hex: &str) -> anyhow::Result<Rgb> {
    Rgb::parse(hex).ok_or_else(|| anyhow!("{hex:?} isn't a colour like #828bb8"))
}

fn expect_line((number, line): (usize, &str), word: &str) -> anyhow::Result<()> {
    ensure!(
        line == word,
        "line {number}: {line:?} where the rows end and {word:?} starts"
    );
    Ok(())
}

/// A row of text, a character a cell, padded with blanks to `width`. Each character must take
/// one cell as the terminal measures it, since the screen is read the same way.
fn text_row(line: &str, width: usize) -> anyhow::Result<Vec<char>> {
    let mut row: Vec<char> = line.chars().collect();
    ensure!(
        row.len() <= width,
        "{} characters for {width} cells",
        row.len()
    );
    if let Some(control) = row.iter().find(|c| c.is_control()) {
        bail!("{control:?} isn't a character a screen shows");
    }
    if !row.is_empty() {
        // Room for every character at two cells, so a wide one shows rather than wrapping.
        let room = u16::try_from(row.len() * 2).unwrap_or(u16::MAX);
        let screen = Grid::from_text(room, &[line]);
        for (col, &want) in row.iter().enumerate() {
            let cell = u16::try_from(col)
                .ok()
                .and_then(|col| screen.get(0, col))
                .filter(|cell| !cell.wide && cell.text.chars().eq([want]));
            ensure!(
                cell.is_some(),
                "col {col}: {want:?} isn't one cell wide on a terminal"
            );
        }
    }
    row.resize(width, ' ');
    Ok(row)
}

/// `<key> <paint>`, then any notes after `;`.
fn key_line(line: &str) -> anyhow::Result<(char, Key)> {
    let mut chars = line.chars();
    let key = chars
        .next()
        .filter(|key| !key.is_whitespace() && !key.is_control())
        .ok_or_else(|| anyhow!("a key line starts with its key"))?;
    let rest = chars
        .as_str()
        .strip_prefix(' ')
        .ok_or_else(|| anyhow!("a key is one character, then a space and what it paints"))?;
    let (paint, notes) = rest.split_once(';').unwrap_or((rest, ""));
    let paint = paint_of(key, paint.trim()).with_context(|| format!("the key {key:?}"))?;
    Ok((
        key,
        Key {
            paint,
            notes: notes.trim().to_owned(),
        },
    ))
}

fn paint_of(key: char, words: &str) -> anyhow::Result<Paint> {
    match (key, words) {
        (BLANK, BLANK_WORDS) => return Ok(Paint::Blank),
        (ANY, ANY_WORDS) => return Ok(Paint::Character),
        (BLANK, _) => bail!("{BLANK:?} is `{BLANK} {BLANK_WORDS}`"),
        (ANY, _) => bail!("{ANY:?} is `{ANY} {ANY_WORDS}`"),
        (_, BLANK_WORDS) => bail!("only {BLANK:?} is {BLANK_WORDS}"),
        (_, ANY_WORDS) => bail!("only {ANY:?} is {ANY_WORDS}"),
        _ => {}
    }
    if let Some(name) = words.strip_prefix("logo=") {
        return logo_named(name);
    }
    if let Some(name) = words.strip_prefix("picture=") {
        ensure!(
            !name.is_empty() && !name.contains(char::is_whitespace),
            "picture= names one picture"
        );
        return Ok(Paint::Picture(name.to_owned()));
    }
    let (mut ink, mut fill, mut bold, mut italic) = (None, None, false, false);
    for word in words.split_whitespace() {
        let fresh = match word.split_once('=') {
            Some(("fg", hex)) => ink.replace(colour_of(hex)?).is_none(),
            Some(("bg", hex)) => fill.replace(colour_of(hex)?).is_none(),
            None if word == "bold" => !std::mem::replace(&mut bold, true),
            None if word == "italic" => !std::mem::replace(&mut italic, true),
            _ => bail!("{word:?} isn't fg=#rrggbb, bg=#rrggbb, bold or italic"),
        };
        ensure!(fresh, "{word:?} is given twice");
    }
    match (ink, fill) {
        (Some(ink), fill) => Ok(Paint::Ink {
            ink,
            fill,
            bold,
            italic,
        }),
        (None, Some(fill)) if !bold && !italic => Ok(Paint::Fill(fill)),
        (None, Some(_)) => bail!("a blank shows no weight or slant"),
        (None, None) => bail!("a key paints fg=, bg=, logo= or picture="),
    }
}

/// `codex`, or `codex-2` for the codex logo numbered 2.
fn logo_named(name: &str) -> anyhow::Result<Paint> {
    let numbered = name
        .rsplit_once('-')
        .and_then(|(stem, number)| Some((stem, number.parse::<u8>().ok()?)));
    let (stem, number) = match numbered {
        Some((stem, number)) => (stem, Some(number)),
        None => (name, None),
    };
    ensure!(logo_id(stem, number).is_some(), "no logo is named {name:?}");
    Ok(Paint::Logo {
        stem: stem.to_owned(),
        number,
    })
}

/// The screen size a drawing's file name gives, columns then rows.
pub(crate) fn size_of(name: &str) -> Option<(u16, u16)> {
    let (state, size) = name.strip_suffix(".txt")?.rsplit_once('@')?;
    let size = cells_of(size).ok()?;
    (!state.is_empty()).then_some(size)
}

/// The keys a written drawing gives, in order, as the exported drawings give them.
const KEY_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!$%&*+-/:<=>@^_~";

/// `region` of `drawn` written as a drawing, on a terminal whose own colours are `palette`'s,
/// its colours noted by their names in `names`, so a screen can be read beside the drawing it
/// should match.
pub fn drawing_text(
    drawn: &Drawn,
    region: Region,
    palette: &Palette,
    names: &[Named],
) -> anyhow::Result<String> {
    let mut text = format!(
        "The screen as the app drew it\nscreen {}x{}\nat {},{}\nsize {}x{}\n\
         terminal fg={} bg={}\ntext\n",
        drawn.cols(),
        drawn.rows(),
        region.top,
        region.left,
        region.cols,
        region.rows,
        palette.text,
        palette.background
    );
    let rows = region.top..region.top + region.rows;
    for row in rows.clone() {
        text.push_str(&drawn.text_of(row, region.left, region.cols));
        text.push('\n');
    }
    text.push_str("paint\n");
    let mut given: Vec<Paint> = Vec::new();
    for row in rows {
        let mut keys = String::new();
        for col in region.left..region.left + region.cols {
            let paint = paint_of_cell(drawn, row, col, palette);
            if paint == Paint::Blank {
                keys.push(BLANK);
                continue;
            }
            let place = match given.iter().position(|known| *known == paint) {
                Some(place) => place,
                None => {
                    given.push(paint);
                    given.len() - 1
                }
            };
            let key = KEY_CHARS
                .chars()
                .nth(place)
                .ok_or_else(|| anyhow!("the screen paints more ways than a drawing has keys"))?;
            keys.push(key);
        }
        text.push_str(keys.trim_end_matches(BLANK));
        text.push('\n');
    }
    text.push_str("key\n");
    text.push_str(&format!("{BLANK} {BLANK_WORDS}\n"));
    for (key, paint) in KEY_CHARS.chars().zip(&given) {
        let notes = notes_of(paint, palette, names);
        if notes.is_empty() {
            text.push_str(&format!("{key} {paint}\n"));
        } else {
            text.push_str(&format!("{key} {paint}  ; {notes}\n"));
        }
    }
    Ok(text)
}

/// What a screen's cell paints, in a drawing's words: a picture where it shows, a background
/// where it shows and isn't the terminal's own, and weight and slant where the glyph shows them.
fn paint_of_cell(drawn: &Drawn, row: u16, col: u16, palette: &Palette) -> Paint {
    let painted = drawn.cell(row, col).painted(palette);
    if let Some(picture) = painted.picture.filter(|picture| picture.shows) {
        return match logo_of(picture.id) {
            Some((stem, number)) => Paint::Logo {
                stem: stem.to_owned(),
                number,
            },
            None => Paint::Picture(format!("{:06x}", picture.id)),
        };
    }
    // A placeholder whose picture its screen doesn't hold shows nothing but its background.
    if painted.picture.is_some() || blank(painted.text) {
        return if painted.fill == palette.background {
            Paint::Blank
        } else {
            Paint::Fill(painted.fill)
        };
    }
    let glyph = shows_glyph(painted.text);
    Paint::Ink {
        ink: painted.ink,
        fill: (shows_fill(painted.text) && painted.fill != palette.background)
            .then_some(painted.fill),
        bold: glyph && painted.bold,
        italic: glyph && painted.italic,
    }
}

/// A written key's notes, as the exported drawings note theirs: the text colour's names, the
/// background's after `on`.
fn notes_of(paint: &Paint, palette: &Palette, names: &[Named]) -> String {
    let names_of = |rgb: Rgb| {
        names
            .iter()
            .filter(move |named| named.rgb == rgb)
            .filter_map(|named| named.name.clone())
    };
    let mut notes: Vec<String> = Vec::new();
    match *paint {
        Paint::Ink { ink, fill, .. } => {
            if ink == palette.text {
                notes.push(OWN_NOTE.to_owned());
            }
            notes.extend(names_of(ink));
            if let Some(fill) = fill {
                notes.extend(names_of(fill).map(|name| format!("on {name}")));
            }
        }
        Paint::Fill(fill) => notes.extend(names_of(fill)),
        _ => {}
    }
    notes.dedup();
    notes.join(", ")
}
