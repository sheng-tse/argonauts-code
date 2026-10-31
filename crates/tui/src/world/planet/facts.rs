//! The planet's facts read from the app's own map of actions.

use argonauts_core::behaviors::behavior_map::BehaviorMap;
use time::OffsetDateTime;

use super::{Band, PlanetFacts};
use crate::world::map::fields::Rgb;

impl PlanetFacts {
    /// The facts of a map of actions, in the map's order of actions: the last 7 days' actions
    /// a day, the actions no label fits, the night's actions (from the world the app worked
    /// out, else from each action's own time), and a band for each CLI, the busiest first,
    /// coloured by `colour_of`.
    pub fn from_map(map: &BehaviorMap, colour_of: impl Fn(&str) -> Rgb) -> Self {
        let world = map.world.as_deref();
        let end = world
            .map(|world| world.end)
            .filter(|end| *end != OffsetDateTime::UNIX_EPOCH)
            .or_else(|| map.actions.iter().map(|action| action.at).max());
        let fresh: Vec<bool> = map
            .actions
            .iter()
            .map(|action| end.is_some_and(|end| days_back(action.at, end) < 7))
            .collect();
        let last_week = fresh.iter().filter(|fresh| **fresh).count();
        let night = match world {
            Some(world) => world.crew.night_actions,
            None => map
                .actions
                .iter()
                .filter(|action| !(5..22).contains(&action.at.hour()))
                .count() as u32,
        };
        let mut clis: Vec<(&str, usize)> = Vec::new();
        for action in &map.actions {
            match clis.iter_mut().find(|(cli, _)| *cli == action.cli) {
                Some((_, count)) => *count += 1,
                None => clis.push((action.cli.as_str(), 1)),
            }
        }
        clis.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let total = map.actions.len().max(1) as f64;
        Self {
            per_day: last_week as f64 / 7.0,
            unlabelled: u32::try_from(map.unlabelled).unwrap_or(u32::MAX),
            night,
            fresh,
            bands: clis
                .into_iter()
                .map(|(cli, count)| Band {
                    colour: colour_of(cli),
                    share: count as f64 / total,
                })
                .collect(),
        }
    }
}

/// Whole days from `at` back to `end`, by the calendar where `end` is, as the ground counts
/// them.
fn days_back(at: OffsetDateTime, end: OffsetDateTime) -> u32 {
    let at = at.to_offset(end.offset()).date();
    (end.date() - at).whole_days().max(0) as u32
}

#[cfg(test)]
mod tests {
    use argonauts_core::behaviors::behavior_map::MapAction;
    use argonauts_core::behaviors::behavior_world::WorldFacts;
    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};

    use super::*;

    const END: OffsetDateTime = datetime!(2026-10-14 20:00 UTC);

    fn action(cli: &str, back: Duration) -> MapAction {
        MapAction {
            x: 0.4,
            y: 0.6,
            group: 0,
            behavior: "read files".to_owned(),
            label: "read files".to_owned(),
            summary: None,
            agent: "build".to_owned(),
            model: "Opus 5.5".to_owned(),
            cli: cli.to_owned(),
            role: "builder".to_owned(),
            workflow: "Maker-checker".to_owned(),
            project: "example".to_owned(),
            run: None,
            at: END - back,
            nth: 1,
            of: 1,
            tool: "Read".to_owned(),
            input: "src/lib.rs".to_owned(),
            place: None,
            run_shows: None,
            failed: false,
            outcome: None,
        }
    }

    fn colour_of(cli: &str) -> Rgb {
        match cli {
            "Codex" => [106.0, 166.0, 255.0],
            _ => [224.0, 138.0, 102.0],
        }
    }

    #[test]
    fn the_planet_reads_its_pace_its_nights_and_its_bands_from_the_map() {
        let actions = vec![
            action("Claude Code", Duration::hours(1)),
            action("Claude Code", Duration::days(2)),
            // 23:30 the evening before the end, and 02:00 the night before that.
            action("Codex", Duration::minutes(20 * 60 + 30)),
            action("Codex", Duration::hours(42)),
            action("Claude Code", Duration::days(12)),
            action("Claude Code", Duration::days(20)),
        ];
        let map = BehaviorMap {
            actions,
            unlabelled: 3,
            ..BehaviorMap::default()
        };
        let facts = PlanetFacts::from_map(&map, colour_of);
        assert_eq!(facts.fresh, [true, true, true, true, false, false]);
        assert!((facts.per_day - 4.0 / 7.0).abs() < 1e-12);
        assert_eq!(facts.unlabelled, 3);
        assert_eq!(facts.night, 2);
        assert_eq!(facts.bands.len(), 2);
        assert_eq!(facts.bands[0].colour, colour_of("Claude Code"));
        assert!((facts.bands[0].share - 4.0 / 6.0).abs() < 1e-12);

        // Where the app worked out the world, its night count and its end stand.
        let world = WorldFacts {
            end: END + Duration::days(10),
            crew: argonauts_core::behaviors::behavior_world::CrewFacts {
                night_actions: 5,
                ..Default::default()
            },
            ..WorldFacts::default()
        };
        let worked = BehaviorMap {
            world: Some(Box::new(world)),
            ..map
        };
        let facts = PlanetFacts::from_map(&worked, colour_of);
        assert_eq!(facts.night, 5);
        assert!(facts.fresh.iter().all(|fresh| !fresh));
        assert_eq!(facts.per_day, 0.0);
    }
}
