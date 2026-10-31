//! The world as plain lines, for a screen reader and for Ctrl+S: what the map is drawn from, each
//! group with its distinctive behaviors, then the map, the planet and the universe in a few
//! words. No line lists the kinds of nature, land, wonders or bodies the world holds: each
//! place is found where it is, and what the pointer rests on, or ← → picked, says its own line.

use argonauts_core::behaviors::behavior_map::BehaviorMap;
use argonauts_core::behaviors::behavior_world::WorldFacts;
use argonauts_core::look::display_names::DisplayNames;

use super::drawn::{Grown, SEED};
use super::hover::{self, plural};
use super::universe::Universe;
use super::{BehaviorsTab, LIST_ROWS, Level};
use crate::world::map::fields::Look as GroundLook;
use crate::world::map::land::{self, GroundKind, GroundLine, IceKind};
use crate::world::planet::{PlanetFacts, has_aurora};
use crate::world::{LonLat, Plate};

/// The map as plain lines: what it covers, each group's count and its distinctive behaviors,
/// and the world drawn from it where the app worked one out.
pub fn spoken(map: &BehaviorMap, names: &DisplayNames) -> Vec<String> {
    let mut lines = vec![super::summary::headline_words(map)];
    for (group, comparison) in map.groups.iter().zip(&map.comparisons) {
        lines.push(format!("{}: {} actions", group.name, group.actions));
        if let Some(why) = &comparison.why_not {
            lines.push(format!("  {why}"));
        }
        for row in comparison.rows.iter().take(LIST_ROWS) {
            lines.push(format!(
                "  {}: {} of its actions, the others {}",
                row.behavior, row.mine, row.others
            ));
        }
    }
    if map.unlabelled > 0 {
        lines.push(format!("{} actions no label fits", map.unlabelled));
    }
    if let Some(world) = map.world.as_deref() {
        let grown = Grown::new(map, SEED);
        lines.extend(world_lines(map, world, &grown, names));
    }
    lines
}

/// Every place on the world the map draws, west to east and then north to south, each as the
/// lines the pointer's line reads there: each behavior's land, each mark of nature, each wonder
/// and the ice; then what only the planet shows, its aurora and its clouds. A screen reader
/// steps through them one at a time, where the full screen points. A model or a CLI keeps its
/// whole name, since a screen reader hears no logo.
pub fn places(map: &BehaviorMap, names: &DisplayNames) -> Vec<Vec<String>> {
    let Some(world) = map.world.as_deref() else {
        return Vec::new();
    };
    let grown = Grown::new(map, SEED);
    let ground = &grown.ground;
    let ground_lines = |line: GroundLine| -> Vec<String> {
        [Some(line.text), line.note].into_iter().flatten().collect()
    };
    let mut placed: Vec<((f64, f64), Vec<String>)> = Vec::new();
    for at in (0..ground.behaviors()).filter(|at| ground.count[*at] > 0) {
        let (x, y) = ground.centre[at];
        let line = grown
            .polar
            .line_at(ground, x, y)
            .unwrap_or_else(|| land::line_at(ground, x, y));
        placed.push(((x, y), ground_lines(line)));
    }
    let nature = &grown.nature;
    for (at, feature) in nature.features.iter().enumerate() {
        if let Some(line) = nature.line(at, ground, &grown.facts, &mut |cli| names.cli(cli)) {
            placed.push(((feature.x, feature.y), vec![line]));
        }
    }
    for shoal in &nature.shoals {
        placed.push(((shoal.x, shoal.y), vec![shoal.line()]));
    }
    for wonder in &grown.wonders {
        let Some(risen) = grown.risen.get(wonder.index) else {
            continue;
        };
        let named: Vec<String> = risen
            .models
            .iter()
            .map(|written| {
                let (cli, model) = written.split_once('/').unwrap_or((written.as_str(), ""));
                let model = Some(model).filter(|model| !model.is_empty());
                names.model(cli, model).unwrap_or_else(|| names.cli(cli))
            })
            .collect();
        placed.push((wonder.ground, hover::wonder_lines(risen, &named).into()));
    }
    let plate = Plate::MAP;
    for berg in &grown.polar.bergs {
        let (x, y) = plate.pixel(LonLat {
            lon: berg.lon,
            lat: berg.lat,
        });
        if let Some(line) = grown.polar.line_at(ground, x, y) {
            placed.push(((x, y), ground_lines(line)));
        }
    }
    // The ice at either pole, read at its middle.
    for y in [0.5, plate.height() - 0.5] {
        let x = plate.width() / 2.0;
        if let Some(line) = grown.polar.line_at(ground, x, y)
            && matches!(line.kind, GroundKind::Ice(IceKind::Cap | IceKind::South))
        {
            placed.push(((x, y), ground_lines(line)));
        }
    }
    placed.sort_by(|(a, _), (b, _)| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut places: Vec<Vec<String>> = placed.into_iter().map(|(_, lines)| lines).collect();
    let reading = hover::Reading {
        world,
        actions: u32::try_from(map.actions.len()).unwrap_or(u32::MAX),
        unlabelled: u32::try_from(map.unlabelled).unwrap_or(u32::MAX),
        clock: crate::base::format::WallClock::local(),
        names,
    };
    let planet = PlanetFacts::from_map(map, |_| Default::default());
    if has_aurora(ground, &planet) {
        places.extend(hover::Nature::Aurora.line(&reading).map(|line| vec![line]));
    }
    if nature.clouds.is_some() {
        places.extend(hover::Nature::Clouds.line(&reading).map(|line| vec![line]));
    }
    places
}

/// The planet as a whole in a few words.
fn look_words(look: GroundLook) -> &'static str {
    match look {
        GroundLook::Rock => "continents",
        GroundLook::Ocean => "an ocean world",
        GroundLook::Gas => "a gas giant, too young for land",
        GroundLook::Ice => "frozen",
    }
}

/// The map, the planet and each maker with its models, in a few words.
fn world_lines(
    map: &BehaviorMap,
    world: &WorldFacts,
    grown: &Grown,
    names: &DisplayNames,
) -> Vec<String> {
    let mut lines = Vec::new();
    let used: Vec<_> = world.behaviors.iter().filter(|b| b.count > 0).collect();
    let count = |many: usize| u32::try_from(many).unwrap_or(u32::MAX);
    lines.push(format!(
        "Map: {} actions on {}; {} on the list never used.",
        map.actions.len(),
        plural(count(used.len()), "behavior"),
        plural(count(grown.facts.unused.len()), "behavior"),
    ));
    let crew = &world.crew;
    let recent: u32 = world.models.iter().map(|m| m.last_7_days).sum();
    let unlabelled = u32::try_from(map.unlabelled).unwrap_or(u32::MAX);
    let all = u32::try_from(map.actions.len())
        .unwrap_or(u32::MAX)
        .saturating_add(unlabelled);
    let clouds = (f64::from(unlabelled) * 100.0 / f64::from(all.max(1))).round();
    let sea = ((1.0 - grown.ground.land_share) * 100.0).round();
    lines.push(format!(
        "Planet: {}; {sea}% of the map is sea; {}; {}; {} actions a day last week; clouds \
         {clouds}%; {recent} actions in the last 7 days.",
        look_words(grown.ground.look),
        plural(crew.idle_days, "idle day"),
        plural(crew.quiet_days, "quiet day"),
        (f64::from(recent) / 7.0).round(),
    ));
    let universe = Universe::of(world);
    if !universe.makers.is_empty() {
        lines.push("Universe, most actions first:".to_owned());
    }
    for maker in &universe.makers {
        let models: Vec<_> = maker
            .stars
            .iter()
            .filter_map(|star| world.models.get(universe.stars.get(*star)?.model))
            .collect();
        let named: Vec<String> = models
            .iter()
            .map(|model| format!("{} {}", model_name(map, model, names), model.actions))
            .collect();
        lines.push(format!(
            "  {}: {} actions, {} ({}); {} failed; {} in the last 7 days.",
            argonauts_core::models::registry::provider::provider_name(&maker.key),
            maker.actions,
            plural(u32::try_from(models.len()).unwrap_or(u32::MAX), "model"),
            named.join(", "),
            models.iter().map(|m| m.failed).sum::<u32>(),
            models.iter().map(|m| m.last_7_days).sum::<u32>(),
        ));
    }
    lines
}

/// A model's name as the map's groups give it, else as the names say, else its id.
fn model_name(
    map: &BehaviorMap,
    model: &argonauts_core::behaviors::behavior_world::ModelFacts,
    names: &DisplayNames,
) -> String {
    map.groups
        .iter()
        .find(|group| group.model.as_deref() == Some(model.model.as_str()))
        .map(|group| group.name.clone())
        .or_else(|| names.model(&model.agent, Some(&model.model)))
        .unwrap_or_else(|| model.model.clone())
}

impl BehaviorsTab {
    /// The world as plain lines from where the keys are: the whole of [`spoken`], then the
    /// level shown and what ← → picked or the pointer rests on, in the pointer line's words.
    pub fn spoken_here(&self, names: &DisplayNames) -> Vec<String> {
        let Some(map) = self.map.as_ref() else {
            return Vec::new();
        };
        let mut lines = spoken(map, names);
        let level = match self.level {
            Level::Map => "Map".to_owned(),
            Level::Planet => "Planet".to_owned(),
            Level::Universe => "Universe".to_owned(),
            Level::Galaxy(maker) => self.universe.makers.get(maker).map_or_else(
                || "Universe".to_owned(),
                |m| {
                    let name = argonauts_core::models::registry::provider::provider_name(&m.key);
                    format!("Universe › {name}")
                },
            ),
            Level::System(star) => {
                let model = self
                    .universe
                    .stars
                    .get(star)
                    .and_then(|s| map.world.as_deref()?.models.get(s.model));
                let maker = self
                    .universe
                    .stars
                    .get(star)
                    .and_then(|s| self.universe.makers.get(s.maker));
                match (maker, model) {
                    (Some(maker), Some(model)) => format!(
                        "Universe › {} › {}",
                        argonauts_core::models::registry::provider::provider_name(&maker.key),
                        model_name(map, model, names)
                    ),
                    _ => "Universe".to_owned(),
                }
            }
        };
        lines.push(format!("Shown: {level}"));
        // On the map and the planet, the place the pointer rests on says its own line, as the
        // row under the map does.
        if matches!(self.level, Level::Map | Level::Planet) {
            let theme =
                crate::base::theme::Theme::dark().with_level(crate::base::theme::ColorLevel::None);
            let kit = crate::base::kit::Kit::new(&theme);
            let colours = super::GroupColours::new(&kit, map, &[], names);
            let look = self.look(&kit, map, &colours, names, usize::from(u16::MAX));
            // A screen reader hears no logo, so a line's words stand without its marks.
            match self.world_said(&look) {
                Some(said) if self.pointed.is_some() => {
                    lines.extend(said.iter().map(hover::Said::words));
                }
                _ if self.pointed.is_some() => {
                    for row in self.pointed_rows(&look) {
                        let text: String = row.iter().map(|span| span.content.as_ref()).collect();
                        let text = text.trim();
                        if !text.is_empty() {
                            lines.push(text.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        let target = self.pointed.or(self.picked);
        if let (Some(target), Some(world)) = (target, map.world.as_deref()) {
            let reading = hover::Reading {
                world,
                actions: u32::try_from(map.actions.len()).unwrap_or(u32::MAX),
                unlabelled: u32::try_from(map.unlabelled).unwrap_or(u32::MAX),
                clock: self.clock,
                names,
            };
            // A screen reader hears no logo, so each model keeps its whole name.
            let named =
                |_: &argonauts_core::look::logos::Logo,
                 model: &argonauts_core::behaviors::behavior_world::ModelFacts| {
                    model_name(map, model, names)
                };
            if let Some(said) =
                hover::cosmos_lines(target, &self.universe, world, &reading, self.level, &named)
            {
                lines.extend(said.iter().map(hover::Said::words));
            }
        }
        lines
    }
}
