//! The colour each group's dots and name draw in: a model in its shade of its CLI's colour, the
//! same as on every other tab; a CLI in its own; any other group in the agents' colours in turn.

use argonauts_core::behavior_map::{BehaviorMap, Grouping};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::ModelTokens;
use ratatui::style::{Color, Style};

use crate::kit::Kit;
use crate::overview::cli_role;
use crate::overview::shades::ModelColors;
use crate::theme::{ColorLevel, Role};

/// The colours of groups that are neither models nor CLIs, in the order groups take them.
const OTHERS: [Role; 8] = [
    Role::AgentBlue,
    Role::AgentGreen,
    Role::AgentPurple,
    Role::AgentYellow,
    Role::AgentPink,
    Role::AgentCyan,
    Role::AgentOrange,
    Role::AgentRed,
];

/// A dot drawn in no colour the theme gives.
const GREY: (u8, u8, u8) = (160, 160, 170);

#[derive(Debug, Clone, Default, Hash)]
pub struct GroupColours {
    styles: Vec<Style>,
    /// The same colours in full, for the picture.
    rgb: Vec<(u8, u8, u8)>,
}

impl GroupColours {
    /// Each of the map's groups, with the models the rest of the panel colours `models` gives
    /// their shades.
    pub fn new(
        kit: &Kit<'_>,
        map: &BehaviorMap,
        models: &[ModelTokens],
        names: &DisplayNames,
    ) -> Self {
        let full_theme = kit.theme.clone().with_level(ColorLevel::TrueColor);
        let full = Kit::new(&full_theme);
        let shades = ModelColors::new(models, names).with(
            map.groups
                .iter()
                .filter_map(|group| Some((group.agent.as_deref()?, group.model.as_deref()))),
        );
        let style = |kit: &Kit<'_>, at: usize| {
            let group = &map.groups[at];
            match (map.request.grouping, group.agent.as_deref()) {
                (Grouping::Model, Some(agent)) => shades.style(kit, agent, group.model.as_deref()),
                (Grouping::Cli, Some(agent)) => kit.fg(cli_role(agent)),
                _ => kit.fg(OTHERS[at % OTHERS.len()]),
            }
        };
        let count = map.groups.len();
        Self {
            styles: (0..count).map(|at| style(kit, at)).collect(),
            rgb: (0..count)
                .map(|at| match style(&full, at).fg {
                    Some(Color::Rgb(r, g, b)) => (r, g, b),
                    _ => GREY,
                })
                .collect(),
        }
    }

    pub fn style(&self, group: usize) -> Style {
        self.styles.get(group).copied().unwrap_or_default()
    }

    pub fn rgb(&self, group: usize) -> (u8, u8, u8) {
        self.rgb.get(group).copied().unwrap_or(GREY)
    }
}
