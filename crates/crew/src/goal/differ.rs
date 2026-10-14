//! Differs: `understand.comparers` workers, from different families than each other where possible,
//! no-tools preferred. One listing is enough to go on.

use futures::future::join_all;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::log::NoteLevel;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::goal::{Differ, DifferInput, DifferOutput, LabeledRestatement};
use argonauts_core::worker::{Access, WorkerKind};
use argonauts_core::workflow::{Count, StepWorkers};

use crate::engine::host::{Ask, Host};
use crate::error::EngineResult;
use crate::prompts;
use crate::roles;
use crate::workflow::StepCall;

use super::restate::dropped;
use super::round;

pub async fn compare(host: &Host, call: &StepCall) -> EngineResult<Vec<(WorkerRef, DifferOutput)>> {
    let (request, restated, compared, guidance) = host.read(|state| {
        (
            round::request(state),
            round::restatements(state),
            state
                .goal
                .compared
                .iter()
                .map(|(worker, _)| worker.clone())
                .collect::<Vec<_>>(),
            round::guidance(state),
        )
    });
    let input = DifferInput {
        request: request?,
        restatements: restated
            .into_iter()
            .map(|(label, _, restatement)| LabeledRestatement { label, restatement })
            .collect(),
    };
    let candidates = candidates(host, call);
    let count = match call.step.count {
        Some(Count::Fixed(count)) => count,
        _ => host.settings().understand.comparers,
    };
    let picked = pick(&candidates, usize::try_from(count.max(1)).unwrap_or(1));
    if picked.is_empty() {
        round::note(
            host,
            NoteLevel::Warn,
            "No worker can compare the restatements, so the goal waits for you.".to_owned(),
        )?;
        return Ok(Vec::new());
    }

    let asks = picked.iter().enumerate().map(|(slot, worker)| {
        let no_tools = candidates
            .iter()
            .any(|(candidate, _, no_tools)| candidate == worker && *no_tools);
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Differ,
            worker.clone(),
            prompts::goal::DIFFER,
            input.clone(),
        );
        ask.access = if no_tools {
            Access::NoTools
        } else {
            Access::ReadOnly
        };
        ask.slot = u32::try_from(slot).unwrap_or(u32::MAX);
        ask.blind = true;
        ask.parts = round::guidance_part(guidance.as_deref());
        host.ask::<Differ>(ask)
    });
    let answers = join_all(asks).await;

    let mut listings = Vec::new();
    let mut fatal = None;
    for (worker, answer) in picked.into_iter().zip(answers) {
        match answer {
            Ok(answered) => {
                // A re-entered step gets the recorded answer back from the host; record it once.
                if !compared.contains(&worker) {
                    host.record(
                        None,
                        Some(&answered.session),
                        RunEventKind::Goal(GoalEvent::Compared {
                            worker: worker.clone(),
                            differences: answered.output.differences.clone(),
                        }),
                    )?;
                }
                listings.push((worker, answered.output));
            }
            Err(error) if dropped(&error) => round::note(
                host,
                NoteLevel::Warn,
                format!(
                    "Dropped {}'s comparison: {error}",
                    crate::roles::display_name(&worker)
                ),
            )?,
            Err(error) => {
                fatal.get_or_insert(error);
            }
        }
    }
    match fatal {
        Some(error) => Err(error),
        None => Ok(listings),
    }
}

/// `(worker, family, no_tools)` for everyone who may compare: the step's named workers, else
/// the differ role's candidates, else every available worker.
pub fn candidates(host: &Host, call: &StepCall) -> Vec<(WorkerRef, String, bool)> {
    let mut workers = match &call.step.workers {
        StepWorkers::Named(workers) if !workers.is_empty() => workers.clone(),
        _ => roles::candidates(host.settings(), RoleName::Differ),
    };
    if workers.is_empty() {
        workers = host.services().workers.available();
    }
    let directory = &host.services().workers;
    workers
        .into_iter()
        .map(|worker| {
            let no_tools = directory
                .get(&worker)
                .is_ok_and(|found| found.kind() == WorkerKind::Endpoint);
            let family = roles::family(host, &worker);
            (worker, family, no_tools)
        })
        .collect()
}

/// Picks the differs: different families than each other where possible, no-tools first.
pub fn pick(candidates: &[(WorkerRef, String, bool)], count: usize) -> Vec<WorkerRef> {
    let mut ordered: Vec<&(WorkerRef, String, bool)> = candidates.iter().collect();
    ordered.sort_by_key(|(_, _, no_tools)| !*no_tools);

    let mut picked: Vec<WorkerRef> = Vec::new();
    let mut families: Vec<&str> = Vec::new();
    for (worker, family, _) in &ordered {
        if picked.len() == count {
            return picked;
        }
        if !families.contains(&family.as_str()) && !picked.contains(worker) {
            families.push(family);
            picked.push(worker.clone());
        }
    }
    for (worker, _, _) in &ordered {
        if picked.len() == count {
            break;
        }
        if !picked.contains(worker) {
            picked.push(worker.clone());
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(worker: &str, family: &str, no_tools: bool) -> (WorkerRef, String, bool) {
        (worker.parse().unwrap(), family.to_owned(), no_tools)
    }

    fn names(picked: Vec<WorkerRef>) -> Vec<String> {
        picked
            .into_iter()
            .map(|worker| worker.to_string())
            .collect()
    }

    #[test]
    fn differs_come_from_different_families_no_tools_first() {
        let candidates = [
            candidate("claude-code/opus", "claude", false),
            candidate("claude-code/sonnet", "claude", false),
            candidate("codex", "gpt", false),
            candidate("openrouter/glm", "glm", true),
        ];
        assert_eq!(
            names(pick(&candidates, 2)),
            ["openrouter/glm", "claude-code/opus"]
        );
        assert_eq!(
            names(pick(&candidates, 3)),
            ["openrouter/glm", "claude-code/opus", "codex"]
        );
    }

    #[test]
    fn one_family_still_fills_the_count() {
        let candidates = [
            candidate("claude-code/opus", "claude", false),
            candidate("claude-code/sonnet", "claude", false),
            candidate("claude-code/opus", "claude", false),
        ];
        assert_eq!(
            names(pick(&candidates, 2)),
            ["claude-code/opus", "claude-code/sonnet"]
        );
        assert_eq!(names(pick(&candidates, 5)).len(), 2);
        assert!(pick(&[], 2).is_empty());
    }
}
