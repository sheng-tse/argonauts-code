//! Restatements, recorded as `Goal::Restated` under labels A, B, C.

use futures::future::join_all;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::run::RunMode;
use argonauts_core::log::NoteLevel;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::goal::{Restate, RestateInput, Restatement};
use argonauts_core::worker::WorkerKind;
use argonauts_core::workflow::{Count, StepWorkers};

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts;
use crate::roles;
use crate::workflow::StepCall;

use super::{context, round};

/// The labeled restatements, recorded ones reused on replay. `guidance` is your send-back note.
pub async fn restate(
    host: &Host,
    call: &StepCall,
    guidance: Option<&str>,
) -> EngineResult<Vec<(String, WorkerRef, Restatement)>> {
    let (request, mut restated, mode) = host.read(|state| {
        (
            round::request(state),
            round::restatements(state),
            round::mode(state),
        )
    });
    let request = request?;
    let workers = restaters(host, call, mode);
    let pending: Vec<(u32, WorkerRef)> = workers
        .into_iter()
        .enumerate()
        .filter(|(_, worker)| !restated.iter().any(|(_, done, _)| done == worker))
        .map(|(slot, worker)| (u32::try_from(slot).unwrap_or(u32::MAX), worker))
        .collect();
    if pending.is_empty() {
        return Ok(restated);
    }

    let (notes, decisions) = context::read(&host.repo().base_worktree, &request);
    let input = RestateInput {
        request,
        notes,
        decisions,
    };
    let asks = pending.iter().map(|(slot, worker)| {
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Restater,
            worker.clone(),
            prompts::goal::RESTATE,
            input.clone(),
        );
        ask.slot = *slot;
        ask.blind = true;
        ask.parts = round::guidance_part(guidance);
        host.ask::<Restate>(ask)
    });
    let answers = join_all(asks).await;

    let mut fatal = None;
    for ((_, worker), answer) in pending.into_iter().zip(answers) {
        match answer {
            Ok(answered) => {
                let label = label(restated.len());
                host.record(
                    None,
                    Some(&answered.session),
                    RunEventKind::Goal(GoalEvent::Restated {
                        label: label.clone(),
                        worker: worker.clone(),
                        restatement: answered.output.clone(),
                    }),
                )?;
                restated.push((label, worker, answered.output));
            }
            Err(error) if dropped(&error) => round::note(
                host,
                NoteLevel::Warn,
                format!(
                    "Dropped {}'s restatement: {error}",
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
        None => Ok(restated),
    }
}

/// Who restates: the step's named workers, else `understand.readers` of the restater role's
/// candidates (every crew member by default). In solo, the default agent alone. Restating reads
/// the repository, so endpoints are left out.
pub fn restaters(host: &Host, call: &StepCall, mode: RunMode) -> Vec<WorkerRef> {
    let settings = host.settings();
    if mode == RunMode::Solo {
        return vec![settings.main_agent.clone()];
    }
    let named = match &call.step.workers {
        StepWorkers::Named(workers) if !workers.is_empty() => workers.clone(),
        _ => roles::candidates(settings, RoleName::Restater),
    };
    let limit = match call.step.count {
        Some(Count::Fixed(count)) => Some(count),
        _ => settings.understand.readers,
    };
    let directory = &host.services().workers;
    let mut chosen: Vec<WorkerRef> = Vec::new();
    for worker in named {
        let endpoint = directory
            .get(&worker)
            .is_ok_and(|found| found.kind() == WorkerKind::Endpoint);
        if !endpoint && !chosen.contains(&worker) {
            chosen.push(worker);
        }
    }
    if let Some(limit) = limit {
        chosen.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    chosen
}

/// A failed session, one no agent could take over, or a bad answer drops that one
/// restatement; anything else (a stop, the log) ends the step.
pub(crate) fn dropped(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Worker(_)
            | EngineError::Step(_)
            | EngineError::Refused(_)
            | EngineError::Stalled(_)
    )
}

/// "A", "B", ... "Z", then "AA".
pub fn label(index: usize) -> String {
    let mut n = index + 1;
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(char::from(b'A' + (n % 26) as u8));
        n /= 26;
    }
    letters.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_run_like_spreadsheet_columns() {
        let labels: Vec<String> = [0, 1, 2, 25, 26, 27, 51, 52, 701, 702]
            .into_iter()
            .map(label)
            .collect();
        assert_eq!(
            labels,
            ["A", "B", "C", "Z", "AA", "AB", "AZ", "BA", "ZZ", "AAA"]
        );
    }
}
