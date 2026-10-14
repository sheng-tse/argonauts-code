//! How the book weighs the outcome log: which outcomes count, how much each one counts, and the
//! tallies a sparse cell borrows from.

use std::collections::BTreeMap;

use time::macros::format_description;
use time::{Date, OffsetDateTime, Time};

use argonauts_core::RunId;
use argonauts_core::limits::TrackRecord;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::protocol::RoutingSettings;

use crate::outcomes::{LogLine, Outcome};
use crate::types::Assignment;

use super::{Beta, POOLED_ALL_TYPES, POOLED_CREW};

/// Cost and time ratios stop here, so one runaway task can't bury a worker.
const RATIO_CAP: f64 = 2.0;

/// Weighted passes and failures, before any prior.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Tally {
    yes: f64,
    no: f64,
}

impl Tally {
    /// `value` runs from 0 to 1, so a relative approval can count as part of one.
    fn add(&mut self, value: f64, weight: f64) {
        self.yes += weight * value;
        self.no += weight * (1.0 - value);
    }

    pub(super) fn weight(&self) -> f64 {
        self.yes + self.no
    }

    fn on(&self, prior: Beta) -> Beta {
        Beta {
            alpha: prior.alpha + self.yes,
            beta: prior.beta + self.no,
        }
    }

    fn without(&self, part: Tally) -> Tally {
        Tally {
            yes: (self.yes - part.yes).max(0.0),
            no: (self.no - part.no).max(0.0),
        }
    }
}

/// One rate's tallies at every level a cell can borrow from.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Pools {
    crew: Tally,
    workers: BTreeMap<ModelRef, Tally>,
    groups: BTreeMap<(ModelRef, String), Tally>,
    cells: BTreeMap<(ModelRef, String), Tally>,
}

impl Pools {
    fn add(
        &mut self,
        worker: &ModelRef,
        task_type: &str,
        settings: &RoutingSettings,
        value: f64,
        weight: f64,
    ) {
        self.crew.add(value, weight);
        self.workers
            .entry(worker.clone())
            .or_default()
            .add(value, weight);
        if let Some(group) = group_of(task_type, settings) {
            self.groups
                .entry((worker.clone(), group.to_owned()))
                .or_default()
                .add(value, weight);
        }
        self.cells
            .entry((worker.clone(), task_type.to_owned()))
            .or_default()
            .add(value, weight);
    }

    pub(super) fn cell(&self, worker: &ModelRef, task_type: &str) -> Tally {
        self.cells
            .get(&(worker.clone(), task_type.to_owned()))
            .copied()
            .unwrap_or_default()
    }

    /// The posterior a cell is scored with, what it borrowed from, and the weighted tasks
    /// behind it. The prior is centered on the worker's rate on other task types, itself
    /// centered on the crew's; a sparse cell takes its group's evidence, then the worker's.
    pub(super) fn posterior(
        &self,
        worker: &ModelRef,
        task_type: &str,
        configured: Option<f64>,
        sparse: bool,
        settings: &RoutingSettings,
    ) -> (Beta, Option<String>, f64) {
        let strength = settings.prior_strength;
        let crew_rate = self.crew.on(Beta::prior(0.5, strength)).mean();
        let overall = self.workers.get(worker).copied().unwrap_or_default();
        // Leaving out the evidence the posterior adds itself, so no task counts twice.
        let elsewhere = |part: Tally| {
            let rate = overall.without(part).on(Beta::prior(crew_rate, strength));
            Beta::prior(rate.mean(), strength)
        };
        let cell = self.cell(worker, task_type);
        // A rate you configured is your knowledge of this very cell, so it outranks pooling.
        if let Some(rate) = configured {
            return (cell.on(Beta::prior(rate, strength)), None, cell.weight());
        }
        if !sparse {
            return (cell.on(elsewhere(cell)), None, cell.weight());
        }
        if let Some(group) = group_of(task_type, settings) {
            let tally = self
                .groups
                .get(&(worker.clone(), group.to_owned()))
                .copied()
                .unwrap_or_default();
            if tally.weight() > 0.0 {
                return (
                    tally.on(elsewhere(tally)),
                    Some(group.to_owned()),
                    tally.weight(),
                );
            }
        }
        if overall.weight() > 0.0 {
            return (
                overall.on(Beta::prior(crew_rate, strength)),
                Some(POOLED_ALL_TYPES.to_owned()),
                overall.weight(),
            );
        }
        (
            Beta::prior(crew_rate, strength),
            Some(POOLED_CREW.to_owned()),
            0.0,
        )
    }
}

/// An outcome the book counts, and how much.
pub(super) struct Counted<'a> {
    pub(super) outcome: &'a Outcome,
    /// Recency alone, which a reviewer's usual rate uses.
    recency: f64,
    /// Recency times the shrink for an older model.
    weight: f64,
}

impl Counted<'_> {
    /// Fallbacks and retries inherit tasks that already failed.
    fn first_pass(&self) -> bool {
        !matches!(
            self.outcome.assigned,
            Some(Assignment::Fallback | Assignment::Retry)
        )
    }
}

/// What the log says as of one moment, before any rate is formed.
pub(super) struct Evidence<'a> {
    pub(super) outcomes: Vec<Counted<'a>>,
    /// Each worker's newest model id.
    pub(super) models: BTreeMap<ModelRef, String>,
    resets: BTreeMap<String, OffsetDateTime>,
}

impl<'a> Evidence<'a> {
    pub(super) fn gather(
        lines: &'a [LogLine],
        settings: &RoutingSettings,
        as_of: OffsetDateTime,
    ) -> Self {
        let run_dates: BTreeMap<&RunId, OffsetDateTime> = lines
            .iter()
            .filter_map(|line| match line {
                LogLine::Run(run) => Some((&run.run, run.at)),
                _ => None,
            })
            .collect();
        let resets = resets(settings);
        let mut kept = Vec::new();
        for line in lines {
            let LogLine::Outcome(outcome) = line else {
                continue;
            };
            // Lines from before outcomes were dated take their run's date when it's known.
            let at = outcome.at.or_else(|| run_dates.get(&outcome.run).copied());
            if at.is_some_and(|at| at > as_of) {
                continue;
            }
            if let Some(reset) = setting_for(&resets, &outcome.worker) {
                // Without a date there's no telling the outcome came after the reset.
                if at.is_none_or(|at| at < *reset) {
                    continue;
                }
            }
            kept.push((outcome, at));
        }
        let models = newest_models(&kept);
        let keep = settings.model_change_keep.clamp(0.0, 1.0);
        let outcomes = kept
            .into_iter()
            .map(|(outcome, at)| {
                let recency = recency(at, as_of, settings.half_life_days);
                let older_model = match (&outcome.model_id, models.get(&outcome.worker)) {
                    (Some(model), Some(newest)) => model != newest,
                    _ => false,
                };
                let weight = if older_model { recency * keep } else { recency };
                Counted {
                    outcome,
                    recency,
                    weight,
                }
            })
            .collect();
        Self {
            outcomes,
            models,
            resets,
        }
    }

    /// Whether `worker`'s reset drops something that happened at `at`.
    pub(super) fn reset_drops(&self, worker: &ModelRef, at: OffsetDateTime) -> bool {
        setting_for(&self.resets, worker).is_some_and(|reset| at < *reset)
    }

    pub(super) fn pass_pools(&self, settings: &RoutingSettings) -> Pools {
        let mut pools = Pools::default();
        for counted in self.outcomes.iter().filter(|counted| counted.first_pass()) {
            let outcome = counted.outcome;
            let value = if outcome.first_try { 1.0 } else { 0.0 };
            pools.add(
                &outcome.worker,
                &outcome.task_type,
                settings,
                value,
                counted.weight,
            );
        }
        pools
    }

    /// Approvals measured against each reviewer's usual rate and recentered on the crew's, so
    /// a strict reviewer's rejection costs a worker less than a lenient one's.
    pub(super) fn approval_pools(&self, settings: &RoutingSettings) -> Pools {
        let strength = settings.prior_strength;
        let reviewed = || {
            self.outcomes.iter().filter_map(|counted| {
                let approved = counted.outcome.approved_first_review?;
                Some((counted, if approved { 1.0 } else { 0.0 }))
            })
        };
        let mut crew = Tally::default();
        let mut reviewers: BTreeMap<&ModelRef, Tally> = BTreeMap::new();
        for (counted, value) in reviewed() {
            crew.add(value, counted.recency);
            if let Some(reviewer) = &counted.outcome.reviewer {
                reviewers
                    .entry(reviewer)
                    .or_default()
                    .add(value, counted.recency);
            }
        }
        let crew_rate = crew.on(Beta::prior(0.5, strength)).mean();
        let mut pools = Pools::default();
        for (counted, value) in reviewed() {
            let outcome = counted.outcome;
            let usual = outcome
                .reviewer
                .as_ref()
                .and_then(|reviewer| reviewers.get(reviewer))
                .map_or(crew_rate, |tally| {
                    tally.on(Beta::prior(crew_rate, strength)).mean()
                });
            let relative = (value - usual + crew_rate).clamp(0.0, 1.0);
            pools.add(
                &outcome.worker,
                &outcome.task_type,
                settings,
                relative,
                counted.weight,
            );
        }
        pools
    }

    /// Plain counts and cost and time ratios for every cell with an outcome.
    pub(super) fn cells(&self) -> BTreeMap<(ModelRef, String), CellFacts> {
        let mut costs: Medians = BTreeMap::new();
        let mut times: Medians = BTreeMap::new();
        let mut counts: BTreeMap<(ModelRef, String), TrackRecord> = BTreeMap::new();
        for counted in &self.outcomes {
            let outcome = counted.outcome;
            let key = (outcome.worker.clone(), outcome.task_type.clone());
            let count = counts
                .entry(key.clone())
                .or_insert(TrackRecord { passed: 0, of: 0 });
            if counted.first_pass() {
                count.of += 1;
                count.passed += u32::from(outcome.first_try);
            }
            add_value(&mut costs, &key, outcome.spent_usd, counted.weight);
            if let Some(minutes) = outcome.minutes {
                add_value(&mut times, &key, minutes, counted.weight);
            }
        }
        let cost_medians = medians(costs);
        let time_medians = medians(times);
        counts
            .into_iter()
            .map(|(key, counts)| {
                let facts = CellFacts {
                    cost_ratio: cost_medians.ratio(&key),
                    time_ratio: time_medians.ratio(&key),
                    counts,
                };
                (key, facts)
            })
            .collect()
    }
}

/// What a cell's record holds besides its rates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CellFacts {
    pub(super) cost_ratio: f64,
    pub(super) time_ratio: f64,
    pub(super) counts: TrackRecord,
}

impl Default for CellFacts {
    fn default() -> Self {
        Self {
            cost_ratio: 1.0,
            time_ratio: 1.0,
            counts: TrackRecord { passed: 0, of: 0 },
        }
    }
}

/// Weighted values per cell, and per task type across workers under the type alone.
type Medians = BTreeMap<(Option<ModelRef>, String), Vec<(f64, f64)>>;

fn add_value(values: &mut Medians, key: &(ModelRef, String), value: f64, weight: f64) {
    values
        .entry((Some(key.0.clone()), key.1.clone()))
        .or_default()
        .push((value, weight));
    values
        .entry((None, key.1.clone()))
        .or_default()
        .push((value, weight));
}

struct MedianTable(BTreeMap<(Option<ModelRef>, String), f64>);

impl MedianTable {
    /// The cell's median over its task type's, 1.0 when either is unknown.
    fn ratio(&self, key: &(ModelRef, String)) -> f64 {
        let cell = self.0.get(&(Some(key.0.clone()), key.1.clone()));
        let crew = self.0.get(&(None, key.1.clone()));
        match (cell, crew) {
            (Some(cell), Some(crew)) if *crew > 0.0 => (cell / crew).min(RATIO_CAP),
            _ => 1.0,
        }
    }
}

fn medians(values: Medians) -> MedianTable {
    MedianTable(
        values
            .into_iter()
            .filter_map(|(key, values)| weighted_median(values).map(|median| (key, median)))
            .collect(),
    )
}

/// The value where half the weight lies on either side, the lower one on a tie.
fn weighted_median(mut values: Vec<(f64, f64)>) -> Option<f64> {
    values.retain(|(value, weight)| value.is_finite() && *weight > 0.0);
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = values.iter().map(|(_, weight)| weight).sum::<f64>() / 2.0;
    let mut seen = 0.0;
    for (value, weight) in &values {
        seen += weight;
        if seen >= half {
            return Some(*value);
        }
    }
    values.last().map(|(value, _)| *value)
}

/// `0.5^(age / half_life)`. A half-life of zero or less would weigh everything to nothing, so
/// it turns decay off instead; an undated line counts in full.
fn recency(at: Option<OffsetDateTime>, as_of: OffsetDateTime, half_life_days: f64) -> f64 {
    let Some(at) = at else {
        return 1.0;
    };
    if half_life_days.is_nan() || half_life_days <= 0.0 {
        return 1.0;
    }
    let age_days = ((as_of - at).as_seconds_f64() / 86_400.0).max(0.0);
    0.5_f64.powf(age_days / half_life_days)
}

/// Each worker's model id on its newest outcome that names one. Later lines win a tie, and
/// undated lines count as the oldest.
fn newest_models(kept: &[(&Outcome, Option<OffsetDateTime>)]) -> BTreeMap<ModelRef, String> {
    let mut newest: BTreeMap<&ModelRef, (Option<OffsetDateTime>, &str)> = BTreeMap::new();
    for (outcome, at) in kept {
        let Some(model) = outcome.model_id.as_deref() else {
            continue;
        };
        let later = newest
            .get(&outcome.worker)
            .is_none_or(|(seen, _)| *at >= *seen);
        if later {
            newest.insert(&outcome.worker, (*at, model));
        }
    }
    newest
        .into_iter()
        .map(|(worker, (_, model))| (worker.clone(), model.to_owned()))
        .collect()
}

fn resets(settings: &RoutingSettings) -> BTreeMap<String, OffsetDateTime> {
    let format = format_description!("[year]-[month]-[day]");
    settings
        .resets
        .iter()
        .filter_map(|(worker, date)| match Date::parse(date.trim(), format) {
            Ok(date) => Some((worker.clone(), date.with_time(Time::MIDNIGHT).assume_utc())),
            Err(error) => {
                tracing::warn!("ignoring routing.resets.{worker} = {date:?}: {error}");
                None
            }
        })
        .collect()
}

/// The group `routing.groups` puts a task type in, if any.
pub(super) fn group_of<'a>(task_type: &str, settings: &'a RoutingSettings) -> Option<&'a str> {
    settings
        .groups
        .iter()
        .find(|(_, types)| types.iter().any(|kind| kind == task_type))
        .map(|(group, _)| group.as_str())
}

/// A per-worker setting: the exact `cli/model` key first, then the CLI alone, which
/// covers every model it runs.
pub(super) fn setting_for<'a, T>(map: &'a BTreeMap<String, T>, worker: &ModelRef) -> Option<&'a T> {
    map.get(&worker.to_string())
        .or_else(|| map.get(&worker.agent))
}
