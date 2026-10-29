//! The world's four made-up crews as the maps the app would send for them, so the tab draws
//! them the way the world's own tests do: each action a dot at its place in its crew's group,
//! with the facts the app works out beside them, its models' among them.

use argonauts_core::behavior_map::{BehaviorMap, MapGroup, MapRequest};
use argonauts_core::behavior_world::{BehaviorFacts, CrewFacts, ModelFacts, Move, WorldFacts};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use time::Duration;
use time::macros::datetime;

use super::sample::action;
use crate::world::cosmos::tests::crews as cosmos;
use crate::world::map::fields::crews::{Crew, Key};
use crate::world::map::nature::crews as nature;

/// A made-up crew's map, its world's facts beside it.
pub(crate) fn map(key: Key) -> BehaviorMap {
    let crew = Crew::get(key);
    let sky = sky(key);
    let ground = crew.facts();
    let found = nature::facts(crew);
    let detail = &crew.detail;
    let start = datetime!(2026-08-03 0:00 UTC);
    let records = records(crew, &sky.models);
    let joined = Joined::new(crew, &records);
    let group_of = |group: usize| joined.into[group];
    let actions = (0..crew.len())
        .map(|at| {
            let minute = f64::from(detail.minute[at]).min(1439.0);
            let minutes = f64::from((detail.period - 1 - detail.day[at]) * 1440) + minute;
            let mut action = action(
                group_of(crew.g[at]),
                &crew.labels[crew.l[at]],
                (crew.x[at] as f32, crew.y[at] as f32),
            );
            action.at = start + Duration::seconds_f64(minutes * 60.0);
            action.failed = !detail.ok[at];
            if let Some(record) = &records[crew.g[at]] {
                action.cli = record.cli.clone();
                action.model = record.said.clone();
            }
            action
        })
        .collect::<Vec<_>>();
    let groups = joined
        .groups
        .iter()
        .map(|members| {
            let record = joined.named_by(members, &records);
            MapGroup {
                name: record.map_or_else(
                    || crew.groups[members[0]].name.clone(),
                    |record| record.name.clone(),
                ),
                // By ids, as the app names a group's CLI and model.
                agent: record.map(|record| record.id.agent.clone()),
                model: record.and_then(|record| record.id.model.clone()),
                actions: crew.g.iter().filter(|g| members.contains(g)).count(),
            }
        })
        .collect();
    // The days the ground grows from are the crew's own, beside what nature found.
    let behaviors = found
        .behaviors
        .iter()
        .zip(&ground.behaviors)
        .map(|(facts, days)| BehaviorFacts {
            last_used: days.last_used,
            days_in_last_7: u8::try_from(days.days_in_last_7).unwrap_or(u8::MAX),
            weeks: days.weeks.clone(),
            after_a_wait: days.after_a_wait,
            last_period: days.last_period,
            ..facts.clone()
        })
        .collect();
    let moves = ground
        .moves
        .iter()
        .map(|moved| Move {
            from: crew.labels[moved.from].clone(),
            to: crew.labels[moved.to].clone(),
            count: moved.count,
        })
        .collect();
    // How many of each crew's sessions were runs, the rest conversations, and the projects
    // they were in.
    let (runs, projects) = match key {
        Key::R => (19, 6),
        Key::S => (31, 4),
        Key::T => (8, 3),
        Key::U => (20, 6),
    };
    let sessions = detail.sessions.len();
    let runs = runs.min(sessions);
    let conversations = sessions - runs;
    BehaviorMap {
        request: MapRequest::default(),
        runs,
        conversations,
        projects: (1..=projects).map(|at| format!("project-{at}")).collect(),
        groups,
        actions,
        unlabelled: usize::try_from(found.unlabelled).unwrap_or(usize::MAX),
        world: Some(Box::new(WorldFacts {
            end: found.end,
            behaviors,
            moves,
            crew: CrewFacts {
                idle_days: ground.idle_days,
                quiet_days: ground.quiet_days,
                quiet_stretches: ground.quiet_stretches.clone(),
                storm: found.storm,
                ..sky.crew
            },
            models: sky.models,
            pairs: sky.pairs,
            mixed_runs: sky.mixed_runs,
            wonders: nature::raised(key),
        })),
        ..BehaviorMap::default()
    }
}

/// What a made-up crew's sky knows: its models, their pairs and the crew's own facts.
fn sky(key: Key) -> WorldFacts {
    match key {
        Key::R => cosmos::r(),
        Key::S => cosmos::s(),
        Key::T => cosmos::t(),
        Key::U => cosmos::u(),
    }
    .facts
}

/// The model each of a made-up crew's groups is, among those its sky knows: the one with as
/// many actions, the first listed among equals.
fn group_models<'a>(crew: &Crew, models: &'a [ModelFacts]) -> Vec<Option<&'a ModelFacts>> {
    let mut left: Vec<&ModelFacts> = models.iter().collect();
    (0..crew.groups.len())
        .map(|group| {
            let count = crew.g.iter().filter(|g| **g == group).count();
            let at = left
                .iter()
                .position(|model| usize::try_from(model.actions) == Ok(count))?;
            Some(left.remove(at))
        })
        .collect()
}

/// What the app's records say of one of a made-up crew's groups: the model they ran, by ids,
/// and the names it shows under.
struct Record {
    id: ModelRef,
    cli: String,
    /// The name its group shows under: the crew's own for a model it names.
    name: String,
    /// The name each of its actions gives its model, as the app's maps name it.
    said: String,
    /// Whether the records name the model, rather than leave it to the CLI's default.
    own: bool,
}

/// Each of a made-up crew's groups as a record of it reads, where its sky knows the model: a
/// group named for its model is that model, and one that bears its CLI's name recorded no
/// model, so it reads as its CLI's default, the one Codex 0.159 runs when none is named.
fn records(crew: &Crew, models: &[ModelFacts]) -> Vec<Option<Record>> {
    let mut names = DisplayNames::default();
    names
        .defaults
        .insert("codex".to_owned(), "gpt-6.1-sol".to_owned());
    group_models(crew, models)
        .iter()
        .enumerate()
        .map(|(group, model)| {
            let model = (*model)?;
            let cli = names.cli(&model.agent);
            let own = crew.groups[group].name != cli && model.model != model.agent;
            let id = names.recorded(&model.agent, own.then_some(model.model.as_str()));
            let said = names
                .model(&id.agent, id.model.as_deref())
                .unwrap_or_else(|| cli.clone());
            let name = if own {
                crew.groups[group].name.clone()
            } else {
                said.clone()
            };
            Some(Record {
                id,
                cli,
                name,
                said,
                own,
            })
        })
        .collect()
}

/// A made-up crew's groups as the app's maps hold them: only those with actions, and one a
/// model, so a CLI's sessions that recorded no model join its default model's group.
struct Joined {
    /// The crew's groups in each of the map's, in the order the crews share them.
    groups: Vec<Vec<usize>>,
    /// The map's group each of the crew's falls in.
    into: Vec<usize>,
}

impl Joined {
    fn new(crew: &Crew, records: &[Option<Record>]) -> Joined {
        let mut joined = Joined {
            groups: Vec::new(),
            into: vec![0; records.len()],
        };
        let id = |group: usize| records[group].as_ref().map(|record| &record.id);
        for group in (0..records.len()).filter(|group| crew.g.contains(group)) {
            let at = match id(group) {
                Some(model) => joined
                    .groups
                    .iter()
                    .position(|members| id(members[0]) == Some(model)),
                None => None,
            };
            let at = at.unwrap_or_else(|| {
                joined.groups.push(Vec::new());
                joined.groups.len() - 1
            });
            joined.groups[at].push(group);
            joined.into[group] = at;
        }
        joined
    }

    /// The record that names a group of the map: one that names its model, else its first.
    fn named_by<'a>(&self, members: &[usize], records: &'a [Option<Record>]) -> Option<&'a Record> {
        let record = |at: &usize| records[*at].as_ref();
        members
            .iter()
            .filter_map(record)
            .find(|record| record.own)
            .or_else(|| members.first().and_then(record))
    }
}

/// Each group's colour in a made-up crew's map: the colour of the dots of the crew's group that
/// names it.
pub(crate) fn colours(key: Key) -> Vec<[u8; 3]> {
    let crew = Crew::get(key);
    let records = records(crew, &sky(key).models);
    let joined = Joined::new(crew, &records);
    joined
        .groups
        .iter()
        .map(|members| {
            let named = members
                .iter()
                .find(|at| records[**at].as_ref().is_some_and(|record| record.own))
                .unwrap_or(&members[0]);
            crew.groups[*named].rgb
        })
        .collect()
}

/// The seed each made-up crew's world grows from.
pub(crate) fn seed(key: Key) -> u32 {
    Crew::get(key).habits.seed
}
