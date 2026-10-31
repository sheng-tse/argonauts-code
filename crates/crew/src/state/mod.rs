//! The run's state, folded from its event log. The fold is pure: the same log always gives the
//! same state, which is how a recovered run picks up exactly where it stopped. Each part folds
//! its own events and is owned by the module that records them.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use argonauts_core::runs::events::run::{RunLifecycle, RunSnapshot};
use argonauts_core::runs::events::{RunEvent, RunEventKind};

use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::events::session::{EndReason, SessionEvent};

use crate::engine::error::{EngineError, EngineResult};

pub mod approvals;
pub mod blackboard;
pub mod divide;
pub mod goal;
pub mod ids;
pub mod merge;
pub mod plan;
pub mod run;
pub mod run_review;
pub mod sessions;
pub mod tasks;
pub mod workflow;

/// Bumped whenever the snapshot's shape changes. A snapshot of another version can't be
/// restored, so a run that has one needs its log replayed from the start.
pub const ENGINE_STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    /// The last event folded in.
    pub seq: Option<u64>,
    pub run: run::RunInfo,
    pub ids: ids::Counters,
    pub sessions: sessions::Sessions,
    pub approvals: approvals::Approvals,
    pub workflows: workflow::Workflows,
    pub goal: goal::GoalState,
    pub plan: plan::PlanState,
    pub divide: divide::DivideState,
    pub tasks: tasks::Tasks,
    pub merge: merge::MergeState,
    pub blackboard: blackboard::Board,
    pub run_review: run_review::RunReviewState,
    /// Each council's talk, folded from its events.
    #[serde(default)]
    pub councils: crate::council::talk::Councils,
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

    /// Whether a `›` note whose first line is `line` was written after session `about` started,
    /// or anywhere in the log when there's no session it's about. A step that replays after a
    /// stop asks, so it says why once, and a later session with the same trouble gets its own.
    pub fn said_after(&self, line: &str, about: Option<&SessionKey>) -> bool {
        let since = about
            .and_then(|key| self.sessions.get(key))
            .map_or(0, |session| session.since);
        self.run.said.get(line).is_some_and(|&seq| seq > since)
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
            RunEventKind::Task(e) => self.tasks.apply(e, event),
            RunEventKind::Session(e) => {
                if let (SessionEvent::Started(_), Some(task)) = (e, &event.task_id) {
                    self.run.task_active(task);
                }
                if let SessionEvent::Started(started) = e {
                    self.run.job_started(started, event.task_id.as_ref());
                }
                self.sessions.apply(e, event);
                // An agent got through a session, so the next wait where no agent could take a job
                // over starts its waits over.
                if let SessionEvent::Ended(end) = e
                    && end.reason == EndReason::Completed
                {
                    self.run.fallback_waits = 0;
                }
                if let (SessionEvent::Ended(end), Some(key)) = (e, &event.session)
                    && end.reason == EndReason::Stopped
                    && self.run.stop_requested
                    && let Some(record) = self.sessions.by_key.get_mut(key)
                {
                    record.stopped_with_run = true;
                }
            }
            RunEventKind::Agent(e) => self.sessions.apply_agent(e, event),
            RunEventKind::Check(e) => self.tasks.apply_check(e, event),
            RunEventKind::Review(e) => self.tasks.apply_review(e, event),
            RunEventKind::Merge(e) => self.merge.apply(e, event),
            RunEventKind::Approval(e) => {
                self.run.apply_approval(e, event);
                self.approvals.apply(e, event);
            }
            RunEventKind::Fallback(e) => {
                self.run.apply_fallback(e, event);
                self.tasks.apply_fallback(e, event);
            }
            RunEventKind::AgentStatus(e) => self.run.apply_agent(e, event),
            RunEventKind::Spend(e) => self.run.apply_limit(e, event),
            RunEventKind::Blackboard(e) => self.blackboard.apply(e, event),
            RunEventKind::RunReview(e) => self.run_review.apply(e, event),
            RunEventKind::Note(note) => self.run.noted(note, event.seq),
            RunEventKind::Workflow(e) => self.workflows.apply(e, event),
            RunEventKind::Council(e) => self.councils.apply_at(e, event.ts),
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

    use argonauts_core::agents::session_runner::{Access, RunnerKind};
    use argonauts_core::base::names::{AgentId, ModelRef, RoleName, SessionKey};
    use argonauts_core::models::limits::{AgentHealth, HealthState};
    use argonauts_core::runs::event::AgentEvent;
    use argonauts_core::runs::events::agent_status::{AgentStatusEvent, FallbackEvent};
    use argonauts_core::runs::events::run::{
        Party, ResumeCause, RunMode, RunOutcome, RunResumed, RunStarted, WaitReason,
    };
    use argonauts_core::runs::events::session::{EndReason, SessionEnd, SessionStarted};
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::sessions::SessionLifetime;
    use argonauts_core::{RunId, Spend, TaskId};

    use crate::state::sessions::SessionRecord;

    pub(crate) fn envelope(
        seq: u64,
        task: Option<&str>,
        session: Option<&str>,
        kind: RunEventKind,
    ) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seq as i64),
            run_id: RunId::new("run-01").unwrap(),
            task_id: task.map(|task| TaskId::new(task).unwrap()),
            session: session.map(|key| SessionKey::parse(key).unwrap()),
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    pub(crate) fn run_started(mode: RunMode) -> RunStarted {
        RunStarted {
            request: "add a flag".to_owned(),
            title: "Add a flag".to_owned(),
            mode,
            forced: false,
            workflow: "council".to_owned(),
            estimate: None,
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: "/repo".into(),
            base_branch: Some("main".to_owned()),
            base_commit: "abc".to_owned(),
            uncommitted: Vec::new(),
            uncommitted_included: false,
            settings_sha256: "hash".to_owned(),
            settings: serde_json::Value::Null,
            overrides: Vec::new(),
            cli_versions: BTreeMap::new(),
            version: "0.0.1".to_owned(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
            starts_from: None,
        }
    }

    pub(crate) fn session_started(worker: &ModelRef, role: RoleName) -> SessionStarted {
        SessionStarted {
            worker: worker.clone(),
            agent_id: worker.agent_id(),
            provider: "fake".to_owned(),
            kind: RunnerKind::Cli,
            role,
            step: None,
            access: Access::ReadOnly,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: None,
            asked: None,
            asked_by: None,
            asked_why: None,
            forked: false,
            seat: None,
            billing: None,
        }
    }

    pub(crate) fn session_record(key: &str, worker: &ModelRef, role: RoleName) -> SessionRecord {
        SessionRecord {
            key: SessionKey::parse(key).unwrap(),
            task: None,
            started: session_started(worker, role),
            since: 0,
            starts: 1,
            cli_session: None,
            transcript: None,
            context_used: None,
            context_window: None,
            end: None,
            stopped_with_run: false,
            asked: None,
            agent_failures: Vec::new(),
            written: Default::default(),
            network_retries: 0,
            workflow: None,
            workdir: None,
        }
    }

    pub(crate) fn end(reason: EndReason, agent_id: Option<&str>) -> SessionEnd {
        SessionEnd {
            cli_session: agent_id.map(str::to_owned),
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

    /// An open approval as the approval fold would hold it. Built through serde, so
    /// fields the fold adds with defaults don't break it.
    pub(crate) fn open_approval(
        n: u32,
        kind: argonauts_core::runs::events::approval::ApprovalKind,
        needs_you: bool,
    ) -> crate::state::approvals::ApprovalRecord {
        use argonauts_core::runs::events::approval::{ApprovalOpened, ApprovalPacket, FreeText};
        let opened = ApprovalOpened {
            id: argonauts_core::base::names::ApprovalId::numbered(n),
            kind,
            title: "Approval".to_owned(),
            task: None,
            packet: ApprovalPacket::WorkflowPromotion(serde_json::json!({})),
            evidence_sha256: "evidence".to_owned(),
            mode: ApprovalMode::Supervised,
            decide_with: argonauts_core::settings::approvals::DecideWith::Rules,
            when_declined: argonauts_core::settings::asking::WhenDeclined::Decide,
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
        serde_json::from_value(record).expect("an approval record")
    }

    fn lifecycle(seq: u64, event: RunLifecycle) -> RunEvent {
        envelope(seq, None, None, RunEventKind::Run(event))
    }

    #[test]
    fn the_run_fold_tracks_start_pause_stop_wait_and_finish() {
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
            RunLifecycle::Waiting {
                reason: WaitReason::Paused,
                waiting_on: Vec::new(),
            },
        ));
        assert_eq!(state.run.waiting, Some(WaitReason::Paused));
        state.apply(&lifecycle(
            5,
            RunLifecycle::Resumed(RunResumed {
                cause: ResumeCause::User,
                settings_changed: false,
                cli_versions_changed: Vec::new(),
                main_moved_by: None,
                sessions: Vec::new(),
            }),
        ));
        assert!(!state.run.paused);
        assert_eq!(state.run.waiting, None);
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

    /// A note says why once for the session it's about, however often a stopped run replays to
    /// it, and anew for a later session with the same trouble, such as a nested workflow's
    /// change refused after its own phase 1.
    #[test]
    fn a_note_is_said_after_the_session_it_is_about() {
        use argonauts_core::Note;
        use argonauts_core::base::log::NoteLevel;

        let worker = ModelRef::new("claude-code", None);
        let started = |seq: u64, key: &str| {
            let started = session_started(&worker, RoleName::TieBreaker);
            let kind = RunEventKind::Session(SessionEvent::Started(Box::new(started)));
            envelope(seq, None, Some(key), kind)
        };
        let line = "The main agent's change after phase 1 wrote a workflow that breaks its rules; it's \
                    fixing that.";
        let noted = |seq: u64| {
            let note = Note::with_detail(NoteLevel::Info, line, "phase 1 already ran");
            envelope(seq, None, None, RunEventKind::Note(note))
        };
        let (first, later) = (SessionKey::numbered(1), SessionKey::numbered(3));
        let mut state = RunState::default();
        state.apply(&started(1, "s-001"));
        assert!(!state.said_after(line, Some(&first)));
        assert!(!state.said_after(line, None));
        state.apply(&noted(2));
        assert!(state.said_after(line, Some(&first)));
        assert!(state.said_after(line, None));
        state.apply(&started(3, "s-003"));
        assert!(!state.said_after(line, Some(&later)));
        assert!(state.said_after(line, Some(&first)));
        state.apply(&noted(4));
        assert!(state.said_after(line, Some(&later)));
    }

    #[test]
    fn the_session_fold_keeps_agent_ids_ends_and_resumes() {
        let worker = ModelRef::new("codex", None);
        let started = || {
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Worker,
            ))))
        };
        let mut state = RunState::default();
        state.apply(&envelope(3, Some("task-001"), Some("s-004"), started()));
        state.apply(&envelope(
            4,
            Some("task-001"),
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
            Some("task-001"),
            Some("s-004"),
            RunEventKind::Agent(AgentEvent::ContextFill {
                used: 1200,
                size: None,
                sent: None,
            }),
        ));
        let key = SessionKey::numbered(4);
        let record = state.sessions.get(&key).unwrap();
        assert_eq!(record.cli_session.as_deref(), Some("v-1"));
        assert_eq!(record.context_used, Some(1200));
        assert_eq!(record.since, 3);
        assert_eq!(state.sessions.running().count(), 1);
        assert_eq!(state.ids.sessions, 4);

        state.apply(&envelope(
            6,
            Some("task-001"),
            Some("s-004"),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end(
                EndReason::Paused,
                Some("v-2"),
            )))),
        ));
        let record = state.sessions.get(&key).unwrap();
        assert!(record.paused() && record.resumable());
        assert_eq!(record.cli_session.as_deref(), Some("v-2"));
        assert_eq!(state.sessions.running().count(), 0);

        state.apply(&envelope(7, Some("task-001"), Some("s-004"), started()));
        let record = state.sessions.get(&key).unwrap();
        assert_eq!((record.starts, record.since), (2, 3));
        assert!(record.end.is_none());

        // A spend limit cut it with its CLI session kept: it carries on once the limit allows.
        let limit = EndReason::LimitHit {
            dimension: argonauts_core::LimitDimension::Spend,
        };
        state.apply(&envelope(
            8,
            Some("task-001"),
            Some("s-004"),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end(
                limit.clone(),
                Some("v-2"),
            )))),
        ));
        assert!(state.sessions.get(&key).unwrap().resumable());
        state.apply(&envelope(9, Some("task-001"), Some("s-004"), started()));
        state.apply(&envelope(
            10,
            Some("task-001"),
            Some("s-004"),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end(limit, None)))),
        ));
        assert!(
            !state.sessions.get(&key).unwrap().resumable(),
            "with no CLI session there is nothing to carry on"
        );
    }

    #[test]
    fn agent_health_plan_usage_and_waits_are_folded() {
        let agent_id = AgentId("codex".to_owned());
        let mut state = RunState::default();
        state.apply(&envelope(
            0,
            None,
            None,
            RunEventKind::AgentStatus(AgentStatusEvent::Health(AgentHealth {
                agent_id: agent_id.clone(),
                state: HealthState::Down,
                since: OffsetDateTime::UNIX_EPOCH,
                reason: None,
                next_test_at: None,
                disabled: None,
            })),
        ));
        assert!(state.run.agent_out(&agent_id));
        state.apply(&envelope(
            1,
            None,
            None,
            RunEventKind::Fallback(FallbackEvent::Waiting {
                task: TaskId::new("task-002").unwrap(),
                agent_id: agent_id.clone(),
                until: None,
                kept_off: None,
            }),
        ));
        assert_eq!(state.run.agent_waits.len(), 1);
        let worker = ModelRef::new("claude-code", None);
        state.apply(&envelope(
            2,
            Some("task-002"),
            Some("s-001"),
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Worker,
            )))),
        ));
        assert!(state.run.agent_waits.is_empty(), "the task has work again");
    }

    #[test]
    fn a_rejected_plan_usage_counts_until_its_reset() {
        use argonauts_core::models::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};

        let agent_id = AgentId("claude-code".to_owned());
        let at = |seconds: i64| OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds);
        let reading = |status, resets_at: Option<i64>| {
            RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(PlanUsageReading {
                agent_id: agent_id.clone(),
                status,
                used: None,
                resets_at: resets_at.map(at),
                window: None,
                source: PlanUsageSource::RateLimitEvent,
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
            reading(PlanUsageStatus::Rejected, Some(100)),
        ));
        assert!(state.run.agent_out(&agent_id));
        assert!(state.run.agent_out_at(&agent_id, at(99)));
        assert!(!state.run.agent_out_at(&agent_id, at(100)));
        assert_eq!(state.run.next_plan_usage_reset(at(50)), Some(100));
        assert_eq!(state.run.next_plan_usage_reset(at(100)), None);

        // Without a reset it counts until a new reading says otherwise.
        state.apply(&envelope(
            1,
            None,
            None,
            reading(PlanUsageStatus::Rejected, None),
        ));
        assert!(state.run.agent_out_at(&agent_id, at(1_000)));
        assert_eq!(state.run.next_plan_usage_reset(at(0)), None);
        state.apply(&envelope(
            2,
            None,
            None,
            reading(PlanUsageStatus::Allowed, Some(100)),
        ));
        assert!(!state.run.agent_out(&agent_id));
        assert!(state.run.plan_usage_resets.is_empty());
    }

    #[test]
    fn a_snapshot_restores_the_state_it_was_taken_from() {
        let mut state = RunState::default();
        state.apply(&lifecycle(
            0,
            RunLifecycle::Started(Box::new(run_started(RunMode::Captain))),
        ));
        let worker = ModelRef::new("codex", None);
        state.apply(&envelope(
            1,
            None,
            Some("s-001"),
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Main,
            )))),
        ));
        let snapshot = crate::engine::snapshot::build(
            &state,
            argonauts_core::runs::api::StatusSummary::default(),
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
            argonauts_core::runs::api::StatusSummary::default(),
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
            RunLifecycle::Started(Box::new(run_started(RunMode::Captain))),
        );
        let other = serde_json::json!({ "version": ENGINE_STATE_VERSION + 1, "state": {} });
        let log = [started, lifecycle(1, snapshot_of(other))];
        let state = RunState::rebuild(&log).unwrap();
        assert_eq!(state.run.mode, Some(RunMode::Captain));
        assert_eq!(state.seq, Some(1));
        assert_eq!(RunState::fold(&log), state);
    }

    #[test]
    fn an_event_recorded_between_building_and_appending_a_snapshot_stays() {
        let mut state = RunState::default();
        state.apply(&lifecycle(
            0,
            RunLifecycle::Started(Box::new(run_started(RunMode::Captain))),
        ));
        let snapshot = crate::engine::snapshot::build(
            &state,
            argonauts_core::runs::api::StatusSummary::default(),
            Vec::new(),
        )
        .unwrap();
        let worker = ModelRef::new("codex", None);
        state.apply(&envelope(
            1,
            None,
            Some("s-001"),
            RunEventKind::Session(SessionEvent::Started(Box::new(session_started(
                &worker,
                RoleName::Main,
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
