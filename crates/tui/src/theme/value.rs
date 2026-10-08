//! Color values as a config file writes them, in Claude Code's forms: `#rrggbb`, `#rgb`,
//! `rgb(r,g,b)`, `ansi256(n)`, `ansi:<name>`, or another role's name.

use std::fmt;
use std::str::FromStr;

use ratatui::style::{Color, Style};

use super::color::fit_override;
use super::{Role, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorValue {
    Color(Color),
    /// Drawn as that role is, so changing it moves this one too.
    Role(Role),
}

/// A color that couldn't be used, for /doctor and the Colors tab to name. The theme keeps its
/// own color for that role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadColor {
    pub role: String,
    pub value: String,
    pub why: String,
}

impl fmt::Display for BadColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {:?}: {}", self.role, self.value, self.why)
    }
}

impl FromStr for ColorValue {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix('#') {
            return hex_color(hex).map(ColorValue::Color);
        }
        if let Some(args) = call(text, "rgb") {
            let parts: Vec<&str> = args.split(',').map(str::trim).collect();
            let [r, g, b] = parts.as_slice() else {
                return Err(String::from("rgb() takes three numbers"));
            };
            let channel = |c: &str| {
                c.parse::<u8>()
                    .map_err(|_| format!("{c:?} isn't a number from 0 to 255"))
            };
            return Ok(ColorValue::Color(Color::Rgb(
                channel(r)?,
                channel(g)?,
                channel(b)?,
            )));
        }
        if let Some(index) = call(text, "ansi256") {
            return index
                .trim()
                .parse::<u8>()
                .map(|i| ColorValue::Color(Color::Indexed(i)))
                .map_err(|_| format!("{index:?} isn't a number from 0 to 255"));
        }
        if let Some(name) = text.strip_prefix("ansi:") {
            return ansi_name(name)
                .map(ColorValue::Color)
                .ok_or_else(|| format!("{name:?} isn't one of the sixteen color names"));
        }
        Role::from_name(text)
            .map(ColorValue::Role)
            .ok_or_else(|| String::from("not a color or a role name"))
    }
}

fn call<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn hex_color(hex: &str) -> Result<Color, String> {
    let bad = || format!("#{hex} isn't #rgb or #rrggbb");
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).map_err(|_| bad());
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
    match hex.len() {
        3 => Ok(Color::Rgb(digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
        6 => Ok(Color::Rgb(pair(0)?, pair(2)?, pair(4)?)),
        _ => Err(bad()),
    }
}

/// The sixteen names, as Claude Code's themes and chalk write them: `brightBlack` and
/// `blackBright` both, in any case, with `gray` for bright black.
fn ansi_name(name: &str) -> Option<Color> {
    let name = name.trim().to_ascii_lowercase();
    let (base, bright) = match (name.strip_prefix("bright"), name.strip_suffix("bright")) {
        (Some(base), _) | (_, Some(base)) => (base.trim_matches(['-', '_']), true),
        _ => (name.as_str(), false),
    };
    let color = match (base, bright) {
        ("black", false) => Color::Black,
        ("red", false) => Color::Red,
        ("green", false) => Color::Green,
        ("yellow", false) => Color::Yellow,
        ("blue", false) => Color::Blue,
        ("magenta", false) => Color::Magenta,
        ("cyan", false) => Color::Cyan,
        ("white", false) => Color::Gray,
        ("black", true) | ("gray" | "grey", false) => Color::DarkGray,
        ("red", true) => Color::LightRed,
        ("green", true) => Color::LightGreen,
        ("yellow", true) => Color::LightYellow,
        ("blue", true) => Color::LightBlue,
        ("magenta", true) => Color::LightMagenta,
        ("cyan", true) => Color::LightCyan,
        ("white", true) => Color::White,
        _ => return None,
    };
    Some(color)
}

impl Theme {
    /// Gives `role` the color `value`, fitted to this theme's color level. Overrides go on
    /// last, after the level and the accent, which refit the palette's own colors.
    pub fn set_color(&mut self, role: Role, value: ColorValue) -> Result<(), String> {
        match value {
            ColorValue::Role(target) => {
                if self.link(role, target) {
                    Ok(())
                } else {
                    Err(format!("{} already follows {}", target.name(), role.name()))
                }
            }
            ColorValue::Color(color) => {
                // A role the palette draws as a fill stays a fill.
                let current = self.style(role);
                let style = if current.fg.is_none() && current.bg.is_some() {
                    Style::new().bg(color)
                } else {
                    Style::new().fg(color)
                };
                let style = Style {
                    add_modifier: current.add_modifier,
                    ..style
                };
                self.set(role, fit_override(role, style, self.level));
                Ok(())
            }
        }
    }

    /// Applies `[ui.colors]` or a theme file's `[colors]`, by role name. Each bad entry is
    /// skipped and returned, and the theme keeps its own color for that role.
    pub fn apply_colors<'a>(
        &mut self,
        colors: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<BadColor> {
        let mut bad = Vec::new();
        for (name, value) in colors {
            let problem = match (Role::from_name(name), value.parse::<ColorValue>()) {
                (None, _) => Some(String::from("no role has this name")),
                (_, Err(why)) => Some(why),
                (Some(role), Ok(color)) => self.set_color(role, color).err(),
            };
            if let Some(why) = problem {
                bad.push(BadColor {
                    role: name.to_owned(),
                    value: value.to_owned(),
                    why,
                });
            }
        }
        bad
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorLevel;

    fn parse(text: &str) -> Result<ColorValue, String> {
        text.parse()
    }

    #[test]
    fn every_form_claude_code_takes_parses() {
        let rgb = |r, g, b| Ok(ColorValue::Color(Color::Rgb(r, g, b)));
        assert_eq!(parse("#b1b9f9"), rgb(0xb1, 0xb9, 0xf9));
        assert_eq!(parse("#fa0"), rgb(0xff, 0xaa, 0x00));
        assert_eq!(parse("rgb(1, 2, 3)"), rgb(1, 2, 3));
        assert_eq!(
            parse("ansi256(205)"),
            Ok(ColorValue::Color(Color::Indexed(205)))
        );
        assert_eq!(
            parse("ansi:brightBlack"),
            Ok(ColorValue::Color(Color::DarkGray))
        );
        assert_eq!(
            parse("ansi:yellowBright"),
            Ok(ColorValue::Color(Color::LightYellow))
        );
        assert_eq!(parse("ansi:white"), Ok(ColorValue::Color(Color::Gray)));
        assert_eq!(parse("card"), Ok(ColorValue::Role(Role::Card)));
    }

    #[test]
    fn bad_values_say_why() {
        for bad in [
            "#12",
            "#gggggg",
            "rgb(1,2)",
            "rgb(1,2,300)",
            "ansi:pink",
            "nope",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_override_is_fitted_and_a_bad_one_is_named() {
        let mut theme = Theme::dark().with_level(ColorLevel::Ansi16);
        let bad = theme.apply_colors([
            ("cat-skills", "#ff0000"),
            ("bar-fill", "ansi:blue"),
            ("cat-tools", "#zz"),
            ("no-such-role", "#ffffff"),
        ]);
        assert_eq!(theme.style(Role::CatSkills).fg, Some(Color::LightRed));
        assert_eq!(theme.style(Role::BarFill).bg, Some(Color::Blue));
        let named: Vec<&str> = bad.iter().map(|b| b.role.as_str()).collect();
        assert_eq!(named, ["cat-tools", "no-such-role"]);
    }

    #[test]
    fn a_state_follows_its_role_until_given_its_own() {
        let mut theme = Theme::dark();
        assert_eq!(theme.style(Role::StateWorking), theme.style(Role::Card));
        theme.apply_colors([("card", "#123456")]);
        assert_eq!(
            theme.style(Role::StateWorking).fg,
            Some(Color::Rgb(0x12, 0x34, 0x56))
        );
        theme.apply_colors([("state-working", "accent")]);
        assert_eq!(theme.style(Role::StateWorking), theme.style(Role::Accent));
        let bad = theme.apply_colors([("accent", "state-working")]);
        assert_eq!(bad.len(), 1, "a loop is refused");
    }
}
