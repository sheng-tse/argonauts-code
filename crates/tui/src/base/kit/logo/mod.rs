//! Logos: the mark of whoever serves a model, before its name. A logo always takes
//! [`LOGO_WIDTH`] cells, whatever draws it, so a row lines up the same with logos on or off.
//! [`logo_spans`] draws one in cells, [`ModelLabel`] draws one with the name after it, worded by
//! whether the picture draws, and [`logo_png`] gives its picture to anything that draws pictures,
//! such as the world's galaxies.
//!
//! In a terminal that draws the kitty protocol's Unicode placeholders, the cells show the logo's
//! picture, made by [`send_pictures`]. The terminal takes the logos for the whole session, and
//! `terminal::Kept` gives them to each screen before its first cells, and again after every entry
//! to the alternate screen or a program you ran. Everywhere else, and with `ui.logos = "off"`,
//! the cells hold the serving agent's colour dot.
//!
//! The pictures ship in the crate: `assets/logos`, made from the SVGs there by `logos.py`.

use argonauts_core::settings::ui::Logos;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::base::mascot::placeholder;
use crate::base::text::marks::decoration;
use crate::base::theme::{Brightness, ColorLevel, Role, Theme};

use argonauts_core::look::logos::{LOGO_CELL_PIXELS, Logo, LogoMark};

/// Cells a logo takes in a row, whatever draws it.
pub const LOGO_WIDTH: u16 = 2;

/// Cells a logo drawn as a picture takes before a name: the picture and the blank after it.
pub const LOGO_BEFORE: u16 = LOGO_WIDTH + 1;

/// A logo in cells, [`LOGO_WIDTH`] wide. Where the theme draws logos as pictures, the
/// placeholders of the logo's picture; elsewhere, and for a logo with no picture, the serving
/// agent's colour dot and a blank for the dot to spill into, since fonts such as Iosevka draw it
/// two cells wide. Either way it only decorates the name, so copied text leaves it out.
pub fn logo_spans(logo: &Logo, theme: &Theme) -> Vec<Span<'static>> {
    if let Some(picture) = picture_of(logo, theme) {
        return vec![picture];
    }
    let dot = if theme.glyphs.is_ascii() { "*" } else { "●" };
    let style = decoration(theme.style(Role::for_cli(&logo.agent.0)));
    vec![Span::styled(dot, style), Span::raw(" ")]
}

/// A logo in a row that has a mark of its own for the agent where no picture draws, such as
/// `● ` for the CLI in use and `○ ` for the rest: the logo's picture and the blank every logo
/// has before what it names where it draws one, else `mark`, whose blank is the dot's own
/// right half.
pub fn logo_or(logo: &Logo, theme: &Theme, mark: Span<'static>) -> Vec<Span<'static>> {
    match picture_of(logo, theme) {
        Some(picture) => vec![picture, Span::raw(" ")],
        None => vec![mark],
    }
}

/// A logo before a name that has no mark where no picture draws: the logo's picture where it
/// draws one, [`LOGO_WIDTH`] cells, and the one blank every logo has before what it names.
/// Nothing elsewhere, so other terminals keep the name as it was.
pub fn logo_before(logo: &Logo, theme: &Theme) -> Vec<Span<'static>> {
    match picture_of(logo, theme) {
        Some(picture) => vec![picture, Span::raw(" ")],
        None => Vec::new(),
    }
}

/// A logo's picture alone where it draws one, for a logo that names something by itself with
/// nothing after it. Nothing elsewhere.
pub fn logo_picture(logo: &Logo, theme: &Theme) -> Vec<Span<'static>> {
    picture_of(logo, theme).into_iter().collect()
}

/// The logo as `theme` draws it, for wording the name after it: itself where its picture draws,
/// else with no mark, since the colour dot in its place doesn't say who made the model, so a
/// name beside it stays whole.
pub fn as_drawn(logo: &Logo, theme: &Theme) -> Logo {
    Logo {
        agent: logo.agent.clone(),
        mark: logo.mark.filter(|_| theme.logo_pictures),
    }
}

fn picture_of(logo: &Logo, theme: &Theme) -> Option<Span<'static>> {
    let mark = logo.mark.filter(|_| theme.logo_pictures)?;
    Some(picture_cells(mark, theme.brightness))
}

/// The two placeholder cells of `mark`'s picture in the ink for a screen as bright as `on`.
fn picture_cells(mark: LogoMark, on: Brightness) -> Span<'static> {
    placeholder_cells(picture_id(mark, on))
}

/// The two placeholder cells that show picture `id`.
fn placeholder_cells(id: u32) -> Span<'static> {
    let cells: String = (0..LOGO_WIDTH)
        .filter_map(|column| placeholder::cell(0, column))
        .collect();
    let picture = Style::new().fg(placeholder::id_colour(id));
    Span::styled(cells, decoration(picture))
}

/// The badge of a seat whose model sits more than once, [`LOGO_WIDTH`] cells: its logo with
/// `number` in a circle at its lower right where logos draw as pictures, else the number in a
/// circle, `①`, in `style` and a blank, since a dot can't carry a number.
pub fn numbered_spans(logo: &Logo, number: u8, style: Style, theme: &Theme) -> Vec<Span<'static>> {
    if let Some(mark) = logo.mark.filter(|_| theme.logo_pictures)
        && (1..=numbered::MOST).contains(&number)
    {
        numbered::want(mark, number);
        return vec![placeholder_cells(numbered::id(
            mark,
            number,
            theme.brightness,
        ))];
    }
    vec![Span::styled(circled(number), style), Span::raw(" ")]
}

/// The escapes that send the numbered logos a screen drew since the last call, for cells
/// `cell` pixels in size. Empty while there are none.
pub fn send_numbered(cell: Option<(u16, u16)>) -> Vec<u8> {
    numbered::unsent(cell)
}

/// The escapes that send every numbered logo drawn so far, drawn anew for cells `cell` pixels
/// in size, as the plain logos are after a font size change calls for another [`LogoSize`].
pub fn resend_numbered(cell: Option<(u16, u16)>) -> Vec<u8> {
    numbered::all_drawn(cell)
}

/// ① to ⑳, and the number itself past them.
pub fn circled(number: u8) -> String {
    match number {
        1..=20 => char::from_u32(0x2460 + u32::from(number) - 1)
            .map(String::from)
            .unwrap_or_default(),
        _ => number.to_string(),
    }
}

/// Whether logos draw as pictures with `ui.logos` set to `logos` on `theme`: the terminal takes
/// the logos this session, a cell's colour can carry a picture's id (true colour), and a placeholder
/// takes the one column the layouts count for it (ambiguous glyphs one cell wide, as
/// `ambiguous_width` says).
pub fn draws_pictures(logos: Logos, theme: &Theme, ambiguous_width: u8) -> bool {
    takes_logos() && draws_once_sent(logos, theme, ambiguous_width)
}

/// [`draws_pictures`] once the terminal takes the logos, as when `ui.logos` turns on before the
/// next frame.
pub fn draws_once_sent(logos: Logos, theme: &Theme, ambiguous_width: u8) -> bool {
    logos == Logos::Auto && theme.level == ColorLevel::TrueColor && ambiguous_width != 2
}

/// The escapes that send every logo's picture for cells `cell` pixels in size, in each ink it
/// ships in, each with the virtual placement its placeholders show. Made again only when the
/// cell size calls for another [`LogoSize`]; with no cell size, the double pictures, which a
/// terminal scales down rather than up.
pub fn send_pictures(cell: Option<(u16, u16)>) -> Vec<u8> {
    let size = LogoSize::for_terminal(cell);
    let mut out = Vec::new();
    for mark in LogoMark::ALL {
        let inks: &[Brightness] = if one_colour(mark) {
            &[Brightness::Dark, Brightness::Light]
        } else {
            &[Brightness::Dark]
        };
        for &on in inks {
            out.extend(placeholder::send(
                mark_png(mark, size, on),
                picture_id(mark, on),
                (LOGO_WIDTH, 1),
            ));
        }
    }
    out
}

fn picture_id(mark: LogoMark, on: Brightness) -> u32 {
    mark.picture_id(None, on == Brightness::Light && one_colour(mark))
}

/// What becomes of the logos' pictures in a terminal that can take them, where `ui.logos` was
/// off when the app started: none were sent and the terminal wasn't asked, so turning logos on
/// sends them before the next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Later {
    /// Nothing waits: the pictures went out at start, or the terminal can't take them.
    Nothing,
    /// The terminal can take them, and none were sent.
    Unsent,
    /// `ui.logos` turned on: they go out before the next frame.
    Due,
}

/// Notes that the terminal can take the logos' pictures that `ui.logos = "off"` kept from it.
pub fn hold_for_later() {
    later_here::store(Later::Unsent);
}

/// `ui.logos` turned on: pictures held for later go out before the next frame. True when some
/// will.
pub fn send_held() -> bool {
    match later_here::load() {
        Later::Unsent | Later::Due => {
            later_here::store(Later::Due);
            true
        }
        Later::Nothing => false,
    }
}

/// The escapes that send the logos' pictures `ui.logos` turned on for, once: from then on the
/// terminal takes the logos, and each screen gets them as it does the ones sent at start.
pub fn held_pictures(cell: Option<(u16, u16)>) -> Option<Vec<u8>> {
    if later_here::load() != Later::Due {
        return None;
    }
    later_here::store(Later::Nothing);
    set_takes_logos(true);
    Some(send_pictures(cell))
}

#[cfg(not(test))]
mod later_here {
    use std::sync::Mutex;

    use super::Later;

    static LATER: Mutex<Later> = Mutex::new(Later::Nothing);

    pub fn store(later: Later) {
        *LATER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = later;
    }

    pub fn load() -> Later {
        *LATER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod later_here {
    use std::cell::Cell;

    use super::Later;

    thread_local! {
        static LATER: Cell<Later> = const { Cell::new(Later::Nothing) };
    }

    pub fn store(later: Later) {
        LATER.with(|cell| cell.set(later));
    }

    pub fn load() -> Later {
        LATER.with(Cell::get)
    }
}

/// Says whether the terminal takes the logos' pictures this session, as its answer at start
/// says or as `ui.logos` turning on does. It says nothing of which screens hold them yet.
pub fn set_takes_logos(takes: bool) {
    takes_here::store(takes);
}

/// Whether the terminal takes the logos' pictures this session.
pub fn takes_logos() -> bool {
    takes_here::load()
}

/// One value for the whole process, since the pictures go to its one terminal.
#[cfg(not(test))]
mod takes_here {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKES: AtomicBool = AtomicBool::new(false);

    pub fn store(takes: bool) {
        TAKES.store(takes, Ordering::Relaxed);
    }

    pub fn load() -> bool {
        TAKES.load(Ordering::Relaxed)
    }
}

/// Unit tests run in parallel threads, so each keeps its own value and one that says the
/// terminal takes the logos can't turn another's dots into pictures.
#[cfg(test)]
mod takes_here {
    use std::cell::Cell;

    thread_local! {
        static TAKES: Cell<bool> = const { Cell::new(false) };
    }

    pub fn store(takes: bool) {
        TAKES.with(|cell| cell.set(takes));
    }

    pub fn load() -> bool {
        TAKES.with(Cell::get)
    }
}

/// A logo's picture as a PNG at one of the sizes it ships in, inked for a screen as bright as
/// `on`: what lies behind the picture, which is the theme's brightness for a picture in a row.
/// `None` for a logo with no picture, drawn as its dot.
pub fn logo_png(logo: &Logo, size: LogoSize, on: Brightness) -> Option<&'static [u8]> {
    logo.mark.map(|mark| mark_png(mark, size, on))
}

/// A shipped logo's picture as a PNG: 8-bit RGBA without interlacing, which the crate's own PNG
/// reader takes.
pub fn mark_png(mark: LogoMark, size: LogoSize, on: Brightness) -> &'static [u8] {
    let shipped = shipped(mark);
    let inked = match (on, &shipped.on_light) {
        (Brightness::Light, Some(on_light)) => on_light,
        _ => &shipped.on_dark,
    };
    match size {
        LogoSize::Cells => inked.cells,
        LogoSize::Cells2x => inked.cells_2x,
        LogoSize::Px64 => inked.px64,
        LogoSize::Px128 => inked.px128,
    }
}

/// Whether a logo draws in one colour, the screen's text colour, and so ships in a light ink for
/// dark screens and a dark one for light screens. A coloured logo keeps its colours on both.
pub fn one_colour(mark: LogoMark) -> bool {
    shipped(mark).on_light.is_some()
}

/// The sizes each logo ships in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogoSize {
    /// Two cells of 18 × 42 px, the cell size the app's pictures are drawn for, with the logo
    /// 28 px square in their middle, as a row shows it.
    Cells,
    /// The same at twice the size, so a terminal with larger cells scales a logo down rather
    /// than up.
    Cells2x,
    /// The logo filling a square, for pictures that draw it themselves.
    Px64,
    Px128,
}

impl LogoSize {
    pub const ALL: [LogoSize; 4] = [
        LogoSize::Cells,
        LogoSize::Cells2x,
        LogoSize::Px64,
        LogoSize::Px128,
    ];

    /// Width and height in pixels.
    pub fn pixels(self) -> (u32, u32) {
        match self {
            LogoSize::Cells => (36, 42),
            LogoSize::Cells2x => (72, 84),
            LogoSize::Px64 => (64, 64),
            LogoSize::Px128 => (128, 128),
        }
    }

    /// The two-cell picture for a terminal whose cells are `cell` pixels wide and tall: the
    /// double one once a cell is larger than [`LOGO_CELL_PIXELS`] either way.
    pub fn for_cell(cell: (u16, u16)) -> LogoSize {
        if cell.0 > LOGO_CELL_PIXELS.0 || cell.1 > LOGO_CELL_PIXELS.1 {
            LogoSize::Cells2x
        } else {
            LogoSize::Cells
        }
    }

    /// [`Self::for_cell`] for a terminal that reports its cells `cell` pixels in size, or the
    /// double picture where it reports none, which a terminal scales down rather than up.
    pub fn for_terminal(cell: Option<(u16, u16)>) -> LogoSize {
        cell.map_or(LogoSize::Cells2x, LogoSize::for_cell)
    }

    /// The smallest square at least `pixels` wide, or the largest for anything bigger.
    pub fn square(pixels: u32) -> LogoSize {
        if pixels <= 64 {
            LogoSize::Px64
        } else {
            LogoSize::Px128
        }
    }
}

/// One logo's pictures at every size, in one ink.
struct Sizes {
    cells: &'static [u8],
    cells_2x: &'static [u8],
    px64: &'static [u8],
    px128: &'static [u8],
}

struct Shipped {
    on_dark: Sizes,
    /// A one-colour logo's pictures in the dark ink.
    on_light: Option<Sizes>,
}

macro_rules! picture {
    ($stem:literal, $size:literal, $ink:literal) => {
        include_bytes!(concat!(
            "../../../../assets/logos/",
            $stem,
            "-",
            $size,
            $ink,
            ".png"
        ))
    };
}

macro_rules! sizes {
    ($stem:literal, $ink:literal) => {
        Sizes {
            cells: picture!($stem, "cells", $ink),
            cells_2x: picture!($stem, "cells-2x", $ink),
            px64: picture!($stem, "64", $ink),
            px128: picture!($stem, "128", $ink),
        }
    };
}

macro_rules! coloured {
    ($stem:literal) => {
        Shipped {
            on_dark: sizes!($stem, ""),
            on_light: None,
        }
    };
}

macro_rules! two_inks {
    ($stem:literal) => {
        Shipped {
            on_dark: sizes!($stem, ""),
            on_light: Some(sizes!($stem, "-on-light")),
        }
    };
}

static CLAUDE_CODE: Shipped = coloured!("claudecode");
static CODEX: Shipped = coloured!("codex");
static ANTIGRAVITY: Shipped = coloured!("antigravity");
static OPENCODE: Shipped = two_inks!("opencode");
static OLLAMA: Shipped = two_inks!("ollama");
static QWEN: Shipped = coloured!("qwen");
static KIMI: Shipped = two_inks!("kimi");
static OPENAI: Shipped = two_inks!("openai");
static MISTRAL: Shipped = coloured!("mistral");
static META: Shipped = coloured!("meta");
static OPENROUTER: Shipped = coloured!("openrouter");

fn shipped(mark: LogoMark) -> &'static Shipped {
    match mark {
        LogoMark::ClaudeCode => &CLAUDE_CODE,
        LogoMark::Codex => &CODEX,
        LogoMark::Antigravity => &ANTIGRAVITY,
        LogoMark::OpenCode => &OPENCODE,
        LogoMark::Ollama => &OLLAMA,
        LogoMark::Qwen => &QWEN,
        LogoMark::Kimi => &KIMI,
        LogoMark::OpenAi => &OPENAI,
        LogoMark::Mistral => &MISTRAL,
        LogoMark::Meta => &META,
        LogoMark::OpenRouter => &OPENROUTER,
    }
}

mod numbered;

mod label;
pub use label::{ModelLabel, NoPicture};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub mod golden;
