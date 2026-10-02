//! The retro, after the run's workflow ends: Brier scores for predictions, one outcome record
//! per card and one run line in the outcome log, the memory checks, notes committed one per
//! commit and decisions by the conductor and landed when the run merged, at most one process
//! change from `Retro`, and then the evolution loop's turn. Each part checks the log first, so a
//! retro re-entered after a crash repeats nothing.
//!
//! The notes and decisions known before the merge checkpoint are committed before it opens, so
//! your approval covers them and they land with the run. Only what came later lands after it.

use std::collections::BTreeSet;
use std::path::Path;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardState;
use argonauts_core::events::run::{PhaseChange, RunOutcome, RunPhase};
use argonauts_core::events::shared::RetroEvent;
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::learning::FlaggedNote;
use argonauts_core::steps::card::{MemoryProposal, Retro};
use argonauts_core::worker::Access;
use argonauts_core::{CardId, PhaseStatus, RunId};
use argonauts_learning::evolution::{self, Action, Evolution, Stage};
use argonauts_learning::memory::{self, Source};
use argonauts_learning::{LearningError, RunRecord, Store};

use crate::divide::{self, dependencies};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::integrate::{branch, conductor, merge_packet};
use crate::{prompts, roles};

pub mod brier;
pub mod facts;
pub mod landing;
pub mod memory_scan;
pub mod notes;
pub mod outcomes;
pub mod recipe;
pub mod reversals;

const DECISIONS_DIR: &str = ".argonauts/decisions";

/// Runs the retro once the run's result is settled. A failed retro is recorded as its own
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
                text: format!("The retro failed: {reason}. The run's result stands."),
            };
            host.record(None, None, RunEventKind::Note(note))?;
            Ok(())
        }
    }
}

pub async fn run(host: &Host) -> EngineResult<()> {
    if host.read(|state| state.run.phase) != Some(RunPhase::Retro) {
        phase(host, PhaseStatus::Started, None)?;
    }
    let settings = host.settings();
    let facts = host.read(|state| facts::card_facts(state, settings));

    for fact in facts.iter().filter(|fact| fact.ran()) {
        let Some(predictions) = &fact.predictions else {
            continue;
        };
        for event in brier::score(&fact.card, predictions, &fact.happened()) {
            let RetroEvent::Scored { card, metric, .. } = &event else {
                continue;
            };
            if !host.read(|state| state.retro.scored(card, metric)) {
                host.record(Some(&fact.card), None, RunEventKind::Retro(event.clone()))?;
            }
        }
    }

    let state_dir = divide::repo_state_dir(host);
    let run_id = host.run_id();
    let lines = outcomes::run_lines(&state_dir, run_id)?;
    let run_recipe = recipe::read(&host.repo().run_dir);
    let context = facts::RunContext {
        draws: lines
            .iter()
            .filter_map(|line| match line {
                outcomes::LogLine::Draw(draw) => Some((draw.card.clone(), draw.clone())),
                _ => None,
            })
            .collect(),
        recipe: run_recipe.as_ref().map(|run| run.recipe.clone()),
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
            outcomes::LogLine::Outcome(record) => Some(record.card.clone()),
            _ => None,
        })
        .collect();
    let missing: Vec<_> = records
        .iter()
        .filter(|record| !logged.contains(&record.card))
        .cloned()
        .collect();
    outcomes::append(&state_dir, run_id, &missing)?;
    for record in &records {
        if host.read(|state| state.retro.has_outcome(&record.card)) {
            continue;
        }
        let event = RetroEvent::Outcome {
            card: record.card.clone(),
            worker: record.worker.clone(),
            task_type: record.task_type.clone(),
            first_try: record.first_try,
            attempts: record.attempts,
            integrated: record.integrated,
        };
        host.record(Some(&record.card), None, RunEventKind::Retro(event))?;
    }

    scan_proposals(host)?;
    commit_notes(host).await?;

    if process_change_due(host, settings)? && !host.read(|state| state.retro.has_candidate()) {
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
        let run = run_record(host, &facts, run_recipe.as_ref());
        evolve(host, &run)?;
        outcomes::append_lines(&state_dir, run_id, &[outcomes::LogLine::Run(run)])?;
    }
    let summary = host.read(|state| match state.retro.mean_brier() {
        Some(mean) => format!("{} cards logged, mean Brier score {mean:.2}", records.len()),
        None => format!("{} cards logged", records.len()),
    });
    phase(host, PhaseStatus::Finished, Some(&summary))
}

/// Before the merge checkpoint opens: the memory proposals so far are scanned, and the accepted
/// notes and the run's decisions are committed on the branch that lands. A failure here leaves
/// them to the retro after the merge, so it never holds the merge up.
pub async fn notes_before_merge(host: &Host) -> EngineResult<()> {
    let committed = async {
        scan_proposals(host)?;
        let (_, worktree) = merge_packet::landing_branch(host).await?;
        commit_notes_in(host, &worktree).await?;
        share_outcomes(host, &worktree).await
    };
    match committed.await {
        Ok(()) => Ok(()),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(err) => {
            let note = Note {
                level: NoteLevel::Warn,
                text: format!(
                    "The run's notes weren't committed before the merge checkpoint: {err}. \
                     The retro commits them after."
                ),
            };
            host.record(None, None, RunEventKind::Note(note))?;
            Ok(())
        }
    }
}

/// With `outcomes.share`, every finished run's outcome log that `.argonauts/outcomes/` lacks
/// goes on the branch that lands, in one conductor commit, so your team's track records add
/// up. A run's log is finished only once its retro wrote the run line, after it landed, so each
/// log lands with the next run that lands.
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
        RunEventKind::Retro(RetroEvent::Committed { sha, paths }),
    )?;
    Ok(())
}

/// Checks each memory proposal not checked yet: the engine's scan, then learning's limits and
/// scan under `[memory]`. A note that passes is accepted when the card's reviewer approved the
/// card that carried it. A flagged note waits for you (`memory.flagged = "ask"`) or is dropped;
/// it never applies on its own.
fn scan_proposals(host: &Host) -> EngineResult<()> {
    let settings = host.settings();
    let state_dir = divide::repo_state_dir(host);
    for found in host.read(proposals) {
        let Proposed {
            from,
            card,
            index,
            proposal,
            reviewed,
            evidence,
        } = found;
        if host.read(|state| state.retro.scanned(&from, &proposal)) {
            continue;
        }
        let verdict = match memory_scan::scan(&proposal) {
            Err(reason) => Err(reason),
            Ok(()) => match memory::check(&proposal, index, &settings.memory) {
                memory::Verdict::Pass if reviewed => Ok(()),
                memory::Verdict::Pass => {
                    Err("its card's reviewer didn't approve the card that carried it".to_owned())
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
                                    card: card.clone(),
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
        let event = RetroEvent::Memory {
            from,
            proposal,
            accepted: verdict.is_ok(),
            reason: verdict.err(),
        };
        host.record(Some(&card), None, RunEventKind::Retro(event))?;
    }
    Ok(())
}

/// One memory proposal in an owner's report.
struct Proposed {
    from: WorkerRef,
    card: CardId,
    /// Its place among the card's proposals, for `memory.max_per_card`.
    index: usize,
    proposal: MemoryProposal,
    /// The card's reviewer approved it, or with review off, it integrated.
    reviewed: bool,
    /// The card's passing checks.
    evidence: Vec<String>,
}

/// Every memory proposal in the owners' reports, with who wrote it and on which card.
fn proposals(state: &crate::state::RunState) -> Vec<Proposed> {
    use argonauts_core::events::card::ReviewEvent;
    use argonauts_core::steps::judgment::ReviewVerdict;

    let mut found = Vec::new();
    for record in state.cards.by_id.values() {
        let Some(from) = record.owner.clone().or_else(|| record.spec.owner.clone()) else {
            continue;
        };
        let last_review = record.reviews.iter().rev().find_map(|review| match review {
            ReviewEvent::Reviewed { review, .. } => Some(review.verdict),
            _ => None,
        });
        let reviewed = match last_review {
            Some(verdict) => verdict == ReviewVerdict::Approve,
            None => record.state == CardState::Integrated,
        };
        let evidence: Vec<String> = record
            .gates
            .iter()
            .filter(|gate| gate.passed)
            .map(|gate| gate.name.clone())
            .collect();
        let mut index = 0;
        for report in &record.reports {
            for proposal in &report.memory_proposals {
                found.push(Proposed {
                    from: from.clone(),
                    card: record.spec.id.clone(),
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

/// Accepted notes go to the integration branch beside the decisions, in one conductor commit.
/// When the run already landed, that commit lands too, or the next run would never read them.
async fn commit_notes(host: &Host) -> EngineResult<()> {
    if !has_notes(host) {
        return Ok(());
    }
    let git = host.git();
    let worktree = branch::ensure_integration(host).await?;
    let landed = host.read(|state| state.integration.landed.clone());
    let mut kept = None;
    if let Some(landed) = &landed {
        kept = landing::catch_up(git, &worktree, landed).await?.err();
    }
    commit_notes_in(host, &worktree).await?;
    // A retro re-entered after a crash may have committed already, so this looks at git.
    let Some(landed) = landed else {
        return Ok(());
    };
    let outcome = match kept {
        Some(why) => landing::NotesLanding::Kept(why),
        None => landing::land(git, &host.repo().root, &worktree, &landed).await?,
    };
    let note = match outcome {
        landing::NotesLanding::Nothing => return Ok(()),
        landing::NotesLanding::Landed(sha) => Note {
            level: NoteLevel::Info,
            text: format!("The retro's notes landed on {} at {sha}.", landed.target),
        },
        landing::NotesLanding::Kept(why) => Note {
            level: NoteLevel::Warn,
            text: format!(
                "The retro's notes stay on {}, since {why}. Merge that branch to keep them for \
                 later runs.",
                branch::integration_branch(host.run_id())
            ),
        },
    };
    host.record(None, None, RunEventKind::Note(note))?;
    Ok(())
}

/// Accepted notes, or decisions the plan made.
fn has_notes(host: &Host) -> bool {
    host.read(|state| !state.retro.accepted().is_empty() || !state.plan.decisions.is_empty())
}

/// Commits each accepted note on its own, naming its card and run, so `git revert` removes
/// exactly one fact; then the run's decisions together.
async fn commit_notes_in(host: &Host, worktree: &Path) -> EngineResult<()> {
    if !has_notes(host) {
        return Ok(());
    }
    let accepted = host.read(|state| state.retro.accepted());
    for (from, note) in accepted {
        let card = host.read(|state| card_of(state, &from, &note));
        let source = Source {
            run: host.run_id().clone(),
            card: card.clone(),
            worker: from,
            date: host.now(),
            evidence: Vec::new(),
        };
        let Some(path) = notes::write_note(worktree, &note, &source)? else {
            continue;
        };
        let subject = format!("Note from {card}: {}", note.topic.trim());
        let sha = conductor::commit(host, worktree, &[path.as_str()], &subject).await?;
        host.record(
            None,
            None,
            RunEventKind::Retro(RetroEvent::Committed {
                sha,
                paths: vec![path],
            }),
        )?;
    }
    let status = host.git().status_of(worktree, &[DECISIONS_DIR]).await?;
    let paths = dependencies::porcelain_paths(&status);
    if paths.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let sha = conductor::commit(host, worktree, &refs, "Retro: decisions").await?;
    host.record(
        None,
        None,
        RunEventKind::Retro(RetroEvent::Committed { sha, paths }),
    )?;
    Ok(())
}

/// The card whose owner proposed `note`.
fn card_of(state: &crate::state::RunState, from: &WorkerRef, note: &MemoryProposal) -> CardId {
    proposals(state)
        .into_iter()
        .find(|found| &found.from == from && &found.proposal == note)
        .map_or_else(|| CardId::numbered(0), |found| found.card)
}

/// The whole run, as the canary and recipe statistics read it.
fn run_record(
    host: &Host,
    facts: &[facts::CardFacts],
    recipe: Option<&recipe::RunRecipe>,
) -> RunRecord {
    let (task_type, outcome, final_check, workflow, spent) = host.read(|state| {
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
                .or_else(|| crate::integrate::merge::ended(&state.integration)),
            state.integration.final_check,
            workflow,
            spent,
        )
    });
    let escalations = facts
        .iter()
        .filter(|fact| fact.state == CardState::Escalated)
        .count();
    RunRecord {
        run: host.run_id().clone(),
        at: host.now(),
        task_type,
        recipe: recipe.map(|run| run.recipe.clone()),
        workflow,
        outcome: outcome.unwrap_or(RunOutcome::BranchLeft),
        escalations: u32::try_from(escalations).unwrap_or(u32::MAX),
        final_checks_passed: final_check.unwrap_or(false),
        cost_usd: spent,
        minutes: facts.iter().map(|fact| fact.owner_minutes).sum(),
        canary: recipe.is_some_and(|run| run.canary),
    }
}

/// The evolution loop's turn after the run: it may queue the retro's proposal as a candidate,
/// record a stage-1 recipe, or roll a canary back. Each action is told in the run's log. The
/// loop never holds up the run's own result.
fn evolve(host: &Host, run: &RunRecord) -> EngineResult<()> {
    let settings = host.settings();
    let proposal = host.read(|state| {
        state.retro.events.iter().find_map(|event| match event {
            RetroEvent::Candidate(change) => Some(change.clone()),
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
            format!("Recipe candidate {candidate} is queued for a replay test.")
        }
        Action::AskPromotion { candidate, .. } => {
            format!(
                "Recipe candidate {candidate} passed its replays and waits for you in /recipes."
            )
        }
        Action::Promoted { recipe, branch, .. } => {
            format!("{recipe} was promoted; its commit waits on {branch}.")
        }
        Action::RolledBack { recipe, to, reason } => {
            format!("{recipe} was rolled back to v{to}: {reason}.")
        }
        Action::Recorded { recipe, branch } => {
            format!("This run's workflow was saved as {recipe}, on {branch} for you.")
        }
        Action::Tell { text } => text.clone(),
    };
    let level = match action {
        Action::RolledBack { .. } => NoteLevel::Warn,
        _ => NoteLevel::Info,
    };
    Note { level, text }
}

/// Every `evolution.propose_every` runs that reached the retro; 0 means only when you ask. At
/// stage `record` the loop only saves what worked, so the retro isn't asked for a change.
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

async fn propose(host: &Host, facts: &[facts::CardFacts], settings: &Settings) -> EngineResult<()> {
    let worker = roles::candidates(settings, RoleName::Retro)
        .into_iter()
        .next()
        .unwrap_or_else(|| settings.default_agent.clone());
    let mut ask = Ask::new(
        None,
        RoleName::Retro,
        worker,
        prompts::divide::RETRO,
        facts::digest(facts, settings),
    );
    ask.access = Access::NoTools;
    let Some(change) = host.ask::<Retro>(ask).await?.output.change else {
        return Ok(());
    };
    let kind = if settings.evolution.evolvable.contains(&change.setting) {
        RunEventKind::Retro(RetroEvent::Candidate(change))
    } else {
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: format!(
                "The retro proposed changing {}, which isn't on the evolvable list, so it was dropped.",
                change.setting
            ),
        })
    };
    host.record(None, None, kind)?;
    Ok(())
}

fn phase(host: &Host, status: PhaseStatus, summary: Option<&str>) -> EngineResult<()> {
    let change = PhaseChange {
        phase: RunPhase::Retro,
        status,
        summary: summary.map(str::to_owned),
    };
    host.record(None, None, RunEventKind::Phase(change))?;
    Ok(())
}
