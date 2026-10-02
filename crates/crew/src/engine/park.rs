//! When the run parks: nothing can happen until something outside changes. Never because time
//! passed, so a parked run never expires.

use time::OffsetDateTime;

use argonauts_core::events::run::ParkReason;
use argonauts_core::names::CheckpointId;

use crate::state::RunState;
use crate::state::checkpoints::CheckpointRecord;

/// Why the run should park now, if it should: a checkpoint needs you and no session runs, you
/// paused, every candidate is out (`limits.all_out`), or the remaining cards wait for a vendor.
pub fn reason(
    state: &RunState,
    live_sessions: usize,
    all_out: Option<ParkReason>,
) -> Option<ParkReason> {
    if state.run.finished.is_some() {
        return None;
    }
    if state.run.paused {
        return Some(ParkReason::Paused);
    }
    if live_sessions > 0 {
        return None;
    }
    if state.checkpoints.open().any(needs_you) {
        return Some(ParkReason::Checkpoint);
    }
    if all_out.is_some() {
        return all_out;
    }
    let wait = state
        .run
        .vendor_waits
        .values()
        .min_by_key(|wait| wait.until.unwrap_or(i64::MAX))?;
    Some(ParkReason::WaitingForVendor {
        vendor: wait.vendor.clone(),
        until: wait
            .until
            .and_then(|until| OffsetDateTime::from_unix_timestamp(until).ok()),
    })
}

/// The checkpoints a parked run waits on.
pub fn waiting_on(state: &RunState) -> Vec<CheckpointId> {
    state
        .checkpoints
        .open()
        .filter(|record| needs_you(record))
        .map(|record| record.opened.id.clone())
        .collect()
}

/// Opened and waiting for you: the policy said so when it opened it, or has spoken since and
/// left it open (it asked you, or escalated). One the policy is still deciding doesn't park the
/// run.
fn needs_you(record: &CheckpointRecord) -> bool {
    record.opened.needs_you || record.checks.iter().any(|check| !check.shadow)
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::CardId;
    use argonauts_core::events::checkpoint::CheckpointKind;
    use argonauts_core::names::VendorId;

    use crate::state::run::VendorWait;
    use crate::state::tests::open_checkpoint;

    fn with_checkpoint(needs_you: bool) -> RunState {
        let mut state = RunState::default();
        let record = open_checkpoint(1, CheckpointKind::Goal, needs_you);
        state
            .checkpoints
            .by_id
            .insert(record.opened.id.clone(), record);
        state
    }

    #[test]
    fn a_checkpoint_parks_only_when_it_needs_you_and_nothing_runs() {
        let state = with_checkpoint(true);
        assert_eq!(reason(&state, 0, None), Some(ParkReason::Checkpoint));
        assert_eq!(reason(&state, 1, None), None, "a session still runs");
        assert_eq!(waiting_on(&state).len(), 1);
        let deciding = with_checkpoint(false);
        assert_eq!(
            reason(&deciding, 0, None),
            None,
            "the policy is still deciding"
        );
    }

    #[test]
    fn pause_all_out_and_vendor_waits_park_in_that_order() {
        let mut state = RunState::default();
        assert_eq!(reason(&state, 0, None), None);
        let out = ParkReason::AllVendorsOut { until: None };
        assert_eq!(reason(&state, 0, Some(out.clone())), Some(out.clone()));
        assert_eq!(reason(&state, 2, Some(out)), None);

        let vendor = VendorId("codex".to_owned());
        state.run.vendor_waits.insert(
            CardId::numbered(2),
            VendorWait {
                vendor: vendor.clone(),
                until: Some(3_600),
            },
        );
        match reason(&state, 0, None) {
            Some(ParkReason::WaitingForVendor { vendor: v, until }) => {
                assert_eq!(v, vendor);
                assert_eq!(until.map(OffsetDateTime::unix_timestamp), Some(3_600));
            }
            other => panic!("{other:?}"),
        }

        state.run.paused = true;
        assert_eq!(reason(&state, 3, None), Some(ParkReason::Paused));
        state.run.finished = Some(argonauts_core::events::run::RunOutcome::Stopped);
        assert_eq!(reason(&state, 0, None), None, "a finished run never parks");
    }
}
