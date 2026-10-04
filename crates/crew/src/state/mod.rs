//! The run's state, folded from its event log. The fold is pure: the same log always gives the
//! same state, which is how a recovered run picks up exactly where it stopped. Each part folds
//! its own events and is owned by the module that records them.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use argonauts_core::events::run::{RunLifecycle, RunSnapshot};
use argonauts_core::events::{RunEvent, RunEventKind};

use argonauts_core::events::session::{EndReason, SessionEvent};

use crate::error::{EngineError, EngineResult};

pub mod blackboard;
pub mod cards;
pub mod checkpoints;
pub mod divide;
pub mod goal;
pub mod ids;
pub mod integration;
pub mod plan;
pub mod retro;
pub mod run;
pub mod sessions;
pub mod workflow;

/// Bumped whenever the snapshot's shape changes. A snapshot of another version can't be
/// restored, so a run that has one needs its log replayed from the start.
pub const ENGINE_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    /// The last event folded in.
    pub seq: Option<u64>,
    pub run: run::RunInfo,
    pub ids: ids::Counters,
    pub sessions: sessions::Sessions,
    pub checkpoints: checkpoints::Checkpoints,
    pub workflows: workflow::Workflows,
    pub goal: goal::GoalState,
    pub plan: plan::PlanState,
    pub divide: divide::DivideState,
    pub cards: cards::Cards,
    pub integration: integration::IntegrationState,
    pub blackboard: blackboard::Board,
    pub retro: retro::RetroState,
}

impl RunState {
    /// Rebuilds the state from the history an engine starts with: the log from its start, or
    /// from its latest `Run::Snapshot`. Fails when it starts at a snapshot this engine can't
    /// restore, since the events before that snapshot aren't in it.
    pub fn rebuild(history: &[RunEvent]) -> EngineResult<Self> {
        let mut state = Self::default();
        for event in history {
            if let RunEventKind::Run(RunLifecycle::Snapshot(snapshot)) = &event.kind {
                state.restore(snapshot, event.seq)?;
            }
            state.apply(event);
        }
        Ok(state)
    }

    /// Folds a log read from its start. A snapshot in it that this engine can't restore is
    /// skipped, since the events before it are folded already.
    pub fn fold(events: &[RunEvent]) -> Self {
        let mut state = Self::default();
        for event in events {
            if let RunEventKind::Run(RunLifecycle::Snapshot(snapshot)) = &event.kind
                && state.behind(snapshot)
                && let Ok(restored) = Self::from_snapshot(&snapshot.engine)
            {
                state = restored;
            }
            state.apply(event);
        }
        state
    }

    /// Folds one event in. A snapshot the engine records live was built from this same state,
    /// so only a fold of a log restores one.
    pub fn apply(&mut self, event: &RunEvent) {
        self.seq = Some(event.seq);
        self.ids.observe(event);
        match &event.kind {
            RunEventKind::Run(e) => self.run.apply(e, event),
            RunEventKind::Phase(e) => self.run.apply_phase(e, event),
            RunEventKind::Goal(e) => self.goal.apply(e, event),
            RunEventKind::Plan(e) => self.plan.apply(e, event),
            RunEventKind::Divide(e) => self.divide.apply(e, event),
            RunEventKind::Card(e) => self.cards.apply(e, event),
            RunEventKind::Session(e) => {
                if let (SessionEvent::Started(_), Some(card)) = (e, &event.card_id) {
                    self.run.card_active(card);
                }
                self.sessions.apply(e, event);
                if let (SessionEvent::Ended(end), Some(key)) = (e, &event.session)
                    && end.reason == EndReason::Stopped
                    && self.run.stop_requested
                    && let Some(record) = self.sessions.by_key.get_mut(key)
                {
                    record.stopped_with_run = true;
                }
            }
            RunEventKind::Agent(e) => self.sessions.apply_agent(e, event),
            RunEventKind::Gate(e) => self.cards.apply_gate(e, event),
            RunEventKind::Review(e) => self.cards.apply_review(e, event),
            RunEventKind::Integration(e) => self.integration.apply(e, event),
            RunEventKind::Checkpoint(e) => {
                self.run.apply_checkpoint(e, event);
                self.checkpoints.apply(e, event);
            }
            RunEventKind::Handoff(e) => {
                self.run.apply_handoff(e, event);
                self.cards.apply_handoff(e, event);
            }
            RunEventKind::Vendor(e) => self.run.apply_vendor(e, event),
            RunEventKind::Budget(e) => self.run.apply_budget(e, event),
            RunEventKind::Blackboard(e) => self.blackboard.apply(e, event),
            RunEventKind::Retro(e) => self.retro.apply(e, event),
            RunEventKind::Note(_) => {}
            RunEventKind::Workflow(e) => self.workflows.apply(e, event),
        }
    }

    /// Replaces the state with the snapshot's when the state is behind it, as when a log starts
    /// at the snapshot. A state that already folded up to the snapshot keeps what it holds,
    /// which may be newer. A snapshot that can't be restored is harmless once the run's start
    /// is folded in, since the events before it already are too.
    fn restore(&mut self, snapshot: &RunSnapshot, seq: u64) -> EngineResult<()> {
        if !self.behind(snapshot) {
            return Ok(());
        }
        match Self::from_snapshot(&snapshot.engine) {
            Ok(state) => *self = state,
            Err(_) if self.run.started.is_some() => {}
            Err(err) => {
                return Err(EngineError::Refused(format!(
                    "the run's snapshot at event {seq} can't be restored ({err}); \
                     replay the run's log from its start"
                )));
            }
        }
        Ok(())
    }

    /// Whether the snapshot holds events this state hasn't folded yet.
    fn behind(&self, snapshot: &RunSnapshot) -> bool {
        self.seq.is_none_or(|folded| folded < snapshot.upto)
    }

    /// The versioned JSON stored in `RunSnapshot.engine`.
    pub fn to_snapshot(&self) -> EngineResult<Value> {
        let state = serde_json::to_value(self).map_err(EngineError::Json)?;
        Ok(json!({ "version": ENGINE_STATE_VERSION, "state": state }))
    }

    pub fn from_snapshot(value: &Value) -> EngineResult<Self> {
        let version = value.get("version").and_then(Value::as_u64);
        if version != Some(u64::from(ENGINE_STATE_VERSION)) {
            let found = version.map_or_else(|| "no version".to_owned(), |v| format!("version {v}"));
            return Err(EngineError::Refused(format!(
                "the snapshot has {found}, and this engine reads version {ENGINE_STATE_VERSION}"
            )));
        }
        let state = value
            .get("state")
            .ok_or_else(|| EngineError::Refused("the snapshot has no state".to_owned()))?;
        Ok(Self::deserialize(state)?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use time::OffsetDateTime;

    use argonauts_core::event::AgentEvent;
    use argonauts_core::events::run::{
        ParkReason, Party, ResumeCause, RunMode, RunOutcome, RunResumed, RunStarted,
    };
    use argonauts_core::events::session::{EndReason, SessionEnd, SessionStarted};
    use argonauts_core::events::vendor::{HandoffEvent, VendorEvent};
    use argonauts_core::limits::{HealthState, VendorHealth};
    use argonauts_core::names::{RoleName, SessionKey, VendorId, WorkerRef};
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::sessions::SessionLifetime;
    use argonauts_core::worker::{Access, WorkerKind};
    use argonauts_core::{CardId, RunId, Spend};

    use crate::state::sessions::SessionRecord;

    pub(crate) fn envelope(
        seq: u64,
        card: Option<&str>,
        session: Option<&str>,
        kind: RunEventKind,
    ) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seq as i64),
            run_id: RunId::new("run-01").unwrap(),
            card_id: card.map(|card| CardId::new(card).unwrap()),
            session: session.map(|key| SessionKey::parse(key).unwrap()),
            source: "conductor".to_owned(),
            kind,
        }
    }

    pub(crate) fn run_started(mode: RunMode) -> RunStarted {
        RunStarted {
            request: "add a flag".to_owned(),
            title: "Add a flag".to_owned(),
            mode,
            forced: false,
            preset: "peer-crew".to_owned(),
            estimate: None,
            triage: None,
            chat_turn: None,
            continues: None,
            repo_root: "/repo".into(),
            base_branch: Some("main".to_owned()),
            base_commit: "abc".to_owned(),
            uncommitted: Vec::new(),
            uncommitted_included: false,
            settings_sha256: "hash".to_owned(),
            settings: serde_json::Value::Null,
            overrides: Vec::new(),
            harness_versions: BTreeMap::new(),
            version: "0.0.1".to_owned(),
            front_end: None,
            instructions: Vec::new(),
        }
    }

    pub(crate) fn session_started(worker: &WorkerRef, role: RoleName) -> SessionStarted {
        SessionStarted {
            worker: worker.clone(),
            vendor: worker.vendor(),
            family: "fake".to_owned(),
            kind: WorkerKind::Harness,
            role,
            step: None,
            access: Access::ReadOnly,
            lifetime: SessionLifetime::Card,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: None,
        }
    }

    pub(crate) fn session_record(key: &str, worker: &WorkerRef, role: RoleName) -> SessionRecord {
        SessionRecord {
            key: SessionKey::parse(key).unwrap(),
            card: None,
            started: session_started(worker, role),
            since: 0,
            starts: 1,
            vendor_session: None,
            transcript: None,
            context_used: None,
            end: None,
            stopped_with_run: false,
        }
    }

    pub(crate) fn end(reason: EndReason, vendor: Option<&str>) -> SessionEnd {
        SessionEnd {
            vendor_session: vendor.map(str::to_owned),
            exit_code: Some(0),
            signal: None,
            duration_ms: 1,
            reason,
            spend: Spend::default(),
            tool_calls: 0,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        }
    }

    /// An open checkpoint as the checkpoint fold would hold it. Built through serde, so
    /// fields the fold adds with defaults don't break it.
    pub(crate) fn open_checkpoint(
        n: u32,
        kind: argonauts_core::events::checkpoint::CheckpointKind,
        needs_you: bool,
    ) -> crate::state::checkpoints::CheckpointRecord {
        use argonauts_core::events::checkpoint::{CheckpointOpened, CheckpointPacket, FreeText};
        let opened = CheckpointOpened {
            id: argonauts_core::names::CheckpointId::numbered(n),
            kind,
            title: "Checkpoint".to_owned(),
            card: None,
            packet: CheckpointPacket::RecipePromotion(serde_json::json!({})),
            evidence_sha256: "evidence".to_owned(),
            mode: ApprovalMode::Supervised,
            decide_with: argonauts_core::settings::approvals::DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you,
        };
        let record = serde_json::json!({
            "opened": opened,
            "since": 0,
            "checks": [],
            "decided": null,
            "withdrawn": null,
        });
        serde_json::from_value(record).expect("a checkpoint record")
    }

    fn lifecycle(seq: u64, event: RunLifecycle) -> RunEvent {
        envelope(seq, None, None, RunEventKind::Run(event))
    }

    #[test]
    fn the_run_fold_tracks_start_pause_stop_park_and_finish() {
        let mut state = RunState::default();
        state.apply(&lifecycle(
            0,
            RunLifecycle::Started(Box::new(run_started(RunMode::Crew))),
        ));
        assert_eq!(state.run.mode, Some(RunMode::Crew));
        state.apply(&lifecycle(
            1,
            RunLifecycle::PauseRequested {
                by: Party::User,
                now: false,
            },
        ));
        assert_eq!(state.run.pause_requested, Some(false));
        state.apply(&lifecycle(2, RunLifecycle::PauseCancelled));
        assert_eq!(state.run.pause_requested, None);
        state.apply(&lifecycle(
            3,
            RunLifecycle::Paused {
                sessions_stopped: 1,
                committed: Vec::new(),
            },
        ));
        assert!(state.run.paused);
        state.apply(&lifecycle(
            4,
            RunLifecycle::Parked {
                reason: ParkReason::Paused,
                waiting_on: Vec::new(),
            },
        ));
        assert_eq!(state.run.parked, Some(ParkReason::Paused));
        state.apply(&lifecycle(
            5,
            RunLifecycle::Resumed(RunResumed {
                cause: ResumeCause::User,
                settings_changed: false,
                harness_versions_changed: Vec::new(),
                main_moved_by: None,
                sessions: Vec::new(),
            }),
        ));
        assert!(!state.run.paused);
        assert_eq!(state.run.parked, None);
        assert_eq!(state.run.resumes, 1);
        state.apply(&lifecycle(
            6,
            RunLifecycle::ApprovalModeChanged {
                mode: ApprovalMode::Auto,
                by: Party::User,
            },
        ));
        assert_eq!(state.run.approval_mode, Some(ApprovalMode::Auto));
        state.apply(&lifecycle(
            7,
            RunLifecycle::StopRequested { by: Party::User },
        ));
        assert!(state.run.stop_requested);
        state.apply(&lifecycle(
            8,
            RunLifecycle::Finished {
                outcome: RunOutcome::Stopped,
                detail: None,
            },
        ));
        assert_eq!(state.run.finished, Some(RunOutcome::Stopped));
        assert_eq!(state.seq, Some(8));
    }

    #[test]
    fn the_session_fold_keeps_vendor_ids_ends_and_resumes() {
        let worker = WorkerRef::new("codex", None);
        let started = || {
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Owner,
            ))))
        };
        let mut state = RunState::default();
        state.apply(&envelope(3, Some("card-001"), Some("s-004"), started()));
        state.apply(&envelope(
            4,
            Some("card-001"),
            Some("s-004"),
            RunEventKind::Agent(AgentEvent::Init {
                session_id: Some("v-1".to_owned()),
                model: None,
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            }),
        ));
        state.apply(&envelope(
            5,
            Some("card-001"),
            Some("s-004"),
            RunEventKind::Agent(AgentEvent::ContextFill {
                used: 1200,
                size: None,
            }),
        ));
        let key = SessionKey::numbered(4);
        let record = state.sessions.get(&key).unwrap();
        assert_eq!(record.vendor_session.as_deref(), Some("v-1"));
        assert_eq!(record.context_used, Some(1200));
        assert_eq!(record.since, 3);
        assert_eq!(state.sessions.running().count(), 1);
        assert_eq!(state.ids.sessions, 4);

        state.apply(&envelope(
            6,
            Some("card-001"),
            Some("s-004"),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end(
                EndReason::Paused,
                Some("v-2"),
            )))),
        ));
        let record = state.sessions.get(&key).unwrap();
        assert!(record.paused() && record.resumable());
        assert_eq!(record.vendor_session.as_deref(), Some("v-2"));
        assert_eq!(state.sessions.running().count(), 0);

        state.apply(&envelope(7, Some("card-001"), Some("s-004"), started()));
        let record = state.sessions.get(&key).unwrap();
        assert_eq!((record.starts, record.since), (2, 3));
        assert!(record.end.is_none());
    }

    #[test]
    fn vendor_health_quota_and_waits_are_folded() {
        let vendor = VendorId("codex".to_owned());
        let mut state = RunState::default();
        state.apply(&envelope(
            0,
            None,
            None,
            RunEventKind::Vendor(VendorEvent::Health(VendorHealth {
                vendor: vendor.clone(),
                state: HealthState::Down,
                since: OffsetDateTime::UNIX_EPOCH,
                reason: None,
                next_test_at: None,
                disabled: None,
            })),
        ));
        assert!(state.run.vendor_out(&vendor));
        state.apply(&envelope(
            1,
            None,
            None,
            RunEventKind::Handoff(HandoffEvent::Waiting {
                card: CardId::new("card-002").unwrap(),
                vendor: vendor.clone(),
                until: None,
            }),
        ));
        assert_eq!(state.run.vendor_waits.len(), 1);
        let worker = WorkerRef::new("claude-code", None);
        state.apply(&envelope(
            2,
            Some("card-002"),
            Some("s-001"),
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Owner,
            )))),
        ));
        assert!(state.run.vendor_waits.is_empty(), "the card has work again");
    }

    #[test]
    fn a_rejected_quota_counts_until_its_reset() {
        use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};

        let vendor = VendorId("claude-code".to_owned());
        let at = |seconds: i64| OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds);
        let reading = |status, resets_at: Option<i64>| {
            RunEventKind::Vendor(VendorEvent::Quota(QuotaReading {
                vendor: vendor.clone(),
                status,
                used: None,
                resets_at: resets_at.map(at),
                window: None,
                source: QuotaSource::RateLimitEvent,
                observed_at: at(0),
                detail: None,
                wrap_up: false,
            }))
        };
        let mut state = RunState::default();
        state.apply(&envelope(
            0,
            None,
            None,
            reading(QuotaStatus::Rejected, Some(100)),
        ));
        assert!(state.run.vendor_out(&vendor));
        assert!(state.run.vendor_out_at(&vendor, at(99)));
        assert!(!state.run.vendor_out_at(&vendor, at(100)));
        assert_eq!(state.run.next_quota_reset(at(50)), Some(100));
        assert_eq!(state.run.next_quota_reset(at(100)), None);

        // Without a reset it counts until a new reading says otherwise.
        state.apply(&envelope(
            1,
            None,
            None,
            reading(QuotaStatus::Rejected, None),
        ));
        assert!(state.run.vendor_out_at(&vendor, at(1_000)));
        assert_eq!(state.run.next_quota_reset(at(0)), None);
        state.apply(&envelope(
            2,
            None,
            None,
            reading(QuotaStatus::Allowed, Some(100)),
        ));
        assert!(!state.run.vendor_out(&vendor));
        assert!(state.run.quota_resets.is_empty());
    }

    #[test]
    fn a_snapshot_restores_the_state_it_was_taken_from() {
        let mut state = RunState::default();
        state.apply(&lifecycle(
            0,
            RunLifecycle::Started(Box::new(run_started(RunMode::Solo))),
        ));
        let worker = WorkerRef::new("codex", None);
        state.apply(&envelope(
            1,
            None,
            Some("s-001"),
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Default,
            )))),
        ));
        let snapshot = crate::engine::snapshot::build(
            &state,
            argonauts_core::api::StatusSummary::default(),
            vec![SessionKey::numbered(1)],
        )
        .unwrap();
        let from_snapshot = RunState::rebuild(&[lifecycle(
            2,
            RunLifecycle::Snapshot(Box::new(snapshot.clone())),
        )])
        .unwrap();
        let mut expected = state.clone();
        expected.apply(&lifecycle(2, RunLifecycle::Snapshot(Box::new(snapshot))));
        assert_eq!(from_snapshot, expected);
        assert_eq!(from_snapshot.run.last_snapshot, Some(2));
    }

    fn snapshot_of(engine: serde_json::Value) -> RunLifecycle {
        let mut snapshot = crate::engine::snapshot::build(
            &RunState::default(),
            argonauts_core::api::StatusSummary::default(),
            Vec::new(),
        )
        .unwrap();
        snapshot.engine = engine;
        RunLifecycle::Snapshot(Box::new(snapshot))
    }

    #[test]
    fn a_log_cut_at_a_snapshot_it_cant_restore_asks_for_a_replay() {
        let other = serde_json::json!({ "version": ENGINE_STATE_VERSION + 1, "state": {} });
        let broken = serde_json::json!({ "version": ENGINE_STATE_VERSION, "state": "not a state" });
        for engine in [other, broken] {
            let err = RunState::rebuild(&[lifecycle(7, snapshot_of(engine))]).unwrap_err();
            let text = err.to_string();
            assert!(text.contains("event 7"), "{text}");
            assert!(
                text.contains("replay the run's log from its start"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_snapshot_it_cant_restore_is_skipped_in_a_log_read_from_its_start() {
        let started = lifecycle(
            0,
            RunLifecycle::Started(Box::new(run_started(RunMode::Solo))),
        );
        let other = serde_json::json!({ "version": ENGINE_STATE_VERSION + 1, "state": {} });
        let log = [started, lifecycle(1, snapshot_of(other))];
        let state = RunState::rebuild(&log).unwrap();
        assert_eq!(state.run.mode, Some(RunMode::Solo));
        assert_eq!(state.seq, Some(1));
        assert_eq!(RunState::fold(&log), state);
    }

    #[test]
    fn an_event_recorded_between_building_and_appending_a_snapshot_stays() {
        let mut state = RunState::default();
        state.apply(&lifecycle(
            0,
            RunLifecycle::Started(Box::new(run_started(RunMode::Solo))),
        ));
        let snapshot = crate::engine::snapshot::build(
            &state,
            argonauts_core::api::StatusSummary::default(),
            Vec::new(),
        )
        .unwrap();
        let worker = WorkerRef::new("codex", None);
        state.apply(&envelope(
            1,
            None,
            Some("s-001"),
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Default,
            )))),
        ));
        state.apply(&lifecycle(
            2,
            RunLifecycle::Snapshot(Box::new(snapshot.clone())),
        ));
        assert!(
            state.sessions.get(&SessionKey::numbered(1)).is_some(),
            "the older snapshot didn't replace the newer state"
        );
        assert_eq!(state.run.last_snapshot, Some(2));
        assert_eq!(state.ids.last(ids::IdKind::Session), 1);

        // A log that starts at the snapshot is folded from it.
        let resumed = RunState::fold(&[lifecycle(2, RunLifecycle::Snapshot(Box::new(snapshot)))]);
        assert!(resumed.run.started.is_some());
        assert!(resumed.sessions.get(&SessionKey::numbered(1)).is_none());
    }
}
