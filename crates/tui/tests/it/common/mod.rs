// Each test file uses its own part of these helpers.
#![allow(dead_code)]

use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use argonauts_tui::base::draw::render::{Clock, RenderCtx};
use argonauts_tui::base::text::str_width;
use argonauts_tui::base::theme::{ColorLevel, Glyphs, Role, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

/// Unicode glyphs, so snapshots read the same on every OS.
pub fn theme() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

pub fn theme_at(level: ColorLevel) -> Theme {
    let mut theme = Theme::dark().with_level(level);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

pub fn t0() -> Instant {
    static T0: OnceLock<Instant> = OnceLock::new();
    *T0.get_or_init(Instant::now)
}

pub fn at(seconds: f64) -> Instant {
    t0() + Duration::from_secs_f64(seconds)
}

/// `elapsed` is in seconds after [`t0`].
pub fn ctx(theme: &Theme, width: u16, elapsed: f64) -> RenderCtx<'_> {
    RenderCtx::new(width, theme, Clock::new(t0(), at(elapsed)))
}

/// A frame with a still clock, the way history prints.
pub fn still(theme: &Theme, width: u16) -> RenderCtx<'_> {
    RenderCtx::new(width, theme, Clock::still(t0()))
}

pub fn text(lines: &[Line<'_>]) -> String {
    argonauts_tui::base::text::plain_text(lines)
}

pub fn row_text(line: &Line<'_>) -> String {
    text(std::slice::from_ref(line))
}

pub fn row_styled(line: &Line<'_>) -> String {
    styled(std::slice::from_ref(line))
}

/// `{bg=#2a2e3f}` opens a row whose fill runs to the right edge, and `[fg=#98c379 b]●[/]` is a
/// styled span. Modifiers are `b` bold, `d` dim, `i` italic, `u` underlined, `r` reversed, and
/// `s` crossed out, with a `-` in front when the style removes one.
pub fn styled(lines: &[Line<'_>]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let fill = describe(line.style);
        if !fill.is_empty() {
            write!(out, "{{{fill}}}").unwrap();
        }
        for span in &line.spans {
            let style = describe(span.style);
            if style.is_empty() {
                out.push_str(&span.content);
            } else {
                write!(out, "[{style}]{}[/]", span.content).unwrap();
            }
        }
    }
    out
}

fn describe(style: Style) -> String {
    let mut parts = Vec::new();
    if let Some(fg) = style.fg {
        parts.push(format!("fg={}", color_name(fg)));
    }
    if let Some(bg) = style.bg {
        parts.push(format!("bg={}", color_name(bg)));
    }
    let modifiers = [
        (Modifier::BOLD, "b"),
        (Modifier::DIM, "d"),
        (Modifier::ITALIC, "i"),
        (Modifier::UNDERLINED, "u"),
        (Modifier::REVERSED, "r"),
        (Modifier::CROSSED_OUT, "s"),
    ];
    for (modifier, name) in modifiers {
        if style.add_modifier.contains(modifier) {
            parts.push(name.to_owned());
        }
        if style.sub_modifier.contains(modifier) {
            parts.push(format!("-{name}"));
        }
    }
    parts.join(" ")
}

pub fn color_name(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("i{i}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Assertions name roles, not colors, so the palette can be tuned without touching the tests.
pub fn fg(role: Role) -> Option<Color> {
    theme().style(role).fg
}

pub fn bg(role: Role) -> Option<Color> {
    theme().style(role).bg
}

pub fn row_with<'a>(lines: &'a [Line<'a>], needle: &str) -> Option<&'a Line<'a>> {
    lines.iter().find(|line| {
        let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        row.contains(needle)
    })
}

/// How the first span holding `needle` is drawn: its own style over its row's.
pub fn style_of(lines: &[Line<'_>], needle: &str) -> Option<Style> {
    lines.iter().find_map(|line| {
        line.spans
            .iter()
            .find(|span| span.content.contains(needle))
            .map(|span| line.style.patch(span.style))
    })
}

pub fn assert_fits(lines: &[Line<'_>], width: u16) {
    for (i, line) in lines.iter().enumerate() {
        let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            str_width(&row) <= usize::from(width),
            "row {i} is {} columns, wider than {width}: {row:?}",
            str_width(&row)
        );
        assert!(
            !row.chars().any(char::is_control),
            "row {i} has a control character: {row:?}"
        );
    }
}
