//! The colour and shape a model draws in, the same on every tab and all day. A CLI's models
//! share its hue and are told apart by shape, each lit in the hue at full strength: █ for the
//! model that ran the most tokens through Argonauts Code, ▚ for the next, ⣿ for the third.
//! Every model after those draws in the solid grey of the rest: one run of it in a bar, and a
//! grey swatch on each of their rows in a table, which keeps every model. Where only a colour
//! shows, a dot or a name, a CLI's first three models share its hue.
//!
//! The shapes are block, quadrant and braille glyphs because Ghostty and kitty draw those
//! themselves, exactly and at full strength, while they paint ▓ ▒ ░ as flat mixes of the
//! colour into the background, which made a CLI's second and third models darker copies of
//! its first.

use std::cmp::Reverse;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::ModelTokens;
use ratatui::style::Style;

use crate::kit::Kit;
use crate::theme::Role;

/// How many of a CLI's models take a shape of their own before the rest share the grey.
pub const OWN_TEXTURES: usize = 3;

/// The shapes a CLI's models take in the order they rank, then the rest's solid cell.
const SHAPES: [&str; OWN_TEXTURES + 1] = ["█", "▚", "⣿", "█"];

/// The same in ASCII, apart from the `.` a bar's free cells take.
const ASCII_SHAPES: [&str; OWN_TEXTURES + 1] = ["#", "=", "+", ":"];

/// The rest's cell without colour, where a solid one would pass for a CLI's first model.
const REST_WITHOUT_COLOUR: &str = "░";

/// The colour of the model at `place` among its CLI's, whose hue is `base`: the hue for the
/// first three, and the grey of the rest after them.
pub(crate) fn shade(kit: &Kit<'_>, base: Role, place: usize) -> Style {
    if place < OWN_TEXTURES {
        kit.fg(base)
    } else {
        kit.fg(Role::CatOther)
    }
}

/// The shape of the model at `place` among its CLI's models, one cell of it.
pub(crate) fn texture(kit: &Kit<'_>, place: usize) -> &'static str {
    if kit.theme.glyphs.is_ascii() {
        return ASCII_SHAPES[place.min(OWN_TEXTURES)];
    }
    if place >= OWN_TEXTURES && !kit.has_color() {
        return REST_WITHOUT_COLOUR;
    }
    SHAPES[place.min(OWN_TEXTURES)]
}

/// The left half of the shape at `place`, for a bar that ends in half a cell: ▌ of █, ▘ of ▚
/// (its top-left quarter) and ⡇ of ⣿ (its left column of dots), each drawn as exactly as its
/// whole. `None` where the shape has no half, as in ASCII, so the bar rounds to whole cells.
pub(crate) fn half_texture(kit: &Kit<'_>, place: usize) -> Option<&'static str> {
    match texture(kit, place) {
        "█" => Some("▌"),
        "▚" => Some("▘"),
        "⣿" => Some("⡇"),
        _ => None,
    }
}

/// Which colour and shape each of a screen's models takes: its place among its CLI's models
/// in the list every tab is given, then the models the screen shows that the list lacks.
///
/// The app ranks that list once a day, by the tokens that ran through Argonauts Code before
/// today, with the whole computer's count breaking ties and placing the models that never ran
/// here after the ones that did, and the models new today last. So a colour never changes
/// while you look, nor between tabs.
#[derive(Debug, Clone)]
pub struct ModelColors<'a> {
    names: &'a DisplayNames,
    /// Each model as a CLI and the name it shows under, in the order of their places.
    ranked: Vec<(String, String)>,
}

impl<'a> ModelColors<'a> {
    /// The colours of a screen that shows `shown`, from the app's list `ranked`. A model the
    /// list hasn't met goes after its CLI's others in the order given, so no two of the
    /// screen's models share a place.
    pub fn for_screen<'m>(
        ranked: &[ModelTokens],
        names: &'a DisplayNames,
        shown: impl IntoIterator<Item = (&'m str, Option<&'m str>)>,
    ) -> Self {
        Self::new(ranked, names).with(shown)
    }

    /// From the app's list alone, for a screen that shows none of its own models. A model
    /// recorded under two names is one, with the tokens of both, at the first one's place
    /// among those with as many.
    pub fn new(ranked: &[ModelTokens], names: &'a DisplayNames) -> Self {
        let mut tokens: Vec<((String, String), u64)> = Vec::new();
        for model in ranked {
            let shown = shown_as(names, &model.agent, model.model.as_deref());
            match tokens.iter_mut().find(|(known, _)| *known == shown) {
                Some((_, sum)) => *sum += model.tokens,
                None => tokens.push((shown, model.tokens)),
            }
        }
        // Stable, so models with as many tokens keep the list's order, which already breaks
        // their ties.
        tokens.sort_by_key(|(_, sum)| Reverse(*sum));
        Self {
            names,
            ranked: tokens.into_iter().map(|(shown, _)| shown).collect(),
        }
    }

    /// With the models a screen shows that the list hasn't met, each after its CLI's others in
    /// the order given.
    pub fn with<'m>(mut self, shown: impl IntoIterator<Item = (&'m str, Option<&'m str>)>) -> Self {
        for (agent, model) in shown {
            self.meet(shown_as(self.names, agent, model));
        }
        self
    }

    /// Like [`ModelColors::with`], for a screen that holds each model by the name it shows
    /// under, as the health card does.
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

    /// The model's place among its CLI's models, the first 0.
    pub fn place(&self, agent: &str, model: Option<&str>) -> usize {
        let (agent, name) = shown_as(self.names, agent, model);
        self.place_named(&agent, &name)
    }

    /// The place of the model that shows under `name`. One the ranking hasn't met takes the
    /// next after its CLI's others, as a model new today does.
    pub fn place_named(&self, agent: &str, name: &str) -> usize {
        let of_its_cli = || self.ranked.iter().filter(|(cli, _)| cli == agent);
        of_its_cli()
            .position(|(_, known)| known == name)
            .unwrap_or_else(|| of_its_cli().count())
    }

    pub fn style(&self, kit: &Kit<'_>, agent: &str, model: Option<&str>) -> Style {
        shade(kit, Role::for_cli(agent), self.place(agent, model))
    }

    /// The colour of the model that shows under `name`.
    pub fn style_named(&self, kit: &Kit<'_>, agent: &str, name: &str) -> Style {
        shade(kit, Role::for_cli(agent), self.place_named(agent, name))
    }

    /// The shape the model draws in, one cell of it.
    pub fn texture(&self, kit: &Kit<'_>, agent: &str, model: Option<&str>) -> &'static str {
        texture(kit, self.place(agent, model))
    }

    /// The left half of the model's shape, for a bar that ends in half a cell.
    pub fn half_texture(
        &self,
        kit: &Kit<'_>,
        agent: &str,
        model: Option<&str>,
    ) -> Option<&'static str> {
        half_texture(kit, self.place(agent, model))
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
