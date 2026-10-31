//! The wonders your crew raises over time: from every run kept, or from every action recorded.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::base::names::ModelRef;
use argonauts_core::behaviors::behavior_world::Wonder;
use argonauts_core::behaviors::{Behavior, Label};
use time::{Duration, OffsetDateTime};

use super::{About, Found, Records};

const TALOS_STOPS: usize = 10;
const PYRAMID_DAYS: usize = 100;
const GARDENS_TASKS: usize = 20;
const ZEUS_SETTLES: usize = 25;
const MAUSOLEUM_ACTIONS: usize = 500;
const MAUSOLEUM_QUIET: Duration = Duration::days(60);
const ATLANTIS_AMONG: usize = 5;
const ATLANTIS_ACTIONS: usize = 30;
const ATLANTIS_QUIET: Duration = Duration::days(30);
/// Your crew still works when it acted this recently.
const CREW_AT_WORK: Duration = Duration::days(3);

/// Judges a line names at most.
const JUDGES_NAMED: usize = 3;

/// The `nth` earliest of `times`, counting from 1.
fn nth_earliest(mut times: Vec<OffsetDateTime>, nth: usize) -> Option<OffsetDateTime> {
    times.sort();
    times.get(nth.checked_sub(1)?).copied()
}

/// Talos: your guard checks stop work ten times, and nothing they guard ever merges past them.
pub(super) fn talos(records: &Records<'_>) -> Option<Found> {
    let tallies = || records.tallies.values().map(|run| &run.tally);
    if tallies().any(|tally| tally.guard_breaches > 0) {
        return None;
    }
    let stops: Vec<OffsetDateTime> = tallies()
        .flat_map(|tally| tally.guard_stops.iter().map(|stop| stop.at))
        .collect();
    let count = stops.len();
    let at = nth_earliest(stops, TALOS_STOPS)?;
    Some(Found::new(
        Wonder::Talos,
        at,
        format!("stopped work {count} times, and nothing got past"),
        Vec::new(),
        About::Labels(&[
            &[Label::ReadConfig, Label::WriteConfig, Label::EditConfig],
            &[Label::GitChange],
        ]),
    ))
}

/// The Great Pyramid: your crew has merged work on 100 different days, in a row or not.
pub(super) fn pyramid(records: &Records<'_>) -> Option<Found> {
    let mut merges: Vec<OffsetDateTime> = records
        .tallies
        .values()
        .filter_map(|run| run.tally.merged_at)
        .collect();
    merges.sort();
    let mut days = BTreeSet::new();
    let mut at = None;
    for merged in merges {
        let day = merged.to_offset(records.zone.offset_at(merged)).date();
        if days.insert(day) && days.len() == PYRAMID_DAYS {
            at = Some(merged);
        }
    }
    Some(Found::new(
        Wonder::Pyramid,
        at?,
        format!("work merged on {} different days", days.len()),
        Vec::new(),
        About::Busiest,
    ))
}

/// The Hanging Gardens: your crew adds tests in 20 tasks that merge, every check passing once
/// each has merged.
pub(super) fn gardens(records: &Records<'_>) -> Option<Found> {
    let added: BTreeSet<(&str, &str, &str)> = records
        .acts
        .iter()
        .filter(|act| {
            act.merged
                && (act.behavior.has(Label::EditTestAdded) || act.behavior.has(Label::WriteTest))
        })
        .filter_map(|act| Some((act.repo, act.run, act.task?)))
        .collect();
    let merges: Vec<OffsetDateTime> = records
        .tallies
        .iter()
        .flat_map(|(key, run)| {
            let (repo, run_id) = key.split_once('/').unwrap_or((key, ""));
            let added = &added;
            run.tally
                .merged_tasks
                .iter()
                .filter(move |task| {
                    task.held
                        && (task.wrote_tests || added.contains(&(repo, run_id, task.task.as_str())))
                })
                .map(|task| task.at)
        })
        .collect();
    let count = merges.len();
    let at = nth_earliest(merges, GARDENS_TASKS)?;
    Some(Found::new(
        Wonder::Gardens,
        at,
        format!("{count} merged tasks added tests"),
        Vec::new(),
        About::Labels(&[
            &[Label::EditTestAdded],
            &[Label::WriteTest],
            &[Label::EditTest],
        ]),
    ))
}

/// The Statue of Zeus: the judge settles 25 disagreements, and nothing it let stand fails its
/// checks after merging.
pub(super) fn zeus(records: &Records<'_>) -> Option<Found> {
    let tallies = || records.tallies.values().map(|run| &run.tally);
    if tallies().any(|tally| tally.let_stand_failed) {
        return None;
    }
    let settles: Vec<_> = tallies().flat_map(|tally| &tally.settles).collect();
    let count = settles.len();
    let mut judges: BTreeMap<&str, usize> = BTreeMap::new();
    for settle in &settles {
        *judges.entry(settle.model.as_str()).or_default() += 1;
    }
    let mut judges: Vec<(&str, usize)> = judges.into_iter().collect();
    judges.sort_by_key(|(_, settled)| Reverse(*settled));
    let at = nth_earliest(
        settles.iter().map(|settle| settle.at).collect(),
        ZEUS_SETTLES,
    )?;
    Some(Found::new(
        Wonder::Zeus,
        at,
        format!("settled {count} disagreements, and nothing it let stand failed after merging"),
        judges
            .into_iter()
            .take(JUDGES_NAMED)
            .map(|(judge, _)| judge.to_owned())
            .collect(),
        About::Labels(&[&[Label::AskReview]]),
    ))
}

/// How much a model or a behavior was used, from every action recorded, and when last.
struct Used {
    actions: usize,
    last: OffsetDateTime,
}

/// The Mausoleum: a model that did 500 of your actions or more has done none for 60 days, and
/// a newer model of its line took its place.
pub(super) fn mausoleum(records: &Records<'_>) -> Option<Found> {
    let mut used: BTreeMap<(&str, &str), Used> = BTreeMap::new();
    for act in records.acts {
        let Some(model) = act.model else { continue };
        let it = used.entry((act.cli, model)).or_insert(Used {
            actions: 0,
            last: act.at,
        });
        it.actions += 1;
        it.last = it.last.max(act.at);
    }
    let name = |(cli, model): (&str, &str)| {
        let model = ModelRef::new(cli, Some(model.to_owned()));
        records.catalog.label(&model).name
    };
    let lines: BTreeMap<(&str, &str), Line> = used
        .keys()
        .map(|model| (*model, Line::of(&name(*model))))
        .collect();
    let ((old, it), newer) = used
        .iter()
        .filter(|(_, it)| {
            it.actions >= MAUSOLEUM_ACTIONS && records.now - it.last >= MAUSOLEUM_QUIET
        })
        .filter_map(|(old, it)| {
            let (newer, _) = used
                .iter()
                .filter(|(newer, since)| since.last > it.last && lines[*newer].follows(&lines[old]))
                .max_by_key(|(_, since)| since.actions)?;
            Some(((old, it), newer))
        })
        .max_by_key(|((_, it), _)| it.actions)?;
    let written = |(cli, model): &(&str, &str)| format!("{cli}/{model}");
    Some(Found::new(
        Wonder::Mausoleum,
        it.last + MAUSOLEUM_QUIET,
        format!(
            "rests here after {} actions; {} took its place",
            it.actions,
            name(*newer)
        ),
        vec![written(old), written(newer)],
        About::Model(written(old)),
    ))
}

/// A model's line and version, read from its name: "Haiku 4.5" is Haiku at 4.5, and "GPT 6
/// Luna" is GPT Luna at 6.
#[derive(Debug, PartialEq)]
struct Line {
    /// Its name's words besides the version, lowercased.
    name: Vec<String>,
    version: Vec<u32>,
}

impl Line {
    fn of(name: &str) -> Self {
        let mut words = Vec::new();
        let mut version = Vec::new();
        for word in name.split_whitespace() {
            let numbers: Option<Vec<u32>> = word.split('.').map(|part| part.parse().ok()).collect();
            match numbers {
                Some(numbers) if version.is_empty() => version = numbers,
                _ => words.push(word.to_lowercase()),
            }
        }
        Self {
            name: words,
            version,
        }
    }

    /// Whether this is a later model of the same line as `older`.
    fn follows(&self, older: &Self) -> bool {
        !self.name.is_empty()
            && !older.version.is_empty()
            && self.name == older.name
            && self.version > older.version
    }
}

/// Atlantis: a behavior that was one of your five most used goes unused for 30 days while your
/// crew keeps working.
pub(super) fn atlantis(records: &Records<'_>) -> Option<Found> {
    let newest = records.acts.iter().map(|act| act.at).max()?;
    if records.now - newest > CREW_AT_WORK {
        return None;
    }
    let mut used: BTreeMap<Behavior, Used> = BTreeMap::new();
    for act in records.acts.iter().filter(|act| act.behavior.is_labelled()) {
        let behavior = used.entry(act.behavior).or_insert(Used {
            actions: 0,
            last: act.at,
        });
        behavior.actions += 1;
        behavior.last = behavior.last.max(act.at);
    }
    let mut most_used: Vec<(&Behavior, &Used)> = used.iter().collect();
    most_used.sort_by_key(|(_, it)| Reverse(it.actions));
    let (behavior, sunk) = most_used
        .into_iter()
        .take(ATLANTIS_AMONG)
        .filter(|(_, it)| it.actions >= ATLANTIS_ACTIONS)
        .find(|(_, it)| records.now - it.last >= ATLANTIS_QUIET)?;
    let others = records.acts.iter().filter(|act| act.at > sunk.last).count();
    let days = (records.now - sunk.last).whole_days();
    let actions = sunk.actions;
    let mut found = Found::new(
        Wonder::Atlantis,
        sunk.last + ATLANTIS_QUIET,
        format!(
            "{actions} actions until {days} days ago, none since, while your crew did {others} others"
        ),
        Vec::new(),
        About::Behavior(*behavior),
    );
    found.lasting = Some(format!(
        "{actions} actions, then none for {days} days, while your crew did {others} others"
    ));
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_s_line_is_its_name_without_its_version() {
        let haiku = Line::of("Haiku 4.5");
        assert!(Line::of("Haiku 5.5").follows(&haiku));
        assert!(!Line::of("Haiku 4.5").follows(&haiku));
        assert!(!Line::of("Sonnet 5.5").follows(&haiku));
        assert!(Line::of("GPT 6 Luna").follows(&Line::of("GPT 5.6 Luna")));
        assert!(!Line::of("GPT 6 Sol").follows(&Line::of("GPT 5.6 Luna")));
        assert!(!Line::of("Qwen3 Coder 30B").follows(&Line::of("Qwen3 Coder 30B")));
    }
}
