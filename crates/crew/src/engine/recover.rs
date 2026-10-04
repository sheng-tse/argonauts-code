//! Waking a run: after a crash, after you answered a parked run's checkpoint, when a vendor is
//! back, when an `ask_timeout` handoff runs out, or when you reopen a stopped run.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{CheckpointKind, CheckpointPacket};
use argonauts_core::events::run::{
    ParkReason, ResumeCause, ResumedSession, RunLifecycle, RunOutcome, RunResumed,
};
use argonauts_core::limits::RecoveryAction;
use argonauts_core::names::{CheckpointId, VendorId};
use argonauts_core::worker::Access;
use argonauts_core::{CardId, Note, NoteLevel};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::state::RunState;
use crate::{card, checkpoints, handoff};

/// Records `Run::Resumed` with what changed (settings hash, harness versions, main moved) and
/// why the run woke, then applies the cause: `VendorBack` retries the vendor or sends one test
/// card; `AskTimeout` withdraws that handoff checkpoint and hands off to the next valid
/// candidate within the cost cap, or reopens it when there is none. Sessions that were running
/// are resumed when their steps are re-entered, never restarted.
pub async fn recover(host: &Host) -> EngineResult<()> {
    let now = host.now().unix_timestamp();
    let ask_timeout = host
        .settings()
        .handoff
        .ask_timeout
        .parse()
        .ok()
        .flatten()
        .map(|timeout| i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX));
    let (cause, timed_out) = host.read(|state| cause(state, now, ask_timeout));
    let Some(mut cause) = cause else {
        return Ok(());
    };
    let back = vendors_back(host);
    // The app wakes a run parked on its merge card once an original that card's debt waits
    // for is back, which the log alone can't tell from an answer.
    if cause == ResumeCause::Answer && host.read(|state| merge_waits_on(state, &back)) {
        cause = ResumeCause::VendorBack;
    }
    // A crash, whether or not a pause was under way: the log ends before the run parked or
    // finished. A stop committed its sessions' work already.
    let crashed = host.read(|state| state.run.parked.is_none() && state.run.finished.is_none());
    let (started, sessions) = host.read(|state| {
        let sessions: Vec<ResumedSession> = state
            .sessions
            .in_order()
            .into_iter()
            .filter(|session| session.resumable())
            .map(|session| ResumedSession {
                session: session.key.clone(),
                card: session.card.clone(),
                vendor_session: session.vendor_session.clone(),
                resumed: session.vendor_session.is_some(),
            })
            .collect();
        (state.run.started.clone(), sessions)
    });

    // A crash can leave a building session's work uncommitted; commit it before anything
    // else touches the worktree.
    if crashed {
        for session in &sessions {
            let builds = host.read(|state| {
                state.sessions.get(&session.session).is_some_and(|record| {
                    matches!(record.started.access, Access::Build | Access::Spike)
                })
            });
            if let (true, Some(card)) = (builds, &session.card) {
                card::worktree::commit_after_end(host, card, &session.session).await?;
            }
        }
    }

    let main_moved_by = match &started {
        Some(started) => {
            main_moved_by(host, started.base_branch.as_deref(), &started.base_commit).await?
        }
        None => None,
    };
    let resumed = RunResumed {
        cause,
        settings_changed: started
            .as_ref()
            .is_some_and(|started| started.settings_sha256 != host.shared().settings_sha256),
        // The supervisor probes harness versions; the engine only sees what the run started with.
        harness_versions_changed: Vec::new(),
        main_moved_by,
        sessions,
    };
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Resumed(resumed)),
    )?;

    match cause {
        ResumeCause::VendorBack => {
            for vendor in back {
                handoff::ask::vendor_back(host, &vendor)?;
                handoff::on_return::vendor_back(host, &vendor).await?;
            }
        }
        ResumeCause::AskTimeout => {
            for (id, card) in timed_out {
                ask_timed_out(host, &id, card.as_ref()).await?;
            }
        }
        ResumeCause::User | ResumeCause::Answer | ResumeCause::Crash => {}
    }
    Ok(())
}

/// Why the run woke, from how its log ends, and the handoff checkpoints whose ask timed out.
/// `None` for a new run, or one that finished other than stopped.
pub fn cause(
    state: &RunState,
    now: i64,
    ask_timeout: Option<i64>,
) -> (Option<ResumeCause>, Vec<(CheckpointId, Option<CardId>)>) {
    match state.run.finished {
        // Only a stopped run you reopened gets an engine that goes on.
        Some(RunOutcome::Stopped) => return (Some(ResumeCause::User), Vec::new()),
        Some(_) => return (None, Vec::new()),
        None => {}
    }
    let timed_out: Vec<_> = state
        .checkpoints
        .open()
        .filter(|record| record.opened.kind == CheckpointKind::Handoff)
        .filter(|record| {
            let opened = state.run.opened_at.get(&record.opened.id).copied();
            matches!((opened, ask_timeout), (Some(opened), Some(timeout)) if now - opened >= timeout)
        })
        .map(|record| (record.opened.id.clone(), record.opened.card.clone()))
        .collect();
    let cause = match &state.run.parked {
        Some(ParkReason::Paused) => ResumeCause::User,
        Some(ParkReason::Checkpoint) if !timed_out.is_empty() => ResumeCause::AskTimeout,
        Some(ParkReason::Checkpoint) => ResumeCause::Answer,
        Some(
            ParkReason::AllVendorsOut { .. }
            | ParkReason::WaitingForVendor { .. }
            | ParkReason::NoStandIn { .. },
        ) => ResumeCause::VendorBack,
        // Only the supervisor's `RunStarted`: a new run, nothing to recover.
        None if state.seq.unwrap_or(0) == 0 => return (None, Vec::new()),
        // You paused it, and the engine stopped before it parked.
        None if state.run.paused || state.run.pause_requested.is_some() => ResumeCause::User,
        None => ResumeCause::Crash,
    };
    (Some(cause), timed_out)
}

/// The run waits on an open merge card whose debt needs one of `back`.
fn merge_waits_on(state: &RunState, back: &[VendorId]) -> bool {
    let Some(ParkReason::Checkpoint) = &state.run.parked else {
        return false;
    };
    let Some(open) = state.checkpoints.open_for(CheckpointKind::Merge, None) else {
        return false;
    };
    let CheckpointPacket::Merge(packet) = &open.opened.packet else {
        return false;
    };
    packet
        .rows
        .iter()
        .flat_map(|row| &row.debt)
        .any(|debt| back.contains(&debt.needs.vendor()))
}

/// Vendors the log saw out that the boards now call healthy.
fn vendors_back(host: &Host) -> Vec<VendorId> {
    let mut out: Vec<VendorId> = host.read(|state| {
        let waiting = state
            .run
            .vendor_waits
            .values()
            .map(|wait| wait.vendor.clone());
        let out = state
            .run
            .health
            .keys()
            .chain(state.run.quota.keys())
            .filter(|vendor| state.run.vendor_out(vendor))
            .cloned();
        waiting.chain(out).collect()
    });
    out.sort();
    out.dedup();
    let services = host.services();
    let now = host.now();
    out.retain(|vendor| {
        let health = services.health.state(vendor);
        let quota_ok = services
            .quota
            .get(vendor)
            .is_none_or(|reading| !handoff::rules::quota_out(&reading, now));
        matches!(
            health.state,
            argonauts_core::limits::HealthState::Healthy
                | argonauts_core::limits::HealthState::Testing
        ) && quota_ok
    });
    out
}

/// Nobody answered the handoff card in time: withdraw it and hand the card to the first
/// candidate that needs no confirmation, or open the card again when there is none.
async fn ask_timed_out(host: &Host, id: &CheckpointId, card: Option<&CardId>) -> EngineResult<()> {
    let plan = host.read(|state| {
        state
            .checkpoints
            .by_id
            .get(id)
            .and_then(|record| match &record.opened.packet {
                CheckpointPacket::Handoff(plan) => Some(plan.clone()),
                _ => None,
            })
    });
    let (Some(plan), Some(card)) = (plan, card) else {
        return Ok(());
    };
    let Some(next) = plan
        .candidates
        .iter()
        .find(|candidate| !candidate.needs_confirm)
    else {
        // Nobody fits within the cost cap, so the card stays open for you.
        return Ok(());
    };
    checkpoints::withdraw(
        host,
        id,
        "nobody answered in the time /handoff allows, so the next agent in line took it",
    )?;
    let spec = host.read(|state| state.cards.get(card).map(|record| record.spec.clone()));
    let Some(spec) = spec else {
        return Ok(());
    };
    let events = card::support::card_events(host, card)?;
    let packet = host.services().handoff.packet(&spec, &events);
    let action = RecoveryAction::HandOff {
        to: next.worker.clone(),
        packet: Box::new(packet),
        checkpoint: None,
    };
    handoff::apply(host, card, action).await
}

/// How many commits the base branch gained since the run started, if any. When git can't tell,
/// a note says why and the count is left out.
async fn main_moved_by(host: &Host, branch: Option<&str>, base: &str) -> EngineResult<Option<u32>> {
    let Some(branch) = branch else {
        return Ok(None);
    };
    let root = &host.repo().root;
    let git = host.git();
    let counted = async {
        let head = git.resolve(root, branch).await?;
        if head == base {
            return Ok(0);
        }
        let moved = git.log_messages(root, &format!("{base}..{head}")).await?;
        Ok::<_, crate::git::GitError>(moved.len())
    };
    match counted.await {
        Ok(0) => Ok(None),
        Ok(moved) => Ok(Some(u32::try_from(moved).unwrap_or(u32::MAX))),
        Err(err) => {
            let note = Note {
                level: NoteLevel::Warn,
                text: format!("couldn't tell how far {branch} moved since the run started: {err}"),
            };
            host.record(None, None, RunEventKind::Note(note))?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::events::checkpoint::CheckpointKind;

    use crate::state::tests::open_checkpoint;

    #[test]
    fn the_cause_follows_how_the_log_ends() {
        let mut state = RunState::default();
        assert_eq!(cause(&state, 0, None).0, None, "an empty log");
        state.seq = Some(0);
        assert_eq!(cause(&state, 0, None).0, None, "only RunStarted");
        state.seq = Some(9);
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::Crash));
        state.run.pause_requested = Some(true);
        assert_eq!(
            cause(&state, 0, None).0,
            Some(ResumeCause::User),
            "a pause under way"
        );
        state.run.pause_requested = None;
        state.run.parked = Some(ParkReason::Paused);
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::User));
        state.run.parked = Some(ParkReason::AllVendorsOut { until: None });
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::VendorBack));
        state.run.parked = Some(ParkReason::Checkpoint);
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::Answer));

        let handoff = open_checkpoint(4, CheckpointKind::Handoff, true);
        let id = handoff.opened.id.clone();
        state.checkpoints.by_id.insert(id.clone(), handoff);
        state.run.opened_at.insert(id.clone(), 100);
        assert_eq!(cause(&state, 150, Some(60)).0, Some(ResumeCause::Answer));
        let (woke, timed_out) = cause(&state, 160, Some(60));
        assert_eq!(woke, Some(ResumeCause::AskTimeout));
        assert_eq!(timed_out, vec![(id, None)]);
        assert_eq!(cause(&state, 10_000, None).0, Some(ResumeCause::Answer));

        state.run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        assert_eq!(cause(&state, 0, None).0, None);
    }

    #[test]
    fn a_parked_merge_card_waits_on_the_originals_its_debt_needs() {
        use argonauts_core::events::card::CardKind;
        use argonauts_core::events::checkpoint::{MergePacket, MergeRow};
        use argonauts_core::handoff::QualityDebt;
        use argonauts_core::{DiffStat, Spend};

        let worker = |name: &str| name.parse().expect("a worker");
        let row = MergeRow {
            card: CardId::numbered(1),
            title: "Redis store".to_owned(),
            kind: CardKind::Build,
            stat: DiffStat::default(),
            owner: Some(worker("codex/gpt-5.6")),
            gates_passed: true,
            review: None,
            debt: vec![QualityDebt {
                since: 1,
                by: worker("codex/gpt-5.6"),
                needs: worker("claude-code/opus"),
            }],
        };
        let mut record = open_checkpoint(2, CheckpointKind::Merge, true);
        record.opened.packet = CheckpointPacket::Merge(MergePacket {
            target: Some("main".to_owned()),
            branch: "argonauts/run-01/integration".to_owned(),
            rows: vec![row],
            suite_passed_on_latest_main: true,
            gates: Vec::new(),
            combined: DiffStat::default(),
            conductor: DiffStat::default(),
            diff_path: std::path::PathBuf::new(),
            tree_sha: String::new(),
            spent: Spend::default(),
            limit: None,
            partial: false,
            in_the_way: Vec::new(),
            edited: Vec::new(),
            refused: None,
        });
        let mut state = RunState::default();
        state
            .checkpoints
            .by_id
            .insert(record.opened.id.clone(), record);
        let claude = VendorId("claude-code".to_owned());
        let codex = VendorId("codex".to_owned());

        assert!(
            !merge_waits_on(&state, std::slice::from_ref(&claude)),
            "not parked"
        );
        state.run.parked = Some(ParkReason::Checkpoint);
        assert!(merge_waits_on(&state, std::slice::from_ref(&claude)));
        assert!(!merge_waits_on(&state, &[codex]));
        assert!(!merge_waits_on(&state, &[]));
    }
}
