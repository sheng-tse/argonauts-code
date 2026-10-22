//! The greeting on the first start in a terminal: the argonaut, the name in block letters,
//! the tagline and the version, shown once before setup, and again by `argonauts welcome`.
//! Also how lines are written to a plain terminal, in its colours where it has them.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::cell::{MARK_COL, MARK_MIN_WIDTH, TEXT_COL};
use crate::mascot::{self, Look, Mascot};
use crate::theme::{ColorLevel, Role, Theme, nearest_ansi16, nearest_ansi256};

/// Under the name.
pub const TAGLINE: &str = "Every coding agent, one orchestrator, you at the helm.";

/// What `argonauts welcome` says to do next, after an install.
pub const NEXT_STEP: &str =
    "Next: cd into a project and run argonauts. argonauts doctor checks your agent CLIs.";

/// The brand's amber, as the header's mascot and the banner use it.
const AMBER: (u8, u8, u8) = (0xE8, 0xA3, 0x3D);

/// The two words, three cells apart.
const WORDS: (&str, &str) = ("ARGONAUTS", "CODE");

/// Each letter on a grid five pixels high, two pixel rows to a cell, so the name stands three
/// rows tall beside the argonaut. Only half and full blocks, which every monospace font fills
/// edge to edge.
fn pixels(letter: char) -> &'static [&'static str; 5] {
    match letter {
        'A' => &[".##.", "#..#", "####", "#..#", "#..#"],
        'R' => &["###.", "#..#", "###.", "#.#.", "#..#"],
        'G' => &[".###", "#...", "#.##", "#..#", ".###"],
        'O' => &[".##.", "#..#", "#..#", "#..#", ".##."],
        'N' => &["#..#", "##.#", "#.##", "#..#", "#..#"],
        'U' => &["#..#", "#..#", "#..#", "#..#", ".##."],
        'T' => &["###", ".#.", ".#.", ".#.", ".#."],
        'S' => &[".###", "#...", ".##.", "...#", "###."],
        'C' => &[".###", "#...", "#...", "#...", ".###"],
        'D' => &["###.", "#..#", "#..#", "#..#", "###."],
        'E' => &["####", "#...", "###.", "#...", "####"],
        _ => &["....", "....", "....", "....", "...."],
    }
}

/// One letter as three rows of cells: two pixel rows to a cell, the last one blank.
fn letter(letter: char) -> [String; 3] {
    let rows = pixels(letter);
    let lit = |row: usize, x: usize| rows.get(row).is_some_and(|row| row.as_bytes()[x] == b'#');
    std::array::from_fn(|cell| {
        (0..rows[0].len())
            .map(|x| match (lit(cell * 2, x), lit(cell * 2 + 1, x)) {
                (false, false) => ' ',
                (true, false) => '▀',
                (false, true) => '▄',
                (true, true) => '█',
            })
            .collect()
    })
}

/// A word as three rows, its letters a cell apart.
fn word(text: &str) -> [String; 3] {
    let letters: Vec<[String; 3]> = text.chars().map(letter).collect();
    std::array::from_fn(|row| {
        letters
            .iter()
            .map(|cells| cells[row].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// The brand's amber as this terminal can show it.
fn amber(theme: &Theme) -> Style {
    let colour = match theme.level {
        ColorLevel::TrueColor => Some(Color::Rgb(AMBER.0, AMBER.1, AMBER.2)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_ansi256(AMBER))),
        ColorLevel::Ansi16 => Some(nearest_ansi16(AMBER)),
        ColorLevel::None => None,
    };
    colour.map_or_else(Style::default, |colour| Style::default().fg(colour))
}

/// The greeting: the argonaut where the header draws it with the name beside it, ARGONAUTS in
/// amber and CODE in the text colour, then the tagline with the version, and `next` under it.
pub fn greeting(version: &str, next: Option<&str>, theme: &Theme) -> Vec<Line<'static>> {
    drawn(version, next, theme, false)
}

/// The greeting, with the argonaut's cells left blank when a picture covers them.
fn drawn(version: &str, next: Option<&str>, theme: &Theme, picture: bool) -> Vec<Line<'static>> {
    let marks = if picture {
        std::array::from_fn(|_| vec![Span::raw(" ".repeat(mascot::WIDTH))])
    } else {
        mascot::rows(Mascot::Argonaut, Look::new(None, false), 0, theme)
    };
    let (first, second) = (word(WORDS.0), word(WORDS.1));
    let mut lines: Vec<Line<'static>> = marks
        .into_iter()
        .zip(first.into_iter().zip(second))
        .map(|(mark, (first, second))| {
            let mut spans = vec![Span::raw(" ".repeat(MARK_COL))];
            spans.extend(mark);
            let drawn: usize = spans.iter().map(Span::width).sum();
            spans.push(Span::raw(" ".repeat(TEXT_COL.saturating_sub(drawn))));
            spans.push(Span::styled(first, amber(theme)));
            spans.push(Span::raw("   "));
            spans.push(Span::styled(second, theme.style(Role::Text)));
            Line::from(spans)
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(format!("  {TAGLINE}"), theme.style(Role::Text)),
        Span::styled(format!("   v{version}"), theme.style(Role::Dim)),
    ]));
    if let Some(next) = next {
        lines.push(Line::from(Span::styled(
            format!("  {next}"),
            theme.style(Role::Dim),
        )));
    }
    lines
}

/// The usage line under the greeting on the first start: dim, with the command that stops the
/// reports drawn as a command.
pub fn usage_line(line: &str, command: &str, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let mut spans = vec![Span::raw("  ")];
    match line.split_once(command) {
        Some((before, after)) => {
            spans.push(Span::styled(before.to_owned(), dim));
            spans.push(Span::styled(
                command.to_owned(),
                theme.style(Role::Question),
            ));
            spans.push(Span::styled(after.to_owned(), dim));
        }
        None => spans.push(Span::styled(line.to_owned(), dim)),
    }
    Line::from(spans)
}

/// What the first start shows above setup's first question: the greeting, and under it the usage
/// line when this start is the one to show it.
pub fn first_start(
    version: &str,
    usage: Option<(&str, &str)>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    with_usage(greeting(version, None, theme), usage, theme)
}

fn with_usage(
    mut lines: Vec<Line<'static>>,
    usage: Option<(&str, &str)>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if let Some((line, command)) = usage {
        lines.push(Line::default());
        lines.push(usage_line(line, command, theme));
    }
    lines
}

/// The greeting as a history cell, for the screen to print before anything else. Its argonaut
/// takes the header's mascot cells: the three rows under the blank row on top, from
/// [`MARK_COL`], so a picture placed for the header's mascot fits it as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Greeting {
    pub version: String,
    /// The usage line and the command in it, when this start shows it.
    pub usage: Option<(String, String)>,
    /// Whether the screen places a picture of the argonaut over its cells. While the theme
    /// shows mascot pictures, those cells print blank, as the header's do.
    pub picture: bool,
}

impl crate::cell::HistoryCell for Greeting {
    fn lines(&self, ctx: &crate::render::RenderCtx<'_>) -> Vec<Line<'static>> {
        let usage = self
            .usage
            .as_ref()
            .map(|(line, command)| (line.as_str(), command.as_str()));
        let theme = ctx.theme;
        // A screen too narrow for the picture keeps the argonaut's cells, as the header does.
        let covered =
            self.picture && theme.mascot_pictures && usize::from(ctx.width) >= MARK_MIN_WIDTH;
        let width = usize::from(ctx.width);
        let mut greeting = drawn(&self.version, None, theme, covered);
        // Narrower than the block letters, the name is words, as the tagline is.
        if greeting.iter().any(|line| line.width() > width) {
            greeting = plainly(&self.version, width, theme, covered);
        }
        // A blank row on top, as the header leaves.
        let mut lines = vec![Line::default()];
        lines.extend(greeting);
        // The usage line wraps, so its way out is always on screen: it shows only once.
        if let Some((line, command)) = usage {
            lines.push(Line::default());
            let said = usage_line(line, command, theme);
            let indent = [Span::raw("  ")];
            lines.extend(crate::text::wrap_spans(
                &said.spans[1..],
                width,
                &indent,
                &indent,
            ));
        }
        crate::text::fit_lines(&mut lines, width);
        lines
    }

    fn takes_the_mascot_picture(&self) -> bool {
        self.picture
    }
}

/// The greeting with its name in words, for a screen `width` cells wide, narrower than its block
/// letters: the argonaut in its cells, as the header has it, then the name and the tagline.
fn plainly(version: &str, width: usize, theme: &Theme, picture: bool) -> Vec<Line<'static>> {
    let marks = if picture {
        std::array::from_fn(|_| vec![Span::raw(" ".repeat(mascot::WIDTH))])
    } else {
        mascot::rows(Mascot::Argonaut, Look::new(None, false), 0, theme)
    };
    let mut lines: Vec<Line<'static>> = marks
        .into_iter()
        .map(|mark| {
            let mut spans = vec![Span::raw(" ".repeat(MARK_COL))];
            spans.extend(mark);
            Line::from(spans)
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("  Argonauts", amber(theme)),
        Span::styled(" Code", theme.style(Role::Text)),
        Span::styled(format!("   v{version}"), theme.style(Role::Dim)),
    ]));
    let indent = [Span::raw("  ")];
    let tagline = [Span::styled(TAGLINE, theme.style(Role::Text))];
    lines.extend(crate::text::wrap_spans(&tagline, width, &indent, &indent));
    lines
}

/// `lines` as text for a plain terminal, each ending in a newline: with colour, each span in its
/// colours and weight as ANSI escapes; without, the characters alone.
pub fn printed(lines: &[Line<'_>], colour: bool) -> String {
    let mut out = String::new();
    for line in lines {
        let mut text = String::new();
        for span in &line.spans {
            let style = line.style.patch(span.style);
            let codes = if colour {
                escapes(style)
            } else {
                String::new()
            };
            if codes.is_empty() {
                text.push_str(&span.content);
            } else {
                text.push_str(&format!("\x1b[{codes}m{}\x1b[0m", span.content));
            }
        }
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

fn escapes(style: Style) -> String {
    let mut codes = Vec::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".to_owned());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2".to_owned());
    }
    codes.extend(style.fg.and_then(|colour| colour_code(colour, false)));
    codes.extend(style.bg.and_then(|colour| colour_code(colour, true)));
    codes.join(";")
}

fn colour_code(colour: Color, background: bool) -> Option<String> {
    let (base, bright, extended) = if background {
        (40, 100, 48)
    } else {
        (30, 90, 38)
    };
    let code = match colour {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => format!("{extended};2;{r};{g};{b}"),
        Color::Indexed(n) => format!("{extended};5;{n}"),
        Color::Black => base.to_string(),
        Color::Red => (base + 1).to_string(),
        Color::Green => (base + 2).to_string(),
        Color::Yellow => (base + 3).to_string(),
        Color::Blue => (base + 4).to_string(),
        Color::Magenta => (base + 5).to_string(),
        Color::Cyan => (base + 6).to_string(),
        Color::Gray => (base + 7).to_string(),
        Color::DarkGray => bright.to_string(),
        Color::LightRed => (bright + 1).to_string(),
        Color::LightGreen => (bright + 2).to_string(),
        Color::LightYellow => (bright + 3).to_string(),
        Color::LightBlue => (bright + 4).to_string(),
        Color::LightMagenta => (bright + 5).to_string(),
        Color::LightCyan => (bright + 6).to_string(),
        Color::White => (bright + 7).to_string(),
    };
    Some(code)
}

#[cfg(test)]
#[path = "welcome_tests.rs"]
mod tests;
