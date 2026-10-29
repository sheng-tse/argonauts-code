//! The wonders: secret landmarks on the Behaviors world, each risen from one rare thing in your
//! own records. A wonder rises once and stays, even when the run behind it is deleted, so the
//! risen ones are kept beside the behavior labels. Nothing here is sent anywhere, nothing lists
//! or hints at the wonders still to come, and every threshold is fixed, so a wonder means the
//! same for everyone.

mod rules;
mod store;
mod tally;

use std::collections::BTreeMap;
use std::fs;

use argonauts_core::Paths;
use argonauts_core::behavior_world::RisenWonder;
use argonauts_core::behaviors::Behavior;
use argonauts_core::journal::read_all;
use argonauts_core::model_catalog::ModelCatalog;
use argonauts_tui::format::WallClock;
use time::{OffsetDateTime, UtcOffset};

use super::actions::{Action, Outcome};
use rules::Found;
use store::{Kept, KeptRun, Store};
use tally::{TALLY_RULES, tally};

/// The clock your night and your days are read by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Zone {
    /// This machine's, at each moment, so a daylight saving switch moves it.
    #[default]
    Local,
    /// One offset at every moment, for reading made-up records the same on every machine.
    #[cfg(test)]
    Fixed(UtcOffset),
}

impl Zone {
    fn offset_at(self, at: OffsetDateTime) -> UtcOffset {
        match self {
            Self::Local => WallClock::local().to_local(at).offset(),
            #[cfg(test)]
            Self::Fixed(offset) => offset,
        }
    }
}

/// One action as the wonders read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Act<'a> {
    pub at: OffsetDateTime,
    pub behavior: Behavior,
    pub cli: &'a str,
    pub model: Option<&'a str>,
    /// The project folder, the run and the task it was part of.
    pub repo: &'a str,
    pub run: &'a str,
    pub task: Option<&'a str>,
    /// Its task merged with this work in it.
    pub merged: bool,
}

impl<'a> From<&'a Action> for Act<'a> {
    fn from(action: &'a Action) -> Self {
        Self {
            at: action.at,
            behavior: action.behavior,
            cli: &action.cli,
            model: action.model.as_deref(),
            repo: &action.repo,
            run: &action.run,
            task: action.task.as_deref(),
            merged: action.outcome == Some(Outcome::Merged),
        }
    }
}

/// The wonders risen by `now`, each beside its behavior, with its line in your own numbers.
/// `actions` are every action recorded: a wonder that reads how a behavior or a model was used
/// over time sees only the days they cover.
pub(crate) fn risen(paths: &Paths, actions: &[Action], now: OffsetDateTime) -> Vec<RisenWonder> {
    let acts: Vec<Act<'_>> = actions.iter().map(Act::from).collect();
    Sky {
        store: Store::of(paths),
        catalog: argonauts_registry::models::shipped_catalog(),
        zone: Zone::Local,
    }
    .risen(&acts, now)
}

/// Where the wonders are read from and kept, and the clock they are read by.
struct Sky<'a> {
    store: Store,
    catalog: &'a ModelCatalog,
    zone: Zone,
}

impl Sky<'_> {
    fn risen(&self, acts: &[Act<'_>], now: OffsetDateTime) -> Vec<RisenWonder> {
        let (mut kept, writable) = self.store.read();
        let runs = self.tallies(&kept.runs);
        let tallies_changed = runs != kept.runs;
        kept.runs = runs;

        let found = rules::found(&rules::Records {
            tallies: &kept.runs,
            acts,
            now,
            zone: self.zone,
            catalog: self.catalog,
        });
        let (shown, risen_changed) = stand(&mut kept, found, acts);
        if writable
            && (tallies_changed || risen_changed)
            && let Err(err) = self.store.write(&kept)
        {
            tracing::warn!("couldn't keep the wonders: {err}");
        }
        shown
    }

    /// Each run's tally: the kept one while its log is as long as it was, else worked out
    /// again. A deleted run's goes with it.
    fn tallies(&self, kept: &BTreeMap<String, KeptRun>) -> BTreeMap<String, KeptRun> {
        let mut runs = BTreeMap::new();
        for log in self.store.logs() {
            let Ok(log_bytes) = fs::metadata(&log.path).map(|meta| meta.len()) else {
                continue;
            };
            let same = kept
                .get(&log.key)
                .filter(|run| run.rules == TALLY_RULES && run.log_bytes == log_bytes);
            let run = match same {
                Some(run) => run.clone(),
                None => match read_all(&log.path) {
                    Ok(read) => KeptRun {
                        rules: TALLY_RULES,
                        log_bytes,
                        tally: tally(&read.events, self.catalog, self.zone),
                    },
                    Err(err) => {
                        tracing::warn!("couldn't read {} for the wonders: {err}", log.key);
                        continue;
                    }
                },
            };
            runs.insert(log.key, run);
        }
        runs
    }
}

/// Raises what was found and isn't standing yet, and gives every standing wonder its line: the
/// one its facts give today, or the one kept when they're gone. Says whether the kept list
/// changed.
fn stand(kept: &mut Kept, found: Vec<Found>, acts: &[Act<'_>]) -> (Vec<RisenWonder>, bool) {
    let mut changed = false;
    let mut shown = Vec::new();
    for found in found {
        let lasting = found.lasting.clone().unwrap_or_else(|| found.line.clone());
        match kept
            .risen
            .iter_mut()
            .find(|risen| risen.wonder == found.wonder)
        {
            Some(standing) => {
                if standing.why != lasting || standing.models != found.models {
                    standing.why = lasting;
                    standing.models = found.models.clone();
                    changed = true;
                }
                shown.push(RisenWonder {
                    why: found.line,
                    models: found.models,
                    ..standing.clone()
                });
            }
            None => {
                let Some(behavior) = found.about.place(acts) else {
                    continue;
                };
                let risen = RisenWonder {
                    wonder: found.wonder,
                    behavior: behavior.words(),
                    risen: found.at,
                    why: lasting,
                    models: found.models.clone(),
                };
                shown.push(RisenWonder {
                    why: found.line,
                    ..risen.clone()
                });
                kept.risen.push(risen);
                changed = true;
            }
        }
    }
    for standing in &kept.risen {
        if !shown.iter().any(|risen| risen.wonder == standing.wonder) {
            shown.push(standing.clone());
        }
    }
    shown.sort_by_key(|risen| (risen.risen, risen.wonder));
    (shown, changed)
}

#[cfg(test)]
mod tests;
