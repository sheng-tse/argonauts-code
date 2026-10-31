//! Readings, recorded as `GoalEvent::Read` under labels A, B, C.

use futures::future::join_all;

use argonauts_core::agents::session_runner::RunnerKind;
use argonauts_core::base::log::NoteLevel;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::goal::GoalEvent;
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::workflows::steps::understand::{Read, ReadInput, Reading};
use argonauts_core::workflows::workflow::{Count, StepModels};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::prompts;
use crate::roles;
use crate::workflow::StepCall;

use super::{context, round};

/// The labeled readings, recorded ones reused on replay. `guidance` is your send-back note.
pub async fn read(
    host: &Host,
    call: &StepCall,
    guidance: Option<&str>,
) -> EngineResult<Vec<(String, ModelRef, Reading)>> {
    let (request, mut readings, mode) = host.read(|state| {
        (
            round::request(state),
            round::readings(state),
            round::mode(state),
        )
    });
    let request = request?;
    let workers = readers(host, call, mode);
    let pending: Vec<(u32, ModelRef)> = workers
        .into_iter()
        .enumerate()
        .filter(|(_, worker)| !readings.iter().any(|(_, done, _)| done == worker))
        .map(|(slot, worker)| (u32::try_from(slot).unwrap_or(u32::MAX), worker))
        .collect();
    if pending.is_empty() {
        return Ok(readings);
    }

    let (notes, decisions) = context::read(
        &host.repo().base_worktree,
        &crate::divide::repo_state_dir(host),
        &request,
    );
    let input = ReadInput {
        request,
        notes,
        decisions,
    };
    let asks = pending.iter().map(|(slot, worker)| {
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Reader,
            worker.clone(),
            prompts::understand::READ,
            input.clone(),
        );
        ask.slot = *slot;
        ask.blind = true;
        ask.parts = round::guidance_part(guidance);
        host.ask::<Read>(ask)
    });
    let answers = join_all(asks).await;

    let mut fatal = None;
    for ((_, worker), answer) in pending.into_iter().zip(answers) {
        match answer {
            Ok(answered) => {
                let label = label(readings.len());
                host.record(
                    None,
                    Some(&answered.session),
                    RunEventKind::Goal(GoalEvent::Read {
                        label: label.clone(),
                        worker: worker.clone(),
                        reading: answered.output.clone(),
                    }),
                )?;
                readings.push((label, worker, answered.output));
            }
            Err(error) if dropped(&error) => round::note(
                host,
                NoteLevel::Warn,
                format!(
                    "Dropped {}'s reading: {error}",
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
        None => Ok(readings),
    }
}

/// Who reads the request back: the step's named workers, else `understand.readers` of the reader
/// role's candidates (every crew member by default). In Captain, the main agent alone. Reading
/// reads the repository, so endpoints are left out.
pub fn readers(host: &Host, call: &StepCall, mode: RunMode) -> Vec<ModelRef> {
    let settings = host.settings();
    if mode == RunMode::Captain {
        return vec![settings.crew.main.clone()];
    }
    let named = match &call.step.models {
        StepModels::Named(workers) if !workers.is_empty() => workers.clone(),
        _ => roles::candidates(settings, RoleName::Reader),
    };
    let limit = match call.step.count {
        Some(Count::Fixed(count)) => Some(count),
        _ => settings.understand.readers,
    };
    let directory = &host.services().workers;
    let mut chosen: Vec<ModelRef> = Vec::new();
    for worker in named {
        let endpoint = directory
            .get(&worker)
            .is_ok_and(|found| found.kind() == RunnerKind::Endpoint);
        if !endpoint && !chosen.contains(&worker) {
            chosen.push(worker);
        }
    }
    if let Some(limit) = limit {
        chosen.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    chosen
}

/// A failed session, one with nobody to fall back to, or a bad answer drops that one
/// reading; anything else (a stop, the log) ends the step.
pub(crate) fn dropped(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Runner(_)
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
