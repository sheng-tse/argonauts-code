//! From the retro's proposal to the promotion checkpoint: stage 1's recorded recipes, the
//! mutation rules, the replay queue under the monthly budget, the comparison, and the answer.

use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;
use time::format_description::well_known::Rfc3339;

use argonauts_core::events::run::{Party, RunOutcome};
use argonauts_core::settings::Settings;
use argonauts_core::steps::card::ProcessChange;

use super::{
    Action, Candidate, CandidateState, Evolution, Stage, Thresholds, candidate, check_mutation,
    compare, holdout_ok, recipe_branch, state, stats,
};
use crate::error::{LearningError, Result};
use crate::git;
use crate::recipes::{self, Change, LoadedRecipe, Match, Recipe};
use crate::types::{RecipeRef, ReplayResult, RunRecord};

impl Evolution {
    /// Stage 1: a run with no recipe that merged and passed its final checks first time
    /// proposes its workflow as v1 of a recipe for its task type.
    pub(super) fn record(&self, settings: &Settings, run: &RunRecord) -> Result<Vec<Action>> {
        if run.outcome != RunOutcome::Merged || !run.final_checks_passed {
            return Ok(Vec::new());
        }
        let Some((workflow, sha256)) = &run.workflow else {
            return Ok(Vec::new());
        };
        let sets = recipes::load_all(&self.store)?;
        let name = recipe_name(&run.task_type);
        let covered = sets.iter().any(|set| {
            set.name == name
                || set
                    .current()
                    .is_some_and(|loaded| loaded.recipe.matches.task_types.contains(&run.task_type))
        });
        if covered {
            return Ok(Vec::new());
        }
        let branch = recipe_branch(&name, 1);
        let repo = &self.store.repo_root;
        let exists = git::run(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        );
        if exists.is_ok() {
            return Ok(Vec::new());
        }
        let recipe = Recipe {
            recipe: name.clone(),
            version: 1,
            parent: None,
            description: format!("What worked for {} tasks", run.task_type),
            workflow: Some(settings.workflow.clone()),
            mode: None,
            phases: Some(workflow.clone()),
            matches: Match {
                task_types: vec![run.task_type.clone()],
                labels_any: Vec::new(),
            },
            prompts: Default::default(),
            settings: Default::default(),
        };
        let problems = recipes::validate(&recipe);
        if !problems.is_empty() {
            return Ok(vec![Action::Tell {
                text: format!(
                    "Run {} worked, but it can't become a learned workflow: {}",
                    run.run,
                    problems.join("; ")
                ),
            }]);
        }
        let entries = vec![(
            recipes::repo_path(&name, &recipes::version_file(1)),
            git::Entry::File(recipes::render(&recipe)?),
        )];
        let message = format!(
            "Record learned workflow {name} v1\n\nRun {} merged with workflow {workflow} \
             ({sha256}) and passed its final checks first time.",
            run.run
        );
        git::commit_on_branch(repo, "HEAD", &branch, &entries, &message)?;
        Ok(vec![Action::Recorded {
            recipe: RecipeRef { name, version: 1 },
            branch,
        }])
    }

    /// Turns the retro's one proposed change into a candidate, if the stage and the rules let it.
    pub(super) fn propose(
        &self,
        settings: &Settings,
        run: &RunRecord,
        proposal: &ProcessChange,
        stage: Stage,
    ) -> Result<Vec<Action>> {
        let tell = |text: String| Ok(vec![Action::Tell { text }]);
        let Some(ran) = &run.recipe else {
            return tell(format!(
                "The run review proposed changing `{}`, but no learned workflow ran, so there's nothing to change",
                proposal.setting
            ));
        };
        // Fixed-core and safety keys never even become candidates.
        if let Some(reason) = recipes::guarded(&proposal.setting) {
            return tell(format!("Refused the run review's proposal: {reason}"));
        }

        // The engine asks the retro for a proposal only every `propose_every` runs, so one that
        // arrives is due.
        if self.canaries()?.contains_key(&ran.name) {
            return tell(format!(
                "The run review's proposal for {} waits: a canary is running",
                ran.name
            ));
        }
        let Some(parent) = self.load_version(&ran.name, ran.version)? else {
            return tell(format!(
                "The run review proposed a change to {ran}, which isn't there"
            ));
        };

        let version = self.next_version(&ran.name)?;
        let id = format!("{}-v{version}", ran.name);
        let proposed = Change {
            key: proposal.setting.clone(),
            class: recipes::class_for_key(&proposal.setting),
            before: None,
            after: recipes::toml_to_json(&proposal.value).ok(),
        };
        let checked = write_child(&parent, version, proposal, &self.candidate_dir(&id))
            .and_then(|child| check_mutation(&parent, &child, settings));
        let (change, state) = match checked {
            Ok(change) if stage == Stage::Suggest => (change, CandidateState::Suggested),
            Ok(change) => (change, CandidateState::Queued),
            Err(reason) => (proposed, CandidateState::Rejected { reason }),
        };
        let thresholds = Thresholds {
            min_gain_points: settings.evolution.min_gain_points,
            max_cost_increase: settings.evolution.max_cost_increase,
            not_worse: settings
                .evolution
                .not_worse_classes
                .contains(&recipes::class_name(change.class)),
        };
        let text = match &state {
            CandidateState::Rejected { reason } => format!("Rejected {id}: {reason}"),
            CandidateState::Suggested => format!(
                "Suggested for {ran}: set `{}` to {}. {} Apply it yourself if you agree.",
                proposal.setting, proposal.value, proposal.why
            ),
            _ => format!("Queued {id} for a replay: {}", proposal.why),
        };
        let mut candidates = self.candidates()?;
        candidates.push(Candidate {
            id,
            parent: ran.clone(),
            version,
            change,
            why: proposal.why.clone(),
            from_run: Some(run.run.clone()),
            thresholds,
            state,
            queued: self.now(),
        });
        self.save_candidates(&candidates)?;
        tell(text)
    }

    /// Hands out the oldest queued candidate when nothing is replaying and this month's
    /// evolution budget has room. Candidates replay one at a time.
    pub fn next_replay(&self, settings: &Settings) -> Result<Vec<Action>> {
        let mut candidates = self.candidates()?;
        if candidates
            .iter()
            .any(|c| matches!(c.state, CandidateState::Replaying { .. }))
        {
            return Ok(Vec::new());
        }
        let limit = settings.limits.evolution_usd_per_month;
        if limit <= 0.0 || self.spent_this_month(self.now())? >= limit {
            return Ok(Vec::new());
        }
        let Some(next) = candidates
            .iter_mut()
            .filter(|c| c.state == CandidateState::Queued)
            .min_by_key(|c| c.queued)
        else {
            return Ok(Vec::new());
        };
        next.state = CandidateState::Replaying { spent_usd: 0.0 };
        let id = next.id.clone();
        self.save_candidates(&candidates)?;
        Ok(vec![Action::Replay { candidate: id }])
    }

    /// After a candidate's replay: compare, check the holdout, and either reject it or ask for
    /// promotion.
    pub fn after_replay(
        &self,
        settings: &Settings,
        candidate: &str,
        results: &[ReplayResult],
        spent_usd: f64,
    ) -> Result<Vec<Action>> {
        let mut candidates = self.candidates()?;
        let Some(found) = candidates.iter_mut().find(|c| c.id == candidate) else {
            return Err(LearningError::Refused(format!(
                "there's no candidate `{candidate}`"
            )));
        };
        if !matches!(
            found.state,
            CandidateState::Replaying { .. } | CandidateState::Queued
        ) {
            return Err(LearningError::Refused(format!(
                "{candidate} isn't waiting on a replay"
            )));
        }
        self.record_spend(candidate, spent_usd)?;

        let child = RecipeRef {
            name: found.parent.name.clone(),
            version: found.version,
        };
        // Seeded by the candidate, so the same replay always gets the same verdict.
        let mut rng = StdRng::seed_from_u64(seed_of(&found.id));
        let verdict = compare(results, &found.parent, &child, &found.thresholds, &mut rng);
        let (holdout_parent, holdout_child) = stats::holdout_passes(results, &found.parent, &child);
        let mut actions = Vec::new();
        if !verdict.passed {
            let reason = verdict.reasons.join("; ");
            actions.push(Action::Tell {
                text: format!("Rejected {candidate}: {reason}"),
            });
            found.state = CandidateState::Rejected { reason };
        } else if stats::holdout_tasks(results, &found.parent, &child) == 0 {
            let reason = "there's no holdout result, so nothing shows it doesn't overfit the \
                          tuning set"
                .to_owned();
            actions.push(Action::Tell {
                text: format!("Rejected {candidate}: {reason}"),
            });
            found.state = CandidateState::Rejected { reason };
        } else if !holdout_ok(results, &found.parent, &child) {
            let reason = format!(
                "it regressed on the holdout ({holdout_child} passes against {holdout_parent}), \
                 so it likely overfits the tuning set"
            );
            actions.push(Action::Tell {
                text: format!("Rejected {candidate}: {reason}"),
            });
            found.state = CandidateState::Rejected { reason };
        } else {
            found.state = CandidateState::AwaitingPromotion;
            let last_promoted = self.last_promoted(&found.parent.name)?;
            let packet = json!({
                "candidate": found.id,
                "recipe": found.parent.name,
                "parent": found.parent.version,
                "version": found.version,
                "change": found.change,
                "change_class": recipes::class_name(found.change.class),
                "last_promoted": last_promoted.and_then(|at| at.format(&Rfc3339).ok()),
                "why": found.why,
                "thresholds": found.thresholds,
                "verdict": verdict,
                "holdout": { "parent_passes": holdout_parent, "child_passes": holdout_child },
                "spent_usd": spent_usd,
            });
            actions.push(Action::AskPromotion {
                candidate: candidate.to_owned(),
                packet,
            });
        }
        self.save_candidates(&candidates)?;
        actions.extend(self.next_replay(settings)?);
        Ok(actions)
    }

    /// A replay the month's budget stopped: what it spent so far counts against this month,
    /// and the candidate keeps replaying when there's budget again.
    pub fn replay_paused(&self, candidate: &str, spent_usd: f64) -> Result<Vec<Action>> {
        let Some(found) = self.candidate(candidate)? else {
            return Err(LearningError::Refused(format!(
                "there's no candidate `{candidate}`"
            )));
        };
        if !matches!(found.state, CandidateState::Replaying { .. }) {
            return Err(LearningError::Refused(format!(
                "{candidate} isn't replaying"
            )));
        }
        self.record_spend(candidate, spent_usd)?;
        let mut candidates = self.candidates()?;
        if let Some(found) = candidates.iter_mut().find(|c| c.id == candidate) {
            found.state = CandidateState::Replaying { spent_usd };
        }
        self.save_candidates(&candidates)?;
        Ok(vec![Action::Tell {
            text: format!(
                "{candidate}'s replay stopped at the month's evolution limit, ${spent_usd:.2} in; \
                 it carries on next month"
            ),
        }])
    }

    /// Books what a candidate's replay has spent in all, less what earlier months booked.
    fn record_spend(&self, candidate: &str, total_usd: f64) -> Result<()> {
        let path = self.file(state::SPEND);
        let mut ledger: state::Ledger = state::read(&path)?;
        let booked: f64 = ledger
            .iter()
            .filter(|spend| spend.candidate == candidate)
            .map(|spend| spend.usd)
            .sum();
        let usd = total_usd - booked;
        if usd <= 0.0 {
            return Ok(());
        }
        ledger.push(state::Spend {
            at: self.now(),
            candidate: candidate.to_owned(),
            usd,
        });
        state::write(&path, &ledger)
    }

    /// The promotion checkpoint's answer. Promoting starts the canary.
    pub fn decide(&self, candidate: &str, promote: bool, by: &Party) -> Result<Vec<Action>> {
        let mut candidates = self.candidates()?;
        let Some(found) = candidates.iter_mut().find(|c| c.id == candidate) else {
            return Err(LearningError::Refused(format!(
                "there's no candidate `{candidate}`"
            )));
        };
        if found.state != CandidateState::AwaitingPromotion {
            return Err(LearningError::Refused(format!(
                "{candidate} isn't waiting for promotion"
            )));
        }
        if !promote {
            let reason = format!("not promoted by {}", who(by));
            found.state = CandidateState::Rejected {
                reason: reason.clone(),
            };
            self.save_candidates(&candidates)?;
            return Ok(vec![Action::Tell {
                text: format!("{candidate} stays out: {reason}"),
            }]);
        }
        let name = found.parent.name.clone();
        let mut canaries = self.canaries()?;
        if canaries.contains_key(&name) {
            return Err(LearningError::Refused(format!(
                "{name} already has a canary running"
            )));
        }
        let now = self.now();
        canaries.insert(
            name.clone(),
            state::Canary {
                recipe: name.clone(),
                candidate: candidate.to_owned(),
                version: found.version,
                parent: found.parent.version,
                started: now,
                by: by.clone(),
                runs: Vec::new(),
            },
        );
        found.state = CandidateState::Canary { runs: 0 };
        let version = found.version;
        let mut states = self.recipe_states()?;
        states.entry(name.clone()).or_default().last_promoted = Some(now);
        self.save_canaries(&canaries)?;
        self.save_candidates(&candidates)?;
        self.save_recipe_states(&states)?;
        Ok(vec![Action::Tell {
            text: format!(
                "{name} v{version} runs as a canary; its runs merge through pull requests and it \
                 rolls back at the first tripwire"
            ),
        }])
    }
}

fn write_child(
    parent: &LoadedRecipe,
    version: u32,
    proposal: &ProcessChange,
    folder: &std::path::Path,
) -> std::result::Result<LoadedRecipe, String> {
    candidate::materialize(parent, version, proposal, folder).map_err(|error| error.to_string())
}

/// A recipe name from a task type: lowercase letters, digits, and dashes.
fn recipe_name(task_type: &str) -> String {
    let mut name = String::new();
    for c in task_type.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c.to_ascii_lowercase());
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() {
        "task".to_owned()
    } else {
        name
    }
}

fn seed_of(id: &str) -> u64 {
    let hash = argonauts_core::sha256_hex(id.as_bytes());
    u64::from_str_radix(&hash[..16], 16).unwrap_or(0)
}

fn who(by: &Party) -> String {
    match by {
        Party::User | Party::Declined => "you".to_owned(),
        Party::Rules => "your rules".to_owned(),
        Party::Approver { model } => format!("the approver {model}"),
        Party::Config => "a setting".to_owned(),
        Party::Autopilot => "autopilot".to_owned(),
        Party::Conductor => "the conductor".to_owned(),
        Party::Agent { worker, .. } => worker.to_string(),
        Party::FrontEnd { client } => client.clone(),
    }
}
