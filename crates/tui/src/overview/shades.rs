//! The color a model draws in, the same on every tab. A CLI's models share its color and are
//! told apart by shade: the CLI's own color for the model that has run the most tokens here,
//! then three more shades for the next three, and round again from the fifth. Shades of one
//! color stand at least 15 apart in CIELAB, far enough to tell at a glance.

use std::cmp::Reverse;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::ModelTokens;
use ratatui::style::{Color, Style};

use super::cards::cli_role;
use crate::kit::Kit;
use crate::theme::{Brightness, ColorLevel, Role, Theme, nearest_ansi256};

/// The shades a color comes in, its own first.
pub const SHADES: usize = 4;

type Rgb = (u8, u8, u8);

/// The three CLIs' shades on a dark screen: the CLI's own color, a deep one, a light one, and
/// a fourth far from all three.
const ON_DARK: [[u32; SHADES]; 3] = [
    [0xe08a66, 0xb4472a, 0xf3b27a, 0xf8dccb],
    [0x6aa6ff, 0x2e5fc0, 0xa8cdff, 0x3fc4f0],
    [0x36cfe6, 0x1b8a9c, 0x93e8f3, 0x4fe0c0],
];

/// How any other color's further shades are mixed: toward white, or toward black where the
/// share is negative. On either screen one shade goes toward the paper and two away from it.
const MIXED_ON_DARK: [f64; SHADES - 1] = [-0.30, 0.50, 0.85];
const MIXED_ON_LIGHT: [f64; SHADES - 1] = [0.35, -0.30, -0.60];

/// The `nth` shade of the color `base` draws in: its own first. Sixteen colors and none have
/// no shades, so every model draws in its CLI's color and its name tells them apart.
pub(in crate::overview) fn shade(kit: &Kit<'_>, base: Role, nth: usize) -> Style {
    let own = kit.fg(base);
    let theme = kit.theme;
    let nth = nth % SHADES;
    if nth == 0 || matches!(theme.level, ColorLevel::Ansi16 | ColorLevel::None) {
        return own;
    }
    let Some(rgb) = full_color(theme, base) else {
        return own;
    };
    let mixed = shades_of(rgb, theme.brightness)[nth];
    Style::new().fg(match theme.level {
        ColorLevel::TrueColor => Color::Rgb(mixed.0, mixed.1, mixed.2),
        _ => Color::Indexed(nearest_ansi256(mixed)),
    })
}

/// The role's color in full. A screen of 256 colors holds only the nearest of them, so there
/// the shades are mixed from the theme's own.
fn full_color(theme: &Theme, role: Role) -> Option<Rgb> {
    let rgb = |color| match color {
        Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    };
    rgb(theme.color(role)).or_else(|| rgb(Theme::for_brightness(theme.brightness).color(role)))
}

fn shades_of(own: Rgb, brightness: Brightness) -> [Rgb; SHADES] {
    let fixed = ON_DARK.iter().find(|shades| rgb(shades[0]) == own);
    if let (Brightness::Dark, Some(fixed)) = (brightness, fixed) {
        return fixed.map(rgb);
    }
    let mixes = match brightness {
        Brightness::Dark => MIXED_ON_DARK,
        Brightness::Light => MIXED_ON_LIGHT,
    };
    let mut shades = [own; SHADES];
    for (shade, mix) in shades[1..].iter_mut().zip(mixes) {
        let toward = if mix < 0.0 { 0.0 } else { 255.0 };
        let part = |c: u8| (f64::from(c) + (toward - f64::from(c)) * mix.abs()).round() as u8;
        *shade = (part(own.0), part(own.1), part(own.2));
    }
    shades
}

fn rgb(hex: u32) -> Rgb {
    ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Which shade each model takes: its place among its CLI's models by their tokens in all.
/// Every tab builds this from the same list, so a model has one color wherever it shows.
#[derive(Debug, Clone)]
pub struct ModelColors<'a> {
    names: &'a DisplayNames,
    /// Each model as a CLI and the name it shows under, the most tokens first.
    ranked: Vec<(String, String)>,
}

impl<'a> ModelColors<'a> {
    /// From every model's tokens in all, as a tab's numbers carry them. A model recorded
    /// under two names counts once, with the tokens of both.
    pub fn new(models: &[ModelTokens], names: &'a DisplayNames) -> Self {
        let mut tokens: Vec<((String, String), u64)> = Vec::new();
        for model in models {
            let shown = shown_as(names, &model.agent, model.model.as_deref());
            match tokens.iter_mut().find(|(known, _)| *known == shown) {
                Some((_, sum)) => *sum += model.tokens,
                None => tokens.push((shown, model.tokens)),
            }
        }
        tokens.sort_by_key(|(_, sum)| Reverse(*sum));
        Self {
            names,
            ranked: tokens.into_iter().map(|(shown, _)| shown).collect(),
        }
    }

    /// With the models a tab shows that the list hasn't met, each after its CLI's others in
    /// the order given, so no two of a tab's models share a place.
    pub fn with<'m>(mut self, shown: impl IntoIterator<Item = (&'m str, Option<&'m str>)>) -> Self {
        for (agent, model) in shown {
            self.meet(shown_as(self.names, agent, model));
        }
        self
    }

    /// Like [`ModelColors::with`], for a tab that holds each model by the name it shows under,
    /// as the health card does.
    pub fn with_named<'m>(mut self, shown: impl IntoIterator<Item = (&'m str, &'m str)>) -> Self {
        for (agent, name) in shown {
            self.meet((agent.to_owned(), name.to_owned()));
        }
        self
    }

    fn meet(&mut self, shown: (String, String)) {
        if !self.ranked.contains(&shown) {
            self.ranked.push(shown);
        }
    }

    /// The model's place among its CLI's models, the one with the most tokens 0.
    pub fn place(&self, agent: &str, model: Option<&str>) -> usize {
        let (agent, name) = shown_as(self.names, agent, model);
        self.place_named(&agent, &name)
    }

    /// The place of the model that shows under `name`.
    pub fn place_named(&self, agent: &str, name: &str) -> usize {
        self.ranked
            .iter()
            .filter(|(cli, _)| cli == agent)
            .position(|(_, known)| known == name)
            .unwrap_or(0)
    }

    pub fn style(&self, kit: &Kit<'_>, agent: &str, model: Option<&str>) -> Style {
        shade(kit, cli_role(agent), self.place(agent, model))
    }

    /// The color of the model that shows under `name`.
    pub fn style_named(&self, kit: &Kit<'_>, agent: &str, name: &str) -> Style {
        shade(kit, cli_role(agent), self.place_named(agent, name))
    }
}

/// A model as its CLI and the name it shows under, as [`DisplayNames::shown_as`] has it.
pub(in crate::overview) fn shown_as(
    names: &DisplayNames,
    agent: &str,
    model: Option<&str>,
) -> (String, String) {
    names.shown_as(agent, model)
}

#[cfg(test)]
#[path = "shades_tests.rs"]
mod tests;
