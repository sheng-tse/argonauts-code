//! Each wonder's rule: the one rare thing in your records that raises it, its line in your own
//! numbers, and the behavior it stands beside.

mod crew;
mod runs;

use std::cmp::Reverse;
use std::collections::BTreeMap;

use argonauts_core::behaviors::behavior_world::Wonder;
use argonauts_core::behaviors::{Behavior, Label};
use argonauts_core::models::model_catalog::ModelCatalog;
use time::OffsetDateTime;

use super::store::KeptRun;
use super::{Act, Zone};

/// How many models a line names at most.
const MODELS_NAMED: usize = 5;

/// Everything the rules read.
pub(super) struct Records<'a> {
    /// Each run's tally, by `<project folder>/<run>`.
    pub tallies: &'a BTreeMap<String, KeptRun>,
    pub acts: &'a [Act<'a>],
    pub now: OffsetDateTime,
    pub zone: Zone,
    pub catalog: &'a ModelCatalog,
}

/// A wonder whose rule holds in the records as they are now.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Found {
    pub wonder: Wonder,
    /// When the record that raised it was made.
    pub at: OffsetDateTime,
    /// What its line says after its name.
    pub line: String,
    /// The models the line names, each `cli/model`.
    pub models: Vec<String>,
    /// The line to keep for when these facts are gone, where the live one would turn untrue.
    pub lasting: Option<String>,
    pub about: About,
}

/// What a wonder's behavior is picked from, so it stands by what raised it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum About {
    /// The busiest behavior with one of the first group's labels, else of the next group's.
    Labels(&'static [&'static [Label]]),
    /// The busiest behavior of one model, `cli/model`.
    Model(String),
    /// Your second busiest behavior, beside the busiest without hiding it.
    SecondBusiest,
    Busiest,
    Behavior(Behavior),
}

impl About {
    /// The behavior to stand beside, or your busiest when none fits. `None` before any action
    /// has a place on the map.
    pub fn place(&self, acts: &[Act<'_>]) -> Option<Behavior> {
        let fits = match self {
            Self::Labels(groups) => groups.iter().find_map(|labels| {
                busiest(acts, |act| {
                    labels.iter().any(|label| act.behavior.has(*label))
                })
            }),
            Self::Model(model) => busiest(acts, |act| model_of(act) == *model),
            Self::SecondBusiest => {
                let first = busiest(acts, |_| true)?;
                busiest(acts, |act| act.behavior != first).or(Some(first))
            }
            Self::Busiest => None,
            Self::Behavior(behavior) => Some(*behavior),
        };
        fits.or_else(|| busiest(acts, |_| true))
    }
}

/// The behavior most of the matching actions count under, a label of its own before a pair.
fn busiest(acts: &[Act<'_>], matches: impl Fn(&Act<'_>) -> bool) -> Option<Behavior> {
    let mut counts: BTreeMap<Behavior, usize> = BTreeMap::new();
    for act in acts
        .iter()
        .filter(|act| act.behavior.is_labelled() && matches(act))
    {
        *counts.entry(act.behavior).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(behavior, count)| {
            (
                matches!(behavior, Behavior::One(_)),
                *count,
                Reverse(*behavior),
            )
        })
        .map(|(behavior, _)| behavior)
}

pub(super) fn model_of(act: &Act<'_>) -> String {
    match act.model {
        Some(model) => format!("{}/{model}", act.cli),
        None => act.cli.to_owned(),
    }
}

/// Every wonder whose rule holds now.
pub(super) fn found(records: &Records<'_>) -> Vec<Found> {
    [
        runs::argo(records),
        runs::fleece(records),
        runs::clashing_rocks(records),
        crew::talos(records),
        runs::sirens(records),
        runs::dragon(records),
        runs::harpies(records),
        runs::dodona(records),
        crew::pyramid(records),
        crew::gardens(records),
        runs::colossus(records),
        runs::pharos(records),
        runs::artemis(records),
        crew::zeus(records),
        crew::mausoleum(records),
        crew::atlantis(records),
    ]
    .into_iter()
    .flatten()
    .collect()
}

impl Found {
    pub(super) fn new(
        wonder: Wonder,
        at: OffsetDateTime,
        line: String,
        models: Vec<String>,
        about: About,
    ) -> Self {
        Self {
            wonder,
            at,
            line,
            models,
            lasting: None,
            about,
        }
    }
}
