//! Replaying a candidate against its parent, and scoring what each run left.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use argonauts_core::AuthMode;
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::CheckRunner;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::sha256_hex;
use argonauts_core::workflow::StandardPhase;

use super::checks::{ScoringSetup, hidden_root, run_tests};
use super::files::{Scratch, nonce, read_json, remove_if_present, write_json};
use super::isolated::isolated_repo;
use super::set::scoring_setup_path;
use super::stopping::{StoppingRule, Verdict};
use super::{Entry, ReplayReport, ReplayRequest, ReplayRunner, ReplaySet};
use crate::error::{LearningError, Result};
use crate::evolution::Thresholds;
use crate::learned_workflows::LoadedLearnedWorkflow;
use crate::types::{LearnedWorkflowRef, ReplayResult};

/// How far a not-worse class may fall behind its parent, as a fraction.
const NOT_WORSE_MARGIN: f64 = -0.05;

/// A replay in progress. Everything that decides it is fixed here before the first run, and
/// it outlives a month whose limit is spent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Progress {
    parent: LearnedWorkflowRef,
    child: LearnedWorkflowRef,
    rule: StoppingRule,
    from_phase: Option<StandardPhase>,
    workers: Vec<ModelRef>,
    tuning: Vec<String>,
    /// Fixed when the holdout starts.
    holdout: Option<Vec<String>>,
    results: Vec<ReplayResult>,
    spent_usd: f64,
}

impl Progress {
    fn has(
        &self,
        task: &str,
        learned_workflow: &LearnedWorkflowRef,
        try_number: u32,
        holdout: bool,
    ) -> bool {
        self.results.iter().any(|result| {
            result.task == task
                && &result.learned_workflow == learned_workflow
                && result.try_number == try_number
                && result.holdout == holdout
        })
    }

    fn report(&self, stopped_early: bool, at_limit: bool) -> ReplayReport {
        ReplayReport {
            results: self.results.clone(),
            spent_usd: self.spent_usd,
            stopped_early,
            at_limit,
        }
    }
}

/// Replays a candidate against its parent: paired runs on the tuning set, then the holdout,
/// at equal limits, stopping early when the result is clear, within `limit_left_usd`. The
/// stopping rule's margin comes from the candidate's `thresholds`, frozen when it was queued.
///
/// Each pair runs the parent, then the child, on the same task with the same limit. After the
/// first pair of every task, tasks both versions passed or both failed are dropped. The holdout
/// runs one pair per task only once the tuning set passes, and then counts the candidate toward
/// its retirement. Every run's spend goes in the month's ledger as it happens. When the limit
/// runs out the replay stops with `at_limit`, and calling it again for the same two
/// versions carries on where it stopped, under the rule it started with.
pub fn replay<'a>(
    set: &'a ReplaySet,
    versions: Versions<'a>,
    settings: &'a Settings,
    limit_left_usd: f64,
    runner: Arc<dyn ReplayRunner>,
    checks: Arc<dyn CheckRunner>,
) -> BoxFuture<'a, Result<ReplayReport>> {
    Box::pin(async move {
        let replayer = Replayer {
            set,
            parent: versions.parent,
            child: versions.child,
            thresholds: versions.thresholds,
            settings,
            limit_left_usd,
            spent_now: 0.0,
            runner,
            checks,
        };
        replayer.run().await
    })
}

/// What a replay compares: a candidate version against its parent, under the bar fixed when
/// the candidate was queued.
#[derive(Debug, Clone, Copy)]
pub struct Versions<'a> {
    pub parent: &'a LoadedLearnedWorkflow,
    pub child: &'a LoadedLearnedWorkflow,
    pub thresholds: &'a Thresholds,
}

struct Replayer<'a> {
    set: &'a ReplaySet,
    parent: &'a LoadedLearnedWorkflow,
    child: &'a LoadedLearnedWorkflow,
    thresholds: &'a Thresholds,
    settings: &'a Settings,
    limit_left_usd: f64,
    /// Spent in this call, which `limit_left_usd` bounds.
    spent_now: f64,
    runner: Arc<dyn ReplayRunner>,
    checks: Arc<dyn CheckRunner>,
}

/// Whether a run went ahead or the limit stopped it.
enum Step {
    Ran,
    AtLimit,
}

impl Replayer<'_> {
    async fn run(mut self) -> Result<ReplayReport> {
        let parent = self.parent.reference();
        let child = self.child.reference();
        if parent == child {
            return Err(LearningError::Refused(format!(
                "{child} can't be replayed against itself"
            )));
        }
        let allowed = replay_workers(self.settings);
        if allowed.is_empty() {
            return Err(LearningError::Refused(
                "no worker can run replays: they need an agent on an API key or an endpoint \
                 with allow_api_billing, or evolution.replay.use_subscriptions = true"
                    .to_owned(),
            ));
        }
        let key = progress_key(self.parent, self.child);
        let path = self.set.progress_path(&key);
        let mut progress = match read_json::<Progress>(&path)? {
            Some(progress) => progress,
            None => self.start(allowed.clone())?,
        };
        if let Some(gone) = progress
            .workers
            .iter()
            .find(|worker| !allowed.contains(worker))
        {
            return Err(LearningError::Refused(format!(
                "{gone} started this replay but may no longer run replays, so it can't go on \
                 at the same terms"
            )));
        }
        write_json(&path, &progress)?;

        let tuning = self.load(&progress.tuning)?;
        let tasks: Vec<String> = tuning.iter().map(|entry| entry.id.clone()).collect();
        let rule = progress.rule;
        let runs = rule.runs_per_task.max(1);
        let mut stopped_early = false;
        'tuning: for try_number in 1..=runs {
            for entry in &tuning {
                if try_number > 1
                    && StoppingRule::dropped(&entry.id, &progress.results, &parent, &child)
                {
                    continue;
                }
                if let Step::AtLimit = self
                    .pair(&mut progress, &path, entry, try_number, false)
                    .await?
                {
                    return Ok(progress.report(false, true));
                }
                let left = planned_left(&progress, &tuning, runs, &parent, &child);
                if rule.sequential
                    && left > 0
                    && rule
                        .clear(&tasks, &progress.results, &parent, &child)
                        .is_some()
                {
                    stopped_early = true;
                    break 'tuning;
                }
            }
        }

        let verdict = rule.verdict(&tasks, &progress.results, &parent, &child);
        if verdict == Verdict::Passes {
            let ids = match &progress.holdout {
                Some(ids) => ids.clone(),
                None => {
                    let ids: Vec<String> = self
                        .set
                        .holdout()?
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect();
                    progress.holdout = Some(ids.clone());
                    write_json(&path, &progress)?;
                    ids
                }
            };
            let holdout = self.load(&ids)?;
            for entry in &holdout {
                if let Step::AtLimit = self.pair(&mut progress, &path, entry, 1, true).await? {
                    return Ok(progress.report(stopped_early, true));
                }
            }
            if !holdout.is_empty() {
                self.set.holdout_used(self.settings)?;
            }
        }
        remove_if_present(&path)?;
        Ok(progress.report(stopped_early, false))
    }

    /// Fixes the rule, the phase, the workers, and the tasks before the first run.
    fn start(&self, workers: Vec<ModelRef>) -> Result<Progress> {
        let replay = &self.settings.evolution.replay;
        let tuning: Vec<String> = self
            .set
            .tuning()?
            .into_iter()
            .take(replay.tasks as usize)
            .map(|entry| entry.id)
            .collect();
        if tuning.is_empty() {
            return Err(LearningError::Refused(
                "the replay set has no tuning tasks yet".to_owned(),
            ));
        }
        Ok(Progress {
            parent: self.parent.reference(),
            child: self.child.reference(),
            rule: StoppingRule {
                margin: if self.thresholds.not_worse {
                    NOT_WORSE_MARGIN
                } else {
                    self.thresholds.min_gain_points / 100.0
                },
                runs_per_task: replay.runs_per_task,
                sequential: replay.sequential,
            },
            from_phase: if replay.prefix_replay {
                start_phase(self.parent, self.child)
            } else {
                None
            },
            workers,
            tuning,
            holdout: None,
            results: Vec::new(),
            spent_usd: 0.0,
        })
    }

    /// Entries by id, leaving out any that left the set since the replay started.
    fn load(&self, ids: &[String]) -> Result<Vec<Entry>> {
        let mut entries = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(entry) = self.set.entry(id)? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// The parent's run and the child's on one task. The limit for both is checked before the
    /// first, so a pair is never split by the month running out.
    async fn pair(
        &mut self,
        progress: &mut Progress,
        path: &Path,
        entry: &Entry,
        try_number: u32,
        holdout: bool,
    ) -> Result<Step> {
        let versions = [self.parent, self.child];
        let pending: Vec<&LoadedLearnedWorkflow> = versions
            .into_iter()
            .filter(|learned_workflow| {
                !progress.has(
                    &entry.id,
                    &learned_workflow.reference(),
                    try_number,
                    holdout,
                )
            })
            .collect();
        let needed = entry.limit_usd * pending.len() as f64;
        if self.spent_now + needed > self.limit_left_usd {
            return Ok(Step::AtLimit);
        }
        for learned_workflow in pending {
            if self.spent_now + entry.limit_usd > self.limit_left_usd {
                return Ok(Step::AtLimit);
            }
            let result = self
                .run_one(progress, entry, learned_workflow, try_number, holdout)
                .await?;
            progress.spent_usd += result.cost_usd;
            progress.results.push(result);
            write_json(path, progress)?;
        }
        Ok(Step::Ran)
    }

    async fn run_one(
        &mut self,
        progress: &Progress,
        entry: &Entry,
        learned_workflow: &LoadedLearnedWorkflow,
        try_number: u32,
        holdout: bool,
    ) -> Result<ReplayResult> {
        let into = self.set.repos_dir().join(format!(
            "{}-v{}-{try_number}-{}",
            entry.id,
            learned_workflow.learned_workflow.version,
            nonce()
        ));
        let repo = Scratch(isolated_repo(
            &self.set.store().repo_root,
            &entry.base,
            &into,
        )?);
        let request = ReplayRequest {
            entry: Entry {
                tests: Vec::new(),
                holdout: false,
                ..entry.clone()
            },
            learned_workflow: learned_workflow.clone(),
            repo: repo.0.clone(),
            limit_usd: entry.limit_usd,
            from_phase: progress.from_phase,
            workers: progress.workers.clone(),
            hidden_paths: self.set.hidden_paths(entry),
        };
        let run = self.runner.run(&request).await?;
        self.spent_now += run.cost_usd;
        self.set
            .record_spend(&entry.id, &learned_workflow.reference(), run.cost_usd)?;
        let passed = match &run.head {
            Some(head) => score(entry, &repo.0, head, self.checks.clone()).await?,
            None => false,
        };
        Ok(ReplayResult {
            task: entry.id.clone(),
            learned_workflow: learned_workflow.reference(),
            try_number,
            passed,
            cost_usd: run.cost_usd,
            minutes: run.minutes,
            holdout,
        })
    }
}

/// Tuning pairs still planned: every try of a live task, the first of a dropped one.
fn planned_left(
    progress: &Progress,
    tuning: &[Entry],
    runs: u32,
    parent: &LearnedWorkflowRef,
    child: &LearnedWorkflowRef,
) -> u32 {
    tuning
        .iter()
        .map(|entry| {
            let planned = if StoppingRule::dropped(&entry.id, &progress.results, parent, child) {
                1
            } else {
                runs
            };
            (1..=planned)
                .filter(|&try_number| {
                    !(progress.has(&entry.id, parent, try_number, false)
                        && progress.has(&entry.id, child, try_number, false))
                })
                .count() as u32
        })
        .sum()
}

fn progress_key(parent: &LoadedLearnedWorkflow, child: &LoadedLearnedWorkflow) -> String {
    let both = format!(
        "{}\n{}\n{}\n{}",
        parent.reference(),
        parent.sha256,
        child.reference(),
        child.sha256
    );
    sha256_hex(both.as_bytes())[..32].to_owned()
}

/// Applies the entry's hidden tests to `head` in the isolated repo and runs them through the
/// check runner, sandboxed like any check.
///
/// `head` is checked out fresh beside the hidden tests in the replay folder, the tests are
/// copied in, and the checkout is deleted once they've run. The tests never touch `repo`, its
/// worktrees, or any commit.
pub fn score<'a>(
    entry: &'a Entry,
    repo: &'a Path,
    head: &'a str,
    checks: Arc<dyn CheckRunner>,
) -> BoxFuture<'a, Result<bool>> {
    Box::pin(async move {
        let root = hidden_root(entry)?;
        let replay_dir = root
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| LearningError::Refused(format!("{} has no replay folder", entry.id)))?
            .to_path_buf();
        let scoring: ScoringSetup =
            read_json(&scoring_setup_path(&replay_dir, &entry.id))?.unwrap_or_default();
        let checkout = Scratch(isolated_repo(
            repo,
            head,
            &replay_dir
                .join("work")
                .join(format!("score-{}-{}", entry.id, nonce())),
        )?);
        let tests = run_tests(&checks, entry, &checkout.0, &scoring, &replay_dir).await?;
        Ok(tests.ran && tests.passed)
    })
}

/// The workers replays may use, from your crew and main agent: agents on an API key, and
/// endpoints you allowed with `allow_api_billing` or that run locally, unless
/// `evolution.replay.use_subscriptions` is on. A worker without a model gets the one its
/// agent settings pin.
pub fn replay_workers(settings: &Settings) -> Vec<ModelRef> {
    let use_subscriptions = settings.evolution.replay.use_subscriptions;
    let mut workers: Vec<ModelRef> = Vec::new();
    let members = settings.crew.members();
    let candidates = members.iter().chain(std::iter::once(&settings.crew.main));
    for worker in candidates {
        let pinned = if let Some(endpoint) = settings.endpoints.get(&worker.agent) {
            // A local endpoint spends neither money nor anyone's plan usage.
            (use_subscriptions || endpoint.allow_api_billing || endpoint.local)
                .then(|| worker.clone())
        } else {
            let agent = settings
                .agents
                .get(&worker.agent)
                .cloned()
                .unwrap_or_default();
            (agent.enabled && (use_subscriptions || agent.auth == AuthMode::ApiKey)).then(|| {
                ModelRef::new(
                    worker.agent.clone(),
                    worker.model.clone().or(agent.model.clone()),
                )
            })
        };
        if let Some(pinned) = pinned
            && !workers.contains(&pinned)
        {
            workers.push(pinned);
        }
    }
    workers
}

/// The phase a replay of `child` starts from, reusing the parent run's recorded phases before
/// it. Critique, checklist, and drafting changes start at planning; task splits, limits, and
/// the build prompt at doing. Anything else, or a mix of phases, replays the whole run.
pub fn start_phase(
    parent: &LoadedLearnedWorkflow,
    child: &LoadedLearnedWorkflow,
) -> Option<StandardPhase> {
    let keys = changed_keys(parent, child);
    let mut phases = keys.iter().map(|key| phase_of(key));
    let first = phases.next()??;
    phases.try_fold(first, |earliest, phase| {
        phase.map(|phase| earliest.min(phase))
    })
}

fn phase_of(key: &str) -> Option<StandardPhase> {
    let under = |prefix: &str| key.starts_with(prefix);
    if under("critique.") || under("drafting.") || key == "prompts.drafting" {
        Some(StandardPhase::Plan)
    } else if under("divide.") || under("limits.") || key == "prompts.build" {
        Some(StandardPhase::Do)
    } else {
        None
    }
}

/// Dotted settings keys, `prompts.<name>`, and the learned workflow's own fields that are
/// different.
fn changed_keys(parent: &LoadedLearnedWorkflow, child: &LoadedLearnedWorkflow) -> Vec<String> {
    let (a, b) = (&parent.learned_workflow, &child.learned_workflow);
    let mut keys = Vec::new();
    let settings = |learned_workflow: &crate::learned_workflows::LearnedWorkflow| {
        let mut flat = BTreeMap::new();
        for (key, value) in &learned_workflow.settings {
            flatten(key, value, &mut flat);
        }
        for (name, file) in &learned_workflow.prompts {
            flat.insert(
                format!("prompts.{name}"),
                serde_json::Value::String(file.clone()),
            );
        }
        flat
    };
    let (before, after) = (settings(a), settings(b));
    for key in before.keys().chain(after.keys()) {
        if before.get(key) != after.get(key) && !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    for (field, changed) in [
        ("phases", a.phases != b.phases),
        ("workflow", a.workflow != b.workflow),
        ("mode", a.mode != b.mode),
        ("match", a.matches != b.matches),
    ] {
        if changed {
            keys.push(field.to_owned());
        }
    }
    keys
}

fn flatten(prefix: &str, value: &serde_json::Value, out: &mut BTreeMap<String, serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                flatten(&format!("{prefix}.{key}"), value, out);
            }
        }
        _ => {
            out.insert(prefix.to_owned(), value.clone());
        }
    }
}
