//! The run review, after the run's workflow ends: Brier scores for predictions, one outcome record
//! per task and one run line in the outcome log, the memory checks, accepted notes kept in the
//! repo's state directory, at most one process change from `RunReview`, and then the evolution
//! loop's turn. Each part checks the log first, so a run review re-entered after a crash repeats
//! nothing. Notes, like goals and decisions, are Argonauts Code's own and never enter your
//! repository.

use std::collections::BTreeSet;
use std::path::Path;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{PhaseChange, RunOutcome, RunPhase};
use argonauts_core::events::session::{EndReason, SessionEnd};
use argonauts_core::events::shared::RunReviewEvent;
use argonauts_core::events::task::TaskState;
use argonauts_core::log::{Note, NoteLevel, RUN_REVIEW_FAILED};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::Settings;
use argonauts_core::settings::learning::FlaggedNote;
use argonauts_core::steps::StepError;
use argonauts_core::steps::task::{MemoryProposal, RunReview};
use argonauts_core::{LimitDimension, PhaseStatus, RunId, TaskId};
use argonauts_learning::evolution::{self, Action, Evolution, Stage};
use argonauts_learning::memory::{self, Source};
use argonauts_learning::{LearningError, RunRecord, Store};

use crate::divide;
use crate::engine::host::{Ask, Host};
use crate::engine::job_fallback;
use crate::error::{EngineError, EngineResult};
use crate::merge::{conductor, merge_packet};
use crate::{prompts, roles};

pub mod brier;
pub mod facts;
pub mod learned_workflow;
pub mod memory_scan;
pub mod notes;
pub mod outcomes;
pub mod reversals;

/// Runs the run review once the run's result is settled. A failed run review is recorded as its own
/// failure and leaves that result alone; only an interruption unwinds, so it runs again.
pub async fn run_or_note(host: &Host) -> EngineResult<()> {
    match run(host).await {
        Ok(()) => Ok(()),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(err) => {
            let reason = err.to_string();
            phase(host, PhaseStatus::Failed, Some(&reason))?;
            let note = Note {
                level: NoteLevel::Warn,
                text: RUN_REVIEW_FAILED.to_owned(),
            };
            host.record(None, None, RunEventKind::Note(note))?;
            Ok(())
        }
    }
}

pub async fn run(host: &Host) -> EngineResult<()> {
    if host.read(|state| state.run.phase) != Some(RunPhase::RunReview) {
        phase(host, PhaseStatus::Started, None)?;
    }
    let settings = host.settings();
    let facts = host.read(|state| facts::task_facts(state, settings));

    for fact in facts.iter().filter(|fact| fact.ran()) {
        let Some(predictions) = &fact.predictions else {
            continue;
        };
        for event in brier::score(&fact.task, predictions, &fact.happened()) {
            let RunReviewEvent::Scored { task, metric, .. } = &event else {
                continue;
            };
            if !host.read(|state| state.run_review.scored(task, metric)) {
                host.record(
                    Some(&fact.task),
                    None,
                    RunEventKind::RunReview(event.clone()),
                )?;
            }
        }
    }

    let state_dir = divide::repo_state_dir(host);
    let run_id = host.run_id();
    let lines = outcomes::run_lines(&state_dir, run_id)?;
    let run_learned_workflow = learned_workflow::read(&host.repo().run_dir);
    let context = facts::RunContext {
        draws: lines
            .iter()
            .filter_map(|line| match line {
                outcomes::LogLine::Draw(draw) => Some((draw.task.clone(), draw.clone())),
                _ => None,
            })
            .collect(),
        learned_workflow: run_learned_workflow
            .as_ref()
            .map(|run| run.learned_workflow.clone()),
        at: Some(host.now()),
    };
    let records: Vec<outcomes::OutcomeRecord> = facts
        .iter()
        .filter(|fact| fact.ran())
        .filter_map(|fact| fact.record(run_id, &context))
        .collect();
    let logged: BTreeSet<_> = lines
        .iter()
        .filter_map(|line| match line {
            outcomes::LogLine::Outcome(record) => Some(record.task.clone()),
            _ => None,
        })
        .collect();
    let missing: Vec<_> = records
        .iter()
        .filter(|record| !logged.contains(&record.task))
        .cloned()
        .collect();
    outcomes::append(&state_dir, run_id, &missing)?;
    for record in &records {
        if host.read(|state| state.run_review.has_outcome(&record.task)) {
            continue;
        }
        let event = RunReviewEvent::Outcome {
            task: record.task.clone(),
            worker: record.worker.clone(),
            task_type: record.task_type.clone(),
            first_try: record.first_try,
            tries: record.tries,
            merged: record.merged,
        };
        host.record(Some(&record.task), None, RunEventKind::RunReview(event))?;
    }

    scan_proposals(host)?;
    keep_notes(host)?;

    if process_change_due(host, settings)? && !host.read(|state| state.run_review.has_candidate()) {
        propose(host, &facts, settings).await?;
    }
    // Before the loop's turn, so a canary's tripwire counts what you did since the last run.
    let root = host.repo().root.clone();
    if let Err(err) = reversals::canary_pull_requests(&state_dir, &root, &reversals::Gh, host.now())
    {
        let note = Note {
            level: NoteLevel::Warn,
            text: format!("The canary's pull requests weren't checked: {err}"),
        };
        host.record(None, None, RunEventKind::Note(note))?;
    }
    if !lines
        .iter()
        .any(|line| matches!(line, outcomes::LogLine::Run(_)))
    {
        let run = run_record(host, &facts, run_learned_workflow.as_ref());
        evolve(host, &run)?;
        outcomes::append_lines(&state_dir, run_id, &[outcomes::LogLine::Run(run)])?;
    }
    let summary = host.read(|state| match state.run_review.mean_brier() {
        Some(mean) => format!("{} tasks logged, mean Brier score {mean:.2}", records.len()),
        None => format!("{} tasks logged", records.len()),
    });
    phase(host, PhaseStatus::Finished, Some(&summary))
}

/// Before the merge approval opens: the memory proposals so far are scanned and the accepted
/// notes kept, and with `outcomes.share` the finished runs' logs go on the branch that lands. A
/// failure here leaves them to the run review after the merge, so it never holds the merge up.
pub async fn notes_before_merge(host: &Host) -> EngineResult<()> {
    let committed = async {
        scan_proposals(host)?;
        keep_notes(host)?;
        let (_, worktree) = merge_packet::landing_branch(host).await?;
        share_outcomes(host, &worktree).await
    };
    match committed.await {
        Ok(()) => Ok(()),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(err) => {
            let note = Note {
                level: NoteLevel::Warn,
                text: format!(
                    "The run's notes weren't committed before the merge approval: {err}. \
                     The run review commits them after."
                ),
            };
            host.record(None, None, RunEventKind::Note(note))?;
            Ok(())
        }
    }
}

/// With `outcomes.share`, every finished run's outcome log that `.argonauts/outcomes/` lacks goes
/// on the branch that lands, in one commit of Argonauts Code's, so your team's track records add
/// up. A run's log is finished only once its run review wrote the run line, after it merged, so
/// each log lands with the next run that lands.
async fn share_outcomes(host: &Host, worktree: &Path) -> EngineResult<()> {
    if !host.settings().outcomes.share {
        return Ok(());
    }
    let store = Store::new(divide::repo_state_dir(host), host.repo().root.clone());
    let logs = match std::fs::read_dir(store.outcomes_dir()) {
        Ok(logs) => logs,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let mut runs: Vec<RunId> = logs
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "jsonl")
                .then(|| RunId::new(path.file_stem()?.to_str()?).ok())
                .flatten()
        })
        .collect();
    runs.sort();
    let mut paths = Vec::new();
    for run in runs {
        let relative = format!(
            ".argonauts/{}/{}.jsonl",
            argonauts_core::layout::project_files::OUTCOMES,
            run.as_str()
        );
        if worktree.join(&relative).exists() {
            continue;
        }
        if outcomes::learned(argonauts_learning::outcomes::share(&store, &run, worktree))?.is_some()
        {
            paths.push(relative);
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let subject = "Outcomes: finished runs' logs";
    let sha = conductor::commit(host, worktree, &refs, subject).await?;
    host.record(
        None,
        None,
        RunEventKind::RunReview(RunReviewEvent::Committed { sha, paths }),
    )?;
    Ok(())
}

/// Checks each memory proposal not checked yet: the engine's scan, then learning's limits and
/// scan under `[memory]`. A note that passes is accepted when the task's reviewer approved the
/// task that carried it. A flagged note waits for you (`memory.flagged = "ask"`) or is dropped;
/// it never applies on its own.
fn scan_proposals(host: &Host) -> EngineResult<()> {
    let settings = host.settings();
    let state_dir = divide::repo_state_dir(host);
    for found in host.read(proposals) {
        let Proposed {
            from,
            task,
            index,
            proposal,
            reviewed,
            evidence,
        } = found;
        if host.read(|state| state.run_review.scanned(&from, &proposal)) {
            continue;
        }
        let verdict = match memory_scan::scan(&proposal) {
            Err(reason) => Err(reason),
            Ok(()) => match memory::check(&proposal, index, &settings.memory) {
                memory::Verdict::Pass if reviewed => Ok(()),
                memory::Verdict::Pass => {
                    Err("its task's reviewer didn't approve the task that carried it".to_owned())
                }
                memory::Verdict::Reject { reasons } => Err(reasons.join("; ")),
                memory::Verdict::Flag { findings } => {
                    let found = findings.len();
                    match settings.memory.flagged {
                        FlaggedNote::Drop => Err(format!(
                            "flagged by the scan ({found} findings) and dropped"
                        )),
                        FlaggedNote::Ask => {
                            let flagged = notes::Flagged {
                                note: proposal.clone(),
                                source: Source {
                                    run: host.run_id().clone(),
                                    task: task.clone(),
                                    worker: from.clone(),
                                    date: host.now(),
                                    evidence: evidence.clone(),
                                },
                                findings,
                            };
                            notes::flag(&state_dir, &flagged)?;
                            Err(format!(
                                "flagged by the scan ({found} findings); it waits for you in /memory"
                            ))
                        }
                    }
                }
            },
        };
        let event = RunReviewEvent::Memory {
            from,
            proposal,
            accepted: verdict.is_ok(),
            reason: verdict.err(),
        };
        host.record(Some(&task), None, RunEventKind::RunReview(event))?;
    }
    Ok(())
}

/// One memory proposal in a worker's report.
struct Proposed {
    from: ModelRef,
    task: TaskId,
    /// Its place among the task's proposals, for `memory.max_per_task`.
    index: usize,
    proposal: MemoryProposal,
    /// The task's reviewer approved it, or with review off, it merged.
    reviewed: bool,
    /// The task's passing checks.
    evidence: Vec<String>,
}

/// Every memory proposal in the workers' reports, with who wrote it and on which task.
fn proposals(state: &crate::state::RunState) -> Vec<Proposed> {
    use argonauts_core::events::task::ReviewEvent;
    use argonauts_core::steps::judgment::ReviewVerdict;

    let mut found = Vec::new();
    for record in state.tasks.by_id.values() {
        let Some(from) = record.worker.clone().or_else(|| record.spec.worker.clone()) else {
            continue;
        };
        let last_review = record.reviews.iter().rev().find_map(|review| match review {
            ReviewEvent::Reviewed { review, .. } => Some(review.verdict),
            _ => None,
        });
        let reviewed = match last_review {
            Some(verdict) => verdict == ReviewVerdict::Approve,
            None => record.state == TaskState::Merged,
        };
        let evidence: Vec<String> = record
            .checks
            .iter()
            .filter(|check| check.passed)
            .map(|check| check.name.clone())
            .collect();
        let mut index = 0;
        for report in &record.reports {
            for proposal in &report.memory_proposals {
                found.push(Proposed {
                    from: from.clone(),
                    task: record.spec.id.clone(),
                    index,
                    proposal: proposal.clone(),
                    reviewed,
                    evidence: evidence.clone(),
                });
                index += 1;
            }
        }
    }
    found
}

/// Keeps each accepted note in the repo's state directory, naming its task and run. Notes are
/// Argonauts Code's own, so they never go into your repository, and later runs read them from
/// there.
fn keep_notes(host: &Host) -> EngineResult<()> {
    let accepted = host.read(|state| state.run_review.accepted());
    let state_dir = divide::repo_state_dir(host);
    for (from, note) in accepted {
        let task = host.read(|state| task_of(state, &from, &note));
        let source = Source {
            run: host.run_id().clone(),
            task,
            worker: from,
            date: host.now(),
            evidence: Vec::new(),
        };
        notes::keep_note(&state_dir, &note, &source)?;
    }
    Ok(())
}

/// The stuck questions the run opened, decided or not.
fn stuck_tasks_of(state: &crate::state::RunState) -> usize {
    state
        .approvals
        .by_id
        .values()
        .filter(|record| {
            record.opened.kind == argonauts_core::events::approval::ApprovalKind::Stuck
        })
        .count()
}

/// The task whose worker proposed `note`.
fn task_of(state: &crate::state::RunState, from: &ModelRef, note: &MemoryProposal) -> TaskId {
    proposals(state)
        .into_iter()
        .find(|found| &found.from == from && &found.proposal == note)
        .map_or_else(|| TaskId::numbered(0), |found| found.task)
}

/// The whole run, as the canary and learned workflow statistics read it.
fn run_record(
    host: &Host,
    facts: &[facts::TaskFacts],
    learned_workflow: Option<&learned_workflow::RunLearnedWorkflow>,
) -> RunRecord {
    let (task_type, outcome, final_check, workflow, spent, needs_you) = host.read(|state| {
        let started = state.run.started.as_ref();
        let task_type = started
            .and_then(|started| started.estimate.as_ref())
            .map_or_else(|| "build".to_owned(), |estimate| estimate.task_type.clone());
        let workflow = state
            .workflows
            .root
            .as_ref()
            .and_then(|root| state.workflows.by_id.get(root))
            .and_then(|record| {
                let name = record.from.as_ref()?.as_str().to_owned();
                Some((name, record.version.clone()))
            });
        let spent: f64 = state
            .sessions
            .by_key
            .values()
            .filter_map(|session| session.end.as_ref())
            .map(|end| end.spend.cost_usd)
            .sum();
        (
            task_type,
            state
                .run
                .finished
                .or_else(|| crate::merge::step::ended(&state.merge)),
            state.merge.final_check,
            workflow,
            spent,
            stuck_tasks_of(state),
        )
    });
    // Every time a task came to you, though a retry took it on afterwards.
    let stuck_tasks = needs_you.max(
        facts
            .iter()
            .filter(|fact| fact.state == TaskState::NeedsYou)
            .count(),
    );
    RunRecord {
        run: host.run_id().clone(),
        at: host.now(),
        task_type,
        learned_workflow: learned_workflow.map(|run| run.learned_workflow.clone()),
        workflow,
        outcome: outcome.unwrap_or(RunOutcome::BranchLeft),
        stuck_tasks: u32::try_from(stuck_tasks).unwrap_or(u32::MAX),
        final_checks_passed: final_check.unwrap_or(false),
        cost_usd: spent,
        minutes: facts.iter().map(|fact| fact.worker_minutes).sum(),
        canary: learned_workflow.is_some_and(|run| run.canary),
    }
}

/// The evolution loop's turn after the run: it may queue the run review's proposal as a candidate,
/// record a stage-1 learned workflow, or roll a canary back. Each action is told in the run's log.
/// The loop never holds up the run's own result.
fn evolve(host: &Host, run: &RunRecord) -> EngineResult<()> {
    let settings = host.settings();
    let proposal = host.read(|state| {
        state
            .run_review
            .events
            .iter()
            .find_map(|event| match event {
                RunReviewEvent::Candidate(change) => Some(change.clone()),
                _ => None,
            })
    });
    let store = Store::new(divide::repo_state_dir(host), host.repo().root.clone());
    let actions = Evolution::open(&store)
        .and_then(|evolution| evolution.after_run(settings, run, proposal.as_ref()));
    let notes: Vec<Note> = match actions {
        Ok(actions) => actions.iter().map(action_note).collect(),
        Err(LearningError::NotYet(_)) => Vec::new(),
        Err(err) => vec![Note {
            level: NoteLevel::Warn,
            text: format!("The evolution loop skipped this run: {err}"),
        }],
    };
    for note in notes {
        host.record(None, None, RunEventKind::Note(note))?;
    }
    Ok(())
}

/// One line for an action of the loop, as the run's log tells it.
pub fn action_note(action: &Action) -> Note {
    let text = match action {
        Action::Replay { candidate } => {
            format!("Workflow candidate {candidate} is queued for a replay test.")
        }
        Action::AskPromotion { candidate, .. } => {
            format!(
                "Workflow candidate {candidate} passed its replays and waits for you in /learned."
            )
        }
        Action::Promoted {
            learned_workflow,
            branch,
            ..
        } => {
            format!("{learned_workflow} was promoted; its commit waits on {branch}.")
        }
        Action::RolledBack {
            learned_workflow,
            to,
            reason,
        } => {
            format!("{learned_workflow} was rolled back to v{to}: {reason}.")
        }
        // Its branch is plumbing, so the line names the learned workflow alone.
        Action::Recorded {
            learned_workflow, ..
        } => {
            format!(
                "This run's workflow was saved as {learned_workflow}, a learned workflow for you to review."
            )
        }
        Action::Tell { text } => text.clone(),
    };
    let level = match action {
        Action::RolledBack { .. } => NoteLevel::Warn,
        _ => NoteLevel::Info,
    };
    Note { level, text }
}

/// Every `evolution.propose_every` runs that reached the run review; 0 means only when you ask. At
/// stage `record` the loop only saves what worked, so the run review isn't asked for a change.
fn process_change_due(host: &Host, settings: &Settings) -> EngineResult<bool> {
    let every = settings.evolution.propose_every as usize;
    if every == 0
        || settings.evolution.evolvable.is_empty()
        || evolution::stage(settings) == Stage::Record
    {
        return Ok(false);
    }
    let runs = outcomes::runs_logged(&divide::repo_state_dir(host))?;
    Ok(runs.is_multiple_of(every))
}

async fn propose(host: &Host, facts: &[facts::TaskFacts], settings: &Settings) -> EngineResult<()> {
    let worker = roles::candidates(settings, RoleName::RunReview)
        .into_iter()
        .next()
        .unwrap_or_else(|| settings.crew.main.clone());
    let mut ask = Ask::new(
        None,
        RoleName::RunReview,
        worker,
        prompts::divide::RUN_REVIEW,
        facts::digest(facts, settings),
    );
    ask.access = Access::NoTools;
    // The run has its result and waits only on this to end, so the proposal gets one try by
    // its own agent, for no longer than `evolution.propose_timeout`: an agent that is out is
    // left alone, one that fails isn't tried again, and one that is slow is stopped.
    ask.optional = true;
    let timeout = &settings.evolution.propose_timeout;
    ask.time_limit = timeout.parse().ok().flatten();
    let agent = roles::display_name(&ask.worker);
    if job_fallback::is_out(host, &ask.worker).await {
        return no_proposal(host, &format!("{agent} is out"));
    }
    let change = match host.ask::<RunReview>(ask).await {
        Ok(answered) => answered.output.change,
        Err(EngineError::Step(StepError::Session(end))) if out_of_time(&end) => {
            return no_proposal(host, &format!("{agent} took longer than {}", timeout.0));
        }
        Err(err) => return Err(err),
    };
    let Some(change) = change else {
        return Ok(());
    };
    let kind = if settings.evolution.evolvable.contains(&change.setting) {
        RunEventKind::RunReview(RunReviewEvent::Candidate(change))
    } else {
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: format!(
                "The run review proposed changing {}, which isn't on the evolvable list, so it was dropped.",
                change.setting
            ),
        })
    };
    host.record(None, None, kind)?;
    Ok(())
}

/// Says in one line why nobody proposed a change, and leaves the rest of the run review to go on.
fn no_proposal(host: &Host, why: &str) -> EngineResult<()> {
    let note = Note {
        level: NoteLevel::Info,
        text: format!("The run review proposed no change this time, since {why}."),
    };
    host.record(None, None, RunEventKind::Note(note))?;
    Ok(())
}

/// Whether the session was stopped at its time limit.
fn out_of_time(end: &SessionEnd) -> bool {
    end.reason
        == EndReason::LimitHit {
            dimension: LimitDimension::Minutes,
        }
}

fn phase(host: &Host, status: PhaseStatus, summary: Option<&str>) -> EngineResult<()> {
    let change = PhaseChange {
        phase: RunPhase::RunReview,
        status,
        summary: summary.map(str::to_owned),
    };
    host.record(None, None, RunEventKind::Phase(change))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_learning::types::LearnedWorkflowRef;

    use super::*;

    #[test]
    fn a_recorded_learned_workflow_is_named_without_its_branch() {
        let note = action_note(&Action::Recorded {
            learned_workflow: LearnedWorkflowRef {
                name: "api-feature".into(),
                version: 1,
            },
            branch: "argonauts/learned/api-feature-v1".into(),
        });
        assert_eq!(
            note.text,
            "This run's workflow was saved as api-feature v1, a learned workflow for you to review."
        );
    }
}
