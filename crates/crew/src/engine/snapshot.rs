//! `Run::Snapshot` every `run_host.snapshot_every` events, so a long run stays cheap to
//! resume and reattach.

use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::api::StatusSummary;
use argonauts_core::runs::events::run::RunSnapshot;

use crate::engine::error::EngineResult;
use crate::state::RunState;

/// Whether a snapshot is due now: `every` events since the last one. 0 never snapshots.
pub fn due(state: &RunState, every: u64) -> bool {
    let Some(seq) = state.seq else {
        return false;
    };
    let since = match state.run.last_snapshot {
        Some(last) => seq.saturating_sub(last),
        None => seq + 1,
    };
    every > 0 && since >= every
}

/// The engine's versioned state, the user's status summary, the open approvals, and the
/// running sessions.
pub fn build(
    state: &RunState,
    summary: StatusSummary,
    running: Vec<SessionKey>,
) -> EngineResult<RunSnapshot> {
    Ok(RunSnapshot {
        upto: state.seq.unwrap_or(0),
        engine: state.to_snapshot()?,
        summary,
        open_approvals: state
            .approvals
            .open()
            .map(|record| record.opened.clone())
            .collect(),
        running,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_is_due_every_n_events() {
        let mut state = RunState::default();
        assert!(!due(&state, 3), "nothing recorded yet");
        state.seq = Some(1);
        assert!(!due(&state, 3));
        state.seq = Some(2);
        assert!(due(&state, 3), "three events: seq 0, 1, and 2");
        state.run.last_snapshot = Some(2);
        assert!(!due(&state, 3));
        state.seq = Some(5);
        assert!(due(&state, 3));
        assert!(!due(&state, 0), "0 turns snapshots off");
    }
}
