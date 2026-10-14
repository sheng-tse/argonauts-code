//! The canary that watches a promoted version, the promotion that follows a clean one, and
//! rollbacks, which take effect at once.

use std::path::PathBuf;

use argonauts_core::settings::Settings;

use super::{
    Action, CandidateState, Evolution, Tripwire, learned_workflow_branch, state, tripwires,
};
use crate::error::{LearningError, Result};
use crate::learned_workflows::{self, LoadedLearnedWorkflow};
use crate::outcomes::{self, LogLine};
use crate::types::{LearnedWorkflowRef, RunRecord};
use crate::{git, skills};

impl Evolution {
    /// Counts a canary run, rolls back on a tripwire, and promotes after `canary_runs`.
    pub(super) fn watch_canary(
        &self,
        settings: &Settings,
        run: &RunRecord,
        learned_workflow: &LearnedWorkflowRef,
    ) -> Result<Vec<Action>> {
        let mut canaries = self.canaries()?;
        let Some(canary) = canaries.get_mut(&learned_workflow.name) else {
            return Ok(Vec::new());
        };
        if canary.version != learned_workflow.version
            || canary.runs.iter().any(|r| r.run == run.run)
        {
            return Ok(Vec::new());
        }
        canary.runs.push(run.clone());
        let canary = canary.clone();
        self.save_canaries(&canaries)?;
        let mut candidates = self.candidates()?;
        if let Some(found) = candidates.iter_mut().find(|c| c.id == canary.candidate) {
            found.state = CandidateState::Canary {
                runs: canary.runs.len() as u32,
            };
            self.save_candidates(&candidates)?;
        }

        let baseline = self.baseline(
            &canary.learned_workflow,
            canary.parent,
            settings.evolution.tripwire_baseline_runs,
        )?;
        let reversals = self.reversals(&canary.runs)?;
        let trips = tripwires(&canary.runs, &baseline, reversals, settings);
        if !trips.is_empty() {
            let reasons: Vec<String> = trips
                .iter()
                .map(|trip| describe(trip, canary.parent, baseline.len()))
                .collect();
            return self.rollback(&canary.learned_workflow, &reasons.join("; "));
        }
        if canary.runs.len() as u32 >= settings.evolution.canary_runs {
            return self.promote(settings, &canary);
        }
        Ok(Vec::new())
    }

    /// The parent's last runs outside any canary, oldest first.
    fn baseline(&self, learned_workflow: &str, parent: u32, limit: u32) -> Result<Vec<RunRecord>> {
        let wanted = LearnedWorkflowRef {
            name: learned_workflow.to_owned(),
            version: parent,
        };
        let mut runs: Vec<RunRecord> = outcomes::read_all(&self.store)?
            .into_iter()
            .filter_map(|line| match line {
                LogLine::Run(run)
                    if !run.canary && run.learned_workflow.as_ref() == Some(&wanted) =>
                {
                    Some(run)
                }
                _ => None,
            })
            .collect();
        runs.sort_by_key(|run| run.at);
        let skip = runs.len().saturating_sub(limit as usize);
        Ok(runs.split_off(skip))
    }

    fn reversals(&self, runs: &[RunRecord]) -> Result<u32> {
        Ok(outcomes::read_all(&self.store)?
            .iter()
            .filter(|line| {
                matches!(line, LogLine::Reversal(reversal)
                    if runs.iter().any(|run| run.run == reversal.run))
            })
            .count() as u32)
    }

    /// The canary passed: the version is current in Argonauts Code's state now, and its files
    /// go on a branch that waits for you.
    fn promote(&self, settings: &Settings, canary: &state::Canary) -> Result<Vec<Action>> {
        let name = &canary.learned_workflow;
        let version = canary.version;
        let loaded = self.candidate_learned_workflow(&canary.candidate)?;
        let checkout = self.checkout_current(name)?;

        let mut canaries = self.canaries()?;
        canaries.remove(name);
        let mut candidates = self.candidates()?;
        if let Some(found) = candidates.iter_mut().find(|c| c.id == canary.candidate) {
            found.state = CandidateState::Current;
        }
        let mut states = self.learned_workflow_states()?;
        states.entry(name.clone()).or_default().pointer = Some(state::Pointer {
            version,
            checkout_was: checkout,
        });
        self.save_canaries(&canaries)?;
        self.save_candidates(&candidates)?;
        self.save_learned_workflow_states(&states)?;

        let learned_workflow = LearnedWorkflowRef {
            name: name.clone(),
            version,
        };
        let branch = learned_workflow_branch(name, version);
        let why = candidates
            .iter()
            .find(|c| c.id == canary.candidate)
            .map(|c| c.why.clone())
            .unwrap_or_default();
        let title = format!("Promote workflow {name} v{version}");
        let body = format!(
            "{why}\n\nIt beat v{} on replay, held on the holdout, and passed {} canary runs. \
             Argonauts Code already runs it; merging this records it in git.",
            canary.parent,
            canary.runs.len()
        );
        let mut actions = Vec::new();
        let committed = self
            .promotion_entries(name, &canary.candidate, &loaded)
            .and_then(|entries| {
                git::commit_on_branch(
                    &self.store.repo_root,
                    "HEAD",
                    &branch,
                    &entries,
                    &format!("{title}\n\n{body}"),
                )
            });
        match committed {
            Ok(_) => actions.push(Action::Promoted {
                learned_workflow: learned_workflow.clone(),
                pr_command: [
                    "gh", "pr", "create", "--head", &branch, "--title", &title, "--body", &body,
                ]
                .map(str::to_owned)
                .to_vec(),
                branch,
            }),
            Err(error) => actions.push(Action::Tell {
                text: format!(
                    "{learned_workflow} is current, but its branch couldn't be made: {error}"
                ),
            }),
        }
        if settings.skills.export_after_canary {
            let text =
                match skills::propose_export(&self.store, &loaded, settings.skills.claude_link) {
                    Ok(proposal) => format!(
                        "Proposed exporting {learned_workflow} as a skill on {}",
                        proposal.branch
                    ),
                    Err(error) => {
                        format!("Couldn't propose exporting {learned_workflow} as a skill: {error}")
                    }
                };
            actions.push(Action::Tell { text });
        }
        Ok(actions)
    }

    /// The candidate's version file, the files beside it that the checkout lacks or holds
    /// differently, and the `current` pointer.
    fn promotion_entries(
        &self,
        name: &str,
        candidate: &str,
        loaded: &LoadedLearnedWorkflow,
    ) -> Result<Vec<(String, git::Entry)>> {
        let folder = self.candidate_dir(candidate);
        let checkout = self.store.learned_workflows_dir().join(name);
        let mut entries = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        files.sort();
        for path in files {
            let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let is_version = learned_workflows::version_of_file(&path).is_some();
            if is_version
                && file != learned_workflows::version_file(loaded.learned_workflow.version)
            {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let unchanged =
                std::fs::read_to_string(checkout.join(file)).ok().as_ref() == Some(&text);
            if !unchanged {
                entries.push((
                    learned_workflows::repo_path(name, file),
                    git::Entry::File(text),
                ));
            }
        }
        entries.push((
            learned_workflows::repo_path(name, learned_workflows::CURRENT_FILE),
            git::Entry::File(format!("{}\n", loaded.learned_workflow.version)),
        ));
        Ok(entries)
    }

    /// Back to the version before, now, with the reason you or a tripwire gave.
    pub fn rollback(&self, learned_workflow: &str, reason: &str) -> Result<Vec<Action>> {
        let mut canaries = self.canaries()?;
        if let Some(canary) = canaries.remove(learned_workflow) {
            let mut candidates = self.candidates()?;
            if let Some(found) = candidates.iter_mut().find(|c| c.id == canary.candidate) {
                found.state = CandidateState::RolledBack {
                    reason: reason.to_owned(),
                };
            }
            self.save_canaries(&canaries)?;
            self.save_candidates(&candidates)?;
            return Ok(vec![
                Action::RolledBack {
                    learned_workflow: LearnedWorkflowRef {
                        name: learned_workflow.to_owned(),
                        version: canary.version,
                    },
                    to: canary.parent,
                    reason: reason.to_owned(),
                },
                Action::Tell {
                    text: format!(
                        "Rolled {learned_workflow} back from its v{} canary to v{}: {reason}",
                        canary.version, canary.parent
                    ),
                },
            ]);
        }

        let Some(current) = self.running_version(learned_workflow)? else {
            return Err(LearningError::Refused(format!(
                "there's no learned workflow named `{learned_workflow}`"
            )));
        };
        let previous = self
            .load_version(learned_workflow, current)?
            .and_then(|loaded| loaded.learned_workflow.parent)
            .ok_or_else(|| {
                LearningError::Refused(format!(
                    "{learned_workflow} v{current} has no version before it"
                ))
            })?;
        let checkout = self.checkout_set(learned_workflow)?;
        let mut states = self.learned_workflow_states()?;
        states
            .entry(learned_workflow.to_owned())
            .or_default()
            .pointer = Some(state::Pointer {
            version: previous,
            checkout_was: checkout.as_ref().map(|set| set.current),
        });
        self.save_learned_workflow_states(&states)?;
        let mut candidates = self.candidates()?;
        for found in candidates.iter_mut().filter(|c| {
            c.parent.name == learned_workflow
                && c.version == current
                && c.state == CandidateState::Current
        }) {
            found.state = CandidateState::RolledBack {
                reason: reason.to_owned(),
            };
        }
        self.save_candidates(&candidates)?;

        // The rollback already holds; the branch only records it in git for you to merge.
        let branch = format!("argonauts/learned/{learned_workflow}-rollback-v{previous}");
        let mut entries = vec![(
            learned_workflows::repo_path(learned_workflow, learned_workflows::CURRENT_FILE),
            git::Entry::File(format!("{previous}\n")),
        )];
        let in_checkout = checkout.is_some_and(|set| set.version(previous).is_some());
        if !in_checkout && let Some(loaded) = self.load_version(learned_workflow, previous)? {
            entries.push((
                learned_workflows::repo_path(
                    learned_workflow,
                    &learned_workflows::version_file(previous),
                ),
                git::Entry::File(loaded.text),
            ));
        }
        let message = format!("Roll back workflow {learned_workflow} to v{previous}\n\n{reason}");
        let proposed =
            git::commit_on_branch(&self.store.repo_root, "HEAD", &branch, &entries, &message);
        let text = match proposed {
            Ok(_) => format!(
                "{learned_workflow} runs v{previous} again: {reason}. The pointer change waits on {branch}"
            ),
            Err(error) => format!(
                "{learned_workflow} runs v{previous} again: {reason}. The pointer change couldn't \
                 be proposed: {error}"
            ),
        };
        Ok(vec![
            Action::RolledBack {
                learned_workflow: LearnedWorkflowRef {
                    name: learned_workflow.to_owned(),
                    version: current,
                },
                to: previous,
                reason: reason.to_owned(),
            },
            Action::Tell { text },
        ])
    }
}

fn describe(trip: &Tripwire, parent: u32, baseline: usize) -> String {
    match trip {
        Tripwire::StuckTasks => format!(
            "its runs needed you more than v{parent}'s last {baseline} runs can explain by chance"
        ),
        Tripwire::FailedFinalChecks => format!(
            "its final checks failed more often than v{parent}'s last {baseline} runs can explain \
             by chance"
        ),
        Tripwire::Cost => format!(
            "it cost more per run than v{parent}'s last {baseline} runs can explain by chance"
        ),
        Tripwire::Rejected => "you rejected one of its approvals".to_owned(),
        Tripwire::Reverted => {
            "you reverted one of its merges or closed its pull request".to_owned()
        }
    }
}
