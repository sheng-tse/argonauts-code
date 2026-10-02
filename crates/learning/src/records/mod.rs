//! Track records: per worker and task type, derived from the outcome log as of a date. A cache,
//! never a source: the same log and date always give the same book.
//!
//! Each rate is a Beta posterior. Observations count by recency, a sparse cell borrows from its
//! group and the worker's overall rate, and a new model id shrinks old evidence toward the prior
//! so the router tries the new model again.

mod evidence;
mod sample;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::limits::{TrackRecord, TrackRecords};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::RoutingSettings;

use crate::error::Result;
use crate::outcomes::{LogLine, Prediction};
use crate::predictions;
use crate::store::Store;

use evidence::{CellFacts, Evidence, Pools};

/// `pooled_from` when a sparse cell borrowed the worker's rate across every task type.
pub const POOLED_ALL_TYPES: &str = "all task types";
/// `pooled_from` when the worker has no evidence yet and starts from the crew's rate.
pub const POOLED_CREW: &str = "crew";

/// Draws behind `Beta::prob_above`. At 0.9 the estimate's standard error is about 0.003.
const CLAIM_DRAWS: u32 = 10_000;
/// Fixed, so the same two posteriors always give the same answer.
const CLAIM_SEED: u64 = 0x7A_C1A1;
/// Cards a pinned rate is worth: enough that draws barely move from it.
const PIN_STRENGTH: f64 = 1e6;

/// A Beta posterior over a rate: weighted passes and failures on top of the prior.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Beta {
    pub alpha: f64,
    pub beta: f64,
}

impl Beta {
    /// A prior centered on `mean`, worth `strength` cards.
    pub fn prior(mean: f64, strength: f64) -> Self {
        let mean = mean.clamp(0.0, 1.0);
        Self {
            alpha: mean * strength,
            beta: (1.0 - mean) * strength,
        }
    }

    pub fn mean(&self) -> f64 {
        let total = self.alpha + self.beta;
        if total > 0.0 { self.alpha / total } else { 0.5 }
    }

    /// One draw, for Thompson sampling.
    pub fn sample(&self, rng: &mut StdRng) -> f64 {
        let alpha = if self.alpha.is_finite() {
            self.alpha.max(0.0)
        } else {
            0.0
        };
        let beta = if self.beta.is_finite() {
            self.beta.max(0.0)
        } else {
            0.0
        };
        match (alpha > 0.0, beta > 0.0) {
            (true, true) => sample::beta(alpha, beta, rng),
            (true, false) => 1.0,
            (false, true) => 0.0,
            (false, false) => 0.5,
        }
    }

    /// The probability that this rate is above `other`'s, for "claim little" in the retro.
    /// Estimated from seeded draws, so the same posteriors always give the same answer.
    pub fn prob_above(&self, other: &Beta) -> f64 {
        let mut rng = StdRng::seed_from_u64(CLAIM_SEED);
        let mut above = 0.0;
        for _ in 0..CLAIM_DRAWS {
            let mine = self.sample(&mut rng);
            let theirs = other.sample(&mut rng);
            if mine > theirs {
                above += 1.0;
            } else if mine == theirs {
                above += 0.5;
            }
        }
        above / f64::from(CLAIM_DRAWS)
    }
}

/// One worker's record on one task type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub worker: WorkerRef,
    pub task_type: String,
    /// The group or overall rate it borrowed from while its own cell was sparse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pooled_from: Option<String>,
    /// The worker's newest model id: the model its full-weight evidence is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// First-pass gate rate.
    pub pass: Beta,
    /// Review approval rate, relative to each reviewer's usual rate.
    pub approval: Beta,
    /// Median recent cost over the task type's median across workers, capped at 2.
    pub cost_ratio: f64,
    /// The same for wall time.
    pub time_ratio: f64,
    /// Brier skill score of its predictions; 0 is no better than its base rate.
    pub prediction: f64,
    /// Recency-weighted cards behind the record.
    pub evidence: f64,
    /// Plain counts, for handoff cards: passed first try, of cards.
    pub counts: TrackRecord,
    /// Fixed by `routing.pins`.
    pub pinned: bool,
}

/// Every record as of one moment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Book {
    records: Vec<Record>,
    settings: RoutingSettings,
    pass: Pools,
    approval: Pools,
    /// Each worker's newest model id.
    models: BTreeMap<WorkerRef, String>,
    /// Each worker's prediction score, for cells it has no outcome in yet.
    prediction: BTreeMap<WorkerRef, f64>,
}

impl Book {
    /// Builds the book from log lines, counting only what happened before `as_of`.
    pub fn build(lines: &[LogLine], settings: &RoutingSettings, as_of: OffsetDateTime) -> Self {
        let evidence = Evidence::gather(lines, settings, as_of);
        let mut book = Self {
            records: Vec::new(),
            settings: settings.clone(),
            pass: evidence.pass_pools(settings),
            approval: evidence.approval_pools(settings),
            models: evidence.models.clone(),
            prediction: prediction_scores(lines, &evidence, settings, as_of),
        };
        let mut cells = evidence.cells();
        for (worker, types) in &settings.pins {
            let Ok(worker) = worker.parse::<WorkerRef>() else {
                tracing::warn!("ignoring routing.pins.{worker}: not a worker name");
                continue;
            };
            for task_type in types.keys() {
                cells
                    .entry((worker.clone(), task_type.clone()))
                    .or_default();
            }
        }
        let mut records: Vec<Record> = cells
            .iter()
            .map(|((worker, task_type), facts)| book.compose(worker, task_type, facts))
            .collect();
        records.sort_by(|a, b| (&a.task_type, &a.worker).cmp(&(&b.task_type, &b.worker)));
        book.records = records;
        book
    }

    /// Reads the whole outcome log and builds the book. `argonauts records rebuild` and the
    /// live service both come here.
    pub fn rebuild(
        store: &Store,
        settings: &RoutingSettings,
        as_of: OffsetDateTime,
    ) -> Result<Self> {
        let lines = crate::outcomes::read_all(store)?;
        Ok(Self::build(&lines, settings, as_of))
    }

    /// What the router reads for a worker and task type: its own cell, pooled while sparse, or
    /// the prior when there's no evidence at all.
    pub fn record(&self, worker: &WorkerRef, task_type: &str) -> Record {
        if let Some(record) = self
            .records
            .iter()
            .find(|record| &record.worker == worker && record.task_type == task_type)
        {
            return record.clone();
        }
        self.compose(worker, task_type, &CellFacts::default())
    }

    /// Every record, by task type and then worker, for `/records`.
    pub fn rows(&self) -> &[Record] {
        &self.records
    }

    /// Whether `a` is better than `b` at `task_type` with the confidence `routing.claim_above`
    /// asks for. `None` means the difference isn't clear yet.
    pub fn clearly_better(
        &self,
        a: &WorkerRef,
        b: &WorkerRef,
        task_type: &str,
        settings: &RoutingSettings,
    ) -> Option<bool> {
        let p = self
            .record(a, task_type)
            .pass
            .prob_above(&self.record(b, task_type).pass);
        if p > settings.claim_above {
            Some(true)
        } else if 1.0 - p > settings.claim_above {
            Some(false)
        } else {
            None
        }
    }

    fn compose(&self, worker: &WorkerRef, task_type: &str, facts: &CellFacts) -> Record {
        let settings = &self.settings;
        let configured =
            evidence::setting_for(&settings.priors, worker).and_then(|types| types.get(task_type));
        // Both rates pool on the first-pass evidence, so a record is pooled or not as a whole.
        let sparse =
            self.pass.cell(worker, task_type).weight() < f64::from(settings.own_type_after);
        let (pass, pooled_from, evidence) = self.pass.posterior(
            worker,
            task_type,
            configured.and_then(|rates| rates.first_pass_rate),
            sparse,
            settings,
        );
        let (approval, _, _) = self.approval.posterior(
            worker,
            task_type,
            configured.and_then(|rates| rates.approval_rate),
            sparse,
            settings,
        );
        let pin = evidence::setting_for(&settings.pins, worker)
            .and_then(|types| types.get(task_type))
            .copied();
        let (pass, pooled_from) = match pin {
            Some(rate) => (Beta::prior(rate, PIN_STRENGTH), None),
            None => (pass, pooled_from),
        };
        Record {
            worker: worker.clone(),
            task_type: task_type.to_owned(),
            pooled_from,
            model_id: self.models.get(worker).cloned(),
            pass,
            approval,
            cost_ratio: facts.cost_ratio,
            time_ratio: facts.time_ratio,
            prediction: self.prediction.get(worker).copied().unwrap_or(0.0),
            evidence,
            counts: facts.counts,
            pinned: pin.is_some(),
        }
    }
}

/// Each worker's Brier skill over the metrics an owner predicts, from the predictions and
/// outcomes the book counts. Clamped so one wild score can't swamp the utility's other terms,
/// and shrunk toward 0 while there are few predictions, the way a prior fades.
fn prediction_scores(
    lines: &[LogLine],
    evidence: &Evidence<'_>,
    settings: &RoutingSettings,
    as_of: OffsetDateTime,
) -> BTreeMap<WorkerRef, f64> {
    let mut scored: Vec<LogLine> = evidence
        .outcomes
        .iter()
        .map(|counted| LogLine::Outcome(counted.outcome.clone()))
        .collect();
    let counts = |prediction: &Prediction| {
        prediction.at <= as_of && !evidence.reset_drops(&prediction.worker, prediction.at)
    };
    scored.extend(
        lines
            .iter()
            .filter(|line| matches!(line, LogLine::Prediction(prediction) if counts(prediction)))
            .cloned(),
    );
    let mut sums: BTreeMap<WorkerRef, (f64, u32)> = BTreeMap::new();
    for calibration in predictions::calibration(&scored, settings) {
        if !predictions::OWNER_METRICS.contains(&calibration.metric.as_str()) {
            continue;
        }
        let sum = sums.entry(calibration.worker).or_default();
        sum.0 += calibration.skill * f64::from(calibration.count);
        sum.1 += calibration.count;
    }
    sums.into_iter()
        .filter(|(_, (_, count))| *count > 0)
        .map(|(worker, (total, count))| {
            let count = f64::from(count);
            let skill = (total / count).clamp(-1.0, 1.0);
            let trust = count / (count + settings.prior_strength.max(0.0));
            (worker, skill * trust)
        })
        .collect()
}

/// The `TrackRecords` service the engine and handoff cards read: the book, rebuilt from the log
/// when a run's outcomes change it.
pub struct LiveRecords {
    store: Store,
    settings: Arc<Settings>,
    book: std::sync::Mutex<Option<Arc<Book>>>,
}

impl LiveRecords {
    pub fn new(state_dir: PathBuf, repo_root: PathBuf, settings: Arc<Settings>) -> Self {
        Self {
            store: Store::new(state_dir, repo_root),
            settings,
            book: std::sync::Mutex::new(None),
        }
    }

    /// The current book, built on first use.
    pub fn book(&self) -> Arc<Book> {
        let mut book = self
            .book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        book.get_or_insert_with(|| {
            let built = Book::rebuild(
                &self.store,
                &self.settings.routing,
                OffsetDateTime::now_utc(),
            )
            .unwrap_or_else(|err| {
                tracing::warn!("track records unavailable: {err}");
                Book::default()
            });
            Arc::new(built)
        })
        .clone()
    }

    /// Drops the cached book, so the next read sees a run that just ended.
    pub fn refresh(&self) {
        *self
            .book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

impl TrackRecords for LiveRecords {
    /// Plain counts on similar cards; `None` until the worker has finished one.
    fn get(&self, worker: &WorkerRef, task_type: &str) -> Option<TrackRecord> {
        let counts = self.book().record(worker, task_type).counts;
        (counts.of > 0).then_some(counts)
    }
}
