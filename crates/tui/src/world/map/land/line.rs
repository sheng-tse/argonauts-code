//! What the land says when you point at it. Land takes the climate of the behaviors round it,
//! the bigger ones more, so the line names the behavior whose days drew the land there, and a
//! dim second line says why when the nearest behavior's own days would draw another.

use super::ice::Polar;
use crate::base::draw::geo::Plate;
use crate::world::map::climate::Climate;
use crate::world::map::fields::{COARSE, Ground, LAND, Look};

/// What a point of the ground is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundKind {
    Sea,
    /// Open sea beyond the land stood up in relief, at no one place.
    OpenSea,
    Rock,
    Snow,
    Climate(Climate),
    Ice(IceKind),
}

/// Which ice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceKind {
    /// The north cap, for days since the last action.
    Cap,
    /// The south ice, for quiet days.
    South,
    /// An iceberg, by its quiet stretch's place in the list.
    Berg(usize),
}

/// A point's line, and the dim line under it that says why, where one does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundLine {
    pub kind: GroundKind,
    pub text: String,
    pub note: Option<String>,
}

impl GroundLine {
    fn new(kind: GroundKind, text: String) -> Self {
        Self {
            kind,
            text,
            note: None,
        }
    }

    /// The line over open sea beyond the stood-up land.
    pub fn open_sea() -> Self {
        Self::new(
            GroundKind::OpenSea,
            "Open sea · no behavior here".to_owned(),
        )
    }
}

/// The rule the land's climate follows, said where a line names another behavior than the
/// nearest.
const RULE: &str = "The land takes the climate of the behaviors round it, the bigger ones more";
/// How far round a behavior its climate reaches, as the climate field weighs it.
const CLIMATE_REACH: f64 = 40.0 * LAND;

/// A count of days back, as the lines say it.
pub fn days_ago(days: u32) -> String {
    match days {
        0 => "today".to_owned(),
        1 => "1 day ago".to_owned(),
        n => format!("{n} days ago"),
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The line for the land or sea at pixel (x, y) of the flat map.
pub fn line_at(ground: &Ground, x: f64, y: f64) -> GroundLine {
    if ground.land_at(x, y) < 1.0 {
        return GroundLine::new(GroundKind::Sea, "Sea · no behavior here".to_owned());
    }
    let quoted = |at: usize| format!("‘{}’", ground.facts.behaviors[at].name);
    let Some(near) = nearest(ground, x, y) else {
        return GroundLine::new(GroundKind::Sea, "Sea · no behavior here".to_owned());
    };
    let height = ground.height_at(x, y);
    let actions = height.round();
    if height >= 100.0 {
        return GroundLine::new(
            GroundKind::Snow,
            format!(
                "Snow and glacier · about {actions} actions here, 100 or more · {}",
                quoted(near)
            ),
        );
    }
    if height >= 30.0 {
        return GroundLine::new(
            GroundKind::Rock,
            format!(
                "Bare rock · about {actions} actions here, 30 to 99 · {}",
                quoted(near)
            ),
        );
    }
    let here = if height >= 1.0 {
        let word = if height < 1.5 { "action" } else { "actions" };
        format!(" · about {actions} {word} here")
    } else {
        String::new()
    };
    let climate = ground.climate_at(x, y);
    let kind = GroundKind::Climate(climate);
    let class = &ground.climate.class;
    let days = |at: usize| {
        if class[at] == Some(Climate::Rainforest) {
            format!(
                "used on {} of the last 7 days",
                ground.climate.days_in_last_7[at]
            )
        } else {
            let last = ground.climate.last_used[at].unwrap_or(0);
            format!("last used {}", days_ago(last))
        }
    };
    let name = capitalised(climate.name());
    if class[near] == Some(climate) {
        return GroundLine::new(
            kind,
            format!("{name} · near {}, {}{here}", quoted(near), days(near)),
        );
    }

    // Weighed as the climate field weighs them: at the place the ground's warp moves this to.
    let g = COARSE as f64;
    let (gx, gy) = (x / g, y / g);
    let wx = x + ground.warp_x.sample(gx, gy) * g;
    let wy = y + ground.warp_y.sample(gx, gy) * g;
    let reach = 2.0 * CLIMATE_REACH * CLIMATE_REACH;
    let mut weights: Vec<(usize, f64)> = (0..ground.behaviors())
        .filter(|at| ground.count[*at] > 0)
        .map(|at| {
            let (cx, cy) = ground.centre[at];
            let d2 = (cx - wx).powi(2) + (cy - wy).powi(2);
            (at, f64::from(ground.count[at]).sqrt() * (-d2 / reach).exp())
        })
        .collect();
    let total = weights.iter().map(|(_, w)| w).sum::<f64>();
    let total = if total == 0.0 { 1.0 } else { total };
    let nearest_was = format!("{RULE} · {} itself was {}", quoted(near), days(near));
    weights.sort_by(|a, b| b.1.total_cmp(&a.1));
    let same = weights
        .iter()
        .find(|(at, _)| class[*at] == Some(climate))
        .copied();
    if let Some((by, weight)) = same
        && weight / total >= 0.15
    {
        let bigger = if ground.count[by] > ground.count[near] {
            "the bigger "
        } else {
            ""
        };
        return GroundLine {
            kind,
            text: format!(
                "{name} · from {bigger}{} beside it, {}{here}",
                quoted(by),
                days(by)
            ),
            note: Some(nearest_was),
        };
    }
    // "Between" only two behaviors whose climates lie either side of this land's, one wetter
    // and one drier, as a blend can only fall between them; where every strong neighbour lies
    // on one side, the land is at the edge of their climate.
    let wetter = weights
        .iter()
        .find(|(at, _)| class[*at] > Some(climate))
        .copied();
    let drier = weights
        .iter()
        .find(|(at, _)| class[*at].is_some_and(|c| c < climate))
        .copied();
    let share = |side: Option<(usize, f64)>| side.map_or(0.0, |(_, w)| w / total);
    let (mixed, named) = match (wetter, drier) {
        (Some(wet), Some(dry)) if share(wetter) >= 0.05 && share(drier) >= 0.05 => {
            let (first, second) = if wet.1 >= dry.1 {
                (wet.0, dry.0)
            } else {
                (dry.0, wet.0)
            };
            (
                format!(
                    "between {} ({}) and {} ({})",
                    quoted(first),
                    days(first),
                    quoted(second),
                    days(second)
                ),
                vec![first, second],
            )
        }
        _ => {
            let strongest = weights.first().map_or(near, |(at, _)| *at);
            let theirs = class[strongest].map_or("", |c| c.name());
            (
                format!(
                    "at the edge of the {theirs} round {} ({})",
                    quoted(strongest),
                    days(strongest)
                ),
                vec![strongest],
            )
        }
    };
    GroundLine {
        kind,
        text: format!("{name} · {mixed}{here}"),
        note: Some(if named.contains(&near) {
            RULE.to_owned()
        } else {
            nearest_was
        }),
    }
}

/// The behavior nearest a pixel, the bigger ones reaching further.
fn nearest(ground: &Ground, x: f64, y: f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for at in (0..ground.behaviors()).filter(|at| ground.count[*at] > 0) {
        let (cx, cy) = ground.centre[at];
        let d = (cx - x).hypot(cy - y) / f64::from(ground.count[at]).sqrt().sqrt();
        if best.is_none_or(|(_, nearest)| d < nearest) {
            best = Some((at, d));
        }
    }
    best.map(|(at, _)| at)
}

impl Polar {
    /// The ice under a pixel of the map, and its line: an iceberg, the north cap or the south
    /// ice.
    pub fn line_at(&self, ground: &Ground, x: f64, y: f64) -> Option<GroundLine> {
        if ground.look == Look::Gas {
            return None;
        }
        let place = Plate::MAP.place(x, y);
        let (lon, lat) = (place.lon, place.lat);
        for berg in &self.bergs {
            let dl = (lat - berg.lat) / berg.radius;
            let dn = (lon - berg.lon).sin().atan2((lon - berg.lon).cos()) * lat.cos() / berg.radius;
            if dl.hypot(dn * 0.7) < 1.6 {
                let stretch = ground.facts.quiet_stretches.get(berg.index)?;
                return Some(GroundLine::new(
                    GroundKind::Ice(IceKind::Berg(berg.index)),
                    format!(
                        "Iceberg · {} quiet days in a row, {}",
                        stretch.days,
                        days_ago(stretch.ends + stretch.days - 1)
                    ),
                ));
            }
        }
        if self.cap_north.is_some() && lat > self.edge_north() - 0.02 {
            return Some(GroundLine::new(
                GroundKind::Ice(IceKind::Cap),
                format!(
                    "Ice cap · {} since the last action",
                    plural(ground.facts.idle_days, "day", "days")
                ),
            ));
        }
        if self.cap_south.is_some() && lat < -self.edge_south() + 0.02 {
            return Some(GroundLine::new(
                GroundKind::Ice(IceKind::South),
                format!(
                    "South ice · {} in the period",
                    plural(ground.facts.quiet_days, "quiet day", "quiet days")
                ),
            ));
        }
        None
    }
}
