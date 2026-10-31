//! The Behaviors map, built from the run records whenever the tab asks, at no token cost: every
//! labelled action as a dot near its label's place, grouped as the tab asks, and for each group
//! its density difference against the rest and the behaviors it shows most.

pub(crate) mod read;
pub(crate) mod world;

use std::collections::BTreeMap;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::base::clock;
use argonauts_core::behaviors::behavior_map::{
    ActionOutcome, BehaviorMap, BehaviorRow, Comparison, Grouping, MapAction, MapGroup, MapRequest,
    Placing, Scope,
};
use argonauts_core::behaviors::deep_map::says_more_than;
use argonauts_core::look::display_names::{DisplayNames, Spellings};
use argonauts_core::settings::behaviors::BehaviorMap as MapSetting;
use argonauts_tui::base::format::WallClock;

pub(crate) use read::Recorded;
use read::{Records, every_action, project_name, records};

use crate::behaviors::deep::places::{KeptPlaces, kept_places};
use crate::behaviors::map::{self, Counted, NoOverlay, Point};
use crate::controller::{Ctx, Effect};

/// An action's label, and the label's place on the map, both in 0..1.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placed {
    pub label: String,
    pub place: (f64, f64),
}

/// How many behaviors a group's list ranks, and how many dots Summaries' row lights: the
/// action itself and the ones summarized most like it.
const LISTED: usize = 8;
const SUMMARIZED_ALIKE: usize = 14;

/// An action's label from the list, at the label's place with `y` up as the map draws it.
pub(crate) fn labelled(recorded: &Recorded) -> Option<Placed> {
    let (x, from_top) = recorded.behavior.place()?;
    Some(Placed {
        label: recorded.behavior.words(),
        place: (f64::from(x), 1.0 - f64::from(from_top)),
    })
}

/// Reads the records the request covers, builds the map, and sends it to the screen.
pub(crate) async fn view(ctx: &mut Ctx, request: MapRequest) -> anyhow::Result<Vec<Effect>> {
    let paths = ctx.env.paths.clone();
    let here = ctx.env.state_dir.clone();
    let names = ctx.names.names();
    let wanted = request.map.unwrap_or(ctx.settings().behaviors.map);
    let view = ctx.settings().behaviors.view;
    let map = tokio::task::spawn_blocking(move || {
        let only = match request.scope {
            Scope::Everywhere => None,
            Scope::ThisProject => Some(here.as_path()),
        };
        let period =
            world::Period::new(clock::now_utc(), request.period.days(), WallClock::local());
        // The world reads past the period's start: when each behavior was last used, the
        // period before, and the months before that. A wonder is the whole crew's, so it
        // reads every project's actions whatever the map covers.
        let every = every_action(&paths);
        let wonders = crate::behaviors::wonders::risen(&paths, &every, period.end);
        let mut all = records(every, only);
        spell_models(&mut all.actions, &names);
        let records = all.since(period.start());
        let kept = kept_places(&paths, &names);
        let mut map = match &kept {
            Some(kept) if wanted == MapSetting::Summaries => {
                build_summaries(&records, request, &names, kept)
            }
            _ => build(&records, request, &names),
        };
        // With nothing recorded there's nothing to summarize.
        map.offer_summaries = kept.is_none() && !map.actions.is_empty();
        map.here = Some(project_name(&here));
        map.view = view;
        let zone = world::storm::zone();
        let mut world = world::facts(&all.actions, &period, &names, zone.as_deref());
        world.wonders = wonders;
        map.world = Some(Box::new(world));
        map
    })
    .await?;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Behaviors { map: Box::new(map) }),
    });
    Ok(Vec::new())
}

/// Names each action's model the one way Stats does: the catalog's spelling of it, and for an
/// action whose session named none, the default its CLI runs. So Codex's sessions on its
/// default model join that model's group, hover line and star rather than reading as Codex.
fn spell_models(actions: &mut [Recorded], names: &DisplayNames) {
    let mut spellings = Spellings::new(names);
    for action in actions.iter() {
        spellings.meet(&action.cli, action.model.as_deref());
    }
    for action in actions {
        action.model = spellings.of(&action.cli, action.model.as_deref());
    }
}

/// Where an action sits and what it counts under.
struct Spot {
    x: f32,
    y: f32,
    behavior: String,
    label: String,
    summary: Option<String>,
}

/// The map of `records` as `request` groups them, each action near its label's place.
pub(crate) fn build(records: &Records, request: MapRequest, names: &DisplayNames) -> BehaviorMap {
    build_placed(records, request, names, Placing::Labels, &|recorded| {
        let placed = labelled(recorded)?;
        let point = map::jittered(placed.place, &recorded.key);
        Some(Spot {
            x: point.x as f32,
            y: point.y as f32,
            behavior: placed.label.clone(),
            label: placed.label,
            summary: None,
        })
    })
}

/// Summaries of `records`: each action at its summary's place, counting under its
/// summary, its label beside it. An action no label fits has a place too; one not summarized
/// yet waits for the next update, and is counted for `d` to offer.
pub(crate) fn build_summaries(
    records: &Records,
    request: MapRequest,
    names: &DisplayNames,
    kept: &KeptPlaces,
) -> BehaviorMap {
    let placing = Placing::Summaries {
        by: kept.by.clone(),
    };
    let mut out = build_placed(records, request, names, placing, &|recorded| {
        let place = kept.of(&recorded.key)?;
        let label = recorded.behavior.words();
        Some(Spot {
            x: place.x,
            y: place.y,
            behavior: place.summary.clone(),
            label,
            summary: Some(place.summary.clone()),
        })
    });
    out.unsummarized = out.unlabelled;
    out.unlabelled = 0;
    out.by_words = kept.by_words;
    out
}

fn build_placed(
    records: &Records,
    request: MapRequest,
    names: &DisplayNames,
    placing: Placing,
    spot: &dyn Fn(&Recorded) -> Option<Spot>,
) -> BehaviorMap {
    let mut out = BehaviorMap {
        request,
        placing,
        runs: records.runs,
        conversations: records.conversations,
        offer_summaries: true,
        ..BehaviorMap::default()
    };
    let mut groups: Vec<MapGroup> = Vec::new();
    let mut projects: Vec<String> = Vec::new();
    for recorded in &records.actions {
        let Some(spot) = spot(recorded) else {
            out.unlabelled += 1;
            continue;
        };
        let group = group_of(recorded, request.grouping, names);
        let at = match groups.iter().position(|known| known.name == group.name) {
            Some(at) => at,
            None => {
                groups.push(group);
                groups.len() - 1
            }
        };
        groups[at].actions += 1;
        if !projects.contains(&recorded.project) {
            projects.push(recorded.project.clone());
        }
        out.actions.push(MapAction {
            x: spot.x,
            y: spot.y,
            group: at,
            behavior: spot.behavior,
            label: spot.label,
            summary: spot.summary,
            agent: recorded.agent.clone(),
            model: names
                .model(&recorded.cli, recorded.model.as_deref())
                .unwrap_or_else(|| names.cli(&recorded.cli)),
            cli: names.cli(&recorded.cli),
            role: recorded.role.clone(),
            workflow: recorded.workflow.clone(),
            project: recorded.project.clone(),
            run: recorded.run.clone(),
            at: recorded.at,
            nth: recorded.nth,
            of: recorded.of,
            tool: recorded.tool.clone(),
            input: recorded.what.clone(),
            place: recorded.place.clone(),
            run_shows: recorded.run_shows.clone(),
            failed: recorded.failed,
            outcome: recorded.outcome.map(|outcome| match outcome {
                read::Outcome::Merged => ActionOutcome::Merged,
                read::Outcome::SentBack => ActionOutcome::SentBack,
                read::Outcome::Dropped => ActionOutcome::Dropped,
            }),
        });
    }
    out.groups = order_groups(groups, &mut out.actions);
    out.projects = projects;
    out.comparisons = compare_all(&out);
    out
}

/// The groups, largest first, with each action's group index following its group.
fn order_groups(groups: Vec<MapGroup>, actions: &mut [MapAction]) -> Vec<MapGroup> {
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|a, b| groups[*b].actions.cmp(&groups[*a].actions).then(a.cmp(b)));
    let mut moved = vec![0; groups.len()];
    for (new, old) in order.iter().enumerate() {
        moved[*old] = new;
    }
    for action in actions.iter_mut() {
        action.group = moved[action.group];
    }
    order.into_iter().map(|old| groups[old].clone()).collect()
}

fn group_of(recorded: &Recorded, grouping: Grouping, names: &DisplayNames) -> MapGroup {
    let plain = |name: &str| MapGroup {
        name: name.to_owned(),
        ..MapGroup::default()
    };
    match grouping {
        Grouping::Model => MapGroup {
            name: names
                .model(&recorded.cli, recorded.model.as_deref())
                .unwrap_or_else(|| names.cli(&recorded.cli)),
            agent: Some(recorded.cli.clone()),
            model: recorded.model.clone(),
            actions: 0,
        },
        Grouping::Cli => MapGroup {
            name: names.cli(&recorded.cli),
            agent: Some(recorded.cli.clone()),
            ..MapGroup::default()
        },
        Grouping::Role => plain(&recorded.role),
        Grouping::Workflow => plain(&recorded.workflow),
        Grouping::Project => plain(&recorded.project),
        Grouping::Outcome => plain(outcome_group(recorded)),
    }
}

/// How the work an action was part of ended, as the outcome grouping names it: an action
/// outside any task, as the main agent's in a conversation and a run's own steps are, has no
/// outcome to wait for.
fn outcome_group(recorded: &Recorded) -> &'static str {
    match recorded.outcome {
        Some(outcome) => outcome.words(),
        None if recorded.run.is_none() => "in conversation",
        None if !recorded.in_task => "no task",
        None => "not finished",
    }
}

/// Each group against the rest: its overlay where it has one, and its list.
fn compare_all(map: &BehaviorMap) -> Vec<Comparison> {
    let points: Vec<Vec<Point>> = (0..map.groups.len())
        .map(|group| {
            map.in_group(group)
                .map(|(_, action)| Point {
                    x: f64::from(action.x),
                    y: f64::from(action.y),
                })
                .collect()
        })
        .collect();
    let total = map.actions.len();
    map::differences(&points)
        .into_iter()
        .enumerate()
        .map(|(group, difference)| {
            let mine = map.groups[group].actions;
            let rows = match (&map.placing, &difference) {
                (Placing::Labels, _) => label_rows(map, group, mine, total - mine),
                (Placing::Summaries { .. }, Ok(difference)) => {
                    summary_rows(map, group, &points[group], difference)
                }
                (Placing::Summaries { .. }, Err(_)) => Vec::new(),
            };
            let (overlay, why_not) = match difference {
                Ok(difference) => (Some(difference.scaled()), None),
                Err(why) => (None, Some(why_not(why, map.request.grouping))),
            };
            Comparison {
                overlay,
                why_not,
                rows,
                mine,
                others: total - mine,
            }
        })
        .collect()
}

/// Summaries' list for `group`: its actions in the paper's order, each row an action's
/// summary, lighting that action and the ones summarized most like it, its nearest dots.
fn summary_rows(
    map: &BehaviorMap,
    group: usize,
    points: &[Point],
    difference: &map::Difference,
) -> Vec<BehaviorRow> {
    let own: Vec<usize> = map.in_group(group).map(|(at, _)| at).collect();
    let near = |at: usize| {
        let here = &map.actions[at];
        let mut by_distance: Vec<usize> = (0..map.actions.len()).collect();
        let distance = |other: &usize| {
            let there = &map.actions[*other];
            (here.x - there.x).hypot(here.y - there.y)
        };
        by_distance.sort_by(|a, b| distance(a).total_cmp(&distance(b)).then(a.cmp(b)));
        by_distance.truncate(SUMMARIZED_ALIKE);
        by_distance
    };
    map::farthest_first(points, &difference.at_points, LISTED)
        .into_iter()
        .filter_map(|chosen| own.get(chosen).copied())
        .map(|at| {
            let action = &map.actions[at];
            let lit = near(at);
            let mine = lit
                .iter()
                .filter(|a| map.actions[**a].group == group)
                .count();
            BehaviorRow {
                behavior: action.behavior.clone(),
                label: Some(action.label.clone()),
                mine,
                others: lit.len() - mine,
                lit,
                new: says_more_than(&action.label),
            }
        })
        .collect()
}

fn why_not(why: NoOverlay, grouping: Grouping) -> String {
    match why {
        NoOverlay::Small(1) => format!(
            "1 action is too few to draw where it acts: it takes {}",
            map::SMALLEST
        ),
        NoOverlay::Small(count) => format!(
            "{count} actions are too few to draw where it acts: it takes {}",
            map::SMALLEST
        ),
        NoOverlay::Alone => format!(
            "No other {} has {} actions to compare it with",
            grouping.word(),
            map::SMALLEST
        ),
    }
}

/// The label map's list for `group`, each row lighting every action under its label.
fn label_rows(map: &BehaviorMap, group: usize, mine: usize, others: usize) -> Vec<BehaviorRow> {
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for action in &map.actions {
        let count = counts.entry(action.behavior.as_str()).or_default();
        if action.group == group {
            count.0 += 1;
        } else {
            count.1 += 1;
        }
    }
    let counted = counts
        .into_iter()
        .map(|(behavior, (mine, others))| Counted {
            behavior: behavior.to_owned(),
            mine,
            others,
        })
        .collect();
    map::by_share(counted, mine, others)
        .into_iter()
        .map(|counted| BehaviorRow {
            lit: map
                .actions
                .iter()
                .enumerate()
                .filter(|(_, action)| action.behavior == counted.behavior)
                .map(|(at, _)| at)
                .collect(),
            behavior: counted.behavior,
            label: None,
            mine: counted.mine,
            others: counted.others,
            new: false,
        })
        .collect()
}

#[cfg(test)]
mod tests;
