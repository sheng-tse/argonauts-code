//! Each model's facts, for its star and its system; the models that shared runs; and the
//! runs of the last day that mixed makers.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::behavior_world::{
    CliFailure, MixedRun, ModelBehavior, ModelFacts, ModelPair, NewerModel,
};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::Settings;
use time::{Duration, OffsetDateTime};

use super::super::read::{Outcome, Recorded};
use super::{Read, behavior_of};

/// A model doing a behavior this many times in a row or more loops on it.
const IN_A_ROW: usize = 4;
/// A run that began this long before the end, or less, is recent enough to fly as a comet.
const RECENT_RUN: Duration = Duration::DAY;

/// The CLI and the model's id, the model's id standing for itself when only the CLI is known.
type Key = (String, String);

fn key_of(recorded: &Recorded) -> Key {
    let model = recorded
        .model
        .clone()
        .unwrap_or_else(|| recorded.cli.clone());
    (recorded.cli.clone(), model)
}

/// The run or conversation a session belongs to.
fn source_of(recorded: &Recorded) -> &str {
    recorded
        .session
        .rsplit_once('/')
        .map_or(recorded.session.as_str(), |(source, _)| source)
}

pub(super) struct Models {
    pub models: Vec<ModelFacts>,
    pub pairs: Vec<ModelPair>,
    pub mixed_runs: Vec<MixedRun>,
}

pub(super) fn facts(read: &Read<'_>, names: &DisplayNames) -> Models {
    let mut models: BTreeMap<Key, ModelFacts> = BTreeMap::new();
    let mut days: BTreeMap<Key, BTreeSet<u32>> = BTreeMap::new();
    let mut sessions: BTreeMap<Key, BTreeSet<&str>> = BTreeMap::new();
    let mut runs: BTreeMap<Key, BTreeSet<&str>> = BTreeMap::new();
    let mut behaviors: BTreeMap<Key, BTreeMap<String, Done>> = BTreeMap::new();
    for recorded in &read.in_period {
        let key = key_of(recorded);
        let day = read.day(recorded.at);
        let facts = models.entry(key.clone()).or_insert_with(|| ModelFacts {
            model: key.1.clone(),
            agent: key.0.clone(),
            maker: maker_of(recorded),
            first_used: day,
            last_used: day,
            ..ModelFacts::default()
        });
        count(facts, recorded, day, read.period.end);
        days.entry(key.clone()).or_default().insert(day);
        sessions
            .entry(key.clone())
            .or_default()
            .insert(recorded.session.as_str());
        runs.entry(key.clone())
            .or_default()
            .insert(source_of(recorded));
        let done = behaviors
            .entry(key)
            .or_default()
            .entry(behavior_of(recorded))
            .or_default();
        done.count += 1;
        done.failed += u32::from(recorded.failed);
        done.last_used = done.last_used.min(day);
        if day < 7 {
            done.days_in_last_7.insert(day);
        }
    }
    for session in &read.sessions {
        follow(session, &mut models, &mut behaviors);
    }
    for (key, facts) in &mut models {
        facts.days_used = days.get(key).map_or(0, |days| days.len() as u32);
        facts.sessions = sessions
            .get(key)
            .map_or(0, |sessions| sessions.len() as u32);
        facts
            .cli_failures
            .sort_by_key(|failure| Reverse(failure.at));
        let mut done: Vec<ModelBehavior> = behaviors
            .remove(key)
            .unwrap_or_default()
            .into_iter()
            .map(|(behavior, done)| ModelBehavior {
                behavior,
                count: done.count,
                failed: done.failed,
                last_used: done.last_used,
                days_in_last_7: done.days_in_last_7.len() as u8,
                runs_in_a_row: done.runs_in_a_row,
            })
            .collect();
        done.sort_by_key(|behavior| Reverse(behavior.count));
        facts.behaviors = done;
    }
    let mut order: Vec<Key> = models.keys().cloned().collect();
    order.sort_by_key(|key| Reverse(models[key].actions));
    newer_models(&order, &mut models, read, names);
    let pairs = pairs(&order, &runs, read);
    let mixed_runs = mixed_runs(read, &models);
    Models {
        models: order.iter().filter_map(|key| models.remove(key)).collect(),
        pairs,
        mixed_runs,
    }
}

/// What one model did of one behavior, while it's counted.
struct Done {
    count: u32,
    failed: u32,
    last_used: u32,
    days_in_last_7: BTreeSet<u32>,
    runs_in_a_row: u32,
}

impl Default for Done {
    fn default() -> Self {
        Self {
            count: 0,
            failed: 0,
            last_used: u32::MAX,
            days_in_last_7: BTreeSet::new(),
            runs_in_a_row: 0,
        }
    }
}

/// The maker as the registry names providers: from the model's id, else the CLI's own maker.
fn maker_of(recorded: &Recorded) -> String {
    argonauts_registry::models::cli_entry(
        &Settings::default(),
        &recorded.cli,
        recorded.model.as_deref(),
    )
    .provider
}

fn count(facts: &mut ModelFacts, recorded: &Recorded, day: u32, end: OffsetDateTime) {
    facts.actions += 1;
    facts.failed += u32::from(recorded.failed);
    let age = end - recorded.at;
    facts.last_7_days += u32::from(day < 7);
    facts.last_24_hours += u32::from(age < Duration::DAY);
    facts.last_hour += u32::from(age < Duration::HOUR);
    facts.last_10_minutes += u32::from(age < Duration::minutes(10));
    facts.first_used = facts.first_used.max(day);
    facts.last_used = facts.last_used.min(day);
    match recorded.outcome {
        Some(Outcome::Merged) => facts.merged += 1,
        Some(Outcome::SentBack) => facts.sent_back += 1,
        Some(Outcome::Dropped) => facts.dropped += 1,
        None => {}
    }
    facts.last_action = facts.last_action.max(Some(recorded.at));
}

/// A session's loops, each the model's that took it, and its end when its CLI failed.
fn follow(
    session: &[&Recorded],
    models: &mut BTreeMap<Key, ModelFacts>,
    behaviors: &mut BTreeMap<Key, BTreeMap<String, Done>>,
) {
    let labelled: Vec<&Recorded> = session
        .iter()
        .copied()
        .filter(|recorded| recorded.behavior.is_labelled())
        .collect();
    for run in labelled.chunk_by(|a, b| a.behavior == b.behavior) {
        if run.len() >= IN_A_ROW
            && let Some(done) = behaviors
                .get_mut(&busiest_model(run))
                .and_then(|done| done.get_mut(&behavior_of(run[0])))
        {
            done.runs_in_a_row += 1;
        }
    }
    let Some(last) = session.last() else {
        return;
    };
    if last.ended.is_some_and(|how| how.cli_failed())
        && let Some(facts) = models.get_mut(&busiest_model(session))
    {
        facts.cli_failures.push(CliFailure {
            at: last.ended_at.unwrap_or(last.at),
        });
    }
}

/// The model that took the most of `actions`, the first met of those that took as many. A
/// session runs one model, so this is its model; it only chooses where records disagree.
fn busiest_model(actions: &[&Recorded]) -> Key {
    let mut counts: Vec<(Key, usize)> = Vec::new();
    for recorded in actions {
        let key = key_of(recorded);
        match counts.iter_mut().find(|(known, _)| *known == key) {
            Some((_, count)) => *count += 1,
            None => counts.push((key, 1)),
        }
    }
    let most = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
    counts
        .into_iter()
        .find(|(_, count)| *count == most)
        .map(|(key, _)| key)
        .unwrap_or_default()
}

/// For each model, the newest of its line its maker made, where you used one: the one of them
/// used most in the last 7 days. A line is the model's name without its version, so 5.5 Haiku
/// and 4.5 Haiku are one line, as 6 Luna and 5.6 Luna are.
fn newer_models(
    order: &[Key],
    models: &mut BTreeMap<Key, ModelFacts>,
    read: &Read<'_>,
    names: &DisplayNames,
) {
    let line_of = |key: &Key| line(&names.model(&key.0, Some(&key.1)).unwrap_or_default());
    let mut newer: Vec<(Key, NewerModel)> = Vec::new();
    for older in order {
        let Some((line_name, version)) = line_of(older) else {
            continue;
        };
        let maker = &models[older].maker;
        let newest = order
            .iter()
            .filter(|other| *other != older && models[*other].maker == *maker)
            .filter(|other| {
                line_of(other).is_some_and(|(other_line, other_version)| {
                    other_line == line_name && other_version > version
                })
            })
            .max_by_key(|other| (models[*other].last_7_days, Reverse(*other)));
        if let Some(newest) = newest {
            let came = read
                .labelled
                .iter()
                .find(|recorded| key_of(recorded) == *newest)
                .map_or(0, |first| read.day(first.at));
            newer.push((
                older.clone(),
                NewerModel {
                    model: newest.1.clone(),
                    came,
                },
            ));
        }
    }
    for (key, model) in newer {
        if let Some(facts) = models.get_mut(&key) {
            facts.newer = Some(model);
        }
    }
}

/// A model's line and version from its name: `GPT 5.6 Luna` is the line `GPT Luna` at 5.6.
pub(super) fn line(name: &str) -> Option<(String, f64)> {
    let words: Vec<&str> = name.split_whitespace().collect();
    let at = words.iter().position(|word| is_version(word))?;
    let version = words[at].parse().ok()?;
    let line = words
        .iter()
        .enumerate()
        .filter(|(n, _)| *n != at)
        .map(|(_, word)| *word)
        .collect::<Vec<_>>()
        .join(" ");
    Some((line, version))
}

fn is_version(word: &str) -> bool {
    let mut parts = word.split('.');
    let whole = parts
        .next()
        .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    let rest: Vec<&str> = parts.collect();
    whole
        && rest.len() <= 1
        && rest
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Every two models that shared a run, the most above chance first.
fn pairs(order: &[Key], runs: &BTreeMap<Key, BTreeSet<&str>>, read: &Read<'_>) -> Vec<ModelPair> {
    let all_runs: BTreeSet<&str> = read.in_period.iter().map(|r| source_of(r)).collect();
    let empty = BTreeSet::new();
    let mut pairs = Vec::new();
    for (at, one) in order.iter().enumerate() {
        for other in &order[at + 1..] {
            let (mine, theirs) = (
                runs.get(one).unwrap_or(&empty),
                runs.get(other).unwrap_or(&empty),
            );
            let both = mine.intersection(theirs).count();
            if both == 0 {
                continue;
            }
            let lift = (both * all_runs.len()) as f32 / (mine.len() * theirs.len()) as f32;
            pairs.push(ModelPair {
                models: [one.1.clone(), other.1.clone()],
                runs: both as u32,
                lift,
            });
        }
    }
    pairs.sort_by(|a, b| b.lift.total_cmp(&a.lift));
    pairs
}

/// The runs that began in the last 24 hours with models of two makers or more, newest first.
fn mixed_runs(read: &Read<'_>, models: &BTreeMap<Key, ModelFacts>) -> Vec<MixedRun> {
    let mut by_run: BTreeMap<&str, Vec<&Recorded>> = BTreeMap::new();
    for recorded in &read.in_period {
        by_run
            .entry(source_of(recorded))
            .or_default()
            .push(recorded);
    }
    let mut mixed = Vec::new();
    for (source, actions) in by_run {
        let Some(first) = actions.first() else {
            continue;
        };
        if read.period.end - first.at > RECENT_RUN {
            continue;
        }
        let mut makers: Vec<String> = Vec::new();
        for recorded in &actions {
            let maker = &models[&key_of(recorded)].maker;
            if !makers.contains(maker) {
                makers.push(maker.clone());
            }
        }
        if makers.len() < 2 {
            continue;
        }
        let models = makers
            .iter()
            .map(|maker| {
                let of_maker: Vec<&Recorded> = actions
                    .iter()
                    .copied()
                    .filter(|recorded| models[&key_of(recorded)].maker == *maker)
                    .collect();
                busiest_model(&of_maker).1
            })
            .collect();
        mixed.push(MixedRun {
            run: source.rsplit('/').next().unwrap_or(source).to_owned(),
            at: first.at,
            actions: actions.len() as u32,
            makers,
            models,
        });
    }
    mixed.sort_by_key(|run| Reverse(run.at));
    mixed
}
